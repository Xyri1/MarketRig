//! F6 — can checks and reservations be serialized with persistence?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`; feature SPEC §1.1, §5.2.)
//!
//! Every test runs the real `LiveNode` on the controlled clock from
//! [`crate::feasibility::clock`] with **no feed**: yesterday's buy is filled at
//! `D−1 09:35`, the clock is advanced a whole day, and every later quote is
//! published by hand. The instrument is `000001.XSHE` (Ping An Bank, tick 0.01,
//! lot 100) at 12.00 rather than Moutai, because a CN venue account holds
//! 500,000 CNY (`crate::node::seed`) and 300 Moutai does not fit in it.
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`sellable_facts_are_one_cache_read`] — position, today's BUY fills, and
//!    outstanding SELL leaves are **all** in the node's cache and readable in one
//!    `Node::call` closure: `cache.positions_open(None, Some(&id), …)` gives the
//!    quantity *and* `Position::events: Vec<OrderFilled>` (per-fill `last_qty`,
//!    `order_side`, `ts_event`), and `cache.orders(…)` gives `leaves_qty()`. No
//!    store read is needed, and no store read is *safe*: a probe subscribed ahead
//!    of `crate::trade::install_capture` shows the cache already carrying the
//!    fill while the `fills` table has no row for it.
//! 2. [`competing_sells_serialize_in_one_node_job`] — today's `trade::submit` has
//!    no eligibility check, so two concurrent SELL 200 against a 300 position are
//!    both accepted (reserved 400 > position 300). Moving the check into the same
//!    node job as `trade::place` makes exactly one win, 20 races out of 20. The
//!    reservation must count `!is_closed()` orders, not `orders_open`: MarketRig's
//!    submit is asynchronous through `risk_engine_queue_execute`, so the winner is
//!    still `INITIALIZED` when the loser's check runs.
//! 3. [`partial_fill_reduces_the_reservation`] — `leaves_qty()` falls with the
//!    partial fill and the position falls with it, so `position − reserved` is
//!    invariant across a SELL fill and the arithmetic needs no extra bookkeeping.
//! 4. [`only_a_confirmed_cancel_releases`] — `CancelOrder` on the queued endpoint
//!    is deferred, so inside the job that sends it the order is still open and
//!    still reserved; the release is observable only once `OrderCanceled` is in
//!    the cache, which is exactly when `trade::cancel` returns.
//! 5. [`pending_approval_reserves_nothing_then_reruns_unchecked`] — a pending
//!    approval puts nothing in the node, so it reserves nothing, and two of them
//!    can name the same shares. Today's `trade::decide` re-runs `place_and_settle`
//!    with no eligibility check at all, so both are accepted and 600 shares are
//!    reserved against a 300 position, out of session. The one native guard is the
//!    matching engine's cash-account short-sell check
//!    (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:2876-2895`): it
//!    rejects a SELL that is not reduce-only against the *cached position*, so it
//!    catches an outright overdraft and nothing else — not T+1 locks, not other
//!    resting sells, and not two simultaneous submissions. The same test records
//!    what `finish` stores for a refusal today: the order's own projection, with
//!    the sandbox's reason nowhere in it.
//! 6. [`odd_lot_sell_is_refused_by_marketrig_alone`] — an `Equity`'s
//!    `size_increment()` is the hard-coded `Quantity::from(1)` and its
//!    `size_precision()` is 0, with `lot_size` a separate advisory field neither
//!    the risk engine nor the matching engine reads. SELL 150 on a 250 position
//!    fills natively; the only thing refusing it is `crate::trade::validate`.

use std::rc::Rc;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nautilus_common::cache::Cache;
use nautilus_common::messages::execution::{CancelOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard, TypedHandler};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::enums::{OrderSide, OrderStatus, PositionSide};
use nautilus_model::events::OrderEventAny;
use nautilus_model::identifiers::{ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::instruments::Instrument;
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};
use rust_decimal::Decimal;

use crate::catalog::Entry;
use crate::feasibility::clock::{
    CN_0935, ClockHandle, DAY_NS, SECOND_NS, advance, controlled_registry, publish_book,
};
use crate::node::{Node, Registry, within};
use crate::policy::Decision;
use crate::store::Store;
use crate::trade;

/// The strategy identity `crate::trade` puts on every MarketRig order; pinned by
/// `crate::feasibility::f2::strategy_matches_production`.
const STRATEGY: &str = "MARKETRIG-001";

/// 2026-09-09 00:00:00 Asia/Shanghai — the day boundary §1.1 sums today's BUY
/// fills from. [`CN_0935`] is 09:35 of the same day.
const CN_D_MIDNIGHT: u64 = CN_0935 - (9 * 3600 + 35 * 60) * SECOND_NS;

/// The previous trading day's 09:35, where every test seeds its holding.
const CN_D_MINUS_1: u64 = CN_0935 - DAY_NS;

/// 2026-09-09 11:45 Asia/Shanghai — inside the lunch break, so §5.1 forbids
/// execution there.
const CN_1145: u64 = CN_0935 + (2 * 3600 + 10 * 60) * SECOND_NS;

/// Ping An Bank on the Shenzhen main board: a `CN` catalog entry, tick 0.01,
/// lot 100, cheap enough that 300 shares fit in the venue's 500,000 CNY.
fn pingan() -> &'static Entry {
    crate::catalog::find("000001.XSHE").expect("the CN catalog entry")
}

fn pingan_id() -> InstrumentId {
    InstrumentId::from(pingan().instrument_id)
}

// ---------------------------------------------------------------------------
// §1.1 arithmetic, computed from the node's cache alone
// ---------------------------------------------------------------------------

/// The three §1.1 quantities and their result, as decimals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sellable {
    position: Decimal,
    locked: Decimal,
    reserved: Decimal,
    sellable: Decimal,
}

impl Sellable {
    /// The §1.2 refusal sentence, so the prototype answers what the SPEC asks it
    /// to answer.
    fn refusal(&self, quantity: u32) -> String {
        format!(
            "quantity {quantity} exceeds sellable {} for {}: {} bought today are \
             locked by T+1; {} reserved by outstanding sells",
            self.sellable,
            pingan().instrument_id,
            self.locked,
            self.reserved
        )
    }
}

/// §1.1 from one borrow of the node's cache, with no store read at all.
///
/// - **position** — `Cache::positions_open` filtered to this instrument. There is
///   no `position_for_instrument` accessor in the pinned cache
///   (`nautilus-common-0.62.0/src/cache/mod.rs:1112`).
/// - **locked** — `Position::events` is a public `Vec<OrderFilled>`
///   (`nautilus-model-0.62.0/src/position.rs:63`); each carries `order_side`,
///   `last_qty` and `ts_event`, so today's BUY fills are summed straight off the
///   cached position. Reading them from `cache.order(...)` instead would need one
///   lookup per order id the day touched.
/// - **reserved** — every SELL for the instrument that is not closed, by
///   `leaves_qty()`. `orders_open` is **wrong** here: it matches only
///   `ACCEPTED | TRIGGERED | PENDING_* | PARTIALLY_FILLED`
///   (`nautilus-model-0.62.0/src/orders/mod.rs:471`), and MarketRig's own submit
///   is still `INITIALIZED` when it returns from `place`, because the
///   `SubmitOrder` goes to `risk_engine_queue_execute` and is processed on a later
///   runner turn.
fn sellable_from_cache(cache: &Cache, day_start_ns: u64) -> Sellable {
    let instrument_id = pingan_id();
    let positions = cache.positions_open(None, Some(&instrument_id), None, None, None);
    let position: Decimal = positions
        .iter()
        .filter(|p| p.side == PositionSide::Long)
        .map(|p| p.quantity.as_decimal())
        .sum();
    let locked: Decimal = positions
        .iter()
        .flat_map(|p| p.events.iter())
        .filter(|fill| fill.order_side == OrderSide::Buy && fill.ts_event.as_u64() >= day_start_ns)
        .map(|fill| fill.last_qty.as_decimal())
        .sum();
    let reserved: Decimal = cache
        .orders(None, None, None, None, Some(OrderSide::Sell))
        .iter()
        .filter(|order| order.instrument_id() == instrument_id && !order.is_closed())
        .map(|order| order.leaves_qty().as_decimal())
        .sum();
    Sellable {
        position,
        locked,
        reserved,
        sellable: (position - locked - reserved).max(Decimal::ZERO),
    }
}

/// [`sellable_from_cache`] as a node job, for assertions between actions.
fn sellable(node: &Node) -> Sellable {
    node.call(|context| sellable_from_cache(&context.cache.borrow(), CN_D_MIDNIGHT))
        .expect("the node answers")
}

// ---------------------------------------------------------------------------
// The prototype: one node job that checks and places
// ---------------------------------------------------------------------------

/// The §1.1/§1.2 check and `crate::trade::place` in **one** `Node::call` closure,
/// which is the whole serialization proposal. Nothing else can run on the node
/// thread between the cache read and `cache.add_order`, so two of these racing
/// cannot both pass.
///
/// A `LIMIT` above the standing bid, so an accepted order rests and the test is
/// about the check rather than the fill.
fn checked_sell(
    node: &Node,
    client_order_id: String,
    quantity: u32,
    price: &'static str,
) -> Result<Sellable, (String, Sellable)> {
    node.call(move |context| {
        let state = sellable_from_cache(&context.cache.borrow(), CN_D_MIDNIGHT);
        if Decimal::from(quantity) > state.sellable {
            return Err((state.refusal(quantity), state));
        }
        trade::place_form(
            context,
            pingan(),
            OrderSide::Sell,
            Quantity::from(quantity),
            Some(Price::from(price)),
            ClientOrderId::from(client_order_id.as_str()),
        );
        Ok(state)
    })
    .expect("the node answers")
}

/// `crate::trade::place` with no check at all, for the quantities `validate`
/// refuses and for seeding a position.
fn place_direct(
    node: &Node,
    client_order_id: &'static str,
    side: OrderSide,
    quantity: u32,
    price: Option<&'static str>,
) {
    node.call(move |context| {
        trade::place_form(
            context,
            pingan(),
            side,
            Quantity::from(quantity),
            price.map(Price::from),
            ClientOrderId::from(client_order_id),
        );
    })
    .expect("the node answers");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A started desk on the controlled clock at `D−1 09:35`, no feed, with a
/// standing two-sided 12.00 book deep enough (1000 lots each side) for the
/// seeding fills.
fn desk_yesterday(store: &Store, name: &'static str) -> (Registry, ClockHandle, Arc<Node>) {
    let (registry, handle) = controlled_registry(store, None, name, CN_D_MINUS_1);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    book(&node, "12.00", 1000, CN_D_MINUS_1);
    (registry, handle, node)
}

/// A two-sided quote at one price, both sizes equal.
fn book(node: &Node, price: &'static str, size: u32, ts_ns: u64) {
    publish_book(node, pingan(), (price, size), (price, size), ts_ns);
    let want = Price::from(price);
    within(10, "the quote reaches the book", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&pingan_id())
                .map(|q| q.bid_price)
                == Some(want)
        })
        .unwrap()
    });
}

/// Seeds `quantity` shares filled on `D−1` and moves the clock to `D 09:35`, so
/// those fills are yesterday's for §1.1 and nothing is locked.
fn hold_from_yesterday(store: &Store, registry: &Registry, node: &Node, desk_id: &str, qty: u32) {
    let (record, _) = trade::submit(
        store,
        registry,
        desk_id,
        &format!(
            r#"{{"action_id":"f6-seed-buy","instrument_id":"000001.XSHE",
                 "side":"BUY","type":"MARKET","quantity":"{qty}","price":null}}"#
        ),
        &trade::Source::Session,
    )
    .expect("the seeding market buy is accepted");
    let outcome = record.outcome.clone().unwrap();
    assert_eq!(outcome["status"], "FILLED", "{outcome}");

    // A whole day, dispatching every time event it releases: the sandbox's
    // per-venue 60 s expiry sweep and the portfolio's UTC-midnight equity sample.
    advance(node, CN_0935);
    assert_eq!(
        sellable(node),
        Sellable {
            position: Decimal::from(qty),
            locked: Decimal::ZERO,
            reserved: Decimal::ZERO,
            sellable: Decimal::from(qty),
        },
        "yesterday's fill is unlocked by the Shanghai day boundary"
    );
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

/// Waits for one order to reach `want`, naming what it reached instead.
fn await_status(node: &Node, client_order_id: &str, want: OrderStatus) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = status(node, client_order_id);
        if seen == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{client_order_id} settled as {seen}, not {want}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn fill_rows(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count reads")
}

/// The installation policy a `PUT /settings/policies` would have written.
fn policy(store: &Store, value: &'static str) {
    store
        .unit(move |tx| {
            tx.execute(
                "UPDATE installation_settings SET paper_order_policy = ?1 WHERE id = 1",
                [value],
            )
        })
        .expect("the policy is written");
}

/// The sandbox's own reason for the order's refusal, from the stored event
/// payload — the only place it survives (per D38).
///
/// Unused since slice 014 step 3 turned F6's "the only native guard is the
/// position overdraft" case into a MarketRig eligibility refusal with no native
/// order behind it; kept because the spike's own vocabulary is what makes these
/// files readable as a record.
#[expect(dead_code, reason = "the spike's vocabulary, see above")]
fn last_refusal(store: &Store, desk_id: &str, client_order_id: &str) -> Option<String> {
    use rusqlite::OptionalExtension;

    let (desk, order) = (desk_id.to_owned(), client_order_id.to_owned());
    store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload FROM order_events WHERE desk_id = ?1 AND client_order_id = ?2 \
                 AND kind IN ('OrderRejected', 'OrderDenied') \
                 ORDER BY occurred_at_ns DESC, id DESC LIMIT 1",
                rusqlite::params![desk, order],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .expect("the payload reads")
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
        // NautilusTrader serializes an order event as a one-key object naming the
        // variant, so the reason is one level down (`crate::trade::stored_event_id`).
        .and_then(|event| {
            let (_, body) = event.as_object()?.iter().next()?;
            body["reason"].as_str().map(str::to_owned)
        })
}

// ---------------------------------------------------------------------------
// 1 — where the facts live, and whether the store lags the cache
// ---------------------------------------------------------------------------

/// F6 (1). The three §1.1 inputs come from one borrow of the node's cache, and
/// the `fills` table is *behind* that cache while an order event is being
/// dispatched — so the check must read the cache, inside the node job, and never
/// the store.
#[test]
fn sellable_facts_are_one_cache_read() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-facts");

    // A probe ahead of `trade::install_capture` (priority 20 beats its 10;
    // `nautilus-common-0.62.0/src/msgbus/typed_router.rs:48` — higher first), so
    // it sees exactly what the cache and the store hold at publish time.
    let seen: Arc<Mutex<Vec<(String, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let (probe, writer) = (Arc::clone(&seen), store.clone());
    node.call(move |context| {
        let cache = Rc::clone(&context.cache);
        msgbus::subscribe_order_events(
            "events.order.*".into(),
            TypedHandler::from(move |event: &OrderEventAny| {
                let OrderEventAny::Filled(fill) = event else {
                    return;
                };
                let filled = cache
                    .borrow()
                    .order(&fill.client_order_id)
                    .map(|order| order.filled_qty().to_string())
                    .unwrap_or_else(|| "<uncached>".to_owned());
                let rows = writer
                    .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
                    .unwrap_or(-1);
                probe.lock().unwrap().push((filled, rows));
            }),
            Some(20),
        );
    })
    .expect("the probe subscribes on the node thread");

    hold_from_yesterday(&store, &registry, &node, handle.desk_id(), 300);

    // The window, measured: at publish time the cache already carries the fill
    // and the `fills` table does not.
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [("300".to_owned(), 0_i64)],
        "the cache is ahead of the store while the fill event is dispatched"
    );
    // And it is closed before the node thread takes another job: `capture_order`
    // blocks on `Store::unit` inside the handler, so by the time any `Node::call`
    // runs the row is committed.
    assert_eq!(fill_rows(&store), 1, "the fill row is committed");

    // Today's own BUY locks the same quantity it adds, so `sellable` is unmoved.
    trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f6-facts-buy","instrument_id":"000001.XSHE",
            "side":"BUY","type":"MARKET","quantity":"100","price":null}"#,
        &trade::Source::Session,
    )
    .expect("today's buy is accepted");
    assert_eq!(
        sellable(&node),
        Sellable {
            position: Decimal::from(400),
            locked: Decimal::from(100),
            reserved: Decimal::ZERO,
            sellable: Decimal::from(300),
        },
        "today's BUY fill adds to the position and to the lock in equal measure"
    );

    // The same locked figure from the `fills` table: the store agrees, once it
    // has caught up — but only the cache can be read inside the placing job.
    let desk = handle.desk_id().to_owned();
    let locked_from_store: Vec<String> = store
        .call(move |conn| {
            conn.prepare(
                "SELECT quantity FROM fills WHERE desk_id = ?1 AND instrument_id = ?2 \
                 AND side = 'BUY' AND occurred_at_ns >= ?3",
            )?
            .query_map(
                rusqlite::params![desk, "000001.XSHE", CN_D_MIDNIGHT as i64],
                |r| r.get(0),
            )?
            .collect()
        })
        .expect("today's BUY fills read");
    assert_eq!(
        locked_from_store,
        ["100"],
        "the same lock, one commit later"
    );

    // A resting SELL is the third input, and it is visible the moment `place`
    // returns even though its `SubmitOrder` has not been processed yet.
    let placed = checked_sell(&node, "f6-facts-sell".into(), 300, "13.00")
        .expect("300 is sellable")
        .sellable;
    assert_eq!(placed, Decimal::from(300));
    assert_eq!(
        sellable(&node).reserved,
        Decimal::from(300),
        "the just-placed SELL reserves immediately"
    );
    await_status(&node, "f6-facts-sell", OrderStatus::Accepted);
    assert_eq!(sellable(&node).sellable, Decimal::ZERO);

    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 2 — competing sells
// ---------------------------------------------------------------------------

/// F6 (2). Today's `trade::submit` accepts both halves of the race; the
/// check-and-place job accepts exactly one, twenty races running.
#[test]
fn competing_sells_serialize_in_one_node_job() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-race");
    hold_from_yesterday(&store, &registry, &node, handle.desk_id(), 300);

    // --- what ships today --------------------------------------------------
    let gate = Barrier::new(2);
    let both: Vec<bool> = thread::scope(|scope| {
        let racers: Vec<_> = ["f6-today-a", "f6-today-b"]
            .into_iter()
            .map(|action_id| {
                let (store, registry, desk, gate) = (&store, &registry, handle.desk_id(), &gate);
                scope.spawn(move || {
                    gate.wait();
                    trade::submit(
                        store,
                        registry,
                        desk,
                        &format!(
                            r#"{{"action_id":"{action_id}","instrument_id":"000001.XSHE",
                                 "side":"SELL","type":"LIMIT","quantity":"200","price":"13.00"}}"#
                        ),
                        &trade::Source::Session,
                    )
                    .is_ok()
                })
            })
            .collect();
        racers.into_iter().map(|r| r.join().unwrap()).collect()
    });
    // F6 found `trade::submit` accepting *both*, oversold. Production runs the
    // §1.1 read and the placement in one admission closure now, so exactly one
    // wins (slice 014 step 3).
    assert_eq!(
        both.iter().filter(|accepted| **accepted).count(),
        1,
        "exactly one of two competing sells is accepted: {both:?}"
    );
    let winner = if both[0] { "f6-today-a" } else { "f6-today-b" };
    await_status(&node, winner, OrderStatus::Accepted);
    let reserved = sellable(&node);
    assert_eq!(
        (reserved.position, reserved.reserved, reserved.sellable),
        (Decimal::from(300), Decimal::from(200), Decimal::from(100)),
        "only the winner's 200 shares are reserved: {reserved:?}"
    );

    // Clear the book for the prototype: a confirmed cancel.
    trade::cancel(
        &store,
        &registry,
        handle.desk_id(),
        winner,
        r#"{"action_id":"f6-undo-a"}"#,
        &trade::Source::Session,
    )
    .expect("the cancel is accepted");
    assert_eq!(sellable(&node).reserved, Decimal::ZERO);

    // --- the prototype -----------------------------------------------------
    for round in 0..20 {
        let gate = Barrier::new(2);
        let outcomes: Vec<Result<Sellable, (String, Sellable)>> = thread::scope(|scope| {
            let racers: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|half| {
                    let (node, gate) = (&node, &gate);
                    let id = format!("f6-race-{round}-{half}");
                    scope.spawn(move || {
                        gate.wait();
                        checked_sell(node, id, 200, "13.00")
                    })
                })
                .collect();
            racers.into_iter().map(|r| r.join().unwrap()).collect()
        });

        let winners = outcomes.iter().filter(|o| o.is_ok()).count();
        assert_eq!(
            winners, 1,
            "round {round}: exactly one sell passes {outcomes:?}"
        );
        let (reason, state) = outcomes
            .iter()
            .find_map(|o| o.as_ref().err())
            .expect("the loser");
        assert_eq!(
            (state.position, state.locked, state.reserved, state.sellable),
            (
                Decimal::from(300),
                Decimal::ZERO,
                Decimal::from(200),
                Decimal::from(100)
            ),
            "round {round}: the loser sees the winner's reservation"
        );
        assert_eq!(
            reason,
            "quantity 200 exceeds sellable 100 for 000001.XSHE: 0 bought today are \
             locked by T+1; 200 reserved by outstanding sells"
        );

        // Release for the next round through a confirmed cancel.
        let won = ["a", "b"]
            .into_iter()
            .find(|half| outcomes[usize::from(*half == "b")].is_ok())
            .expect("one half won");
        let order = format!("f6-race-{round}-{won}");
        await_status(&node, &order, OrderStatus::Accepted);
        trade::cancel(
            &store,
            &registry,
            handle.desk_id(),
            &order,
            &format!(r#"{{"action_id":"f6-race-undo-{round}"}}"#),
            &trade::Source::Session,
        )
        .expect("the cancel is accepted");
        assert_eq!(sellable(&node).reserved, Decimal::ZERO, "round {round}");
    }

    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 3 — partial fill
// ---------------------------------------------------------------------------

/// F6 (3). A resting SELL 200 meets a bid for 100 at its limit: `leaves_qty`
/// falls to 100 and the position falls to 200, so `position − reserved` is
/// unchanged and the recomputed sellable is 100.
#[test]
fn partial_fill_reduces_the_reservation() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-partial");
    hold_from_yesterday(&store, &registry, &node, handle.desk_id(), 300);

    checked_sell(&node, "f6-partial-1".into(), 200, "13.00").expect("200 of 300 is sellable");
    await_status(&node, "f6-partial-1", OrderStatus::Accepted);
    assert_eq!(
        sellable(&node),
        Sellable {
            position: Decimal::from(300),
            locked: Decimal::ZERO,
            reserved: Decimal::from(200),
            sellable: Decimal::from(100),
        }
    );

    // A bid for 100 at the limit: the L1 ladder has exactly the quote's size, so
    // `simulate_fills` returns one 100-share fill
    // (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3950`).
    publish_book(&node, pingan(), ("13.00", 100), ("13.00", 100), CN_0935);
    await_status(&node, "f6-partial-1", OrderStatus::PartiallyFilled);

    let after = sellable(&node);
    assert_eq!(
        (after.position, after.locked, after.reserved, after.sellable),
        (
            Decimal::from(200),
            Decimal::ZERO,
            Decimal::from(100),
            Decimal::from(100)
        ),
        "300 − 0 − 100 = 200 held, 100 still reserved, 100 sellable: {after:?}"
    );

    // Take the bid away so the remaining 100 stops crossing and the next checks
    // are about the check, not the book.
    book(&node, "12.00", 1000, CN_0935 + SECOND_NS);

    let refused = checked_sell(&node, "f6-partial-2".into(), 200, "13.00")
        .expect_err("200 is no longer sellable");
    assert_eq!(refused.1.sellable, Decimal::from(100), "{refused:?}");
    checked_sell(&node, "f6-partial-3".into(), 100, "13.00").expect("100 is sellable");
    await_status(&node, "f6-partial-3", OrderStatus::Accepted);
    assert_eq!(sellable(&node).sellable, Decimal::ZERO);

    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 4 — confirmed cancel
// ---------------------------------------------------------------------------

/// F6 (4). The cancel *request* releases nothing: `exec_engine_queue_execute` is
/// deferred through the runner's trading-command sender
/// (`nautilus-execution-0.62.0/src/engine/mod.rs:188-201`), so a sell attempted
/// in the very job that sent the cancel still sees the reservation. Only
/// `OrderCanceled` in the cache releases it, which is what `trade::cancel`
/// returns on.
#[test]
fn only_a_confirmed_cancel_releases() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-cancel");
    hold_from_yesterday(&store, &registry, &node, handle.desk_id(), 300);

    checked_sell(&node, "f6-cancel-1".into(), 200, "13.00").expect("200 is sellable");
    await_status(&node, "f6-cancel-1", OrderStatus::Accepted);

    // One job: send the cancel, then run the very next sell's check.
    let (status_after_request, attempt) = node
        .call(|context| {
            let target = ClientOrderId::from("f6-cancel-1");
            let (instrument_id, venue_order_id) = {
                let cache = context.cache.borrow();
                let order = cache.order(&target).expect("the resting sell");
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
                    UnixNanos::from(CN_0935),
                    None,
                    None,
                )),
            );
            let status = context
                .cache
                .borrow()
                .order(&target)
                .expect("the order")
                .status();
            let attempt = sellable_from_cache(&context.cache.borrow(), CN_D_MIDNIGHT);
            (status, attempt)
        })
        .expect("the node answers");
    assert_eq!(
        status_after_request,
        OrderStatus::Accepted,
        "the queued cancel is not processed inside the job that sent it"
    );
    assert_eq!(
        (attempt.reserved, attempt.sellable),
        (Decimal::from(200), Decimal::from(100)),
        "a sell between request and confirmation still faces the reservation"
    );

    // The confirmation, and only then the release.
    await_status(&node, "f6-cancel-1", OrderStatus::Canceled);
    let released = sellable(&node);
    assert_eq!(
        (released.position, released.reserved, released.sellable),
        (Decimal::from(300), Decimal::ZERO, Decimal::from(300)),
        "`OrderCanceled` in the cache is what releases: {released:?}"
    );
    checked_sell(&node, "f6-cancel-2".into(), 300, "13.00").expect("the whole holding is sellable");
    await_status(&node, "f6-cancel-2", OrderStatus::Accepted);

    // `trade::cancel` itself settles on `is_closed`, so its return is already the
    // confirmed point.
    trade::cancel(
        &store,
        &registry,
        handle.desk_id(),
        "f6-cancel-2",
        r#"{"action_id":"f6-cancel-undo"}"#,
        &trade::Source::Session,
    )
    .expect("the cancel is accepted");
    assert_eq!(
        sellable(&node).reserved,
        Decimal::ZERO,
        "`trade::cancel` returns only once the reservation is gone"
    );

    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 5 — pending approval, then changed state
// ---------------------------------------------------------------------------

/// F6 (5). A pending approval puts nothing in the node, so it reserves nothing —
/// two of them can be recorded against the same 300 shares. Today's
/// `trade::decide` then re-runs `place_and_settle` from the stored request with
/// no eligibility check of any kind, so both are accepted and 600 shares end up
/// reserved against a 300 position.
///
/// The only native guard is the matching engine's cash-account short-sell check
/// (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:2876-2895`): a SELL is
/// rejected when it is not reduce-only against the **cached position**. It knows
/// nothing about T+1 locks or other resting sells, and it fires only once the
/// order reaches the venue, so it cannot serialize two competing sells and it is
/// not the §1.1 check.
#[test]
fn pending_approval_reserves_nothing_then_reruns_unchecked() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-approval");
    hold_from_yesterday(&store, &registry, &node, handle.desk_id(), 300);

    let gated_sell = |action_id: &str| {
        format!(
            r#"{{"action_id":"{action_id}","instrument_id":"000001.XSHE",
                 "side":"SELL","type":"LIMIT","quantity":"300","price":"13.00"}}"#
        )
    };

    // --- two pendings for the same shares, neither reserving ----------------
    policy(&store, "REQUIRE_APPROVAL");
    let mut pending = Vec::new();
    for action_id in ["f6-gated-a", "f6-gated-b"] {
        let (record, submitted) = trade::submit(
            &store,
            &registry,
            handle.desk_id(),
            &gated_sell(action_id),
            &trade::Source::Session,
        )
        .expect("a gated order is recorded");
        assert_eq!(submitted, trade::Submitted::Pending);
        assert!(
            trade::open_orders(&node).unwrap().is_empty(),
            "{action_id}: a pending approval puts nothing in the node"
        );
        assert_eq!(
            sellable(&node),
            Sellable {
                position: Decimal::from(300),
                locked: Decimal::ZERO,
                reserved: Decimal::ZERO,
                sellable: Decimal::from(300),
            },
            "{action_id}: and therefore reserves nothing"
        );
        pending.push(record.id);
    }

    // --- approval re-enters acceptance with no check ------------------------
    trade::decide(
        &store,
        &registry,
        handle.desk_id(),
        &pending[0],
        Decision::Approve,
    )
    .expect("the first approval re-enters acceptance");
    await_status(&node, "f6-gated-a", OrderStatus::Accepted);
    assert_eq!(
        sellable(&node).sellable,
        Decimal::ZERO,
        "the shares are gone"
    );

    // F6 found the second approval re-entering acceptance with no check at all,
    // reserving 600 shares against a 300 position. §5.2's approval now reruns
    // the eligibility checks: the second one is a terminal refused action with
    // no sandbox order behind it (slice 014 step 3).
    trade::decide(
        &store,
        &registry,
        handle.desk_id(),
        &pending[1],
        Decision::Approve,
    )
    .expect("an eligibility refusal is not a failed decision");
    let refused = trade::history_actions(&store, handle.desk_id())
        .expect("the actions read")
        .into_iter()
        .find(|row| row.action_id == "f6-gated-b")
        .expect("the row");
    assert_eq!(refused.approval, "APPROVED");
    let outcome = refused.outcome.clone().expect("a terminal outcome");
    assert_eq!(outcome["failure_code"], "ORDER_INVALID", "{outcome}");
    assert_eq!(
        outcome["reason"],
        "quantity 300 exceeds sellable 0 for 000001.XSHE: 0 bought today are \
         locked by T+1; 300 reserved by outstanding sells",
        "{outcome}"
    );
    let held = sellable(&node);
    assert_eq!(
        (held.position, held.reserved, held.sellable),
        (Decimal::from(300), Decimal::from(300), Decimal::ZERO),
        "and the second approval reserved nothing: {held:?}"
    );

    // --- changed holdings and a closed session ------------------------------
    trade::cancel(
        &store,
        &registry,
        handle.desk_id(),
        "f6-gated-a",
        r#"{"action_id":"f6-undo-a"}"#,
        &trade::Source::Session,
    )
    .expect("a cancel is never gated");
    policy(&store, "ALWAYS_ALLOW");
    trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f6-approval-sell","instrument_id":"000001.XSHE",
            "side":"SELL","type":"MARKET","quantity":"200","price":null}"#,
        &trade::Source::Session,
    )
    .expect("the ungated sell is accepted");
    await_status(&node, "f6-approval-sell", OrderStatus::Filled);
    assert_eq!(sellable(&node).position, Decimal::from(100));

    policy(&store, "REQUIRE_APPROVAL");
    let (stale, _) = trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        &gated_sell("f6-gated-c"),
        &trade::Source::Session,
    )
    .expect("a gated order is recorded");
    // And the session closes under it: 11:45 is the lunch break, where §5.1
    // forbids execution outright. F6 found nothing in the daemon consulting the
    // clock — the approval reached the sandbox and got its short-selling
    // refusal. §5.1's check now answers first, on the node clock.
    advance(&node, CN_1145);
    trade::decide(
        &store,
        &registry,
        handle.desk_id(),
        &stale.id,
        Decision::Approve,
    )
    .expect("a session refusal is not a failed decision");
    let row = trade::history_actions(&store, handle.desk_id())
        .expect("the actions read")
        .into_iter()
        .find(|row| row.action_id == "f6-gated-c")
        .expect("the row");
    let outcome = row.outcome.clone().expect("an outcome was stored");
    assert_eq!(row.approval, "APPROVED", "the decision itself stands");
    assert_eq!(outcome["failure_code"], "ORDER_INVALID", "{outcome}");
    assert_eq!(
        outcome["reason"],
        "000001.XSHE is outside the supported session [09:30,11:30) and \
         [13:00,14:57) Asia/Shanghai",
        "{outcome}"
    );
    assert!(
        trade::open_orders(&node).unwrap().is_empty(),
        "and no sandbox order was created"
    );

    registry.stop_all();
}

// ---------------------------------------------------------------------------
// 6 — the odd-lot increment
// ---------------------------------------------------------------------------

/// F6 (6). Nothing native stands in the way of an odd-lot sell: an `Equity`'s
/// `size_increment()` is a hard-coded `Quantity::from(1)` and its
/// `size_precision()` is 0 (`nautilus-model-0.62.0/src/instruments/equity.rs:355`,
/// `:349`), `lot_size` is a separate advisory field, and neither
/// `nautilus-risk-0.62.0/src/engine/mod.rs` nor the matching engine reads either
/// one for order sizing. SELL 150 against a 250 position fills. The refusal
/// today is `crate::trade::validate`'s own blanket lot check.
#[test]
fn odd_lot_sell_is_refused_by_marketrig_alone() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_yesterday(&store, "f6-oddlot");

    // What the catalog gives the sandbox for a CN equity.
    let shape = node
        .call(|context| {
            let cache = context.cache.borrow();
            let instrument = cache.instrument(&pingan_id()).expect("the CN instrument");
            (
                instrument.size_increment().to_string(),
                instrument.size_precision(),
                instrument.lot_size().map(|q| q.to_string()),
                instrument.max_quantity().map(|q| q.to_string()),
                instrument.min_quantity().map(|q| q.to_string()),
            )
        })
        .expect("the node answers");
    assert_eq!(
        shape,
        ("1".to_owned(), 0, Some("100".to_owned()), None, None),
        "size_increment 1 already; `lot_size` 100 is advisory and the caps are ours"
    );

    // 250 shares from yesterday, placed past `validate` because 250 is not a lot
    // multiple either.
    place_direct(&node, "f6-oddlot-buy", OrderSide::Buy, 250, None);
    await_status(&node, "f6-oddlot-buy", OrderStatus::Filled);
    advance(&node, CN_0935);
    assert_eq!(sellable(&node).sellable, Decimal::from(250));

    // F6 found today's blanket lot check refusing 150 outright. §3.1's rule
    // replaces it for a CN SELL: 150 % 100 == 50 == 250 % 100, so 150 is the
    // whole odd remainder plus a lot and is accepted (slice 014 step 3). 125 is
    // still refused, by MarketRig alone.
    let refused = trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f6-oddlot-125","instrument_id":"000001.XSHE",
            "side":"SELL","type":"MARKET","quantity":"125","price":null}"#,
        &trade::Source::Session,
    )
    .expect_err("§3.1 refuses 125 against a sellable 250");
    assert_eq!(refused.code(), "ORDER_INVALID");
    assert_eq!(
        refused.to_string(),
        "The order is not well formed: quantity 125 for 000001.XSHE is neither a \
         multiple of 100 nor the whole odd remainder of sellable 250."
    );

    // 150 fills, and the position is the remaining odd 100.
    let sold = trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f6-oddlot-150","instrument_id":"000001.XSHE",
            "side":"SELL","type":"MARKET","quantity":"150","price":null}"#,
        &trade::Source::Session,
    )
    .expect("§3.1 accepts the odd remainder");
    assert_eq!(sold.0.outcome.clone().unwrap()["status"], "FILLED");
    await_status(&node, "f6-oddlot-150", OrderStatus::Filled);
    let after = sellable(&node);
    assert_eq!(
        (after.position, after.locked, after.reserved, after.sellable),
        (
            Decimal::from(100),
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::from(100)
        ),
        "the odd-lot sell filled natively: {after:?}"
    );
    let chain: Vec<String> = crate::feasibility::clock::stored_events(&store, handle.desk_id())
        .into_iter()
        .filter(|(id, _, _)| id == "f6-oddlot-150")
        .map(|(_, kind, _)| kind)
        .collect();
    assert_eq!(
        chain,
        ["OrderInitialized", "OrderSubmitted", "OrderFilled"],
        "no OrderDenied and no OrderRejected anywhere in the odd-lot chain \
         (a MARKET order that fills on arrival is never accepted first)"
    );

    registry.stop_all();
}
