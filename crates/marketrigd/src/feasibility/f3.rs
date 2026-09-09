//! F3 — can recovery terminate expired orders before matching?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`; feature SPEC §5.3.)
//!
//! Every test starts a real desk node on the controlled clock from
//! [`crate::feasibility::clock`], rests a CN limit order, stops the registry,
//! and starts a **second** registry over the same store so `trade::restore`
//! runs for real.
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`restored_order_fills_against_a_crossing_quote`] — nothing in the pinned
//!    sandbox terminates a prior-day order. A restored resting order is
//!    `ACCEPTED` again and the first crossing quote fills it under the original
//!    client order id, whatever the clock says.
//! 2. [`restart_cancel_before_data_terminates_once`] — the supported mechanism:
//!    the same `TradingCommand::CancelOrder` `crate::trade::cancel` sends,
//!    issued after restoration and before any quote is published, produces one
//!    `OrderCanceled` under the original id and leaves the later crossing quote
//!    inert. Run for a next-day restart and for a same-day post-14:57 restart.
//!    In the initial spike it did **not** release the venue reservation (repaired
//!    by R1, `r1.rs`, via `Portfolio::initialize_orders` after restore), and
//!    [`a_fresh_order_on_a_restarted_node_still_releases`] shows why: the pinned
//!    `CashAccount::balances_locked` map is `#[serde(skip, default)]`, so
//!    `book_snapshots` cannot carry it and `clear_balance_locked` has nothing to
//!    recompute. A fill settles the same reservation correctly; only the
//!    cancel/expire path strands it.
//! 3. [`queued_quote_loses_to_a_queued_cancel`] — the ordering guarantee that
//!    makes this safe: `AsyncRunner::recv` is a `biased` select
//!    (`nautilus-live-0.62.0/src/runner.rs:579-606`) polling `exec_cmd_rx`
//!    *before* `data_evt_rx`, so a crossing quote already sitting in the data
//!    channel still loses to a cancel queued after it. The daemon only has to
//!    keep a quote from being **processed** before restoration, not from being
//!    sent.
//! 4. [`the_first_poll_can_beat_restoration`] — and that is exactly what the
//!    current start order does not do. `Registry::start` reaches
//!    `trade::restore` only after the node is `Running`, while
//!    `ChartDataClient::start` has already spawned the polling tasks inside
//!    `LiveNode::run` and `LiveNode::start` flushes their first replies into the
//!    cache. With a stand-in feed quoting a crossing price, the restored order
//!    fills before the daemon can decide anything — 3 restarts out of 3 on this
//!    machine. [`holding_the_first_publish_lets_recovery_win`] runs the same
//!    real poller with the first publish held until restoration and the cancel
//!    have run, and nothing fills: the fix is a reorder inside MarketRig's own
//!    data client, not a NautilusTrader change.
//! 5. [`lunch_and_afternoon_restarts_fill_once`] — the sandbox has no session:
//!    a 12:10 restart fills on a crossing quote exactly as a 13:05 one does, so
//!    the lunch gate is MarketRig's, not the venue's.
//! 6. [`stored_state_identifies_the_owning_trading_date`] — the snapshot payload
//!    carries each order's own `ts_accepted` and its whole event chain, so
//!    restoration can decide "prior day" from `book_snapshots.payload` alone;
//!    `book_snapshots.written_at_ns` cannot be used, it is MarketRig's wall
//!    clock.
//! 7. Crossing UTC midnight costs four `portfolio_equity_curve.*` firings —
//!    asserted in [`restored_order_fills_against_a_crossing_quote`] — and
//!    nothing else: they touch no order, and `crate::trade::install_capture`
//!    subscribes only order and position events, so no row is written.
//! 8. [`a_closed_market_does_not_survive_a_restart`] — F1's `InstrumentStatus`
//!    gate is per-`OrderMatchingEngine` in-memory state
//!    (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:200`, engines built
//!    lazily at `nautilus-sandbox-0.62.0/src/execution.rs:185-208`) and
//!    `book_snapshots` carries none of it. A desk that closed CN at 14:57 comes
//!    back `Open`, and the same crossing quote that matched nothing before the
//!    stop fills the restored order after it. The daemon must re-publish the
//!    session status on every node start.
//! 9. [`the_recommended_restart_order_terminates_once`] — the recommended order
//!    is **restore → terminate → status → first quote**, proven end to end.
//!    Publishing the status *before* restoration is wrong: the same
//!    `process_order` refuses everything it validates while the market is not
//!    `Open` (`mod.rs:2728-2738`, exercised here through a new order), so it
//!    would reject the orders that must survive the restart too. The status also
//!    cannot protect the restoration window at all, because it travels the data
//!    channel and finding 3 puts exec commands ahead of it.
//! 10. [`a_second_rehand_under_a_closed_market_is_a_no_op`] — restoration is
//!     idempotent at the engine level: `process_order` returns on
//!     `core.order_exists(..)` (`mod.rs:2696-2699`) before the status check, so
//!     re-handing an order the engine already holds changes nothing.
//!
//! # How prior-day state is produced (FEASIBILITY.md's rule)
//!
//! Session 1 is a real node on a `TestClock` seeded at
//! [`clock::CN_0935`] = 2026-09-09 09:35 Asia/Shanghai. The order is placed
//! through `crate::trade::submit`, and the `book_snapshots` row is the one
//! `crate::trade::capture_order` wrote when `OrderAccepted` was captured — no
//! row is written, edited, or back-dated by these tests. Session 2 is a fresh
//! `Registry` over the same `Store` and the same desk id.
//!
//! **Harness limit.** `clock::controlled_registry` fixes the desk's clock start
//! at registration and `clock::CLOCKS` memoizes per node thread, so session 2's
//! node also *starts* at `CN_0935`; a restart cannot be handed a later opening
//! instant. Each test therefore calls `clock::advance` to the intended restart
//! instant immediately after `Registry::ensure` returns and **before any market
//! data exists** — which is the same node state a real restart at that instant
//! would have, because F2 established that advancing the clock alone changes no
//! order. The one thing that cannot be modelled here is the daemon's own
//! restart-time "now": `trade::restore` would read `crate::store::now_ns()`,
//! which is wall clock and which no clock seam in this crate can move.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::execution::{CancelOrder, SubmitOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::data::{Data, InstrumentStatus, QuoteTick};
use nautilus_model::enums::{MarketStatusAction, OrderStatus};
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};

use crate::catalog::Entry;
use crate::feasibility::clock::{
    self, CN_0935, CN_1457, ClockHandle, DAY_NS, SECOND_NS, advance, controlled_registry,
    stored_events,
};
use crate::feed::{self, FeedBase, MarketState};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade;

/// `crate::trade::STRATEGY`, pinned by `f2::strategy_matches_production`.
const STRATEGY: &str = "MARKETRIG-001";

/// The order every test rests: 100 shares of Moutai at 1600.00, under a
/// 1700.00 book, so it rests on day D and crosses any later quote below 1600.
const ORDER: &str = "f3-rest-1";

const REST_BODY: &str = r#"{"action_id":"f3-rest-1","instrument_id":"600519.XSHG",
    "side":"BUY","type":"LIMIT","quantity":"100","price":"1600.00"}"#;

/// 2026-09-10 09:35 Asia/Shanghai — the next trading day's open.
const NEXT_0935: u64 = CN_0935 + DAY_NS;
/// 11:45 — the last minute of the CN morning session.
const CN_1145: u64 = CN_0935 + 7_800 * SECOND_NS;
/// 12:10 — inside the lunch break.
const CN_1210: u64 = CN_0935 + 9_300 * SECOND_NS;
/// 13:05 — just after the afternoon session opens.
const CN_1305: u64 = CN_0935 + 12_600 * SECOND_NS;
/// 15:30 — the same trading day, after the 14:57 day-lifetime deadline.
const CN_1530: u64 = CN_0935 + 21_300 * SECOND_NS;

fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

/// Produces the prior-day state: a controlled desk at 09:35 with one resting CN
/// limit buy, then stops its node. Returns the handle, which keeps the desk
/// registered for controlled time so the restart's node gets a `TestClock` too.
///
/// No feed at all, so the only market data day D ever saw is the single
/// non-crossing quote published here.
fn rested_at_0935(store: &Store, name: &'static str) -> ClockHandle {
    rested_at_0935_then(store, name, false)
}

/// [`rested_at_0935`], optionally closing the CN instrument's matching engine
/// at 14:57 before the stop — which is what the daemon would have done at the
/// day-lifetime boundary, and what
/// [`a_closed_market_does_not_survive_a_restart`] then looks for.
fn rested_at_0935_then(store: &Store, name: &'static str, close_at_1457: bool) -> ClockHandle {
    let (registry, handle) = controlled_registry(store, None, name, CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    clock::publish_quote(&node, moutai(), "1700.00", 100, CN_0935);
    let instrument_id = InstrumentId::from(moutai().instrument_id);
    within(10, "the opening quote reaches the book", || {
        node.call(move |context| context.cache.borrow().quote(&instrument_id).is_some())
            .unwrap()
    });

    let (rested, _) = trade::submit(
        &store.clone(),
        &registry,
        handle.desk_id(),
        REST_BODY,
        &trade::Source::Session,
    )
    .expect("the resting limit buy is accepted");
    assert_eq!(
        rested.outcome.clone().unwrap()["status"],
        "ACCEPTED",
        "day D leaves the order resting"
    );

    // The snapshot the restart will read was written by `capture_order` when
    // `OrderAccepted` was captured — before this registry stops.
    let payload = snapshot_payload(store, handle.desk_id());
    assert!(
        payload.contains(ORDER),
        "the day-D snapshot holds the resting order"
    );

    if close_at_1457 {
        advance(&node, CN_1457);
        publish_status(&node, MarketStatusAction::Close, CN_1457);
        // The gate is on: a crossing quote at the boundary matches nothing.
        clock::publish_quote(&node, moutai(), "1500.00", 100, CN_1457);
        await_quote(&node, "1500.00");
        assert_eq!(
            status(&node, ORDER),
            OrderStatus::Accepted,
            "the closed engine did not match the boundary quote"
        );
    }

    registry.stop_all();
    handle
}

/// Publishes one `InstrumentStatus` for the CN instrument on the same data path
/// a quote takes, and waits for the `DataEngine` to have cached it — which is
/// what proves the sandbox's status handler ran
/// (`nautilus-sandbox-0.62.0/src/execution.rs:288-308`). Copied from
/// `crate::feasibility::f1_f5`, which owns the F1 findings.
fn publish_status(node: &Node, action: MarketStatusAction, ts_ns: u64) {
    let instrument_id = InstrumentId::from(moutai().instrument_id);
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
                .map(|cached| cached.action)
                == Some(action)
        })
        .unwrap()
    });
}

/// Re-hands an already-cached order to the node exactly as
/// `crate::trade::hand_to_node(context, order, false)` does inside
/// `crate::trade::apply`: `SubmitOrder::from_order`, no `OrderInitialized`
/// announcement, the risk engine's queued endpoint. `apply` adds the order to
/// the cache first; here it is already there, which is the same state.
fn rehand(node: &Node, client_order_id: &'static str) {
    node.call(move |context| {
        let order = context
            .cache
            .borrow()
            .order(&ClientOrderId::from(client_order_id))
            .expect("the order is cached")
            .cloned();
        let command = SubmitOrder::from_order(
            &order,
            context.trader_id,
            None,
            None,
            UUID4::new(),
            context.clock.borrow().timestamp_ns(),
        );
        msgbus::send_trading_command(
            MessagingSwitchboard::risk_engine_queue_execute(),
            TradingCommand::SubmitOrder(command),
        );
    })
    .expect("the node answers");
}

/// Waits for the CN instrument's cached quote to read `price` on both sides.
fn await_quote(node: &Node, price: &'static str) {
    let instrument_id = InstrumentId::from(moutai().instrument_id);
    within(10, "the quote reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&instrument_id)
                .map(|quote| quote.ask_price)
                == Some(Price::from(price))
        })
        .unwrap()
    });
}

/// The desk's single `book_snapshots` row.
fn snapshot_payload(store: &Store, desk_id: &str) -> String {
    let desk = desk_id.to_owned();
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload FROM book_snapshots WHERE desk_id = ?1",
                [desk],
                |r| r.get(0),
            )
        })
        .expect("the snapshot row")
}

/// A second registry over the same store: the restart. `Registry::ensure` runs
/// `crate::trade::restore` before it returns.
fn restart(store: &Store, feed: Option<FeedBase>, desk_id: &str) -> (Registry, Arc<Node>) {
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), feed);
    let node = registry
        .ensure(desk_id)
        .expect("the node restores and starts");
    (registry, node)
}

fn status(node: &Node, client_order_id: &'static str) -> OrderStatus {
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

/// The desk's captured chain for [`ORDER`]: `(kind, ts_event)` oldest first.
fn chain(store: &Store, desk_id: &str) -> Vec<(String, i64)> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == ORDER)
        .map(|(_, kind, ns)| (kind, ns))
        .collect()
}

/// [`chain`], for any client order id.
fn chain_of(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<(String, i64)> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, ns)| (kind, ns))
        .collect()
}

fn kinds(chain: &[(String, i64)]) -> Vec<&str> {
    chain.iter().map(|(kind, _)| kind.as_str()).collect()
}

/// The XSHG account's CNY balance as `total|locked|free` — the sandbox's own
/// reservation accounting, which a terminal event is supposed to move.
fn balance_cny(node: &Node, desk_id: &str) -> String {
    let account_id = AccountId::from(format!("XSHG-{desk_id}").as_str());
    node.call(move |context| {
        context
            .cache
            .borrow()
            .account(&account_id)
            .expect("the venue account")
            .balances()
            .values()
            .map(|balance| format!("{}|{}|{}", balance.total, balance.locked, balance.free))
            .collect::<Vec<_>>()
            .join(",")
    })
    .expect("the node answers")
}

/// Places one extra limit order on an already-restarted node, the ordinary way.
fn place_extra(store: &Store, registry: &Registry, desk_id: &str, action_id: &str, price: &str) {
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"{price}"}}"#
    );
    let (record, _) = trade::submit(store, registry, desk_id, &body, &trade::Source::Session)
        .expect("the extra limit buy is accepted");
    assert_eq!(record.outcome.clone().unwrap()["status"], "ACCEPTED");
}

fn fills(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count")
}

/// The `CancelOrder` `crate::trade::cancel` sends, built the same way — from the
/// order's own cached instrument and venue order id — and stamped at `at_ns`
/// rather than `store::now_ns()`, so the terminal event carries the controlled
/// instant. `also_queue` runs first, inside the same closure, which is how the
/// tests put a quote in the data channel *before* the cancel reaches the
/// execution channel.
fn cancel_at(node: &Node, at_ns: u64, also_queue: Option<QuoteTick>) {
    node.call(move |context| {
        if let Some(tick) = also_queue {
            get_data_event_sender()
                .send(DataEvent::Data(Data::Quote(tick)))
                .expect("the data runner takes the quote");
        }
        let target = ClientOrderId::from(ORDER);
        let (instrument_id, venue_order_id) = {
            let cache = context.cache.borrow();
            let order = cache.order(&target).expect("the restored order is cached");
            (order.instrument_id(), order.venue_order_id())
        };
        msgbus::send_trading_command(
            MessagingSwitchboard::exec_engine_queue_execute(),
            TradingCommand::CancelOrder(CancelOrder::new(
                context.trader_id,
                None,
                StrategyId::new(STRATEGY),
                instrument_id,
                target,
                venue_order_id,
                UUID4::new(),
                UnixNanos::from(at_ns),
                None,
                None,
            )),
        );
    })
    .expect("the node answers");
}

/// A crossing quote: both sides 1500.00, under the 1600.00 buy limit.
fn crossing(ts_ns: u64) -> QuoteTick {
    let price = Price::from("1500.00");
    let size = Quantity::from(100);
    QuoteTick::new(
        InstrumentId::from(moutai().instrument_id),
        price,
        price,
        size,
        size,
        UnixNanos::from(ts_ns),
        UnixNanos::from(ts_ns),
    )
}

/// F3 (1), the baseline: nothing in the pinned sandbox expires a prior-day
/// order. The restored order is `ACCEPTED` again under its original id, and the
/// first crossing quote after the restart fills it — proving the terminal event
/// has to come from the daemon.
///
/// Also F3 (5): the advance from 09:35 day D to 09:35 day D+1 crosses UTC
/// midnight, which fires the portfolio's daily equity-curve timers. They are
/// asserted here so the design knows what a cross-day advance costs: four
/// `portfolio_equity_curve.*` firings and nothing captured.
#[test]
fn restored_order_fills_against_a_crossing_quote() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-baseline");
    let desk_id = handle.desk_id().to_owned();

    let (registry, node) = restart(&store, None, &desk_id);
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Accepted,
        "the restored order rests again"
    );
    assert_eq!(
        clock::now_ns(&node),
        CN_0935,
        "the restart's clock re-seeds at the registration instant (harness limit)"
    );

    // Cross into the next trading day with no data at all.
    let fired = advance(&node, NEXT_0935);
    assert_eq!(
        fired
            .iter()
            .filter(|name| name.starts_with("portfolio_equity_curve."))
            .count(),
        4,
        "crossing UTC midnight fires one equity-curve sample per venue: {fired:?}"
    );
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Accepted,
        "a whole day of clock, and no market data, changes nothing"
    );
    assert_eq!(
        kinds(&chain(&store, &desk_id)),
        vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"],
        "restoration added no events, and the equity-curve timers captured none"
    );

    // The next trading day's first crossing quote.
    clock::publish_quote(&node, moutai(), "1500.00", 100, NEXT_0935);
    within(10, "the restored order closes", || {
        status(&node, ORDER) != OrderStatus::Accepted
    });
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Filled,
        "the prior-day order fills on the next day's first crossing quote"
    );
    let captured = chain(&store, &desk_id);
    assert_eq!(
        kinds(&captured),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "and the fill lands on the original chain: {captured:?}"
    );
    assert_eq!(
        captured.last().unwrap().1,
        NEXT_0935 as i64,
        "stamped at the next day's instant"
    );
    // A fill *does* settle the restored reservation: it fills at the resting
    // order's own 1600.00 (maker), 160000.00 plus 48.00 commission, and `locked`
    // returns to zero. The cancel path in
    // [`restart_cancel_before_data_terminates_once`] does not — so the defect is
    // the non-fill terminal path on a restored order, not restoration as such.
    assert_eq!(
        balance_cny(&node, &desk_id),
        "339952.00 CNY|0.00 CNY|339952.00 CNY",
        "the fill settles the restored reservation"
    );
    registry.stop_all();
}

/// F3 (1a), (2) and (4): the smallest supported termination. After restoration
/// and before any quote, the daemon sends the same `CancelOrder`
/// `crate::trade::cancel` sends; the order ends `CANCELED` once under its
/// original id, and the crossing quote that follows does nothing. Run for the
/// next-day restart and for a same-day restart past the 14:57 deadline.
///
/// **Initial finding (superseded by R1): the reservation was not released**, a
/// pinned-crate limitation rather than a property of the cancel: `CashAccount::balances_locked` — the per-instrument
/// lock map every release recomputes from — is
/// `#[serde(skip, default)]`, "transient, not persisted"
/// (`nautilus-model-0.62.0/src/accounts/cash.rs:69-71`). `book_snapshots`
/// serializes `AccountAny`, so the map is empty after restoration while
/// `AccountBalance.locked` still carries the day-D figure.
/// `AccountsManager::update_balance_locked` takes the `orders_open.is_empty()`
/// branch and calls `clear_balance_locked`
/// (`nautilus-portfolio-0.62.0/src/manager.rs:384-387`), which iterates that
/// empty map (`nautilus-model-0.62.0/src/accounts/base.rs:425-443`) and
/// therefore recalculates nothing. The stranded lock survives every terminal
/// outcome; [`a_fresh_order_on_a_restarted_node_still_releases`] shows the same
/// node releases a lock it took itself. R1 (`r1.rs`) repairs it through the
/// public `Portfolio::initialize_orders` after `trade::apply`, so this test now
/// asserts the release.
#[test]
fn restart_cancel_before_data_terminates_once() {
    for (name, restart_ns) in [("f3-nextday", NEXT_0935), ("f3-1530", CN_1530)] {
        let (_dir, store) = crate::store::open_temp();
        let handle = rested_at_0935(&store, name);
        let desk_id = handle.desk_id().to_owned();
        let before = chain(&store, &desk_id);
        assert_eq!(
            kinds(&before),
            vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"],
            "day D's chain: {before:?}"
        );

        let (registry, node) = restart(&store, None, &desk_id);
        advance(&node, restart_ns);
        assert_eq!(
            balance_cny(&node, &desk_id),
            "500000.00 CNY|160000.00 CNY|340000.00 CNY",
            "restoration brought day D's reservation back with the account"
        );

        cancel_at(&node, restart_ns, None);
        within(10, "the restored order is terminated", || {
            status(&node, ORDER) == OrderStatus::Canceled
        });

        let captured = chain(&store, &desk_id);
        assert_eq!(
            kinds(&captured),
            vec![
                "OrderInitialized",
                "OrderSubmitted",
                "OrderAccepted",
                "OrderCanceled"
            ],
            "exactly one terminal event, and restoration replayed nothing: {captured:?}"
        );
        assert_eq!(
            captured.last().unwrap().1,
            restart_ns as i64,
            "`OrderCanceled.ts_event` is the instant the daemon chose"
        );
        assert!(
            trade::open_orders(&node).unwrap().is_empty(),
            "nothing is open"
        );
        // Initial spike (2026-09-09): this read `500000.00|160000.00|340000.00` —
        // the restored reservation was stranded because the snapshot cannot carry
        // `CashAccount::balances_locked`. R1 repaired it by calling
        // `Portfolio::initialize_orders` after `apply` (`r1.rs` keeps the failing
        // reproduction); the expectation below is the repaired behaviour.
        assert_eq!(
            balance_cny(&node, &desk_id),
            "500000.00 CNY|0.00 CNY|500000.00 CNY",
            "{name}: the terminal event releases the restored reservation (R1)"
        );

        // The crossing quote the restart was racing.
        clock::publish_quote(&node, moutai(), "1500.00", 100, restart_ns);
        within(5, "the crossing quote is processed", || {
            node.call(|context| {
                context
                    .cache
                    .borrow()
                    .quote(&InstrumentId::from("600519.XSHG"))
                    .map(|q| q.ask_price)
                    == Some(Price::from("1500.00"))
            })
            .unwrap()
        });
        assert_eq!(
            chain(&store, &desk_id),
            captured,
            "no event follows the terminal one"
        );
        assert_eq!(fills(&store), 0, "nothing filled");

        let history = trade::history_orders(&store, &desk_id).expect("the history reads");
        assert_eq!(history.len(), 1, "the chain appears once: {history:?}");
        assert_eq!(history[0]["client_order_id"], ORDER);
        assert_eq!(history[0]["status"], "CANCELED");
        registry.stop_all();
    }
}

/// F3 (1a), the ordering guarantee. A crossing quote is pushed into the data
/// channel *first*, in the same `Node::call` closure that then queues the
/// cancel. `AsyncRunner::recv` is a `biased` select whose `exec_cmd_rx` branch
/// precedes `data_evt_rx` (`nautilus-live-0.62.0/src/runner.rs:579-606`), so
/// the cancel is handled first and the quote finds a closed order.
///
/// This is what makes the same-job mechanism safe: the daemon must keep a quote
/// from being *processed* before restoration decides, not from being *sent*.
#[test]
fn queued_quote_loses_to_a_queued_cancel() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-queued");
    let desk_id = handle.desk_id().to_owned();

    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, NEXT_0935);
    // One closure, no await point: quote into `data_evt_rx`, then cancel into
    // `exec_cmd_rx`.
    cancel_at(&node, NEXT_0935, Some(crossing(NEXT_0935)));
    within(10, "the restored order closes", || {
        status(&node, ORDER) != OrderStatus::Accepted
    });

    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Canceled,
        "the queued cancel beats the already-queued crossing quote"
    );
    within(5, "the quote is processed too", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from("600519.XSHG"))
                .is_some()
        })
        .unwrap()
    });
    assert_eq!(fills(&store), 0, "and it filled nothing");
    assert_eq!(
        kinds(&chain(&store, &desk_id)),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ]
    );
    registry.stop_all();
}

/// F3 (1b), the race that makes a product change necessary. The stand-in feed's
/// first poll is immediate (`crate::node::poll_cn` polls once before its first
/// cadence sleep) and `LiveNode::start` flushes pending data events into the
/// cache before the node reports `Running`, while `Registry::start` reaches
/// `crate::trade::restore` only *after* that. With the stand-in quoting a
/// crossing 1500.00, the restored order is filled by data the daemon never had
/// a chance to gate.
///
/// Repeated, because it is a race: the count is reported, not asserted at one.
#[test]
fn the_first_poll_can_beat_restoration() {
    const ROUNDS: usize = 3;
    let names = ["f3-race-1", "f3-race-2", "f3-race-3"];
    let mut filled = 0;
    for name in names.into_iter().take(ROUNDS) {
        let (_dir, store) = crate::store::open_temp();
        let handle = rested_at_0935(&store, name);
        let desk_id = handle.desk_id().to_owned();

        // Every symbol gets the same crossing body; only the CN order can act on
        // it, and it crosses at 1500.00 < the 1600.00 limit.
        let (base, _hits, _requests) = feed::scripted_server(vec![(
            200,
            feed::chart_body("600519.SS", "CNY", "1500.00", 1_788_917_700),
        )]);
        let (registry, node) = restart(&store, Some(FeedBase::standin(base)), &desk_id);

        // Whatever the daemon would do next, it can only run after `ensure`
        // returned — which is where the daemon's own cancel would go.
        within(10, "the restored order settles", || {
            !matches!(
                status(&node, ORDER),
                OrderStatus::Initialized | OrderStatus::Submitted
            )
        });
        // Give the first poll a full second to land if it has not already.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && status(&node, ORDER) != OrderStatus::Filled {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if status(&node, ORDER) == OrderStatus::Filled {
            filled += 1;
            assert_eq!(fills(&store), 1, "one fill, under the original id");
        }
        registry.stop_all();
    }
    // The record for the report: how often the first poll won.
    println!("f3: the first poll filled the restored order in {filled}/{ROUNDS} restarts");
    assert!(
        filled > 0,
        "the first poll never beat restoration in {ROUNDS} rounds; the race is not \
         reproducible on this machine, which is evidence about timing, not about safety"
    );
}

/// A stand-in chart feed that answers nothing until its flag is set. Each
/// connection is served on its own thread, so the whole catalog's first poll is
/// held at once — the smallest test-only model of "hold the first publish until
/// restoration has decided".
fn held_server(body: String) -> (String, Arc<AtomicBool>) {
    use std::io::{BufRead, BufReader, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let (body, flag) = (body.clone(), Arc::clone(&flag));
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    (base, released)
}

/// F3 (1b), the proposed fix, proven on the real poller. The only change from
/// [`the_first_poll_can_beat_restoration`] is that the feed publishes nothing
/// until restoration has run *and* the daemon's cancel is queued. Same real
/// `ChartDataClient`, same `poll_cn` task, same crossing 1500.00 body: the
/// restored order ends `CANCELED` and the quote that follows fills nothing.
///
/// In the product this is a reorder inside MarketRig's own code — the first
/// publish held until `trade::restore` has decided — not a NautilusTrader
/// change.
#[test]
fn holding_the_first_publish_lets_recovery_win() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-gated");
    let desk_id = handle.desk_id().to_owned();

    let (base, release) = held_server(feed::chart_body(
        "600519.SS",
        "CNY",
        "1500.00",
        1_788_917_700,
    ));
    let (registry, node) = restart(&store, Some(FeedBase::standin(base)), &desk_id);
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Accepted,
        "restoration finished with no quote in the book"
    );

    advance(&node, NEXT_0935);
    cancel_at(&node, NEXT_0935, None);
    within(10, "the restored order is terminated", || {
        status(&node, ORDER) == OrderStatus::Canceled
    });

    // Only now does the feed answer.
    release.store(true, Ordering::SeqCst);
    within(10, "the held crossing quote lands", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from("600519.XSHG"))
                .map(|q| q.ask_price)
                == Some(Price::from("1500.00"))
        })
        .unwrap()
    });
    assert_eq!(fills(&store), 0, "the gated restart filled nothing");
    assert_eq!(
        kinds(&chain(&store, &desk_id)),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ]
    );
    registry.stop_all();
}

/// F3 (3): the sandbox has no session. A restart inside the lunch break fills a
/// crossing quote exactly as an afternoon restart does — so the 11:30–13:00 gate
/// is MarketRig's to enforce, and after 13:00 the same restored order fills once
/// under the same id.
#[test]
fn lunch_and_afternoon_restarts_fill_once() {
    for (name, restart_ns) in [("f3-lunch", CN_1210), ("f3-afternoon", CN_1305)] {
        let (_dir, store) = crate::store::open_temp();
        let handle = rested_at_0935(&store, name);
        let desk_id = handle.desk_id().to_owned();

        // Day D ran to 11:45 before the stop; the clock is a fresh node's, so the
        // test advances to the restart instant with no data in between.
        let (registry, node) = restart(&store, None, &desk_id);
        advance(&node, CN_1145);
        advance(&node, restart_ns);
        clock::publish_quote(&node, moutai(), "1500.00", 100, restart_ns);
        within(10, "the restored order closes", || {
            status(&node, ORDER) != OrderStatus::Accepted
        });

        assert_eq!(
            status(&node, ORDER),
            OrderStatus::Filled,
            "{name}: the sandbox fills whatever the session"
        );
        let captured = chain(&store, &desk_id);
        assert_eq!(
            kinds(&captured),
            vec![
                "OrderInitialized",
                "OrderSubmitted",
                "OrderAccepted",
                "OrderFilled"
            ],
            "{name}: exactly one fill under the original id: {captured:?}"
        );
        assert_eq!(fills(&store), 1, "{name}: one fill row");
        assert_eq!(captured.last().unwrap().1, restart_ns as i64);

        let history = trade::history_orders(&store, &desk_id).expect("the history reads");
        assert_eq!(history.len(), 1, "{name}: {history:?}");
        assert_eq!(history[0]["client_order_id"], ORDER);
        assert_eq!(history[0]["status"], "FILLED");
        registry.stop_all();
    }
}

/// F3 (5) and (6): what restoration can decide from stored data alone.
///
/// `book_snapshots.written_at_ns` is `crate::store::now_ns()` — MarketRig's wall
/// clock — so it says when the row was written, never which trading date the
/// order belongs to. The snapshot *payload* carries the order's own event chain,
/// including `OrderAccepted.ts_event`, which is NautilusTrader's stamp and is
/// the field the design should key "prior day" on. `order_events` holds the same
/// instant, but the snapshot alone is enough, so restoration needs no second
/// query.
#[test]
fn stored_state_identifies_the_owning_trading_date() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-instants");
    let desk_id = handle.desk_id().to_owned();

    let written_at: i64 = {
        let desk = desk_id.clone();
        store
            .call(move |conn| {
                conn.query_row(
                    "SELECT written_at_ns FROM book_snapshots WHERE desk_id = ?1",
                    [desk],
                    |r| r.get(0),
                )
            })
            .expect("the snapshot row")
    };
    assert_ne!(
        written_at, CN_0935 as i64,
        "`book_snapshots.written_at_ns` is wall clock, not the trading instant"
    );

    let accepted = chain(&store, &desk_id)
        .into_iter()
        .find(|(kind, _)| kind == "OrderAccepted")
        .expect("the acceptance was captured");
    assert_eq!(
        accepted.1, CN_0935 as i64,
        "`order_events.occurred_at_ns` for OrderAccepted is the venue clock's instant"
    );

    // The same instant is inside the snapshot the restart reads, on the order's
    // own serialized chain — no second table needed.
    let payload = snapshot_payload(&store, &desk_id);
    let snapshot: serde_json::Value = serde_json::from_str(&payload).expect("the snapshot parses");
    let order = snapshot["orders"]
        .as_array()
        .and_then(|orders| orders.first())
        .expect("the resting order is in the snapshot");
    // `OrderAny` serializes externally tagged: `{"Limit": {"core": {...}}}`.
    let core = &order["Limit"]["core"];
    assert_eq!(
        core["client_order_id"].as_str(),
        Some(ORDER),
        "the snapshot's order is the one day D rested"
    );
    assert_eq!(
        core["ts_accepted"].as_u64(),
        Some(CN_0935),
        "`ts_accepted` — the field the design should key the owning trading date on"
    );
    let stamps = core["events"]
        .as_array()
        .expect("the order carries its event chain")
        .iter()
        .flat_map(|event| event.as_object().map(|o| o.values().collect::<Vec<_>>()))
        .flatten()
        .map(|event| {
            (
                event["type"].as_str().unwrap_or_default().to_owned(),
                event["ts_event"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stamps,
        vec![
            ("OrderInitialized".to_owned(), CN_0935),
            ("OrderSubmitted".to_owned(), CN_0935),
            ("OrderAccepted".to_owned(), CN_0935),
        ],
        "and the whole day-D chain is inside the snapshot"
    );
    assert_eq!(
        core["ts_init"].as_u64(),
        Some(CN_0935),
        "`ts_init` is the order's creation instant, which is the same here only \
         because day D placed and accepted it in one instant"
    );
}

/// The contrast that names the cause of the stranded reservation in
/// [`restart_cancel_before_data_terminates_once`]: on the very same restarted
/// node, an order the node locks *itself* is released in full when it is
/// canceled. Only the reservation restoration brought back is stuck, because
/// `CashAccount::balances_locked` does not survive `book_snapshots`.
#[test]
fn a_fresh_order_on_a_restarted_node_still_releases() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-fresh");
    let desk_id = handle.desk_id().to_owned();

    let (registry, node) = restart(&store, None, &desk_id);
    let restored = balance_cny(&node, &desk_id);
    assert_eq!(restored, "500000.00 CNY|160000.00 CNY|340000.00 CNY");

    place_extra(&store, &registry, &desk_id, "f3-fresh-1", "1400.00");
    assert_eq!(
        balance_cny(&node, &desk_id),
        "500000.00 CNY|300000.00 CNY|200000.00 CNY",
        "the fresh order's 140000.00 lock is taken by this node"
    );

    trade::cancel(
        &store,
        &registry,
        &desk_id,
        "f3-fresh-1",
        r#"{"action_id":"f3-fresh-cancel"}"#,
        &trade::Source::Session,
    )
    .expect("the fresh order cancels");
    assert_eq!(
        balance_cny(&node, &desk_id),
        restored,
        "and its lock is released in full, back to the stranded restored figure"
    );
    registry.stop_all();
}

/// F3 × F1: `MarketStatus` is per-`OrderMatchingEngine` in-memory state and does
/// **not** survive a restart. Day D ends with the CN instrument `Closed` — the
/// F1 gate, proven there to stop matching and refuse new orders — and the
/// crossing boundary quote matches nothing. The restart builds a brand-new
/// engine at `MarketStatus::Open`
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:200`, created lazily
/// by `nautilus-sandbox-0.62.0/src/execution.rs:185-208`), `book_snapshots`
/// carries no status, and the very same crossing quote now fills the restored
/// order.
///
/// So the daemon must re-publish the session status on every node start; a
/// restart is always a reopened market until it does.
#[test]
fn a_closed_market_does_not_survive_a_restart() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935_then(&store, "f3-status-restart", true);
    let desk_id = handle.desk_id().to_owned();
    assert_eq!(fills(&store), 0, "day D closed with nothing filled");

    let (registry, node) = restart(&store, None, &desk_id);
    assert_eq!(status(&node, ORDER), OrderStatus::Accepted);
    assert!(
        node.call(|context| {
            context
                .cache
                .borrow()
                .instrument_status(&InstrumentId::from("600519.XSHG"))
                .is_none()
        })
        .unwrap(),
        "and no status came back with the snapshot"
    );

    advance(&node, CN_1530);
    clock::publish_quote(&node, moutai(), "1500.00", 100, CN_1530);
    within(10, "the restored order closes", || {
        status(&node, ORDER) != OrderStatus::Accepted
    });
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Filled,
        "the restart reopened the market and the day-D gate is gone"
    );
    registry.stop_all();
}

/// F3 × F1, the recommended restart order, and the counter-evidence against the
/// other one.
///
/// Recommended: **restore → terminate → status → first quote.** Proven here on
/// the restarted node: the `CancelOrder` terminates the prior-day order once,
/// `MarketStatusAction::Close` then re-establishes the session gate, and the
/// first quote after it fills nothing.
///
/// Not "status → restore": on a fresh node the engine core holds no order, so
/// `OrderMatchingEngine::process_order` runs its validate block for every
/// re-handed order, and that block refuses anything while
/// `market_status != MarketStatus::Open`
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:2728-2738`). The
/// refusal itself is exercised here, on that same function, with the sandbox's
/// own words. Applied to restoration it would turn every re-handed order into
/// `OrderRejected` — including the ones that must survive: an order rested at
/// 09:35 and restarted at 12:10 still has to be resting at 13:00. The restored
/// order's own first hand could not be exercised from test-only code; see
/// [`a_second_rehand_under_a_closed_market_is_a_no_op`].
///
/// The status is also the wrong tool for the restoration window on its own: it
/// travels the **data** channel, and `AsyncRunner::recv` drains `exec_cmd_rx`
/// before `data_evt_rx`, so a status queued in the same job as the re-handed
/// `SubmitOrder` is processed *after* it. What protects the window is holding
/// the first publish ([`holding_the_first_publish_lets_recovery_win`]); the
/// status is what re-establishes the steady-state session gate afterwards.
#[test]
fn the_recommended_restart_order_terminates_once() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-order-status");
    let desk_id = handle.desk_id().to_owned();

    // 1. restore — done by `Registry::ensure`, with no data published at all.
    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    assert_eq!(status(&node, ORDER), OrderStatus::Accepted);

    // 2. terminate the expired order.
    cancel_at(&node, CN_1530, None);
    within(10, "the restored order is terminated", || {
        status(&node, ORDER) == OrderStatus::Canceled
    });
    let captured = chain(&store, &desk_id);
    assert_eq!(
        kinds(&captured),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "exactly one terminal event: {captured:?}"
    );

    // 3. re-establish the session gate for the current phase.
    publish_status(&node, MarketStatusAction::Close, CN_1530);

    // 4. only now the first quote.
    clock::publish_quote(&node, moutai(), "1500.00", 100, CN_1530);
    await_quote(&node, "1500.00");
    assert_eq!(
        fills(&store),
        0,
        "no fill: the order is terminal and the market closed"
    );
    assert_eq!(
        chain(&store, &desk_id),
        captured,
        "and no event follows the terminal one"
    );

    // The counter-evidence: while the market is closed, `process_order` refuses
    // whatever it is handed — which is what restoration would hand it if the
    // status were published first.
    let refused = trade::submit(
        &store,
        &registry,
        &desk_id,
        r#"{"action_id":"f3-after-close","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"1600.00"}"#,
        &trade::Source::Session,
    )
    .expect_err("the closed engine refuses the order");
    let crate::trade::TradeError::Rejected(reason) = &refused else {
        panic!("expected a sandbox rejection, got {refused:?}");
    };
    assert_eq!(
        reason, "Market 600519.XSHG is CLOSED, cannot accept order f3-after-close",
        "the sandbox's own words — and what every re-handed order would get"
    );
    assert_eq!(
        kinds(&chain_of(&store, &desk_id, "f3-after-close")),
        vec!["OrderInitialized", "OrderSubmitted", "OrderRejected"],
        "one native terminal event, not a fabricated one"
    );
    registry.stop_all();
}

/// Why "status → restore" could not be exercised here, and what was measured
/// instead.
///
/// `trade::restore` runs inside `Registry::ensure`, before any test job can
/// reach the node, so a status cannot be published ahead of the *first*
/// re-hand. Handing the order a **second** time under a `Closed` market — the
/// closest reachable approximation — turns out to be a no-op:
/// `OrderMatchingEngine::process_order` returns immediately on
/// `self.core.order_exists(order.client_order_id())`
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:2696-2699`), before
/// the `market_status` check at `:2728`. Nothing is captured, the order stays
/// `ACCEPTED` and open, and the engine still holds it — proven by reopening and
/// filling it on the next crossing quote.
///
/// So restoration is idempotent at the engine level, but the "status first"
/// ordering itself stays a source-level claim: `process_order` rejects whatever
/// it validates while `market_status != MarketStatus::Open`, which
/// [`the_recommended_restart_order_terminates_once`] exercises on the same
/// function through a new order, and F1's `pause_gates_cn_matching_and_submission`
/// exercises for `Paused`.
#[test]
fn a_second_rehand_under_a_closed_market_is_a_no_op() {
    let (_dir, store) = crate::store::open_temp();
    let handle = rested_at_0935(&store, "f3-rehand");
    let desk_id = handle.desk_id().to_owned();
    let (registry, node) = restart(&store, None, &desk_id);
    advance(&node, CN_1530);
    let restored = chain(&store, &desk_id);

    publish_status(&node, MarketStatusAction::Close, CN_1530);
    rehand(&node, ORDER);
    // A rejection would have to travel exec_evt_rx, which the runner drains
    // ahead of the data channel; the reopen and quote below give it every
    // chance to land.
    assert_eq!(
        chain(&store, &desk_id),
        restored,
        "the second re-hand produced no event at all"
    );
    assert_eq!(status(&node, ORDER), OrderStatus::Accepted);
    assert_eq!(trade::open_orders(&node).unwrap().len(), 1);

    publish_status(&node, MarketStatusAction::Trading, CN_1530);
    clock::publish_quote(&node, moutai(), "1500.00", 100, CN_1530);
    within(10, "the restored order closes", || {
        status(&node, ORDER) != OrderStatus::Accepted
    });
    assert_eq!(
        status(&node, ORDER),
        OrderStatus::Filled,
        "the engine still held the order, so the re-hand neither rejected nor \
         dropped it"
    );
    assert_eq!(fills(&store), 1, "and it filled exactly once");
    registry.stop_all();
}
