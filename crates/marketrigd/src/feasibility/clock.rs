//! Controlled-clock harness for the A-share feasibility spike
//! (`sdd/features/a-share-engine/FEASIBILITY.md`, Part A). Test-only.
//!
//! # What it does
//!
//! The seam itself is production code now (slice 014 step 1): the registration,
//! the shared-`TestClock` memo, the `with_clock_factory` install and the
//! `ControlledSandboxFactory` swap all live in `crate::node`, where the daemon
//! reads them from `MARKETRIG_TEST_CLOCK_NS` alongside the data-root seam, and
//! `crate::node::controlled_registry` registers one desk in-process for a module
//! check. What remains here is the spike's own vocabulary over it: the day
//! frame, hand-published data, and clock alerts.
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

use std::rc::Rc;

use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_common::timer::{TimeEvent, TimeEventCallback};
use nautilus_core::UnixNanos;
use nautilus_model::data::{Data, QuoteTick};
use nautilus_model::identifiers::InstrumentId;
use nautilus_model::types::{Price, Quantity};

use crate::catalog::Entry;
use crate::node::{Node, NodeContext};
use crate::store::Store;

/// The registration, the memo, the sandbox factory swap and the `build` seam all
/// live in `crate::node` now, as always-compiled code: the production daemon
/// takes exactly this path under `MARKETRIG_TEST_CLOCK_NS`
/// (`crate::node::TEST_CLOCK_ENV`), and a module check registers one desk
/// in-process instead. What remains here is the spike's own vocabulary over it.
pub(crate) use crate::node::{ClockHandle, advance, controlled_registry, now_ns_of as now_ns};

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
// The spike's day frame
// ---------------------------------------------------------------------------

/// 2026-09-09 09:35:00 Asia/Shanghai == 2026-09-09T01:35:00Z, in nanoseconds.
/// Every feasibility test starts inside the CN morning session so the injected
/// instant is meaningful to §5.1.
pub(crate) const CN_0935: u64 = crate::node::CN_0935;

/// 2026-09-09 14:57:00 Asia/Shanghai — the §5.1 day-lifetime boundary — as an
/// offset from [`CN_0935`] (5h22m).
pub(crate) const CN_1457: u64 = CN_0935 + 19_320 * SECOND_NS;

pub(crate) const SECOND_NS: u64 = crate::node::SECOND_NS;

/// One trading day.
pub(crate) const DAY_NS: u64 = crate::node::DAY_NS;
