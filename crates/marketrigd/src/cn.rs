//! The CN execution boundary: the supported session window, the day-lifetime
//! deadline, and the per-node critical section that publishes an executable
//! book and admits an order without the two overlapping.
//!
//! Every decision here reads the **node clock** (`NodeContext.clock`), never
//! `crate::store::now_ns()`: the session window, the owning trading date of a
//! restored order and the 14:57 deadline are all judged on the same time source
//! the sandbox stamps its events with (feature SPEC `a-share-engine` §5.1,
//! §5.3).
//!
//! The state in [`CnExec`] is per node, in memory only, and touched exclusively
//! on the node thread through [`Node::call`]. Nothing here is persisted: a
//! restart is a [`CnExec::reset`] (feature SPEC §2.5).
//!
//! Contract: `sdd/features/a-share-engine/SPEC.md` §2.5, §5.1, §5.3.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, TimeZone, Timelike, Weekday};
use chrono_tz::Tz;
use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_core::UnixNanos;
use nautilus_model::data::{Data, InstrumentStatus, QuoteTick};
use nautilus_model::enums::MarketStatusAction;
use nautilus_model::identifiers::InstrumentId;
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;

use crate::catalog::Entry;
use crate::node::{Node, NodeContext};

/// The reasons a CN execution attempt answers with (feature SPEC §2.1). They are
/// MarketRig vocabulary, never a NautilusTrader or HiThink word.
pub const PUBLICATION_FAILED: &str = "PUBLICATION_FAILED";
pub const PUBLICATION_PENDING: &str = "PUBLICATION_PENDING";
pub const NODE_NOT_STARTED: &str = "NODE_NOT_STARTED";

/// How long a caller waits for a publication to be visible in the node's cache,
/// and for the instrument to stop being busy. Both are bounded so a boundary
/// alert or a cancel never waits on a failed publish (feature SPEC §2.5).
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_POLL: Duration = Duration::from_millis(2);

/// The supported continuous sessions as minutes from Shanghai midnight,
/// half-open: [09:30,11:30) and [13:00,14:57) (feature SPEC §5.1). This is the
/// *execution* window and is deliberately narrower than [`crate::feed`]'s
/// market-phase table, whose 15:00 close is the market phase.
const MORNING: (u32, u32) = (9 * 60 + 30, 11 * 60 + 30);
const AFTERNOON: (u32, u32) = (13 * 60, 14 * 60 + 57);

/// The day-lifetime boundary, 14:57 Asia/Shanghai (feature SPEC §5.1).
const DEADLINE_MINUTE: (u32, u32) = (14, 57);

fn shanghai(at_ns: u64) -> DateTime<Tz> {
    DateTime::from_timestamp_nanos(at_ns.min(i64::MAX as u64) as i64)
        .with_timezone(&Tz::Asia__Shanghai)
}

/// The Shanghai calendar date of an instant, `yyyyMMdd` — the form the day
/// comparisons and the provider's own calendar use.
pub fn shanghai_date(at_ns: u64) -> String {
    shanghai(at_ns).format("%Y%m%d").to_string()
}

/// 14:57 Asia/Shanghai on the date `at_ns` falls in.
pub fn deadline_ns(at_ns: u64) -> u64 {
    let local = shanghai(at_ns);
    let naive = local
        .date_naive()
        .and_hms_opt(DEADLINE_MINUTE.0, DEADLINE_MINUTE.1, 0)
        .expect("14:57 is a valid time of day");
    Tz::Asia__Shanghai
        .from_local_datetime(&naive)
        .earliest()
        .and_then(|at| at.timestamp_nanos_opt())
        .map_or(at_ns, |ns| ns.max(0) as u64)
}

/// Is `at_ns` inside a supported continuous session on a weekday? The confirmed
/// trading day itself is the provider's answer and arrives with readiness; this
/// is the clock half of the gate.
pub fn in_session(at_ns: u64) -> bool {
    let local = shanghai(at_ns);
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minute = local.hour() * 60 + local.minute();
    [MORNING, AFTERNOON]
        .iter()
        .any(|&(open, close)| minute >= open && minute < close)
}

/// The `InstrumentStatus` the CN instruments carry at `at_ns` (feature SPEC
/// §5.3): `Trading` inside a supported session, `Close` outside it.
///
/// ponytail: the clock is the whole gate for now. Step 3 extends this with the
/// confirmed trading day and the per-instrument readiness, which turn a
/// clock-open instrument into `Pause` while its data is not usable.
pub fn status_for(at_ns: u64) -> MarketStatusAction {
    if in_session(at_ns) {
        MarketStatusAction::Trading
    } else {
        MarketStatusAction::Close
    }
}

/// The trading date a restored order belongs to: its own `ts_accepted` when the
/// venue accepted it, its `ts_init` otherwise (feature SPEC §5.3; F3 (6) — the
/// snapshot payload carries both, `book_snapshots.written_at_ns` is MarketRig's
/// wall clock and says nothing about the trading date).
pub fn owning_date(order: &OrderAny) -> String {
    let ts = order
        .ts_accepted()
        .unwrap_or_else(|| order.ts_init())
        .as_u64();
    shanghai_date(ts)
}

/// Has this order outlived its trading day at `now_ns`? Either it belongs to an
/// earlier Shanghai date, or today's 14:57 has passed. `yyyyMMdd` orders
/// lexicographically, so the comparison is the date comparison.
///
/// An order dated *after* `now_ns` is not expired: only a clock that moved
/// backwards can produce one, and terminating it would destroy a live order.
pub fn expired(order: &OrderAny, now_ns: u64) -> bool {
    owning_date(order).as_str() < shanghai_date(now_ns).as_str() || now_ns >= deadline_ns(now_ns)
}

// ---------------------------------------------------------------------------
// Per-node execution state
// ---------------------------------------------------------------------------

/// One CN instrument's execution state on this node.
///
/// ponytail: the AE-9 temporal fields (`seq`, `prev_volume`, per-order
/// baselines) are Step 3's and are not declared here, because a field nothing
/// reads is dead code under `-D warnings`. They join this struct with the code
/// that reads them.
#[derive(Debug)]
pub struct Inst {
    /// A crossing or sized book is in flight: admission waits rather than
    /// filling against it (F8 item 2).
    pub busy: bool,
    /// The last `ts_event` published for this instrument, so every publish is
    /// strictly monotonic (F5: a stamp below the book's own skips the update).
    pub ts_last: u64,
    /// `Err` leaves the instrument non-executable until a recovery clears it.
    pub readiness: Result<(), &'static str>,
}

impl Default for Inst {
    fn default() -> Inst {
        Inst {
            busy: false,
            ts_last: 0,
            readiness: Ok(()),
        }
    }
}

/// The node's whole CN execution state, plus the release latch that holds the
/// first feed publish until recovery has decided (feature SPEC §5.3).
#[derive(Debug, Default)]
pub struct CnExec {
    /// False until `Registry::start` has restored, reconciled and re-gated the
    /// desk. The polling tasks neither fetch nor publish before it is true.
    pub released: bool,
    per: HashMap<InstrumentId, Inst>,
}

impl CnExec {
    pub fn new() -> CnExec {
        CnExec::default()
    }

    pub fn inst(&mut self, instrument_id: InstrumentId) -> &mut Inst {
        self.per.entry(instrument_id).or_default()
    }

    /// Restart, feed recovery, provider switch, volume decrease: every temporal
    /// baseline is discarded (feature SPEC §2.5). Nothing is persisted, so a
    /// restart *is* this.
    pub fn reset(&mut self) {
        self.per.clear();
    }

    /// The strictly monotonic stamp for the next publish on this instrument.
    pub fn stamp(&mut self, instrument_id: InstrumentId, candidate_ns: u64) -> u64 {
        let inst = self.inst(instrument_id);
        let ts = candidate_ns.max(inst.ts_last + 1);
        inst.ts_last = ts;
        ts
    }
}

/// The state's handle as the node thread holds it.
pub type Exec = Rc<RefCell<CnExec>>;

// ---------------------------------------------------------------------------
// Publishing (node thread)
// ---------------------------------------------------------------------------

/// One `InstrumentStatus` on the data path — the session gate the sandbox's own
/// status handler reads (F1; `feasibility/f8_restart.rs::publish_status`).
pub fn publish_status(
    _context: &NodeContext,
    entry: &'static Entry,
    action: MarketStatusAction,
    ts_ns: u64,
) {
    let status = InstrumentStatus::new(
        InstrumentId::from(entry.instrument_id),
        action,
        UnixNanos::from(ts_ns),
        UnixNanos::from(ts_ns),
        None,
        None,
        None,
        None,
        None,
    );
    if let Err(e) = get_data_event_sender().send(DataEvent::Data(Data::InstrumentStatus(status))) {
        tracing::error!("the node did not take the CN instrument status: {e}");
    }
}

/// One book for a CN instrument, at the catalog's own precision. A zero size
/// clears that side of the ladder (F4), which is what the idle book is made of.
pub fn publish_quote(
    _context: &NodeContext,
    entry: &'static Entry,
    bid: (Decimal, Decimal),
    ask: (Decimal, Decimal),
    ts_ns: u64,
) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price = |value: Decimal| {
        Price::from(crate::feed::at_precision(value, entry.price_increment).as_str())
    };
    let size = |value: Decimal| Quantity::from(value.normalize().to_string().as_str());
    let tick = QuoteTick::new(
        instrument_id,
        price(bid.0),
        price(ask.0),
        size(bid.1),
        size(ask.1),
        UnixNanos::from(ts_ns),
        UnixNanos::from(ts_ns),
    );
    if let Err(e) = get_data_event_sender().send(DataEvent::Data(Data::Quote(tick))) {
        tracing::error!("the node did not take the CN quote: {e}");
    }
}

// ---------------------------------------------------------------------------
// The critical section (off the node thread)
// ---------------------------------------------------------------------------

/// Waits until the instrument's cached quote carries `ts_ns` — the proof that
/// the engine processed the publish rather than that the daemon sent it.
pub fn confirm(node: &Node, instrument_id: InstrumentId, ts_ns: u64) -> Result<(), &'static str> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let landed = node
            .call(move |context| {
                context
                    .cache
                    .borrow()
                    .quote(&instrument_id)
                    .is_some_and(|quote| quote.ts_event.as_u64() == ts_ns)
            })
            .map_err(|_| NODE_NOT_STARTED)?;
        if landed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(PUBLICATION_FAILED);
        }
        thread::sleep(WAIT_POLL);
    }
}

/// Publishes an executable book and restores the idle one, confirming both, with
/// the instrument marked busy throughout (feature SPEC §2.5). A failure to
/// confirm either leaves the instrument non-executable and `Pause`d, so nothing
/// can fill against whatever is standing.
///
/// `crossing` is `(bid, ask)` as `(price, size)` pairs; `idle_price` is the book
/// restored afterwards, both sides at size zero.
pub fn publish_confirmed(
    node: &Node,
    entry: &'static Entry,
    crossing: ((Decimal, Decimal), (Decimal, Decimal)),
    idle_price: Decimal,
    ts_ns: u64,
) -> Result<(), &'static str> {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let (bid, ask) = crossing;
    let sent = node
        .call(move |context| {
            let ts = {
                let mut exec = context.cn.borrow_mut();
                let ts = exec.stamp(instrument_id, ts_ns);
                exec.inst(instrument_id).busy = true;
                ts
            };
            publish_quote(context, entry, bid, ask, ts);
            ts
        })
        .map_err(|_| NODE_NOT_STARTED)?;

    let restored = confirm(node, instrument_id, sent).and_then(|()| {
        let idle = node
            .call(move |context| {
                let ts = context.cn.borrow_mut().stamp(instrument_id, sent + 1);
                publish_quote(
                    context,
                    entry,
                    (idle_price, Decimal::ZERO),
                    (idle_price, Decimal::ZERO),
                    ts,
                );
                ts
            })
            .map_err(|_| NODE_NOT_STARTED)?;
        confirm(node, instrument_id, idle)
    });

    match restored {
        Ok(()) => {
            let _ =
                node.call(move |context| context.cn.borrow_mut().inst(instrument_id).busy = false);
            Ok(())
        }
        Err(reason) => {
            mark_failed(node, entry, reason, ts_ns);
            Err(reason)
        }
    }
}

/// A publication that could not be confirmed leaves the instrument
/// non-executable and `Pause`d, so nothing can fill against whatever book is
/// standing; the next successful recovery clears and re-baselines it
/// ([`CnExec::reset`], feature SPEC §2.5).
pub fn mark_failed(node: &Node, entry: &'static Entry, reason: &'static str, ts_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let _ = node.call(move |context| {
        {
            let mut exec = context.cn.borrow_mut();
            let inst = exec.inst(instrument_id);
            inst.readiness = Err(reason);
            inst.busy = false;
        }
        let ts = context.cn.borrow_mut().stamp(instrument_id, ts_ns);
        publish_status(context, entry, MarketStatusAction::Pause, ts);
    });
}

/// Runs `admission` on the node thread once the instrument is neither busy nor
/// unready — one closure that reads the cache and hands the order, so nothing
/// can be published between the check and the placement (feature SPEC §2.5).
///
/// ponytail: a per-node flag plus a bounded 2 ms poll, not an async mutex,
/// because the publish it waits for is completed *by* the node runner this
/// caller must not block. The upgrade path is a proper async guard if the wait
/// latency ever matters.
pub fn admit<T, F>(
    node: &Node,
    instrument_id: InstrumentId,
    admission: F,
) -> Result<T, &'static str>
where
    T: Send + 'static,
    F: FnOnce(&NodeContext) -> T + Clone + Send + 'static,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let attempt = admission.clone();
        let outcome = node
            .call(move |context| {
                {
                    let mut exec = context.cn.borrow_mut();
                    let inst = exec.inst(instrument_id);
                    if inst.busy {
                        return Err(PUBLICATION_PENDING);
                    }
                    inst.readiness?;
                }
                Ok(attempt(context))
            })
            .map_err(|_| NODE_NOT_STARTED)?;
        match outcome {
            Ok(value) => return Ok(value),
            Err(PUBLICATION_PENDING) if Instant::now() < deadline => thread::sleep(WAIT_POLL),
            Err(reason) => return Err(reason),
        }
    }
}

// ---------------------------------------------------------------------------
// cn::session_window_is_the_execution_window, cn::owning_day_is_the_order_stamp
// (feature SPEC §5.1, §5.3)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[test]
fn session_window_is_the_execution_window() {
    // 09:35 is inside the morning session; 11:30 and 14:57 are already out,
    // because both intervals are half-open.
    assert!(in_session(CN_0935));
    assert!(!in_session(CN_0935 + 6_900 * SECOND_NS), "11:30");
    assert!(in_session(CN_0935 + 6_899 * SECOND_NS), "11:29:59");
    assert!(!in_session(CN_0935 + 9_300 * SECOND_NS), "12:10, lunch");
    assert!(in_session(CN_0935 + 12_300 * SECOND_NS), "13:00");
    assert!(!in_session(CN_0935 + 19_320 * SECOND_NS), "14:57");
    assert!(in_session(CN_0935 + 19_319 * SECOND_NS), "14:56:59");

    assert_eq!(status_for(CN_0935), MarketStatusAction::Trading);
    assert_eq!(
        status_for(CN_0935 + 9_300 * SECOND_NS),
        MarketStatusAction::Close
    );

    // The deadline is 14:57 on the instant's own Shanghai date, whatever the
    // time of day it is read at.
    assert_eq!(deadline_ns(CN_0935), CN_0935 + 19_320 * SECOND_NS);
    assert_eq!(
        deadline_ns(CN_0935 + 21_300 * SECOND_NS),
        CN_0935 + 19_320 * SECOND_NS,
        "15:30 still names its own day's 14:57"
    );
    assert_eq!(shanghai_date(CN_0935), "20260909");

    // A Saturday is never a session, whatever the hour.
    let saturday = CN_0935 + 4 * 86_400 * SECOND_NS;
    assert_eq!(shanghai_date(saturday), "20260913");
    assert!(!in_session(saturday));
}

#[cfg(test)]
#[test]
fn stamps_are_strictly_monotonic() {
    let id = InstrumentId::from("600519.XSHG");
    let mut exec = CnExec::new();
    assert_eq!(exec.stamp(id, 100), 100);
    assert_eq!(
        exec.stamp(id, 100),
        101,
        "a repeated instant still advances"
    );
    assert_eq!(
        exec.stamp(id, 50),
        102,
        "and a backwards one never regresses"
    );
    assert_eq!(exec.stamp(id, 500), 500);
    exec.reset();
    assert_eq!(exec.stamp(id, 50), 50, "a reset forgets the instrument");
}

// ---------------------------------------------------------------------------
// cn::a_limit_admitted_during_a_crossing_publish_never_takes,
// cn::a_publication_that_never_lands_pauses_the_instrument,
// cn::cancellation_and_the_clock_run_while_a_publish_is_outstanding
// (feature SPEC §2.5, §5.1)
// ---------------------------------------------------------------------------

#[cfg(test)]
use crate::node::{
    CN_0935, SECOND_NS, controlled_registry, fill_count, order_status, publish, within,
};
#[cfg(test)]
use crate::store::Store;
#[cfg(test)]
use crate::trade;

#[cfg(test)]
fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

/// Submits one CN limit buy, answering what the sandbox said.
#[cfg(test)]
fn buy(
    store: &Store,
    registry: &crate::node::Registry,
    desk_id: &str,
    action_id: &str,
    price: &str,
) -> Result<serde_json::Value, trade::TradeError> {
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"000001.XSHE",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"{price}"}}"#
    );
    trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .map(|(record, _)| record.outcome.clone().unwrap())
}

/// The F8 hazard, as the **correct** outcome. With a crossing book standing in
/// the engine, a compatible LIMIT submitted from another thread waits for the
/// idle book to be restored instead of filling at submission as a taker (§2.4,
/// §2.5). The window is opened by hand here — the same three steps
/// [`publish_confirmed`] takes — so the admission is inside it every time, not
/// when a thread happens to win a race.
#[cfg(test)]
#[test]
fn a_limit_admitted_during_a_crossing_publish_never_takes() {
    const ROUNDS: u64 = 3;
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "cn-race", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let node = registry.ensure(&desk_id).expect("the node starts");
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    publish(&node, pingan(), ("9.90", 0), ("9.90", 0), CN_0935);

    let registry = std::sync::Arc::new(registry);
    let crossing = Decimal::new(995, 2);
    for round in 1..=ROUNDS {
        let ts = CN_0935 + round * SECOND_NS;
        // 1. The crossing ask goes up and the instrument is busy — a book that
        //    would fill any compatible BUY on arrival.
        let sent = node
            .call(move |context| {
                let ts = {
                    let mut exec = context.cn.borrow_mut();
                    let ts = exec.stamp(instrument_id, ts);
                    exec.inst(instrument_id).busy = true;
                    ts
                };
                publish_quote(
                    context,
                    pingan(),
                    (crossing, Decimal::ZERO),
                    (crossing, Decimal::from(100)),
                    ts,
                );
                ts
            })
            .unwrap();
        confirm(&node, instrument_id, sent).expect("the crossing book stands");

        // 2. The submit lands inside the window, from its own thread.
        let action = format!("cn-race-{round}");
        let submitting = {
            let (store, registry, desk_id, action) = (
                store.clone(),
                std::sync::Arc::clone(&registry),
                desk_id.clone(),
                action.clone(),
            );
            std::thread::spawn(move || buy(&store, &registry, &desk_id, &action, "10.00"))
        };
        thread::sleep(Duration::from_millis(50));
        assert!(
            !submitting.is_finished(),
            "{action}: admission waited for the idle restore"
        );

        // 3. The idle book is restored and the instrument released.
        let idle = node
            .call(move |context| {
                let ts = context.cn.borrow_mut().stamp(instrument_id, sent + 1);
                publish_quote(
                    context,
                    pingan(),
                    (crossing, Decimal::ZERO),
                    (crossing, Decimal::ZERO),
                    ts,
                );
                ts
            })
            .unwrap();
        confirm(&node, instrument_id, idle).expect("the idle book is restored");
        node.call(move |context| context.cn.borrow_mut().inst(instrument_id).busy = false)
            .unwrap();

        let outcome = submitting
            .join()
            .expect("the submitting thread")
            .unwrap_or_else(|e| panic!("{action} is accepted: {e:?}"));
        assert_eq!(
            outcome["status"], "ACCEPTED",
            "{action} rests on the idle book: {outcome}"
        );
        trade::cancel(
            &store,
            &registry,
            &desk_id,
            &action,
            &format!(r#"{{"action_id":"{action}-x"}}"#),
            &trade::Source::Session,
        )
        .expect("the resting order cancels");
    }

    let prices: Vec<String> = store
        .call(|conn| {
            conn.prepare("SELECT price FROM fills")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .expect("the fill prices");
    assert!(
        prices.iter().all(|price| price == "10.00"),
        "no order took liquidity at the published 9.95: {prices:?}"
    );
    registry.stop_all();
}

/// A publication the node never confirms leaves the instrument non-executable
/// and paused, and admission refuses with that reason until a recovery resets
/// it (feature SPEC §2.5).
#[cfg(test)]
#[test]
fn a_publication_that_never_lands_pauses_the_instrument() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "cn-confirm", CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    let instrument_id = InstrumentId::from(pingan().instrument_id);

    // Nothing is ever published at this stamp, so the confirmation runs out.
    let started = Instant::now();
    assert_eq!(
        confirm(&node, instrument_id, CN_0935 + SECOND_NS),
        Err(PUBLICATION_FAILED)
    );
    assert!(
        started.elapsed() >= WAIT_TIMEOUT,
        "the wait is bounded, not endless"
    );

    mark_failed(&node, pingan(), PUBLICATION_FAILED, CN_0935 + SECOND_NS);
    within(10, "the instrument is paused", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .instrument_status(&instrument_id)
                .map(|cached| cached.action)
                == Some(MarketStatusAction::Pause)
        })
        .unwrap()
    });
    assert_eq!(
        admit(&node, instrument_id, |_| ()),
        Err(PUBLICATION_FAILED),
        "a non-executable instrument admits nothing"
    );
    assert_eq!(
        buy(&store, &registry, handle.desk_id(), "cn-confirm-1", "10.00")
            .expect_err("the submit is refused")
            .code(),
        "MARKET_UNAVAILABLE"
    );

    // Recovery clears it.
    node.call(|context| context.cn.borrow_mut().reset())
        .unwrap();
    assert_eq!(admit(&node, instrument_id, |_| 7), Ok(7));
    registry.stop_all();
}

/// §2.5: a busy instrument blocks admission and nothing else. Cancellation and
/// the kernel-clock alerts that stop execution at a boundary must never wait on
/// a publication.
#[cfg(test)]
#[test]
fn cancellation_and_the_clock_run_while_a_publish_is_outstanding() {
    use nautilus_common::timer::{TimeEvent, TimeEventCallback};
    use nautilus_model::identifiers::ClientOrderId;

    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "cn-busy", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let node = registry.ensure(&desk_id).expect("the node starts");
    let instrument_id = InstrumentId::from(pingan().instrument_id);
    publish(&node, pingan(), ("9.90", 0), ("9.90", 0), CN_0935);
    buy(&store, &registry, &desk_id, "cn-busy-a", "9.00").expect("a rests");
    buy(&store, &registry, &desk_id, "cn-busy-b", "9.00").expect("b rests");

    // A publication is in flight.
    node.call(move |context| context.cn.borrow_mut().inst(instrument_id).busy = true)
        .unwrap();
    assert_eq!(
        admit(&node, instrument_id, |_| ()),
        Err(PUBLICATION_PENDING),
        "admission waits, then gives up with the pending reason"
    );

    // The cancel does not.
    trade::cancel(
        &store,
        &registry,
        &desk_id,
        "cn-busy-a",
        r#"{"action_id":"cn-busy-a-x"}"#,
        &trade::Source::Session,
    )
    .expect("the cancel proceeds while the instrument is busy");
    assert_eq!(
        order_status(&node, "cn-busy-a").as_deref(),
        Some("CANCELED")
    );

    // Neither does a boundary alert on the node clock: it fires on the advance
    // and terminates the remaining order, with the instrument still busy.
    let at = CN_0935 + 60 * SECOND_NS;
    node.call(move |context| {
        let cache = Rc::clone(&context.cache);
        let trader_id = context.trader_id;
        let callback: Rc<dyn Fn(TimeEvent)> = Rc::new(move |_event| {
            let order = cache
                .borrow()
                .order(&ClientOrderId::from("cn-busy-b"))
                .map(|order| order.cloned());
            if let Some(order) = order.filter(|order| !order.is_closed()) {
                crate::trade::cancel_on_node(trader_id, &order, at);
            }
        });
        context
            .clock
            .borrow_mut()
            .set_time_alert_ns(
                "cn-session-end",
                UnixNanos::from(at),
                Some(TimeEventCallback::from(callback)),
                Some(false),
            )
            .expect("the alert registers");
    })
    .unwrap();
    let fired = node.advance_to(at).expect("the clock advances");
    assert!(
        fired.iter().any(|name| name == "cn-session-end"),
        "{fired:?}"
    );
    within(10, "the boundary alert terminated the order", || {
        order_status(&node, "cn-busy-b").as_deref() == Some("CANCELED")
    });
    assert_eq!(fill_count(&store), 0);
    assert!(
        node.call(move |context| context.cn.borrow_mut().inst(instrument_id).busy)
            .unwrap(),
        "and the publication is still marked in flight throughout"
    );
    registry.stop_all();
}
