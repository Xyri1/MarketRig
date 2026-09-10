//! F8 item 4 — restart, feed loss, provider switch, and the AE-9 observation
//! baseline (`sdd/features/a-share-engine/FEASIBILITY.md` §F8; feature SPEC
//! §2.5, §5.1, §5.3; DECISIONS AE-7–AE-9).
//!
//! Every test runs the real `LiveNode` + `SandboxExecutionClient` on the
//! controlled clock from [`crate::feasibility::clock`] with `feed_base: None`,
//! so **nothing** reaches the node that this file did not publish. That is also
//! the degenerate form of §5.3's "hold the first publish until restoration has
//! decided": after `Registry::ensure` returns there is still no market data, and
//! the tests assert it.
//!
//! # The model under test
//!
//! AE-9's "baseline" is **daemon-side per-order metadata** — the accepted
//! observation sequence and cumulative volume at admission — held in [`Gate`]
//! here. It is never in the sandbox payload and never in a node event. The node
//! only ever sees what the daemon publishes:
//!
//! - `InstrumentStatus` `Trading` while CN execution is allowed, `Pause` while it
//!   is not (F1's gate: `MarketStatus::Paused` makes `iterate` skip matching,
//!   `nautilus-execution-0.62.0/src/matching_engine/mod.rs:3702`);
//! - a **flat book** — a `QuoteTick` with both sizes zero, which clears both
//!   ladders (F4, `ladder.rs:238-244`) — for every observation that does not
//!   qualify, so nothing executable is ever left standing;
//! - a **crossing book** sized to the resting order's remaining quantity at its
//!   own limit price, exactly once, when an observation qualifies, immediately
//!   followed by the flat book again.
//!
//! The order-specific trigger itself is `f8_limit`'s question; this file assumes
//! it and asks what the daemon must do around restart, feed loss, a
//! cumulative-volume decrease, a provider switch, lunch, 14:57, approval, and a
//! second competing SELL, so that no fill happens against stale or pre-baseline
//! data.
//!
//! # What each test establishes
//!
//! - [`restart_rebaselines_and_only_a_later_qualifying_observation_fills`] — the
//!   restart sequence, and that baselines need no persistence.
//! - [`feed_failure_discards_the_baseline_and_recovery_re_establishes_it`]
//! - [`a_volume_decrease_pauses_and_the_reset_observation_does_not_fill`]
//! - [`a_provider_switch_that_skips_the_clear_fills_off_the_old_book`] — the F5
//!   hazard, re-confirmed in the AE-9 shape.
//! - [`a_provider_switch_blocks_clears_and_re_baselines`] — the ordering that
//!   avoids it.
//! - [`lunch_retains_the_same_day_baseline`]
//! - [`session_end_cancels_with_a_baseline_standing`]
//! - [`approval_takes_the_baseline_at_decide_time`]
//! - [`two_competing_sells_survive_a_restart`]
//! - [`restored_partial_fill_keeps_one_acceptance_and_replays`] — R1's retained
//!   regression, now asserting the corrected behaviour (slice 014 step 1).
//!
//! Platform: macOS only; no Windows run.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::execution::{CancelOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard};
use nautilus_common::timer::TimeEvent;
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::accounts::AccountAny;
use nautilus_model::data::{Data, InstrumentStatus, QuoteTick};
use nautilus_model::enums::{MarketStatusAction, OrderSide, OrderStatus, PositionSide};
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    self, CN_0935, CN_1457, ClockHandle, DAY_NS, SECOND_NS, advance, controlled_registry,
    publish_book, stored_events,
};
use crate::feed::MarketState;
use crate::node::{Node, Registry, within};
use crate::policy::Decision;
use crate::store::Store;
use crate::trade;

/// `crate::trade::STRATEGY`, pinned by `f2::strategy_matches_production`.
const STRATEGY: &str = "MARKETRIG-001";

/// 11:30 / 13:00 / 15:30 Asia/Shanghai, relative to [`CN_0935`].
const CN_1130: u64 = CN_0935 + 6_900 * SECOND_NS;
const CN_1300: u64 = CN_0935 + 12_300 * SECOND_NS;
const CN_1530: u64 = CN_0935 + 21_300 * SECOND_NS;

/// 09:35 on day D−1 — F6's day frame, so the seeding fill is a *prior*-day BUY
/// and §1.1's T+1 term is zero on day D.
const CN_D_MINUS_1: u64 = CN_0935 - DAY_NS;

/// The XSHG / XSHE venue accounts' opening cash (`crate::node::seed`).
const OPENING: &str = "500000.00 CNY|0.00 CNY|500000.00 CNY";

fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

// ---------------------------------------------------------------------------
// The daemon-side gate: per-order baselines and what it publishes
// ---------------------------------------------------------------------------

/// AE-9's per-order metadata: the accepted observation sequence and cumulative
/// volume at execution-time admission. Daemon-side only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Baseline {
    seq: u64,
    volume: u64,
}

/// One resting order, as the daemon knows it while deciding what to publish.
#[derive(Debug, Clone, Copy)]
struct Resting {
    id: &'static str,
    side: OrderSide,
    limit: &'static str,
    quantity: u32,
}

/// The publisher in front of one CN instrument's matching engine.
#[derive(Debug)]
struct Gate {
    entry: &'static Entry,
    /// Accepted observations so far.
    seq: u64,
    /// The preceding accepted observation's cumulative volume.
    prev: Option<u64>,
    baselines: HashMap<&'static str, Baseline>,
    /// Is the instrument `Trading` (as opposed to `Pause`)?
    open: bool,
    /// The last `ts_event` published, so every publish is monotonic (F5).
    ts_last: u64,
}

impl Gate {
    fn new(entry: &'static Entry) -> Self {
        Self {
            entry,
            seq: 0,
            prev: None,
            baselines: HashMap::new(),
            open: false,
            ts_last: 0,
        }
    }

    /// Readiness established: `InstrumentStatus::Trading`. §5.3 republishes it on
    /// every start; F1 proves the reopen itself never settles a resting order.
    fn start(&mut self, node: &Node, ts: u64) {
        publish_status(node, self.entry, MarketStatusAction::Trading, ts);
        self.open = true;
    }

    /// Execution blocked (lunch, feed loss, provider switch, unreadiness):
    /// `InstrumentStatus::Pause`.
    fn block(&mut self, node: &Node, ts: u64) {
        publish_status(node, self.entry, MarketStatusAction::Pause, ts);
        self.open = false;
    }

    /// Clears stale executable state: a zero-size `QuoteTick` empties both
    /// ladders. The stamp is forced strictly monotonic, because a `ts_event`
    /// below `book.ts_last` skips the book update and still iterates (F5,
    /// `matching_engine/mod.rs:1584-1592`).
    fn clear(&mut self, node: &Node, price: &str, ts: u64) {
        let ts = ts.max(self.ts_last + 1);
        publish_book(node, self.entry, (price, 0), (price, 0), ts);
        self.ts_last = ts;
    }

    /// Restart, feed recovery, provider switch, volume decrease: discard every
    /// temporal baseline (§2.5). Nothing is persisted, so restart is this.
    fn reset(&mut self) {
        self.seq = 0;
        self.prev = None;
        self.baselines.clear();
    }

    /// Execution-time admission (approval time for a pending action).
    fn admit(&mut self, id: &'static str) {
        self.baselines.insert(
            id,
            Baseline {
                seq: self.seq,
                volume: self.prev.unwrap_or(0),
            },
        );
    }

    fn baseline(&self, id: &str) -> Option<Baseline> {
        self.baselines.get(id).copied()
    }

    /// One provider observation. Returns the orders it released — at most one
    /// here, because a single book cannot isolate two compatible orders (that is
    /// `f8_limit`'s question, not this file's).
    fn observe(
        &mut self,
        node: &Node,
        price: &'static str,
        volume: u64,
        ts: u64,
        resting: &[Resting],
    ) -> Vec<&'static str> {
        let ts = ts.max(self.ts_last + 1);
        advance(node, ts);

        if !self.open {
            // Blocked: the observation is awareness only, never accepted, and
            // the book it leaves behind is flat.
            self.clear(node, price, ts);
            return Vec::new();
        }
        if self.prev.is_some_and(|prev| volume < prev) {
            // §2.5: a same-day cumulative-volume decrease pauses and discards.
            self.block(node, ts);
            self.clear(node, price, ts);
            self.reset();
            return Vec::new();
        }

        self.seq += 1;
        let released: Vec<&'static str> = match self.prev {
            // The first accepted observation after a reset only re-establishes.
            None => Vec::new(),
            Some(prev) => resting
                .iter()
                .filter(|order| {
                    self.baselines
                        .get(order.id)
                        .is_some_and(|base| self.seq > base.seq && volume > base.volume)
                        && volume > prev
                        && compatible(order, price)
                })
                .map(|order| order.id)
                .collect(),
        };
        assert!(
            released.len() <= 1,
            "this file publishes one crossing book at a time: {released:?}"
        );

        if let Some(id) = released.first() {
            let order = resting
                .iter()
                .find(|order| &order.id == id)
                .expect("the released order is one of the resting orders");
            cross(node, self.entry, order, ts);
            self.ts_last = ts;
        }
        // The flat book again, immediately: §5.3's "clear stale executable
        // state" is an invariant, not only a recovery step, so a LIMIT submitted
        // next can never fill on arrival (§2.4).
        self.clear(node, price, ts);

        self.prev = Some(volume);
        for order in resting {
            self.baselines.entry(order.id).or_insert(Baseline {
                seq: self.seq,
                volume,
            });
        }
        released
    }
}

/// BUY fills at `last <= limit`, SELL at `last >= limit` (§2.5).
fn compatible(order: &Resting, price: &str) -> bool {
    let (last, limit) = (Price::from(price), Price::from(order.limit));
    match order.side {
        OrderSide::Buy => last <= limit,
        _ => last >= limit,
    }
}

/// The crossing book for one order: the opposite side sized to its remaining
/// quantity **at its own limit price**, so the fill price is the limit price
/// whether the engine treats it as maker or taker.
fn cross(node: &Node, entry: &'static Entry, order: &Resting, ts: u64) {
    match order.side {
        OrderSide::Buy => publish_book(
            node,
            entry,
            (order.limit, 0),
            (order.limit, order.quantity),
            ts,
        ),
        _ => publish_book(
            node,
            entry,
            (order.limit, order.quantity),
            (order.limit, 0),
            ts,
        ),
    }
}

/// Publishes one `InstrumentStatus` on the data path and waits for the node to
/// have cached it (F1's gate; `f1_f5::publish_status`, copied because that
/// module's helpers are private to it).
fn publish_status(node: &Node, entry: &'static Entry, action: MarketStatusAction, ts_ns: u64) {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    node.call(move |_| {
        let status = InstrumentStatus::new(
            instrument_id,
            action,
            UnixNanos::from(ts_ns),
            UnixNanos::from(ts_ns),
            None,
            None,
            None,
            None,
            None,
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::InstrumentStatus(status)))
            .expect("the data runner takes the status");
    })
    .expect("the node answers");
    within(10, "the instrument status reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .instrument_status(&instrument_id)
                .map(|s| s.action)
                == Some(action)
        })
        .unwrap()
    });
}

/// A quote whose `ts_event` and `ts_init` differ — what a provider transition
/// produces (`f1_f5::publish_quote_stamped`).
fn publish_quote_stamped(
    node: &Node,
    entry: &'static Entry,
    price: &'static str,
    size: u32,
    ts_event_ns: u64,
    ts_init_ns: u64,
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
            UnixNanos::from(ts_event_ns),
            UnixNanos::from(ts_init_ns),
        );
        get_data_event_sender()
            .send(DataEvent::Data(Data::Quote(tick)))
            .expect("the data runner takes the quote");
    })
    .expect("the node answers");
}

// ---------------------------------------------------------------------------
// Harness (the shape r1.rs uses; its helpers are private to that module)
// ---------------------------------------------------------------------------

fn open_desk(store: &Store, name: &'static str, at_ns: u64) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, at_ns);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    (registry, handle, node)
}

/// A second registry over the same store: the restart. `Registry::ensure` runs
/// `crate::trade::restore` — and therefore `apply`, and therefore
/// `Portfolio::initialize_orders` (R1) — before it returns.
fn restart(store: &Store, desk_id: &str) -> (Registry, Arc<Node>) {
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), None);
    let node = registry
        .ensure(desk_id)
        .expect("the node restores and starts");
    (registry, node)
}

/// Rests one CN limit order through the production submit path.
#[expect(
    clippy::too_many_arguments,
    reason = "one call shape for every scenario, as f1_f5::order has"
)]
fn rest(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    entry: &'static Entry,
    action_id: &str,
    side: &str,
    quantity: u32,
    price: &str,
) {
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"{}",
            "side":"{side}","type":"LIMIT","quantity":"{quantity}","price":"{price}"}}"#,
        entry.instrument_id
    );
    let (record, _) = trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .expect("the limit order is accepted");
    assert_eq!(
        record.outcome.clone().unwrap()["status"],
        "ACCEPTED",
        "{action_id} must rest, not fill on arrival (§2.4)"
    );
}

/// The venue account's balance as `total|locked|free`.
fn balance(node: &Node, venue: &str, desk_id: &str) -> String {
    let account_id = AccountId::from(format!("{venue}-{desk_id}").as_str());
    node.call(move |context| {
        let cache = context.cache.borrow();
        let account = cache.account(&account_id).expect("the venue account");
        let balance = account
            .balances()
            .values()
            .copied()
            .next()
            .expect("one CNY balance");
        format!("{}|{}|{}", balance.total, balance.locked, balance.free)
    })
    .expect("the node answers")
}

/// The per-(instrument, currency) lock map's size — the state `book_snapshots`
/// cannot carry, rebuilt by `Portfolio::initialize_orders` (R1).
fn lock_map_len(node: &Node, venue: &str, desk_id: &str) -> usize {
    let account_id = AccountId::from(format!("{venue}-{desk_id}").as_str());
    node.call(move |context| {
        match &*context
            .cache
            .borrow()
            .account(&account_id)
            .expect("the venue account")
        {
            AccountAny::Cash(cash) => cash.balances_locked.len(),
            other => panic!("the sandbox account is a cash account, got {other:?}"),
        }
    })
    .expect("the node answers")
}

fn status(node: &Node, client_order_id: &str) -> OrderStatus {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        context
            .cache
            .borrow()
            .order(&id)
            .map(|order| order.status())
            .expect("the order is cached")
    })
    .expect("the node answers")
}

fn venue_order_id(node: &Node, client_order_id: &str) -> String {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        context
            .cache
            .borrow()
            .order(&id)
            .and_then(|order| order.venue_order_id())
            .expect("the accepted order carries a venue order id")
            .to_string()
    })
    .expect("the node answers")
}

/// The desk's captured event kinds for one client order id, oldest first.
fn kinds(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, _)| kind)
        .collect()
}

fn fills(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count")
}

/// `(quantity, price)` of the single fill row.
fn fill_row(store: &Store) -> (String, String) {
    store
        .call(|conn| {
            conn.query_row("SELECT quantity, price FROM fills", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        })
        .expect("the fill row")
}

fn await_status(node: &Node, client_order_id: &'static str, want: OrderStatus, why: &str) {
    within(10, why, || status(node, client_order_id) == want);
}

/// Nothing filled and the order is still resting — the assertion every
/// "must not fill" step makes.
fn still_resting(store: &Store, node: &Node, client_order_id: &str, why: &str) {
    assert_eq!(fills(store), 0, "{why}");
    assert_eq!(
        status(node, client_order_id),
        OrderStatus::Accepted,
        "{why}"
    );
}

// ---------------------------------------------------------------------------
// 4a — restart
// ---------------------------------------------------------------------------

/// **Restart.** Hold publishing (there is no feed at all), restore, rebuild the
/// native reservation, republish the status, discard the baselines, re-establish
/// on the first post-restart observation — which must not fill the restored
/// LIMIT even though it is compatible **and** carries more volume than the
/// pre-restart baseline — and fill only on the next qualifying one, at the limit
/// price, with the identifiers and the event chain preserved.
///
/// This is also the answer to "must the baseline be persisted": no. §2.5 says
/// discard and re-establish, and the discard is what makes the restored order
/// safe against the first post-restart observation.
#[test]
fn restart_rebaselines_and_only_a_later_qualifying_observation_fills() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-restart", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-restart-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-restart-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-restart-1");
    assert_eq!(
        gate.baseline("f8r-restart-1"),
        Some(Baseline {
            seq: 1,
            volume: 1000
        })
    );
    let rested = balance(&node, "XSHG", &desk_id);
    assert_eq!(rested, "500000.00 CNY|160000.00 CNY|340000.00 CNY");
    let venue_id = venue_order_id(&node, "f8r-restart-1");
    let chain_before = kinds(&store, &desk_id, "f8r-restart-1");
    assert_eq!(
        chain_before,
        vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"]
    );

    // A compatible observation with equal volume never triggers (§2.5).
    let released = gate.observe(&node, "1500.00", 1000, CN_0935 + 10 * SECOND_NS, &resting);
    assert!(released.is_empty());
    still_resting(
        &store,
        &node,
        "f8r-restart-1",
        "equal volume does not qualify",
    );
    registry.stop_all();

    // --- The restart -------------------------------------------------------
    let (registry, node) = restart(&store, &desk_id);
    // Restoration finished with no market data at all: the publish is held.
    assert!(
        node.call(|context| context
            .cache
            .borrow()
            .quote(&InstrumentId::from(moutai().instrument_id))
            .is_none())
            .unwrap(),
        "no observation reached the node before restoration decided"
    );
    assert_eq!(status(&node, "f8r-restart-1"), OrderStatus::Accepted);
    assert_eq!(
        venue_order_id(&node, "f8r-restart-1"),
        venue_id,
        "the restored order kept its venue order id"
    );
    assert_eq!(
        balance(&node, "XSHG", &desk_id),
        rested,
        "the reservation came back with the account"
    );
    assert_eq!(
        lock_map_len(&node, "XSHG", &desk_id),
        1,
        "`Portfolio::initialize_orders` rebuilt the lock map (R1)"
    );
    assert_eq!(
        kinds(&store, &desk_id, "f8r-restart-1"),
        chain_before,
        "restoration replayed no event"
    );

    // Nothing about the baseline survives the restart: it lives in the daemon,
    // and §2.5 discards it.
    gate.reset();
    gate.start(&node, CN_0935 + 20 * SECOND_NS);

    // The re-establishing observation: compatible, and with **more volume than
    // the pre-restart baseline of 1000**. It must not fill.
    let released = gate.observe(&node, "1500.00", 1002, CN_0935 + 30 * SECOND_NS, &resting);
    assert!(released.is_empty(), "{released:?}");
    still_resting(
        &store,
        &node,
        "f8r-restart-1",
        "the first post-restart observation only re-establishes the baseline",
    );
    assert_eq!(
        gate.baseline("f8r-restart-1"),
        Some(Baseline {
            seq: 1,
            volume: 1002
        }),
        "…and the order is re-baselined against it"
    );

    // The next qualifying observation fills, in full, at the limit price.
    let released = gate.observe(&node, "1500.00", 1003, CN_0935 + 40 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-restart-1"]);
    await_status(
        &node,
        "f8r-restart-1",
        OrderStatus::Filled,
        "the qualifying observation released the restored order",
    );
    assert_eq!(fills(&store), 1);
    assert_eq!(
        fill_row(&store),
        ("100".to_owned(), "1600.00".to_owned()),
        "the whole remaining quantity at the order's own limit price"
    );
    assert_eq!(
        kinds(&store, &desk_id, "f8r-restart-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "one chain, the restart invisible in it"
    );
    assert_eq!(venue_order_id(&node, "f8r-restart-1"), venue_id);
    assert_eq!(
        balance(&node, "XSHG", &desk_id),
        "339952.00 CNY|0.00 CNY|339952.00 CNY",
        "500000 − 100 × 1600 − the sandbox's own 3 bp commission, reservation \
         released"
    );
    assert_eq!(lock_map_len(&node, "XSHG", &desk_id), 0);
    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["status"], "FILLED");
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4b — feed failure and recovery
// ---------------------------------------------------------------------------

/// **Feed failure.** While blocked, even a crossing book cannot fill (F1's
/// `Pause`). Recovery clears the stale executable state, republishes `Trading`,
/// and re-establishes the baseline: the first post-recovery observation carries
/// far more volume than the pre-failure baseline and is compatible, and still
/// must not fill.
#[test]
fn feed_failure_discards_the_baseline_and_recovery_re_establishes_it() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-feed", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-feed-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-feed-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-feed-1");

    // The feed is lost: block, clear, discard (§2.1, §2.5).
    gate.block(&node, CN_0935 + 10 * SECOND_NS);
    gate.clear(&node, "1500.00", CN_0935 + 10 * SECOND_NS);
    gate.reset();

    // A crossing book delivered while blocked matches nothing: `iterate` skips
    // while `MarketStatus::Paused`.
    advance(&node, CN_0935 + 20 * SECOND_NS);
    cross(&node, moutai(), &resting[0], CN_0935 + 20 * SECOND_NS);
    gate.ts_last = CN_0935 + 20 * SECOND_NS;
    still_resting(
        &store,
        &node,
        "f8r-feed-1",
        "a crossing book cannot fill a paused instrument",
    );

    // Recovery: clear the crossing book **before** reopening, then reopen. The
    // reopen itself never settles a resting order (F1).
    gate.clear(&node, "1500.00", CN_0935 + 30 * SECOND_NS);
    gate.start(&node, CN_0935 + 30 * SECOND_NS);
    still_resting(
        &store,
        &node,
        "f8r-feed-1",
        "the reopen does not iterate the book",
    );

    // The first post-recovery observation: compatible, volume 1500 — far above
    // the discarded baseline of 1000. It re-establishes and does not fill.
    let released = gate.observe(&node, "1500.00", 1500, CN_0935 + 40 * SECOND_NS, &resting);
    assert!(released.is_empty(), "{released:?}");
    still_resting(
        &store,
        &node,
        "f8r-feed-1",
        "recovery re-establishes the baseline; it never triggers",
    );
    assert_eq!(
        gate.baseline("f8r-feed-1"),
        Some(Baseline {
            seq: 1,
            volume: 1500
        })
    );

    let released = gate.observe(&node, "1550.00", 1501, CN_0935 + 50 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-feed-1"]);
    await_status(
        &node,
        "f8r-feed-1",
        OrderStatus::Filled,
        "the first qualifying post-recovery observation fills",
    );
    assert_eq!(fill_row(&store), ("100".to_owned(), "1600.00".to_owned()));
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4c — a same-day cumulative-volume decrease
// ---------------------------------------------------------------------------

/// **Volume decrease.** An increasing volume with an incompatible price advances
/// the preceding-observation baseline; a decrease pauses, discards, and the
/// re-establishing observation does not fill even though its volume is above
/// every earlier figure.
#[test]
fn a_volume_decrease_pauses_and_the_reset_observation_does_not_fill() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-decrease", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-decrease-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-decrease-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-decrease-1");

    // Increased volume, incompatible price (1700 > the 1600 limit): no fill, but
    // the preceding-observation volume advances to 1200.
    let released = gate.observe(&node, "1700.00", 1200, CN_0935 + 10 * SECOND_NS, &resting);
    assert!(released.is_empty());
    still_resting(&store, &node, "f8r-decrease-1", "an incompatible price");
    assert_eq!(gate.prev, Some(1200));

    // The decrease: pause, discard, no fill on the reset itself.
    let released = gate.observe(&node, "1500.00", 900, CN_0935 + 20 * SECOND_NS, &resting);
    assert!(released.is_empty());
    still_resting(&store, &node, "f8r-decrease-1", "no fill on the reset");
    assert_eq!(gate.prev, None, "the temporal baselines are discarded");
    assert_eq!(gate.baseline("f8r-decrease-1"), None);
    assert!(!gate.open, "the instrument is paused");

    // Re-establish on the next valid observation: compatible, volume 1300 —
    // above the discarded 1000 and 1200. No fill.
    gate.start(&node, CN_0935 + 30 * SECOND_NS);
    let released = gate.observe(&node, "1500.00", 1300, CN_0935 + 40 * SECOND_NS, &resting);
    assert!(released.is_empty(), "{released:?}");
    still_resting(
        &store,
        &node,
        "f8r-decrease-1",
        "the next valid observation only re-establishes",
    );

    let released = gate.observe(&node, "1500.00", 1301, CN_0935 + 50 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-decrease-1"]);
    await_status(
        &node,
        "f8r-decrease-1",
        OrderStatus::Filled,
        "and the one after it qualifies",
    );
    assert_eq!(fill_row(&store), ("100".to_owned(), "1600.00".to_owned()));
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4d — provider switch
// ---------------------------------------------------------------------------

/// **The hazard.** F5's stale-`ts_event` defect in the AE-9 shape: a crossing
/// book left in the engine while paused, then a HiThink→Yahoo switch whose first
/// Yahoo quote carries `regularMarketTime` (older than the HiThink receipt
/// stamp). The book update is skipped and `iterate` still runs
/// (`matching_engine/mod.rs:1584-1592`), so the order fills off the discarded
/// old book — with no qualifying observation at all.
#[test]
fn a_provider_switch_that_skips_the_clear_fills_off_the_old_book() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-switch-bad", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = Resting {
        id: "f8r-switch-bad-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    };

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-switch-bad-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-switch-bad-1");

    // A release interrupted by the block: the crossing book stands, unmatched,
    // because the instrument is paused.
    let stale_at = CN_0935 + 60 * SECOND_NS;
    advance(&node, stale_at);
    gate.block(&node, stale_at);
    cross(&node, moutai(), &resting, stale_at);
    still_resting(&store, &node, "f8r-switch-bad-1", "paused, so no match");

    // The switch, done wrong: reopen and let the new provider's first quote in
    // without clearing, stamped with its own (older) source time.
    gate.start(&node, stale_at + SECOND_NS);
    still_resting(
        &store,
        &node,
        "f8r-switch-bad-1",
        "the reopen alone does not iterate",
    );
    publish_quote_stamped(
        &node,
        moutai(),
        "1500.00",
        0,
        stale_at - 60 * SECOND_NS,
        stale_at + 2 * SECOND_NS,
    );
    await_status(
        &node,
        "f8r-switch-bad-1",
        OrderStatus::Filled,
        "DEFECT (F5): the stale-stamped quote skipped the book update and still \
         iterated, so the order filled off the old provider's book",
    );
    assert_eq!(fills(&store), 1);
    registry.stop_all();
}

/// **The ordering that avoids it.** Block, clear the stale executable state with
/// a strictly monotonic stamp while still paused, reopen, discard the baselines,
/// re-establish on the first observation of the new provider, and release only
/// the next qualifying one.
#[test]
fn a_provider_switch_blocks_clears_and_re_baselines() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-switch", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-switch-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-switch-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-switch-1");

    let stale_at = CN_0935 + 60 * SECOND_NS;
    advance(&node, stale_at);
    gate.block(&node, stale_at);
    cross(&node, moutai(), &resting[0], stale_at);
    gate.ts_last = stale_at;
    still_resting(&store, &node, "f8r-switch-1", "paused, so no match");

    // Clear while still paused, with a monotonic stamp. `Gate::clear` forces
    // `ts_last + 1`, which is what makes the update land instead of being
    // skipped.
    gate.clear(&node, "1500.00", stale_at);
    gate.reset();
    gate.start(&node, stale_at + 5 * SECOND_NS);
    still_resting(
        &store,
        &node,
        "f8r-switch-1",
        "the cleared book has nothing to match",
    );

    // Yahoo's first observation, stamped older than HiThink's last receipt — the
    // exact input that filled the previous test. Because the executable state
    // was cleared first, iterating it matches nothing; and it only
    // re-establishes the baseline.
    publish_quote_stamped(
        &node,
        moutai(),
        "1500.00",
        0,
        stale_at - 60 * SECOND_NS,
        stale_at + 6 * SECOND_NS,
    );
    still_resting(
        &store,
        &node,
        "f8r-switch-1",
        "an older-stamped quote over a cleared book matches nothing",
    );
    let released = gate.observe(&node, "1500.00", 9000, stale_at + 10 * SECOND_NS, &resting);
    assert!(released.is_empty(), "{released:?}");
    still_resting(
        &store,
        &node,
        "f8r-switch-1",
        "the new provider's first observation only re-establishes",
    );

    let released = gate.observe(&node, "1500.00", 9001, stale_at + 20 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-switch-1"]);
    await_status(
        &node,
        "f8r-switch-1",
        OrderStatus::Filled,
        "and the next qualifying one releases it",
    );
    assert_eq!(fill_row(&store), ("100".to_owned(), "1600.00".to_owned()));
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4e — lunch
// ---------------------------------------------------------------------------

/// **Lunch.** `Pause` at 11:30 keeps the same-day baseline; `Trading` at 13:00
/// settles nothing by itself; an equal-volume observation after the reopen does
/// not fill; the first qualifying one does.
#[test]
fn lunch_retains_the_same_day_baseline() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-lunch", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-lunch-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-lunch-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-lunch-1");
    let baseline = gate.baseline("f8r-lunch-1");

    // 11:30 — the morning close. Nothing is discarded (§2.5, "ordinary lunch
    // reopening can use the retained same-day baseline").
    advance(&node, CN_1130);
    gate.block(&node, CN_1130);
    gate.ts_last = CN_1130;

    // A crossing book delivered during the break matches nothing.
    advance(&node, CN_1130 + 60 * SECOND_NS);
    cross(&node, moutai(), &resting[0], CN_1130 + 60 * SECOND_NS);
    gate.ts_last = CN_1130 + 60 * SECOND_NS;
    still_resting(&store, &node, "f8r-lunch-1", "the lunch break holds it");
    gate.clear(&node, "1500.00", CN_1130 + 61 * SECOND_NS);

    // 13:00 — the afternoon open. The reopen itself never settles anything.
    advance(&node, CN_1300);
    gate.start(&node, CN_1300);
    still_resting(&store, &node, "f8r-lunch-1", "the reopen does not iterate");
    assert_eq!(
        gate.baseline("f8r-lunch-1"),
        baseline,
        "the same-day baseline is retained across lunch"
    );

    // Equal volume: still no fill.
    let released = gate.observe(&node, "1500.00", 1000, CN_1300 + 10 * SECOND_NS, &resting);
    assert!(released.is_empty());
    still_resting(
        &store,
        &node,
        "f8r-lunch-1",
        "equal volume after the reopen does not qualify",
    );

    let released = gate.observe(&node, "1500.00", 1001, CN_1300 + 20 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-lunch-1"]);
    await_status(
        &node,
        "f8r-lunch-1",
        OrderStatus::Filled,
        "a new qualifying post-reopen observation releases it",
    );
    assert_eq!(fill_row(&store), ("100".to_owned(), "1600.00".to_owned()));
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4f — 14:57, with a baseline standing
// ---------------------------------------------------------------------------

/// **14:57.** F2's kernel-clock alert cancels with no quote while a baseline
/// exists; the reservation is released; a later qualifying observation fills
/// nothing; and the next restart replays no event and cannot be woken.
#[test]
fn session_end_cancels_with_a_baseline_standing() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-1457", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-1457-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-1457-1",
        "BUY",
        100,
        "1600.00",
    );
    gate.admit("f8r-1457-1");
    assert!(gate.baseline("f8r-1457-1").is_some());

    // The session-end alert on the node's own clock, as F2 established it.
    let registered = clock::alert(&node, "cn-session-end", CN_1457, |context| {
        let cache = Rc::clone(&context.cache);
        let trader_id = context.trader_id;
        Rc::new(move |_event| {
            let target = ClientOrderId::from("f8r-1457-1");
            let Some((instrument_id, venue_order_id)) = ({
                let cache = cache.borrow();
                cache
                    .order(&target)
                    .filter(|order| !order.is_closed())
                    .map(|order| (order.instrument_id(), order.venue_order_id()))
            }) else {
                return;
            };
            msgbus::send_trading_command(
                MessagingSwitchboard::exec_engine_queue_execute(),
                TradingCommand::CancelOrder(CancelOrder::new(
                    trader_id,
                    None,
                    StrategyId::new(STRATEGY),
                    instrument_id,
                    target,
                    venue_order_id,
                    UUID4::new(),
                    UnixNanos::from(CN_1457),
                    None,
                    None,
                )),
            );
        }) as Rc<dyn Fn(TimeEvent)>
    });
    assert_eq!(registered, Ok(()));

    // No data at all between here and the boundary.
    let fired = advance(&node, CN_1457);
    assert!(
        fired.iter().any(|name| name == "cn-session-end"),
        "{fired:?}"
    );
    await_status(
        &node,
        "f8r-1457-1",
        OrderStatus::Canceled,
        "the day lifetime ends without a tick",
    );
    assert_eq!(
        balance(&node, "XSHG", &desk_id),
        OPENING,
        "the reservation is released"
    );
    let terminal: Vec<(String, i64)> = stored_events(&store, &desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == "f8r-1457-1")
        .map(|(_, kind, ns)| (kind, ns))
        .collect();
    assert_eq!(
        terminal.last(),
        Some(&("OrderCanceled".to_owned(), CN_1457 as i64)),
        "stamped exactly at the boundary: {terminal:?}"
    );

    // The baseline outlives nothing: the terminal event takes the order out of
    // the daemon's resting set (F6's `!is_closed()`), so a later observation —
    // qualifying on every temporal test — has nothing to release, and the flat
    // book it publishes matches nothing either.
    assert!(
        gate.baseline("f8r-1457-1").is_some(),
        "the daemon still holds the metadata of the order it just cancelled"
    );
    gate.block(&node, CN_1457);
    gate.clear(&node, "1500.00", CN_1457);
    gate.start(&node, CN_1530);
    let released = gate.observe(&node, "1500.00", 5000, CN_1530, &[]);
    assert!(released.is_empty(), "{released:?}");
    // Even the crossing book the release would have published matches nothing.
    advance(&node, CN_1530 + SECOND_NS);
    cross(&node, moutai(), &resting[0], CN_1530 + SECOND_NS);
    within(10, "the crossing book reaches the node", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from(moutai().instrument_id))
                .map(|q| q.ask_price)
                == Some(Price::from("1600.00"))
        })
        .unwrap()
    });
    assert_eq!(fills(&store), 0);
    assert_eq!(status(&node, "f8r-1457-1"), OrderStatus::Canceled);
    let chain = kinds(&store, &desk_id, "f8r-1457-1");
    registry.stop_all();

    // A restart after the deadline: the closed order is not in the snapshot's
    // open orders, so nothing is re-handed and nothing replays.
    let (registry, node) = restart(&store, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(
        kinds(&store, &desk_id, "f8r-1457-1"),
        chain,
        "no duplicate terminal event"
    );
    assert_eq!(balance(&node, "XSHG", &desk_id), OPENING);
    assert_eq!(lock_map_len(&node, "XSHG", &desk_id), 0);
    let history = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["status"], "CANCELED");
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4g — approval-time baseline
// ---------------------------------------------------------------------------

/// **Approval.** The baseline is taken at `decide` time, not at submission time:
/// an observation received while the action was pending cannot trigger the
/// order, and the first observation after approval only re-reads the same
/// volume.
#[test]
fn approval_takes_the_baseline_at_decide_time() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-approval", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let mut gate = Gate::new(moutai());
    let resting = [Resting {
        id: "f8r-approval-1",
        side: OrderSide::Buy,
        limit: "1600.00",
        quantity: 100,
    }];
    store
        .unit(|tx| {
            tx.execute(
                "UPDATE installation_settings SET paper_order_policy = 'REQUIRE_APPROVAL' \
                 WHERE id = 1",
                [],
            )
        })
        .expect("the policy a `PUT /settings/policies` would have written");

    gate.start(&node, CN_0935);
    gate.observe(&node, "1500.00", 1000, CN_0935 + SECOND_NS, &[]);

    let (pending, submitted) = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"f8r-approval-1","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"1600.00"}"#,
        &trade::Source::Session,
    )
    .expect("a gated order is recorded, not refused");
    assert_eq!(submitted, trade::Submitted::Pending);

    // Between submission and approval: a compatible observation with increased
    // volume. It reserves nothing and can trigger nothing — the order has never
    // reached the sandbox, so it is not in the daemon's resting set at all.
    let released = gate.observe(&node, "1500.00", 1100, CN_0935 + 10 * SECOND_NS, &[]);
    assert!(released.is_empty(), "{released:?}");
    assert_eq!(fills(&store), 0);
    assert!(
        gate.baseline("f8r-approval-1").is_none(),
        "a pending action has no baseline"
    );

    trade::decide(&store, &registry, &desk_id, &pending.id, Decision::Approve)
        .expect("the approval re-enters acceptance");
    // Admission is here, at approval time (§2.5).
    gate.admit("f8r-approval-1");
    assert_eq!(
        gate.baseline("f8r-approval-1"),
        Some(Baseline {
            seq: 2,
            volume: 1100
        }),
        "the baseline is the latest accepted observation at `decide` time"
    );
    assert_eq!(
        status(&node, "f8r-approval-1"),
        OrderStatus::Accepted,
        "an approved compatible LIMIT rests (§2.4)"
    );
    assert_eq!(fills(&store), 0);

    // The pre-approval observation's own volume cannot trigger it.
    let released = gate.observe(&node, "1500.00", 1100, CN_0935 + 20 * SECOND_NS, &resting);
    assert!(released.is_empty(), "{released:?}");
    still_resting(
        &store,
        &node,
        "f8r-approval-1",
        "an observation received before admission cannot trigger the order",
    );

    let released = gate.observe(&node, "1500.00", 1101, CN_0935 + 30 * SECOND_NS, &resting);
    assert_eq!(released, vec!["f8r-approval-1"]);
    await_status(
        &node,
        "f8r-approval-1",
        OrderStatus::Filled,
        "the first observation after admission qualifies",
    );
    assert_eq!(fill_row(&store), ("100".to_owned(), "1600.00".to_owned()));
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4h — two competing SELLs across a restart
// ---------------------------------------------------------------------------

/// Sellable shares from the node cache alone: position minus the leaves quantity
/// of every non-closed SELL. F6's §1.1 read, without the T+1 term, which the
/// prior-day fill makes zero here.
fn sellable(node: &Node) -> Decimal {
    node.call(|context| {
        let cache = context.cache.borrow();
        let instrument_id = InstrumentId::from(pingan().instrument_id);
        let position: Decimal = cache
            .positions_open(None, Some(&instrument_id), None, None, None)
            .iter()
            .filter(|p| p.side == PositionSide::Long)
            .map(|p| p.quantity.as_decimal())
            .sum();
        let reserved: Decimal = cache
            .orders(None, None, None, None, Some(OrderSide::Sell))
            .iter()
            .filter(|order| order.instrument_id() == instrument_id && !order.is_closed())
            .map(|order| order.leaves_qty().as_decimal())
            .sum();
        (position - reserved).max(Decimal::ZERO)
    })
    .expect("the node answers")
}

/// F6's check-and-place in **one** `Node::call`: nothing runs on the node thread
/// between the cache read and `cache.add_order`.
fn checked_sell(node: &Node, client_order_id: &'static str, quantity: u32) -> Result<(), Decimal> {
    node.call(move |context| {
        let cache = context.cache.borrow();
        let instrument_id = InstrumentId::from(pingan().instrument_id);
        let position: Decimal = cache
            .positions_open(None, Some(&instrument_id), None, None, None)
            .iter()
            .filter(|p| p.side == PositionSide::Long)
            .map(|p| p.quantity.as_decimal())
            .sum();
        let reserved: Decimal = cache
            .orders(None, None, None, None, Some(OrderSide::Sell))
            .iter()
            .filter(|order| order.instrument_id() == instrument_id && !order.is_closed())
            .map(|order| order.leaves_qty().as_decimal())
            .sum();
        let available = (position - reserved).max(Decimal::ZERO);
        drop(cache);
        if Decimal::from(quantity) > available {
            return Err(available);
        }
        trade::place_form(
            context,
            pingan(),
            OrderSide::Sell,
            Quantity::from(quantity),
            Some(Price::from("13.00")),
            ClientOrderId::from(client_order_id),
        );
        Ok(())
    })
    .expect("the node answers")
}

/// **Competing SELLs.** One order's reservation refuses the second, and the
/// refusal survives a restart: the restored SELL is back in the cache with its
/// identifiers, so a third attempt is refused too, and only its cancellation
/// frees the shares.
#[test]
fn two_competing_sells_survive_a_restart() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-sells", CN_D_MINUS_1);
    let desk_id = handle.desk_id().to_owned();

    // Day D−1: a sized book and a market buy, so day D opens with 200 shares and
    // no T+1 lock.
    publish_book(
        &node,
        pingan(),
        ("12.00", 1000),
        ("12.00", 1000),
        CN_D_MINUS_1,
    );
    within(10, "the seeding quote reaches the book", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from(pingan().instrument_id))
                .is_some()
        })
        .unwrap()
    });
    let (seed, _) = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"f8r-sells-seed","instrument_id":"000001.XSHE",
            "side":"BUY","type":"MARKET","quantity":"200","price":null}"#,
        &trade::Source::Session,
    )
    .expect("the seeding buy is accepted");
    assert_eq!(seed.outcome.clone().unwrap()["status"], "FILLED");

    // Day D, and a flat book so nothing is executable.
    advance(&node, CN_0935);
    publish_book(&node, pingan(), ("12.00", 0), ("12.00", 0), CN_0935);
    assert_eq!(sellable(&node), Decimal::from(200));

    assert_eq!(checked_sell(&node, "f8r-sells-a", 200), Ok(()));
    within(10, "the first sell rests", || {
        status(&node, "f8r-sells-a") == OrderStatus::Accepted
    });
    assert_eq!(
        checked_sell(&node, "f8r-sells-b", 200),
        Err(Decimal::ZERO),
        "the resting sell reserves every share"
    );
    let venue_id = venue_order_id(&node, "f8r-sells-a");
    let sold_balance = balance(&node, "XSHE", &desk_id);
    // 500000 − 200 × 12.00 − 3 bp. The reservation is 200 — the **share**
    // quantity, which `CashAccount::calculate_balance_locked` puts in the
    // account's single CNY balance slot for a SELL
    // (`nautilus-model-0.62.0/src/accounts/cash.rs`), because MarketRig's venue
    // account has one currency. R1 left this case unestablished; it is recorded
    // here as observed, not endorsed.
    assert_eq!(sold_balance, "497599.28 CNY|200.00 CNY|497399.28 CNY");
    assert_eq!(lock_map_len(&node, "XSHE", &desk_id), 1);
    registry.stop_all();

    // --- The restart -------------------------------------------------------
    let (registry, node) = restart(&store, &desk_id);
    advance(&node, CN_0935 + 60 * SECOND_NS);
    assert_eq!(status(&node, "f8r-sells-a"), OrderStatus::Accepted);
    assert_eq!(
        venue_order_id(&node, "f8r-sells-a"),
        venue_id,
        "the restored sell kept its venue order id"
    );
    assert_eq!(
        balance(&node, "XSHE", &desk_id),
        sold_balance,
        "the cash account is reproduced byte for byte"
    );
    assert_eq!(
        lock_map_len(&node, "XSHE", &desk_id),
        1,
        "`Portfolio::initialize_orders` rebuilt the SELL's lock map too (R1)"
    );
    assert_eq!(sellable(&node), Decimal::ZERO);
    assert_eq!(
        checked_sell(&node, "f8r-sells-c", 200),
        Err(Decimal::ZERO),
        "the reservation survived the restart"
    );

    // Only the terminal event frees the shares.
    trade::cancel(
        &store,
        &registry,
        &desk_id,
        "f8r-sells-a",
        r#"{"action_id":"f8r-sells-cancel"}"#,
        &trade::Source::Session,
    )
    .expect("the restored sell cancels");
    assert_eq!(status(&node, "f8r-sells-a"), OrderStatus::Canceled);
    assert_eq!(sellable(&node), Decimal::from(200));
    assert_eq!(checked_sell(&node, "f8r-sells-d", 200), Ok(()));
    within(10, "the replacement sell rests", || {
        status(&node, "f8r-sells-d") == OrderStatus::Accepted
    });
    assert_eq!(
        kinds(&store, &desk_id, "f8r-sells-a"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "one chain, one terminal event"
    );
    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4i — the retained R1 partial-fill history regression
// ---------------------------------------------------------------------------

/// **Retained regression (R1's "new defect found by S2").** Restoring a
/// *partially filled* order re-applies `OrderAccepted` — `(PartiallyFilled,
/// Accepted) => Accepted` is a legal transition in the pinned crate
/// (`nautilus-model-0.62.0/src/orders/mod.rs:287`), unlike the accepted case,
/// so the re-hand's event is applied and therefore published. `capture_order`
/// stores a second `OrderAccepted` stamped at the restarted node's clock, which
/// sorts before the fill, and `history_orders` can no longer rebuild the chain
/// and returns `[]`.
///
/// **Fixed in slice 014 step 1**, and asserted here as the correct outcome:
/// `crate::trade::capture_order` stores an order's `OrderSubmitted` and
/// `OrderAccepted` exactly once, so the re-hand's repeat is not new history, the
/// stored chain replays, and the fill is still in it. AE-9 executions use the
/// full synthetic quantity and so cannot produce a partial fill of their own,
/// but a partial fill can still exist from a Yahoo-mode desk, so the scenario is
/// retained.
#[test]
fn restored_partial_fill_keeps_one_acceptance_and_replays() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = open_desk(&store, "f8r-partial", CN_0935);
    let desk_id = handle.desk_id().to_owned();

    publish_book(&node, moutai(), ("1700.00", 0), ("1700.00", 0), CN_0935);
    rest(
        &store,
        &registry,
        &desk_id,
        moutai(),
        "f8r-partial-1",
        "BUY",
        100,
        "1600.00",
    );

    // An ask smaller than the order: `determine_limit_price_and_volume` sizes the
    // fill from the book level (`matching_engine/mod.rs:3950-3965`).
    let at = CN_0935 + 60 * SECOND_NS;
    advance(&node, at);
    publish_book(&node, moutai(), ("1500.00", 100), ("1500.00", 60), at);
    within(10, "the partial fill lands", || {
        status(&node, "f8r-partial-1") == OrderStatus::PartiallyFilled
    });
    assert_eq!(fills(&store), 1);
    registry.stop_all();

    let (registry, node) = restart(&store, &desk_id);
    advance(&node, CN_1530);
    let restored = trade::open_orders(&node).expect("the open orders read");
    assert_eq!(restored.len(), 1, "{restored:?}");
    assert_eq!(
        restored[0]["status"], "ACCEPTED",
        "the node's own live view: `(PartiallyFilled, Accepted)` is a legal \
         transition, so the re-hand's acceptance is applied"
    );
    assert_eq!(restored[0]["filled_quantity"], "60");
    assert_eq!(
        kinds(&store, &desk_id, "f8r-partial-1"),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "one acceptance, and the fill is still in the chain"
    );
    let history: Vec<Value> = trade::history_orders(&store, &desk_id).expect("the history reads");
    assert_eq!(history.len(), 1, "the chain replays: {history:?}");
    assert_eq!(history[0]["status"], "PARTIALLY_FILLED");
    assert_eq!(history[0]["filled_quantity"], "60");
    registry.stop_all();
}
