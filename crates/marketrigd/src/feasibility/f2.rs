//! F2 — can session-end termination occur without a tick?
//! (`sdd/features/a-share-engine/FEASIBILITY.md`; feature SPEC §5.1.)
//!
//! Every test here runs the real `LiveNode` on the controlled clock from
//! [`crate::feasibility::clock`], with **no feed at all**: each quote is
//! published by hand at a chosen instant, which is the only way to separate
//! "time passed" from "data arrived".
//!
//! Findings, in the order the tests establish them:
//!
//! 1. [`gtd_needs_a_tick_to_expire`] — the sandbox has `support_gtd_orders`
//!    on (its builder default, `nautilus-sandbox-0.62.0/src/config.rs:95`, and
//!    `crate::node::sandbox_config` never sets it), so a GTD limit order does
//!    expire natively. But it expires only inside
//!    `OrderMatchingEngine::iterate`, which runs from data processing alone;
//!    advancing the clock hours past `expire_time` with no tick leaves the
//!    order `ACCEPTED`.
//! 2. [`gtd_fills_at_the_boundary_before_it_expires`] — worse, `iterate` matches
//!    **before** it runs post-match maintenance
//!    (`nautilus-execution-0.62.0/src/matching_engine/mod.rs:3700-3760`), so the
//!    first tick at or after `expire_time` fills a crossing order instead of
//!    expiring it, and the fill is stamped at the post-boundary instant. Native
//!    GTD therefore cannot enforce §5.1.
//! 3. [`scheduled_cancel_terminates_without_a_tick`] — a `set_time_alert_ns` on
//!    the kernel clock whose callback sends the same `TradingCommand::CancelOrder`
//!    `crate::trade::cancel` sends does terminate the order with no market data,
//!    stamped exactly at the alert instant, and a later crossing quote produces
//!    no fill.
//! 4. [`a_boundary_quote_delivered_first_still_fills`] — the ordering at the
//!    boundary is MarketRig's to enforce: the sandbox has no notion of a session,
//!    so a crossing quote delivered to the node thread before the alert fires
//!    fills at the boundary instant. The alert must be dispatched before any data
//!    for that instant.

use std::rc::Rc;

use nautilus_common::factories::OrderFactory;
use nautilus_common::messages::execution::{CancelOrder, SubmitOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::enums::{MarketStatusAction, OrderSide, OrderStatus, TimeInForce};
use nautilus_model::events::OrderEventAny;
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::orders::Order;
use nautilus_model::types::{Price, Quantity};

use crate::catalog::Entry;
use crate::feasibility::clock::{
    self, CN_0935, CN_1457, ClockHandle, SECOND_NS, advance, controlled_registry, publish_quote,
    stored_events,
};
use crate::node::{Node, Registry, within};
use crate::store::Store;
use crate::trade;

/// The strategy identity `crate::trade` puts on every MarketRig order (its
/// private `STRATEGY`). [`strategy_matches_production`] pins the copy.
const STRATEGY: &str = "MARKETRIG-001";

/// Kweichow Moutai on the Shanghai main board — a `CN` catalog entry, tick 0.01,
/// lot 100.
fn moutai() -> &'static Entry {
    crate::catalog::find("600519.XSHG").expect("the CN catalog entry")
}

/// A started desk on the controlled clock at 09:35 Asia/Shanghai, with no feed,
/// and one standing non-crossing quote at 1700.00 so the venue has a book.
fn desk_at_0935(
    store: &Store,
    name: &'static str,
) -> (Registry, ClockHandle, std::sync::Arc<Node>) {
    let registry = controlled_registry(store, None, name, CN_0935);
    let (registry, handle) = registry;
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    let instrument_id = InstrumentId::from(moutai().instrument_id);
    publish_quote(&node, moutai(), "1700.00", 100, CN_0935);
    within(10, "the opening quote reaches the book", || {
        node.call(move |context| context.cache.borrow().quote(&instrument_id).is_some())
            .unwrap()
    });
    (registry, handle, node)
}

/// Reopens the instrument's own session gate. Production now arms F2's
/// recommendation itself — a kernel-clock alert that `Pause`s at 11:30 and
/// `Close`s at 14:57 (`crate::cn::arm_session_alert`, slice 014 step 3) — so a
/// test that asks what the *native* engine does past a boundary has to put the
/// engine back into `Trading` first.
fn reopen(node: &Node, ts_ns: u64) {
    node.call(move |_| {
        crate::cn::publish_status(moutai(), MarketStatusAction::Trading, ts_ns);
    })
    .expect("the node answers");
    let instrument_id = InstrumentId::from(moutai().instrument_id);
    within(10, "the instrument reopens", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .instrument_status(&instrument_id)
                .map(|cached| cached.action)
                == Some(MarketStatusAction::Trading)
        })
        .unwrap()
    });
}

/// Places a LIMIT order the way [`crate::trade::place`] does — cache, publish
/// `OrderInitialized`, `SubmitOrder` to the risk engine's queued endpoint — but
/// with a caller-chosen time in force and expire time, which is the only thing
/// `place` would have to grow for a native day order. Returns nothing; the
/// caller waits on the order's status.
fn place_limit(
    node: &Node,
    client_order_id: &'static str,
    side: OrderSide,
    quantity: u32,
    price: &'static str,
    time_in_force: TimeInForce,
    expire_ns: Option<u64>,
) {
    let instrument_id = InstrumentId::from(moutai().instrument_id);
    let price = Price::from(price);
    let quantity = Quantity::from(quantity);
    node.call(move |context| {
        let mut factory = OrderFactory::new(
            context.trader_id,
            StrategyId::new(STRATEGY),
            None,
            None,
            Rc::clone(&context.clock),
            false,
            false,
        );
        let order = factory.limit(
            instrument_id,
            side,
            quantity,
            price,
            Some(time_in_force),
            expire_ns.map(UnixNanos::from),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(ClientOrderId::from(client_order_id)),
        );
        let command = SubmitOrder::from_order(
            &order,
            context.trader_id,
            None,
            None,
            UUID4::new(),
            context.clock.borrow().timestamp_ns(),
        );
        let initialized = OrderEventAny::Initialized(order.init_event().clone());
        context
            .cache
            .borrow_mut()
            .add_order(order, None, None, true)
            .expect("the node takes the order");
        msgbus::publish_order_event(format!("events.order.{STRATEGY}").into(), &initialized);
        msgbus::send_trading_command(
            MessagingSwitchboard::risk_engine_queue_execute(),
            TradingCommand::SubmitOrder(command),
        );
    })
    .expect("the node answers");
}

/// The order's current status, live off the node's cache.
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

fn await_status(node: &Node, client_order_id: &'static str, want: OrderStatus, what: &str) {
    within(10, what, || status(node, client_order_id) == want);
}

/// The CNY the venue account has locked against pending orders — the sandbox's
/// own reservation, which a terminal event must release.
fn locked_cny(node: &Node, desk_id: &str) -> String {
    let account_id = AccountId::from(format!("XSHG-{desk_id}").as_str());
    node.call(move |context| {
        context
            .cache
            .borrow()
            .account(&account_id)
            .expect("the venue account")
            .balances()
            .values()
            .map(|balance| balance.locked.to_string())
            .collect::<Vec<_>>()
            .join(",")
    })
    .expect("the node answers")
}

/// The order as the agent reads it (`crate::trade::order_projection`), which is
/// where the exposed time in force lives.
fn projection(node: &Node, client_order_id: &'static str) -> serde_json::Value {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        trade::order_projection(
            &context
                .cache
                .borrow()
                .order(&id)
                .expect("the order is cached"),
        )
    })
    .expect("the node answers")
}

/// The desk's captured chain for one order: `(kind, ts_event)` oldest first.
fn chain(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<(String, i64)> {
    stored_events(store, desk_id)
        .into_iter()
        .filter(|(id, _, _)| id == client_order_id)
        .map(|(_, kind, ns)| (kind, ns))
        .collect()
}

/// The private `crate::trade::STRATEGY` this file copies. An order placed
/// through the production path must carry the same identity, or the events these
/// tests capture would not be the events MarketRig captures.
#[test]
fn strategy_matches_production() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f2-strategy");
    trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f2-strategy-1","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"1600.00"}"#,
        &trade::Source::Session,
    )
    .expect("the resting limit buy is accepted");
    let id = ClientOrderId::from("f2-strategy-1");
    let produced = node
        .call(move |context| {
            context
                .cache
                .borrow()
                .order(&id)
                .map(|order| order.strategy_id().to_string())
                .expect("the order is cached")
        })
        .unwrap();
    assert_eq!(
        produced, STRATEGY,
        "the copied strategy identity is current"
    );
    registry.stop_all();
}

/// F2 (a)+(b): the sandbox does support GTD — the daemon leaves
/// `support_gtd_orders` at its `true` default — but expiry is not on a timer.
/// Advancing the controlled clock past `expire_time` with no market data expires
/// nothing; the first later tick does.
#[test]
fn gtd_needs_a_tick_to_expire() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f2-gtd-tick");

    // A resting BUY at 1600.00 under the 1700.00 ask, GTD to 14:57.
    place_limit(
        &node,
        "f2-gtd-1",
        OrderSide::Buy,
        100,
        "1600.00",
        TimeInForce::Gtd,
        Some(CN_1457),
    );
    await_status(&node, "f2-gtd-1", OrderStatus::Accepted, "the order rests");
    assert_eq!(
        projection(&node, "f2-gtd-1")["time_in_force"],
        "GTD",
        "the exposed time in force is the native GTD"
    );

    // Two hours past the deadline, and every time event the clock released
    // dispatched: the order is untouched. The only timer the node owns is the
    // sandbox's expired-*engine* sweep, which does not look at orders.
    let fired = advance(&node, CN_1457 + 7200 * SECOND_NS);
    assert!(
        fired
            .iter()
            .all(|name| name.ends_with("-sandbox-expiry-sweep") || name == "cn-session-boundary"),
        "the node's only timers are the sandbox engine sweep and MarketRig's own \
         session boundary: {fired:?}"
    );
    assert_eq!(
        status(&node, "f2-gtd-1"),
        OrderStatus::Accepted,
        "advancing the clock past expire_time expires nothing"
    );
    assert_eq!(
        chain(&store, handle.desk_id(), "f2-gtd-1")
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["OrderInitialized", "OrderSubmitted", "OrderAccepted"],
        "no terminal event was produced without a tick"
    );

    // A single non-crossing quote — 1700.00, still above the 1600.00 buy — now
    // expires it. The decision reads the tick's `ts_init`; the stamp is the
    // clock's own instant.
    let tick_at = CN_1457 + 7200 * SECOND_NS;
    publish_quote(&node, moutai(), "1700.00", 100, tick_at);
    await_status(
        &node,
        "f2-gtd-1",
        OrderStatus::Expired,
        "the tick expires the order",
    );

    let captured = chain(&store, handle.desk_id(), "f2-gtd-1");
    assert_eq!(
        captured
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderExpired"
        ],
        "exactly one terminal event, under the original client order id"
    );
    assert_eq!(
        captured.last().unwrap().1,
        tick_at as i64,
        "`OrderExpired.ts_event` is the sandbox clock's instant"
    );
    assert!(
        store
            .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get::<_, i64>(0)))
            .unwrap()
            == 0,
        "nothing filled"
    );
    let history = trade::history_orders(&store, handle.desk_id()).expect("the history reads");
    assert_eq!(history[0]["status"], "EXPIRED", "{history:?}");
    registry.stop_all();
}

/// F2 (e), the decisive negative: at the boundary the matching engine fills
/// before it expires. A crossing quote whose `ts_init` is past `expire_time`
/// produces `OrderFilled`, not `OrderExpired`, stamped after the boundary.
#[test]
fn gtd_fills_at_the_boundary_before_it_expires() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f2-gtd-boundary");

    place_limit(
        &node,
        "f2-gtd-2",
        OrderSide::Buy,
        100,
        "1600.00",
        TimeInForce::Gtd,
        Some(CN_1457),
    );
    await_status(&node, "f2-gtd-2", OrderStatus::Accepted, "the order rests");

    advance(&node, CN_1457);
    // One second past the deadline, and the tick crosses (ask 1500 < the 1600
    // buy limit).
    let tick_at = CN_1457 + SECOND_NS;
    advance(&node, tick_at);
    // MarketRig's own boundary alert has already closed the instrument by now;
    // the question here is what the *native* engine does, so it is reopened.
    reopen(&node, tick_at);
    publish_quote(&node, moutai(), "1500.00", 100, tick_at);
    within(10, "the order closes", || {
        matches!(
            status(&node, "f2-gtd-2"),
            OrderStatus::Filled | OrderStatus::Expired
        )
    });

    assert_eq!(
        status(&node, "f2-gtd-2"),
        OrderStatus::Filled,
        "native GTD does not protect the boundary: the crossing tick fills first"
    );
    let captured = chain(&store, handle.desk_id(), "f2-gtd-2");
    assert_eq!(
        captured
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
    );
    assert!(
        captured.last().unwrap().1 >= CN_1457 as i64,
        "and the fill carries an out-of-session instant: {captured:?}"
    );
    registry.stop_all();
}

/// F2 (d): the supported mechanism. A `Clock::set_time_alert_ns` on the kernel
/// clock, whose callback sends the same `TradingCommand::CancelOrder`
/// `crate::trade::cancel` sends, terminates the resting order with no market
/// data at all, stamped at the alert instant — and no later crossing quote
/// fills it.
#[test]
fn scheduled_cancel_terminates_without_a_tick() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f2-alert");

    // The ordinary production submit: GTC, exactly as MarketRig places orders
    // today.
    let (rested, _) = trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"f2-alert-1","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"1600.00"}"#,
        &trade::Source::Session,
    )
    .expect("the resting limit buy is accepted");
    assert_eq!(rested.outcome.clone().unwrap()["status"], "ACCEPTED");
    assert_eq!(
        rested.outcome.clone().unwrap()["time_in_force"],
        "GTC",
        "MarketRig's own orders are GTC today (`crate::trade::place`)"
    );
    let locked_while_resting = locked_cny(&node, handle.desk_id());
    assert_ne!(
        locked_while_resting, "0.00 CNY",
        "the resting buy reserves cash: {locked_while_resting}"
    );

    // The session-end alert. Its callback runs on the node thread and reads the
    // venue order id off the node's own cache at firing time.
    let err = clock::alert(&node, "cn-session-end", CN_1457, |context| {
        let cache = Rc::clone(&context.cache);
        let trader_id = context.trader_id;
        Rc::new(move |_event| {
            let target = ClientOrderId::from("f2-alert-1");
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
        }) as Rc<dyn Fn(nautilus_common::timer::TimeEvent)>
    });
    assert_eq!(err, Ok(()), "the alert registers on the node clock");

    // No quote is published between here and the boundary.
    let fired = advance(&node, CN_1457);
    assert!(
        fired.iter().any(|name| name == "cn-session-end"),
        "the session-end alert fired under the controlled clock: {fired:?}"
    );
    await_status(
        &node,
        "f2-alert-1",
        OrderStatus::Canceled,
        "the scheduled cancel terminates the order without a tick",
    );

    let captured = chain(&store, handle.desk_id(), "f2-alert-1");
    assert_eq!(
        captured
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderCanceled"
        ],
        "exactly one terminal event, under the original client order id"
    );
    assert_eq!(
        captured.last().unwrap().1,
        CN_1457 as i64,
        "`OrderCanceled.ts_event` is exactly the alert instant"
    );
    assert_eq!(
        locked_cny(&node, handle.desk_id()),
        "0.00 CNY",
        "the terminal event released the sandbox's reservation"
    );
    assert!(
        trade::open_orders(&node).unwrap().is_empty(),
        "and the order is no longer open"
    );

    // A crossing quote after the boundary changes nothing.
    let after = CN_1457 + 60 * SECOND_NS;
    advance(&node, after);
    publish_quote(&node, moutai(), "1500.00", 100, after);
    // Give the runner a turn, then assert the chain is unchanged.
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
        chain(&store, handle.desk_id(), "f2-alert-1"),
        captured,
        "no event follows the terminal one"
    );
    assert_eq!(
        store
            .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get::<_, i64>(0)))
            .unwrap(),
        0,
        "no fill after the boundary"
    );

    // And the durable history agrees.
    let history = trade::history_orders(&store, handle.desk_id()).expect("the history reads");
    let listed: Vec<(&str, &str)> = history
        .iter()
        .map(|order| {
            (
                order["client_order_id"].as_str().unwrap(),
                order["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(listed, vec![("f2-alert-1", "CANCELED")]);
    registry.stop_all();
}

/// F2 (e), the ordering rule the design must carry: the sandbox knows nothing
/// about the session, so a crossing quote for the boundary instant that reaches
/// the node thread *before* the alert is dispatched fills the order at that
/// instant. Sequencing the alert ahead of data delivery is MarketRig's job.
#[test]
fn a_boundary_quote_delivered_first_still_fills() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle, node) = desk_at_0935(&store, "f2-order");

    place_limit(
        &node,
        "f2-order-1",
        OrderSide::Buy,
        100,
        "1600.00",
        TimeInForce::Gtd,
        Some(CN_1457),
    );
    await_status(
        &node,
        "f2-order-1",
        OrderStatus::Accepted,
        "the order rests",
    );

    // The crossing quote for exactly the boundary instant, delivered first.
    publish_quote(&node, moutai(), "1500.00", 100, CN_1457);
    within(10, "the boundary quote is processed", || {
        !matches!(status(&node, "f2-order-1"), OrderStatus::Accepted)
    });
    assert_eq!(
        status(&node, "f2-order-1"),
        OrderStatus::Filled,
        "a boundary quote processed before the alert fills"
    );
    let captured = chain(&store, handle.desk_id(), "f2-order-1");
    assert_eq!(
        captured.last().map(|(kind, ns)| (kind.as_str(), *ns)),
        Some(("OrderFilled", CN_0935 as i64)),
        "and it is stamped by the clock, which the alert had not yet moved: \
         the fill instant is only as correct as the daemon's sequencing"
    );
    registry.stop_all();
}
