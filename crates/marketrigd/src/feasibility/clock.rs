//! Controlled-clock harness for the A-share feasibility spike
//! (`sdd/features/a-share-engine/FEASIBILITY.md`, Part A). Test-only.
//!
//! # What it does
//!
//! [`controlled_registry`] seeds a desk, registers it for controlled time, and
//! hands back the [`Registry`] that will start its node plus a [`ClockHandle`]
//! naming the desk. When [`Registry::ensure`] builds that desk's node,
//! `crate::node::build` reads the registration ([`controlled`]) and does two
//! things:
//!
//! 1. installs `LiveNodeBuilder::with_clock_factory` returning **one shared**
//!    `Rc<RefCell<TestClock>>` (`ClockFactory` memoizes the kernel clock and
//!    calls the closure again per component clock, so returning clones of one
//!    `Rc` makes kernel and component clocks the same instance);
//! 2. swaps `SandboxExecutionClientFactory` for
//!    [`ControlledSandboxFactory`], which builds the very same
//!    `SandboxExecutionClient` with that shared clock.
//!
//! Step 2 is **required**: `nautilus-sandbox-0.62.0/src/factory.rs:80`
//! hard-codes `LiveClock::default()` for the sandbox client, and the sandbox
//! passes that clock to every `OrderMatchingEngine` it creates
//! (`execution.rs:197`). Without the swap the clock factory alone leaves every
//! order/fill event stamped with wall-clock time.
//!
//! The seam in `crate::node::build` is `#[cfg(test)]`-gated on both branches, so
//! a non-test build compiles exactly the code it compiled before.
//!
//! # Driving time
//!
//! [`advance`] runs on the node thread through [`Node::call`], downcasts
//! `NodeContext.clock` to `TestClock`, calls `advance_time(to, true)`, then
//! `match_handlers` and `TimeEventHandler::run()` for each returned event —
//! **the test dispatches time events by hand**. A `TestClock` in a live node has
//! no runner draining it; nothing fires unless [`advance`] is called.
//! `set_time` is passed `true`, so `Clock::timestamp_ns()` reads the injected
//! instant afterwards. `advance_time` panics on a decreasing time.
//!
//! [`alert`] registers a one-shot callback on the same clock from the node
//! thread; [`publish_quote`] publishes a `QuoteTick` with chosen
//! `ts_event`/`ts_init` through the runner's data sender, which is how a test
//! delivers market data without the polling feed.
//!
//! # Limits — what a `TestClock` does NOT drive in a live node
//!
//! - **Nothing fires on its own.** `LiveClock` spawns tokio timer tasks; a
//!   `TestClock` only queues. A started node owns exactly eight timers — per
//!   venue, the sandbox's expired-engine sweep
//!   (`nautilus-sandbox-0.62.0/src/execution.rs:694-730`) and the portfolio's
//!   equity-curve sample, which is armed for the next UTC midnight and repeats
//!   daily (`nautilus-portfolio-0.62.0/src/portfolio.rs:4050-4090`). Both fire
//!   only when [`advance`] reaches them *and* dispatches the handler, so an
//!   advance that crosses UTC midnight will emit portfolio snapshots.
//! - **Order expiry is not on a timer at all.** The matching engine expires GTD
//!   orders only inside `iterate(...)`, which runs from data processing
//!   (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:1590`, `1615`,
//!   `3738-3760`) and uses the *data's* `ts_init`, not the clock. Advancing the
//!   clock past an `expire_time` expires nothing until a tick arrives.
//! - **MarketRig's own `store::now_ns()` stays wall-clock.** Rows the daemon
//!   stamps itself (`trading_actions.created_at_ns`, `book_snapshots`,
//!   `operational_events`) keep real time; only NautilusTrader event stamps
//!   (`order_events.occurred_at_ns`, which is `event.ts_event()`) follow the
//!   injected clock.
//! - **The polling feed stays wall-clock.** `crate::node::synthesized` stamps a
//!   polled quote with `now_ns()`, so a feed-driven tick carries real time even
//!   under a controlled clock. Use [`publish_quote`] when the tick's timestamp
//!   matters, and prefer `feed_base: None`.
//! - **The clock is thread-affine.** It exists only on the node thread; every
//!   read or write goes through [`Node::call`].

// A shared harness: F1–F6 each take a different subset of these helpers, and
// the spike's files land one at a time.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};

use nautilus_common::cache::Cache;
use nautilus_common::clients::ExecutionClient;
use nautilus_common::clock::{Clock, TestClock};
use nautilus_common::factories::{ClientConfig, SimulatedExecutionClientFactory};
use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_common::timer::{TimeEvent, TimeEventCallback};
use nautilus_core::UnixNanos;
use nautilus_execution::client::core::ExecutionClientCore;
use nautilus_model::data::{Data, QuoteTick};
use nautilus_model::identifiers::{ClientId, InstrumentId};
use nautilus_model::types::{Price, Quantity};
use nautilus_sandbox::{SandboxExecutionClient, SandboxExecutionClientConfig};

use crate::catalog::Entry;
use crate::feed::{FeedBase, MarketState};
use crate::node::{Node, NodeContext, Registry};
use crate::store::Store;

/// Desks that get a controlled clock, and the instant their clock starts at.
/// Keyed by desk UUID, so a controlled desk never disturbs the other module
/// checks running in the same test binary.
static CONTROLLED: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// The clock instance for a desk, on the thread that built its node. `build`
// calls `controlled` once for the kernel factory and once per venue for the
// exec factory; every call must answer the *same* clock, which is what this
// memo guarantees.
thread_local! {
    static CLOCKS: RefCell<HashMap<String, Rc<RefCell<TestClock>>>> =
        RefCell::new(HashMap::new());
}

/// The seam `crate::node::build` reads: the desk's shared `TestClock`, or `None`
/// for an ordinary desk.
pub(crate) fn controlled(desk_id: &str) -> Option<Rc<RefCell<TestClock>>> {
    let start_ns = *CONTROLLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(desk_id)?;
    Some(CLOCKS.with_borrow_mut(|clocks| {
        Rc::clone(clocks.entry(desk_id.to_owned()).or_insert_with(|| {
            let mut clock = TestClock::new();
            clock.advance_time(UnixNanos::from(start_ns), true);
            Rc::new(RefCell::new(clock))
        }))
    }))
}

/// The sandbox factory with the shared clock injected — the same
/// `SandboxExecutionClient` the daemon builds, differing only in which clock it
/// and its matching engines read.
#[derive(Debug)]
pub(crate) struct ControlledSandboxFactory(pub Rc<RefCell<TestClock>>);

impl SimulatedExecutionClientFactory for ControlledSandboxFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        cache: Rc<RefCell<Cache>>,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let config = config
            .as_any()
            .downcast_ref::<SandboxExecutionClientConfig>()
            .ok_or_else(|| anyhow::anyhow!("{name} needs a SandboxExecutionClientConfig"))?
            .clone();
        let core = ExecutionClientCore::new(
            config.trader_id,
            ClientId::from(name),
            config.venue,
            config.oms_type,
            config.account_id,
            config.account_type,
            config.base_currency,
            cache.clone(),
        );
        let clock: Rc<RefCell<dyn Clock>> = Rc::clone(&self.0) as Rc<RefCell<dyn Clock>>;
        Ok(Box::new(SandboxExecutionClient::new(
            core, config, clock, cache,
        )))
    }

    fn name(&self) -> &str {
        "SANDBOX"
    }

    fn config_type(&self) -> &str {
        "SandboxExecutionClientConfig"
    }
}

/// Names the controlled desk. Dropping it unregisters the desk, so a later test
/// reusing the same store never inherits controlled time.
#[derive(Debug)]
pub(crate) struct ClockHandle {
    desk_id: String,
    start_ns: u64,
}

impl ClockHandle {
    pub(crate) fn desk_id(&self) -> &str {
        &self.desk_id
    }

    /// The instant the clock was seeded at.
    pub(crate) fn start_ns(&self) -> u64 {
        self.start_ns
    }
}

impl Drop for ClockHandle {
    fn drop(&mut self) {
        CONTROLLED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.desk_id);
    }
}

/// A registry whose `desk_name` desk runs on a `TestClock` seeded at `start_ns`.
/// The desk row is seeded here; the node starts on the first
/// [`Registry::ensure`].
pub(crate) fn controlled_registry(
    store: &Store,
    feed_base: Option<FeedBase>,
    desk_name: &'static str,
    start_ns: u64,
) -> (Registry, ClockHandle) {
    let desk_id = crate::node::seeded_desk(store, desk_name);
    CONTROLLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(desk_id.clone(), start_ns);
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), feed_base);
    (registry, ClockHandle { desk_id, start_ns })
}

/// Advances the node's clock to `to_ns` and dispatches every time event it
/// releases, on the node thread. Returns the dispatched timer names in order.
///
/// Panics if the desk is not on a controlled clock, or if `to_ns` moves
/// backwards.
pub(crate) fn advance(node: &Node, to_ns: u64) -> Vec<String> {
    node.call(move |context| {
        let handlers = {
            let mut clock = context.clock.borrow_mut();
            let test = clock
                .as_any_mut()
                .downcast_mut::<TestClock>()
                .expect("this desk is not on a controlled clock");
            let events = test.advance_time(UnixNanos::from(to_ns), true);
            test.match_handlers(events)
        };
        let mut fired = Vec::new();
        for handler in handlers {
            fired.push(handler.event.name.to_string());
            handler.run();
        }
        fired
    })
    .expect("the node answers")
}

/// The node clock's current instant.
pub(crate) fn now_ns(node: &Node) -> u64 {
    node.call(|context| context.clock.borrow().timestamp_ns().as_u64())
        .expect("the node answers")
}

/// Registers a one-shot alert on the node clock. `make` runs on the node thread
/// and builds the callback from the node's own context (cache, trader id), so
/// the alert can act on the book when [`advance`] passes `at_ns`. Returns the
/// clock's own error unchanged — `set_time_alert_ns` is called with
/// `allow_past: false`, so an alert in the past is an error, not a silent
/// immediate firing.
pub(crate) fn alert(
    node: &Node,
    name: &'static str,
    at_ns: u64,
    make: impl FnOnce(&NodeContext) -> Rc<dyn Fn(TimeEvent)> + Send + 'static,
) -> Result<(), String> {
    node.call(move |context| {
        let callback = make(context);
        context
            .clock
            .borrow_mut()
            .set_time_alert_ns(
                name,
                UnixNanos::from(at_ns),
                Some(TimeEventCallback::from(callback)),
                Some(false),
            )
            .map_err(|e| e.to_string())
    })
    .expect("the node answers")
}

/// Publishes one synthesized quote with chosen timestamps, straight into the
/// node's data runner — the same `DataEvent` path `crate::node::poll_once`
/// uses, without the feed. Both sides carry `price`, both sizes `size` lots.
///
/// The matching engine keys its post-match maintenance off `ts_init`
/// (`iterate(quote.ts_init, ...)`), so that is the instant a fill or a GTD
/// expiry is judged against.
pub(crate) fn publish_quote(
    node: &Node,
    entry: &'static Entry,
    price: &str,
    size: u32,
    ts_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price = Price::from(price);
    let size = Quantity::from(size);
    node.call(move |_| {
        let tick = QuoteTick::new(
            instrument_id,
            price,
            price,
            size,
            size,
            UnixNanos::from(ts_ns),
            UnixNanos::from(ts_ns),
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::Quote(tick)))
            .expect("the data runner takes the quote");
    })
    .expect("the node answers");
}

/// Publishes a quote with independent bid and ask, for tests that need one side
/// suppressed (F4's zeroed side) or a one-sided cross.
pub(crate) fn publish_book(
    node: &Node,
    entry: &'static Entry,
    bid: (&str, u32),
    ask: (&str, u32),
    ts_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let (bid_price, bid_size) = (Price::from(bid.0), Quantity::from(bid.1));
    let (ask_price, ask_size) = (Price::from(ask.0), Quantity::from(ask.1));
    node.call(move |_| {
        let tick = QuoteTick::new(
            instrument_id,
            bid_price,
            ask_price,
            bid_size,
            ask_size,
            UnixNanos::from(ts_ns),
            UnixNanos::from(ts_ns),
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::Quote(tick)))
            .expect("the data runner takes the quote");
    })
    .expect("the node answers");
}

/// The `order_events` rows the desk captured, oldest first: `(client_order_id,
/// kind, occurred_at_ns)`. `occurred_at_ns` is the event's own `ts_event`
/// (`crate::trade::event_row`), which is what proves the injected clock reached
/// persistence.
pub(crate) fn stored_events(store: &Store, desk_id: &str) -> Vec<(String, String, i64)> {
    let desk = desk_id.to_owned();
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT client_order_id, kind, occurred_at_ns FROM order_events \
                 WHERE desk_id = ?1 ORDER BY occurred_at_ns, id",
            )?
            .query_map([desk], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .expect("the order events read")
}

// ---------------------------------------------------------------------------
// Part A — the controlled-clock seam
// ---------------------------------------------------------------------------

/// 2026-09-09 09:35:00 Asia/Shanghai == 2026-09-09T01:35:00Z, in nanoseconds.
/// Every feasibility test starts inside the CN morning session so the injected
/// instant is meaningful to §5.1.
pub(crate) const CN_0935: u64 = 1_788_917_700_000_000_000;

/// 2026-09-09 14:57:00 Asia/Shanghai — the §5.1 day-lifetime boundary — as an
/// offset from [`CN_0935`] (5h22m).
pub(crate) const CN_1457: u64 = CN_0935 + 19_320 * SECOND_NS;

pub(crate) const SECOND_NS: u64 = 1_000_000_000;

/// One trading day.
pub(crate) const DAY_NS: u64 = 86_400 * SECOND_NS;

/// Part A, questions 1–3: the real `LiveNode` runs on an injected `TestClock`,
/// one instance is shared by the kernel and the sandbox matching engines, the
/// test advances it and dispatches its time events by hand, and the stamps
/// NautilusTrader puts on order events — and MarketRig stores — are the injected
/// instant.
#[test]
fn clock_seam_drives_node() {
    let aapl = crate::catalog::find("AAPL.XNAS").unwrap();
    let aapl_id = InstrumentId::from(aapl.instrument_id);
    let (_dir, store) = crate::store::open_temp();

    // No feed: every quote in this test is published by hand, with a timestamp
    // the test chose (the polling feed stamps `now_ns()`, see the module docs).
    let (registry, handle) = controlled_registry(&store, None, "clock-seam", CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");

    // 1. The node built and reached `Running` with a `TestClock` as its kernel
    //    clock, seeded at the injected instant.
    assert_eq!(
        now_ns(&node),
        CN_0935,
        "the kernel clock is the injected one"
    );

    // The data engine and the sandbox client are alive: a hand-published quote
    // reaches the node's cache through the ordinary runner path.
    publish_quote(&node, aapl, "316.85", 100, CN_0935);
    crate::node::within(10, "the published quote reaches the cache", || {
        node.call(move |context| context.cache.borrow().quote(&aapl_id).is_some())
            .unwrap()
    });

    // 2. Advancing is a test call, and the events it releases are dispatched by
    //    hand. The sandbox registers its expired-engine sweep timer on the very
    //    clock we injected — proof the sandbox shares this instance — and that
    //    timer fires only because `advance` dispatched it.
    let timers = node
        .call(|context| {
            context
                .clock
                .borrow()
                .timer_names()
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<String>>()
        })
        .unwrap();
    // Every timer this node owns, so the harness's users know exactly what a
    // `TestClock` stalls: per venue, the sandbox's expired-engine sweep and the
    // portfolio's daily equity-curve sample. Nothing else — no engine, no order
    // path, and no MarketRig code depends on a timer firing on its own.
    let sweeps = timers
        .iter()
        .filter(|name| name.ends_with("-sandbox-expiry-sweep"))
        .count();
    let curves = timers
        .iter()
        .filter(|name| name.starts_with("portfolio_equity_curve."))
        .count();
    assert_eq!((sweeps, curves, timers.len()), (4, 4, 8), "{timers:?}");
    let fired = advance(&node, CN_0935 + 60 * 1_000_000_000);
    assert_eq!(now_ns(&node), CN_0935 + 60 * 1_000_000_000);
    assert!(
        fired.iter().any(|t| t.ends_with("-sandbox-expiry-sweep")),
        "the sweep timer fired under the controlled clock: {fired:?}"
    );

    // 3. A market order against the standing quote: every NautilusTrader stamp
    //    on it is the injected instant, and that is what reached the database.
    let at = CN_0935 + 60 * 1_000_000_000;
    let (record, _) = crate::trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"clock-buy-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"MARKET","quantity":"10","price":null}"#,
        &crate::trade::Source::Session,
    )
    .expect("the market buy is accepted");
    let outcome = record.outcome.clone().unwrap();
    assert_eq!(outcome["status"], "FILLED", "{outcome}");

    let stored = stored_events(&store, handle.desk_id());
    assert!(
        stored.iter().all(|(_, _, ns)| *ns == at as i64),
        "every stored order event carries the injected instant {at}: {stored:?}"
    );
    assert!(
        stored
            .iter()
            .any(|(id, kind, _)| id == "clock-buy-1" && kind == "OrderFilled"),
        "the fill was captured: {stored:?}"
    );

    // The fill row and the book snapshot see it too.
    let fill_ts: i64 = store
        .call(|conn| conn.query_row("SELECT occurred_at_ns FROM fills", [], |r| r.get(0)))
        .expect("the fill row");
    assert_eq!(
        fill_ts, at as i64,
        "the fills row carries the injected time"
    );

    registry.stop_all();
    let snapshot_ts: i64 = store
        .call(|conn| conn.query_row("SELECT written_at_ns FROM book_snapshots", [], |r| r.get(0)))
        .expect("the snapshot row");
    assert!(
        snapshot_ts > at as i64 * 2 || snapshot_ts != at as i64,
        "`book_snapshots.saved_at_ns` is MarketRig's own wall clock, not the \
         injected instant: {snapshot_ts} vs {at}"
    );
}
