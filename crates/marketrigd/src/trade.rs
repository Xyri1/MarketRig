//! The order path: form validation, the durable action record, the synchronous
//! sandbox round trip, event capture, and the book snapshot restoration reads.
//!
//! The daemon validates **form** and nothing else: sufficiency is the sandbox's
//! judgment alone (per D38), and every sandbox refusal answers `ORDER_REJECTED`
//! carrying the sandbox's own reason.
//!
//! How a command reaches the sandbox, verified against the pinned 0.62.0 crates:
//! an order is added to the node's cache, its `OrderInitialized` published, and a
//! `TradingCommand::SubmitOrder` sent to `risk_engine_queue_execute` — the same
//! path `nautilus-trading`'s `Strategy` takes, which is what makes the risk
//! engine (free balance, notional, precision) and then the sandbox the judges of
//! sufficiency. A cancel goes straight to `exec_engine_queue_execute`. Both
//! queued endpoints hand the command to the node's runner, which processes it and
//! the events it produces on the node thread; MarketRig therefore *observes* the
//! outcome by reading the node's own cache until the order settles ([`settle`]),
//! and captures the events themselves off the node's message bus
//! ([`install_capture`]).
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §4.2, §5, §6, per
//! D38, R1-4, R1-5, R1-8; root `sdd/SPEC.md` §12.3, §12.4.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use nautilus_common::cache::Cache;
use nautilus_common::factories::OrderFactory;
use nautilus_common::messages::execution::{CancelOrder, SubmitOrder, TradingCommand};
use nautilus_common::msgbus::{self, MessagingSwitchboard, TypedHandler};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::accounts::AccountAny;
use nautilus_model::enums::{OmsType, OrderSide, OrderStatus, OrderType, TimeInForce};
use nautilus_model::events::{OrderEventAny, PositionClosed, PositionEvent};
use nautilus_model::identifiers::{ClientOrderId, InstrumentId, StrategyId};
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::position::Position;
use nautilus_model::types::{Price, Quantity};
use rusqlite::{ErrorCode, OptionalExtension, Transaction, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::catalog::{self, Entry};
use crate::desk::{self, DeskError};
use crate::node::{Node, NodeContext, NodeError, Registry};
use crate::store::{Store, StoreError, now_ns};

/// The version stamped on every payload this build writes and the only one it
/// reads back (§5).
pub const PAYLOAD_VERSION: i64 = 1;

/// The one strategy identity every MarketRig order carries. NautilusTrader keys
/// position identity and its event topics by strategy, and a desk is one trader
/// with one book, so one constant is the whole vocabulary.
const STRATEGY: &str = "MARKETRIG-001";

/// How long a command waits for the node to answer before the caller gives up.
/// The node processes the command and its events on its own thread; MarketRig
/// reads the node's cache between turns rather than blocking it.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
const SETTLE_POLL: Duration = Duration::from_millis(2);

// ---------------------------------------------------------------------------
// Errors (§7 codes, append-only per D68)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum TradeError {
    /// The instrument is outside the compiled-in catalog (§3).
    InstrumentUnknown(String),
    /// No such resting order, or it is already terminal (§4.2).
    OrderNotFound(String),
    /// A form failure — the only thing the daemon judges (§4.2).
    Invalid(String),
    /// The sandbox refused, and this carries its own reason (per D38).
    Rejected(String),
    NotReady(String),
    /// The desk's node is not usable (§4.3, R1-6).
    Unavailable(String),
    Desk(DeskError),
}

impl TradeError {
    pub fn code(&self) -> &'static str {
        match self {
            TradeError::InstrumentUnknown(_) => "INSTRUMENT_UNKNOWN",
            TradeError::OrderNotFound(_) => "ORDER_NOT_FOUND",
            TradeError::Invalid(_) => "ORDER_INVALID",
            TradeError::Rejected(_) => "ORDER_REJECTED",
            TradeError::NotReady(_) => "DESK_NOT_READY",
            TradeError::Unavailable(_) => "MARKET_UNAVAILABLE",
            TradeError::Desk(e) => e.code(),
        }
    }
}

impl fmt::Display for TradeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TradeError::InstrumentUnknown(id) => {
                write!(f, "Instrument {id:?} is not in MarketRig's catalog.")
            }
            TradeError::OrderNotFound(id) => write!(
                f,
                "This desk has no open order {id:?}; it is unknown or already terminal."
            ),
            TradeError::Invalid(what) => write!(f, "The order is not well formed: {what}."),
            TradeError::Rejected(reason) => {
                // The sandbox's reason, verbatim, inside the envelope's sentence.
                let reason = reason.trim_end();
                let stop = if reason.ends_with('.') { "" } else { "." };
                write!(f, "The desk's paper book refused the order: {reason}{stop}")
            }
            TradeError::NotReady(state) => {
                write!(f, "Only a READY desk can trade; this desk is {state}.")
            }
            TradeError::Unavailable(why) => {
                write!(f, "The desk's market plane is unavailable: {why}.")
            }
            TradeError::Desk(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TradeError {}

impl From<DeskError> for TradeError {
    fn from(e: DeskError) -> Self {
        TradeError::Desk(e)
    }
}

impl From<StoreError> for TradeError {
    fn from(e: StoreError) -> Self {
        TradeError::Desk(DeskError::Store(e))
    }
}

impl From<NodeError> for TradeError {
    fn from(e: NodeError) -> Self {
        TradeError::Unavailable(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The action record (§6, §7, R1-8)
// ---------------------------------------------------------------------------

/// The `trading_actions` row as the routes return it (§7).
#[derive(Debug, Clone, Serialize)]
pub struct ActionRecord {
    pub action_id: String,
    pub id: String,
    pub kind: String,
    pub created_at_ns: i64,
    /// The response record, set when the command answers. `null` only while a
    /// concurrent duplicate of the same `action_id` is still in flight.
    pub outcome: Option<Value>,
}

/// The §4.2 request body. Every field is validated here and nowhere else.
#[derive(Debug, Deserialize)]
struct OrderBody {
    action_id: String,
    instrument_id: String,
    side: String,
    #[serde(rename = "type")]
    order_type: String,
    quantity: String,
    #[serde(default)]
    price: Option<String>,
}

/// A cancel's own body: the action identity, nothing else — the order is named
/// by the route's path (§4.2).
#[derive(Debug, Deserialize)]
struct CancelBody {
    action_id: String,
}

/// One validated submit, ready for the node thread.
#[derive(Debug, Clone, Copy)]
struct Form {
    entry: &'static Entry,
    side: OrderSide,
    quantity: Quantity,
    /// `Some` exactly when the order is a `LIMIT` (§4.2).
    price: Option<Price>,
}

/// What the caller learns once the sandbox has answered.
#[derive(Debug, Clone)]
struct Settled {
    projection: Value,
    /// The sandbox's own refusal reason, verbatim (per D38).
    refusal: Option<String>,
}

// ---------------------------------------------------------------------------
// Submit and cancel (§4.2, §6)
// ---------------------------------------------------------------------------

/// `POST /desks/{desk_id}/orders` (§7). The `bool` is "this was a replay", which
/// the route turns into `200` instead of `201` (R1-8).
pub fn submit(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    body: &str,
) -> Result<(ActionRecord, bool), TradeError> {
    require_ready(store, desk_id)?;
    let (action_id, form) = validate(body)?;
    if let Some(record) = stored(store, desk_id, &action_id)? {
        return Ok((record, true));
    }

    // The node before the row: a node that will not start leaves no action
    // behind, so the same `action_id` stays retryable (§4.3).
    let node = registry.ensure(desk_id)?;

    let mut record = match begin(store, desk_id, "SUBMIT", &action_id, body)? {
        Begun::Replay(record) => return Ok((record, true)),
        Begun::New(record) => record,
    };

    let client_order_id = ClientOrderId::from(action_id.as_str());
    node.call(move |context| place(context, form, client_order_id))?;
    let settled = settle(&node, client_order_id, settled_submit)?;
    finish(store, desk_id, &action_id, &settled.projection)?;
    record.outcome = Some(settled.projection);

    match settled.refusal {
        Some(reason) => Err(TradeError::Rejected(reason)),
        None => Ok((record, false)),
    }
}

/// `POST /desks/{desk_id}/orders/{client_order_id}/cancel` (§7).
pub fn cancel(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    client_order_id: &str,
    body: &str,
) -> Result<ActionRecord, TradeError> {
    require_ready(store, desk_id)?;
    let body: CancelBody = serde_json::from_str(body)
        .map_err(|e| TradeError::Invalid(format!("the request body is not a cancel: {e}")))?;
    let action_id = valid_action_id(body.action_id)?;
    if !client_order_id.is_ascii() || client_order_id.is_empty() {
        return Err(TradeError::OrderNotFound(client_order_id.to_string()));
    }
    if let Some(record) = stored(store, desk_id, &action_id)? {
        return Ok(record);
    }

    let node = registry.ensure(desk_id)?;
    let target = ClientOrderId::from(client_order_id);
    let Some((instrument_id, venue_order_id)) = node.call(move |context| {
        let cache = context.cache.borrow();
        let order = cache.order(&target)?;
        (!order.is_closed()).then(|| (order.instrument_id(), order.venue_order_id()))
    })?
    else {
        return Err(TradeError::OrderNotFound(client_order_id.to_string()));
    };

    // A cancel's request spans the route: the order it names rode the path and
    // its own identity the body, so the recorded request is the pair.
    let mut record = match begin(
        store,
        desk_id,
        "CANCEL",
        &action_id,
        &json!({ "action_id": action_id, "client_order_id": client_order_id }).to_string(),
    )? {
        Begun::Replay(record) => return Ok(record),
        Begun::New(record) => record,
    };

    // A cancel needs no risk check, so it goes straight to the execution engine's
    // queued endpoint, exactly as a NautilusTrader strategy's cancel does.
    node.call(move |context| {
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
                UnixNanos::from(now_ns().max(0) as u64),
                None,
                None,
            )),
        );
    })?;

    let settled = settle(&node, target, settled_cancel)?;
    finish(store, desk_id, &action_id, &settled.projection)?;
    record.outcome = Some(settled.projection);
    match settled.refusal {
        Some(reason) => Err(TradeError::Rejected(reason)),
        None => Ok(record),
    }
}

/// The order as the agent reads it: identity, terms, and current state, money as
/// decimal text (§7). Every listing that shows an order shows this.
pub fn order_projection(order: &OrderAny) -> Value {
    let mut value = json!({
        "client_order_id": order.client_order_id().to_string(),
        "instrument_id": order.instrument_id().to_string(),
        "side": order.order_side().to_string(),
        "type": order.order_type().to_string(),
        "quantity": order.quantity().to_string(),
        "time_in_force": order.time_in_force().to_string(),
        "status": order.status().to_string(),
        "filled_quantity": order.filled_qty().to_string(),
        "ts_last_ns": order.ts_last().as_u64() as i64,
    });
    if let Some(venue_order_id) = order.venue_order_id() {
        value["venue_order_id"] = json!(venue_order_id.to_string());
    }
    if let Some(price) = order.price() {
        value["price"] = json!(price.to_string());
    }
    if let Some(average) = order.avg_px() {
        value["average_price"] = json!(average.to_string());
    }
    value
}

/// The §4.2 form rules, in order. Every failure here is `ORDER_INVALID` except
/// an uncataloged instrument, which is `INSTRUMENT_UNKNOWN` (§3).
fn validate(body: &str) -> Result<(String, Form), TradeError> {
    let body: OrderBody = serde_json::from_str(body)
        .map_err(|e| TradeError::Invalid(format!("the request body is not an order: {e}")))?;
    let action_id = valid_action_id(body.action_id)?;

    let entry = catalog::find(&body.instrument_id)
        .ok_or_else(|| TradeError::InstrumentUnknown(body.instrument_id.clone()))?;

    let side = match body.side.as_str() {
        "BUY" => OrderSide::Buy,
        "SELL" => OrderSide::Sell,
        other => {
            return Err(TradeError::Invalid(format!(
                "side {other:?} is not BUY or SELL"
            )));
        }
    };
    let order_type = match body.order_type.as_str() {
        "MARKET" => OrderType::Market,
        "LIMIT" => OrderType::Limit,
        other => {
            return Err(TradeError::Invalid(format!(
                "type {other:?} is not MARKET or LIMIT"
            )));
        }
    };

    let lot = Decimal::from(entry.lot_size);
    let quantity: Decimal = body.quantity.parse().map_err(|_| {
        TradeError::Invalid(format!("quantity {:?} is not decimal text", body.quantity))
    })?;
    if quantity <= Decimal::ZERO || quantity % lot != Decimal::ZERO {
        return Err(TradeError::Invalid(format!(
            "quantity {:?} is not a positive multiple of the {} lot of {}",
            body.quantity, entry.lot_size, entry.instrument_id
        )));
    }
    // The sandbox drops an order whose size precision disagrees with the
    // instrument, and an equity's size precision is zero, so the multiple above
    // is normalized to its integer text before it becomes a `Quantity`.
    let quantity = Quantity::from(quantity.normalize().to_string().as_str());

    let tick: Decimal = entry
        .price_increment
        .parse()
        .expect("catalog tick is decimal text (catalog::entries_valid)");
    let price = match (order_type, body.price.as_deref()) {
        (OrderType::Limit, Some(text)) => {
            let price: Decimal = text
                .parse()
                .map_err(|_| TradeError::Invalid(format!("price {text:?} is not decimal text")))?;
            if price <= Decimal::ZERO || price % tick != Decimal::ZERO {
                return Err(TradeError::Invalid(format!(
                    "price {text:?} is not a positive multiple of the {} tick of {}",
                    entry.price_increment, entry.instrument_id
                )));
            }
            Some(Price::from(
                crate::feed::at_precision(price, entry.price_increment).as_str(),
            ))
        }
        (OrderType::Limit, None) => {
            return Err(TradeError::Invalid("a LIMIT order needs a price".into()));
        }
        (_, Some(_)) => {
            return Err(TradeError::Invalid(
                "a MARKET order must not carry a price".into(),
            ));
        }
        (_, None) => None,
    };

    Ok((
        action_id,
        Form {
            entry,
            side,
            quantity,
            price,
        },
    ))
}

/// `[a-z0-9-]{1,64}` (§4.2, R1-8).
fn valid_action_id(action_id: String) -> Result<String, TradeError> {
    let ok = !action_id.is_empty()
        && action_id.len() <= 64
        && action_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(action_id)
    } else {
        Err(TradeError::Invalid(format!(
            "action_id {action_id:?} is not 1-64 characters of lowercase letters, digits, and hyphens"
        )))
    }
}

pub(crate) fn require_ready(store: &Store, desk_id: &str) -> Result<(), TradeError> {
    let desk = desk::get(store, desk_id)?;
    if desk.state == "READY" {
        Ok(())
    } else {
        Err(TradeError::NotReady(desk.state))
    }
}

// ---------------------------------------------------------------------------
// Listings (§7): live from the node, history from the event tables (R1-5)
// ---------------------------------------------------------------------------

/// `GET /desks/{desk_id}/orders` (§7): the desk's open orders, read live off the
/// node's own cache — the authority for what the sandbox is holding (R1-5).
pub fn open_orders(node: &Node) -> Result<Vec<Value>, NodeError> {
    node.call(|context| {
        context
            .cache
            .borrow()
            .orders_open(None, None, None, None, None)
            .iter()
            .map(|order| order_projection(order))
            .collect()
    })
}

/// `GET /desks/{desk_id}/positions` (§7): the desk's open positions, live.
pub fn open_positions(node: &Node) -> Result<Vec<Value>, NodeError> {
    node.call(|context| {
        context
            .cache
            .borrow()
            .positions_open(None, None, None, None, None)
            .iter()
            .map(|position| position_projection(position))
            .collect()
    })
}

/// The position as the agent reads it: identity, size, and the node's own money
/// figures as decimal text (§7).
///
/// NautilusTrader keeps the average prices as `f64`, so they are rendered at the
/// instrument's own price precision — the same precision the position itself
/// carries — rather than emitted as a float. Nothing here is recalculated
/// (per D38).
fn position_projection(position: &Position) -> Value {
    let precision = position.price_precision as usize;
    let mut value = json!({
        "position_id": position.id.to_string(),
        "instrument_id": position.instrument_id.to_string(),
        "side": position.side.to_string(),
        "quantity": position.quantity.to_string(),
        "average_open_price": format!("{:.precision$}", position.avg_px_open),
        "currency": position.settlement_currency.code.to_string(),
        "opened_at_ns": position.ts_opened.as_u64() as i64,
        "ts_last_ns": position.ts_last.as_u64() as i64,
    });
    if let Some(realized) = position.realized_pnl {
        value["realized_pnl"] = json!(realized.as_decimal().to_string());
    }
    value
}

/// `GET /desks/{desk_id}/history/orders` (§7): one element per client order id in
/// its latest known projection, newest first — the order rebuilt from its own
/// verbatim event payloads, never a stored projection (§5, R1-5).
///
/// ponytail: the desk's whole order-event history is read and replayed per call.
/// One desk's R1 history is a handful of orders; the upgrade path is the deferred
/// pagination (root §18) plus a bounded window here.
pub fn history_orders(store: &Store, desk_id: &str) -> Result<Vec<Value>, StoreError> {
    let desk = desk_id.to_owned();
    let rows: Vec<(String, String)> = store.call(move |conn| {
        conn.prepare(
            "SELECT client_order_id, payload FROM order_events \
             WHERE desk_id = ?1 ORDER BY occurred_at_ns, id",
        )?
        .query_map([desk], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
    })?;

    // Group in the table's own order, remembering where each order's last event
    // sat: that position, descending, is "newest first".
    let mut grouped: Vec<(String, usize, Vec<OrderEventAny>)> = Vec::new();
    for (at, (client_order_id, payload)) in rows.into_iter().enumerate() {
        let event: OrderEventAny = match serde_json::from_str(&payload) {
            Ok(event) => event,
            Err(e) => {
                tracing::error!(
                    client_order_id,
                    "an order event could not be read back: {e}"
                );
                continue;
            }
        };
        match grouped.iter_mut().find(|(id, _, _)| *id == client_order_id) {
            Some((_, last, events)) => {
                *last = at;
                events.push(event);
            }
            None => grouped.push((client_order_id, at, vec![event])),
        }
    }
    grouped.sort_by_key(|(_, last, _)| std::cmp::Reverse(*last));

    Ok(grouped
        .into_iter()
        .filter_map(
            |(client_order_id, _, events)| match OrderAny::from_events(events) {
                Ok(order) => Some(order_projection(&order)),
                Err(e) => {
                    tracing::error!(client_order_id, "an order could not be replayed: {e}");
                    None
                }
            },
        )
        .collect())
}

/// `GET /desks/{desk_id}/history/fills` (§7): the desk's fills, newest first.
pub fn history_fills(store: &Store, desk_id: &str) -> Result<Vec<Value>, StoreError> {
    listing(
        store,
        desk_id,
        "SELECT id, client_order_id, trade_id, instrument_id, side, quantity, price, \
                commission, currency, occurred_at_ns \
         FROM fills WHERE desk_id = ?1 ORDER BY occurred_at_ns DESC, id DESC",
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "client_order_id": r.get::<_, String>(1)?,
                "trade_id": r.get::<_, String>(2)?,
                "instrument_id": r.get::<_, String>(3)?,
                "side": r.get::<_, String>(4)?,
                "quantity": r.get::<_, String>(5)?,
                "price": r.get::<_, String>(6)?,
                "commission": r.get::<_, String>(7)?,
                "currency": r.get::<_, String>(8)?,
                "occurred_at_ns": r.get::<_, i64>(9)?,
            }))
        },
    )
}

/// `GET /desks/{desk_id}/history/cycles` (§7): the desk's closed position cycles,
/// newest first. The realized P&L is the closing event's own net-of-fees figure
/// (root §12.4).
pub fn history_cycles(store: &Store, desk_id: &str) -> Result<Vec<Value>, StoreError> {
    listing(
        store,
        desk_id,
        "SELECT id, position_id, instrument_id, opened_at_ns, closed_at_ns, \
                realized_pnl, currency \
         FROM position_cycles WHERE desk_id = ?1 ORDER BY closed_at_ns DESC, id DESC",
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "position_id": r.get::<_, String>(1)?,
                "instrument_id": r.get::<_, String>(2)?,
                "opened_at_ns": r.get::<_, i64>(3)?,
                "closed_at_ns": r.get::<_, i64>(4)?,
                "realized_pnl": r.get::<_, String>(5)?,
                "currency": r.get::<_, String>(6)?,
            }))
        },
    )
}

/// One desk-scoped listing: the query, its row projection, complete (pagination
/// stays deferred, root §18).
fn listing(
    store: &Store,
    desk_id: &str,
    sql: &'static str,
    row: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
) -> Result<Vec<Value>, StoreError> {
    let desk = desk_id.to_owned();
    store.call(move |conn| conn.prepare(sql)?.query_map([desk], row)?.collect())
}

// ---------------------------------------------------------------------------
// The node thread: placing, cancelling, and reading back
// ---------------------------------------------------------------------------

/// Builds the order and hands it to the node exactly as a NautilusTrader
/// strategy does: cache, then `OrderInitialized`, then the risk engine's queued
/// endpoint. The client order id is the action id, so an order is traceable to
/// the intent that created it and restoration can re-place it under the same
/// identifier (§4.3).
fn place(context: &NodeContext, form: Form, client_order_id: ClientOrderId) {
    let mut factory = OrderFactory::new(
        context.trader_id,
        StrategyId::new(STRATEGY),
        None,
        None,
        Rc::clone(&context.clock),
        false,
        false,
    );
    let instrument_id = InstrumentId::from(form.entry.instrument_id);
    // Time in force is GTC and implicit (§4.2).
    let order = match form.price {
        Some(price) => factory.limit(
            instrument_id,
            form.side,
            form.quantity,
            price,
            Some(TimeInForce::Gtc),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(client_order_id),
        ),
        None => factory.market(
            instrument_id,
            form.side,
            form.quantity,
            Some(TimeInForce::Gtc),
            None,
            None,
            None,
            None,
            None,
            Some(client_order_id),
        ),
    };
    hand_to_node(context, order, true);
}

/// The shared tail of [`place`] and restoration: cache the order, optionally
/// announce it, and send the submit command.
///
/// A restored order is announced by nobody: it already carries its accepted
/// history, so NautilusTrader's own invalid-state-transition guard drops the
/// `OrderSubmitted` and `OrderAccepted` the re-placement would otherwise repeat,
/// which is exactly why restoration adds no history rows (§4.3).
fn hand_to_node(context: &NodeContext, order: OrderAny, announce: bool) {
    let command = SubmitOrder::from_order(
        &order,
        context.trader_id,
        None,
        None,
        UUID4::new(),
        UnixNanos::from(now_ns().max(0) as u64),
    );
    let initialized = OrderEventAny::Initialized(order.init_event().clone());
    let strategy_id = order.strategy_id();
    if let Err(e) = context
        .cache
        .borrow_mut()
        .add_order(order, None, None, true)
    {
        tracing::error!("the desk's node refused the order: {e}");
        return;
    }
    if announce {
        msgbus::publish_order_event(format!("events.order.{strategy_id}").into(), &initialized);
    }
    msgbus::send_trading_command(
        MessagingSwitchboard::risk_engine_queue_execute(),
        TradingCommand::SubmitOrder(command),
    );
}

/// Waits for the node to answer, reading its cache between the runner's turns.
/// The node processes commands and their events on its own thread, so a blocking
/// wait *on* that thread would deadlock; this waits beside it (§4.2).
fn settle(
    node: &Node,
    client_order_id: ClientOrderId,
    read: fn(&NodeContext, ClientOrderId) -> Option<Settled>,
) -> Result<Settled, TradeError> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if let Some(settled) = node.call(move |context| read(context, client_order_id))? {
            return Ok(settled);
        }
        if Instant::now() >= deadline {
            return Err(TradeError::Unavailable(format!(
                "the paper book did not answer for order {client_order_id} in time"
            )));
        }
        thread::sleep(SETTLE_POLL);
    }
}

/// A submit has answered once the order is past the in-flight states: accepted,
/// filled, or refused (§4.2).
fn settled_submit(context: &NodeContext, client_order_id: ClientOrderId) -> Option<Settled> {
    let cache = context.cache.borrow();
    let order = cache.order(&client_order_id)?;
    if matches!(
        order.status(),
        OrderStatus::Initialized
            | OrderStatus::Submitted
            | OrderStatus::Emulated
            | OrderStatus::Released
    ) {
        return None;
    }
    Some(Settled {
        projection: order_projection(&order),
        refusal: refusal(&order),
    })
}

/// A cancel has answered once the order is terminal, or the sandbox has refused
/// the cancel outright.
fn settled_cancel(context: &NodeContext, client_order_id: ClientOrderId) -> Option<Settled> {
    let cache = context.cache.borrow();
    let order = cache.order(&client_order_id)?;
    if order.is_closed() {
        return Some(Settled {
            projection: order_projection(&order),
            refusal: None,
        });
    }
    match order.last_event() {
        OrderEventAny::CancelRejected(event) => Some(Settled {
            projection: order_projection(&order),
            refusal: Some(event.reason.to_string()),
        }),
        _ => None,
    }
}

/// The sandbox's own words for a refusal, never MarketRig's (per D38).
fn refusal(order: &OrderAny) -> Option<String> {
    if !matches!(
        order.status(),
        OrderStatus::Rejected | OrderStatus::Denied | OrderStatus::Voided
    ) {
        return None;
    }
    order.events().iter().rev().find_map(|event| match event {
        OrderEventAny::Rejected(event) => Some(event.reason.to_string()),
        OrderEventAny::Denied(event) => Some(event.reason.to_string()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// The action row (§6)
// ---------------------------------------------------------------------------

enum Begun {
    New(ActionRecord),
    Replay(ActionRecord),
}

/// The stored record for `(desk_id, action_id)`, which is the whole idempotency
/// contract (R1-8).
fn stored(
    store: &Store,
    desk_id: &str,
    action_id: &str,
) -> Result<Option<ActionRecord>, StoreError> {
    let desk = desk_id.to_owned();
    let action = action_id.to_owned();
    store.call(move |conn| {
        conn.query_row(
            "SELECT action_id, id, kind, created_at_ns, outcome FROM trading_actions \
             WHERE desk_id = ?1 AND action_id = ?2",
            params![desk, action],
            |r| {
                Ok(ActionRecord {
                    action_id: r.get(0)?,
                    id: r.get(1)?,
                    kind: r.get(2)?,
                    created_at_ns: r.get(3)?,
                    outcome: r
                        .get::<_, Option<String>>(4)?
                        .and_then(|outcome| serde_json::from_str(&outcome).ok()),
                })
            },
        )
        .optional()
    })
}

/// Records the action **before** the sandbox sees the command, in its own unit
/// (§6). Losing the race on the same identity is a replay, not a failure.
fn begin(
    store: &Store,
    desk_id: &str,
    kind: &'static str,
    action_id: &str,
    request: &str,
) -> Result<Begun, TradeError> {
    let record = ActionRecord {
        action_id: action_id.to_owned(),
        id: Uuid::now_v7().to_string(),
        kind: kind.to_owned(),
        created_at_ns: now_ns(),
        outcome: None,
    };
    let row = record.clone();
    let desk = desk_id.to_owned();
    let request = request.to_owned();
    let inserted = store.unit(move |tx| {
        tx.execute(
            "INSERT INTO trading_actions \
             (desk_id, action_id, id, kind, source, request, created_at_ns) \
             VALUES (?1, ?2, ?3, ?4, 'SESSION', ?5, ?6)",
            params![
                desk,
                row.action_id,
                row.id,
                row.kind,
                request,
                row.created_at_ns
            ],
        )
    });
    match inserted {
        Ok(_) => Ok(Begun::New(record)),
        Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(f, _)))
            if f.code == ErrorCode::ConstraintViolation =>
        {
            match stored(store, desk_id, action_id)? {
                Some(record) => Ok(Begun::Replay(record)),
                None => Err(TradeError::Unavailable(
                    "the action record could not be read back".into(),
                )),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// The outcome lands on the row when the command answers (§6).
fn finish(
    store: &Store,
    desk_id: &str,
    action_id: &str,
    outcome: &Value,
) -> Result<(), StoreError> {
    let desk = desk_id.to_owned();
    let action = action_id.to_owned();
    let outcome = outcome.to_string();
    store
        .unit(move |tx| {
            tx.execute(
                "UPDATE trading_actions SET outcome = ?3 WHERE desk_id = ?1 AND action_id = ?2",
                params![desk, action, outcome],
            )
        })
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// Capture (§5, R1-5)
// ---------------------------------------------------------------------------

/// The desk's restoration snapshot: account state, open positions, open orders,
/// exactly as NautilusTrader serializes them (§5, per D64).
#[derive(Debug, Serialize, Deserialize)]
struct BookSnapshot {
    accounts: Vec<AccountAny>,
    positions: Vec<Position>,
    orders: Vec<OrderAny>,
}

/// Subscribes this node's durable capture to its own message bus.
///
/// The bus is thread-local, so this runs on the node thread and sees only that
/// desk's events. The execution engine publishes an order or position event
/// **after** it has applied it to the node's cache, so a handler reads a
/// consistent book and writes the event, whatever it implies, and the rewritten
/// snapshot in one unit (§5).
///
/// ponytail: each handler blocks the node thread on the database thread for the
/// length of one small transaction. One desk's event rate is a handful per
/// minute; the upgrade path is a bounded queue drained by a writer task if a
/// desk ever trades fast enough to feel it.
pub(crate) fn install_capture(desk_id: String, store: Store, cache: Rc<RefCell<Cache>>) {
    let (desk, writer, book) = (desk_id.clone(), store.clone(), Rc::clone(&cache));
    msgbus::subscribe_order_events(
        "events.order.*".into(),
        TypedHandler::from(move |event: &OrderEventAny| {
            capture_order(&desk, &writer, &book, event);
        }),
        Some(10),
    );
    msgbus::subscribe_position_events(
        "events.position.*".into(),
        TypedHandler::from(move |event: &PositionEvent| {
            capture_position(&desk_id, &store, &cache, event);
        }),
        Some(10),
    );
}

fn capture_order(desk_id: &str, store: &Store, cache: &Rc<RefCell<Cache>>, event: &OrderEventAny) {
    let payload = match serde_json::to_string(event) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::error!("an order event could not be captured: {e}");
            return;
        }
    };
    let desk = desk_id.to_owned();
    let client_order_id = event.client_order_id().to_string();
    let instrument_id = event.instrument_id().to_string();
    // The event's own type name, which is what `order_events.kind` records (§5).
    let kind = event.clone().into_boxed().type_name();
    let occurred_at_ns = event.ts_event().as_u64() as i64;
    let fill = match event {
        OrderEventAny::Filled(fill) => Some((
            fill.trade_id.to_string(),
            fill.order_side.to_string(),
            fill.last_qty.to_string(),
            fill.last_px.to_string(),
            fill.commission
                .map_or_else(|| "0".to_string(), |money| money.as_decimal().to_string()),
            fill.currency.code.to_string(),
        )),
        _ => None,
    };
    let snapshot = snapshot_payload(&cache.borrow());

    let written = store.unit(move |tx| {
        tx.execute(
            "INSERT INTO order_events \
             (id, desk_id, client_order_id, instrument_id, kind, payload_version, payload, occurred_at_ns) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::now_v7().to_string(),
                desk,
                client_order_id,
                instrument_id,
                kind,
                PAYLOAD_VERSION,
                payload,
                occurred_at_ns
            ],
        )?;
        if let Some((trade_id, side, quantity, price, commission, currency)) = fill {
            tx.execute(
                "INSERT INTO fills \
                 (id, desk_id, client_order_id, trade_id, instrument_id, side, quantity, price, \
                  commission, currency, payload_version, payload, occurred_at_ns) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    Uuid::now_v7().to_string(),
                    desk,
                    client_order_id,
                    trade_id,
                    instrument_id,
                    side,
                    quantity,
                    price,
                    commission,
                    currency,
                    PAYLOAD_VERSION,
                    payload,
                    occurred_at_ns
                ],
            )?;
        }
        write_snapshot(tx, &desk, &snapshot)
    });
    if let Err(e) = written {
        tracing::error!("an order event could not be captured: {e}");
    }
}

fn capture_position(
    desk_id: &str,
    store: &Store,
    cache: &Rc<RefCell<Cache>>,
    event: &PositionEvent,
) {
    let payload = match serde_json::to_string(event) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::error!("a position event could not be captured: {e}");
            return;
        }
    };
    let desk = desk_id.to_owned();
    let (kind, position_id, occurred_at_ns) = match event {
        PositionEvent::PositionOpened(e) => ("PositionOpened", e.position_id, e.ts_event),
        PositionEvent::PositionChanged(e) => ("PositionChanged", e.position_id, e.ts_event),
        PositionEvent::PositionClosed(e) => ("PositionClosed", e.position_id, e.ts_event),
        PositionEvent::PositionAdjusted(e) => ("PositionAdjusted", e.position_id, e.ts_event),
    };
    let instrument_id = event.instrument_id().to_string();
    let position = position_id.to_string();
    let occurred_at_ns = occurred_at_ns.as_u64() as i64;

    let book = cache.borrow();
    // A fill that carries the netting position through zero closes one cycle
    // (§6): the cycle, its evaluation prompt, and the snapshot commit together.
    let cycle = match event {
        PositionEvent::PositionClosed(closed) => Some(cycle_of(closed, &book, &payload)),
        _ => None,
    };
    let snapshot = snapshot_payload(&book);
    drop(book);

    let written = store.unit(move |tx| {
        tx.execute(
            "INSERT INTO position_events \
             (id, desk_id, position_id, instrument_id, kind, payload_version, payload, occurred_at_ns) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::now_v7().to_string(),
                desk,
                position,
                instrument_id,
                kind,
                PAYLOAD_VERSION,
                payload,
                occurred_at_ns
            ],
        )?;
        if let Some(cycle) = cycle {
            let cycle_id = insert_cycle(tx, &desk, &cycle)?;
            queue_evaluation(tx, &desk, &cycle_id, &cycle)?;
        }
        write_snapshot(tx, &desk, &snapshot)
    });
    if let Err(e) = written {
        tracing::error!("a position event could not be captured: {e}");
    }
}

/// One closed position cycle, as the closing event reports it. The realized P&L
/// is the event's own net-of-fees figure and is never recomputed (root §12.4).
#[derive(Debug, Clone)]
pub(crate) struct Cycle {
    pub position_id: String,
    pub instrument_id: String,
    pub opened_at_ns: i64,
    pub closed_at_ns: i64,
    pub realized_pnl: String,
    pub currency: String,
    pub payload: String,
    /// The orders and trades that made the cycle, in fill order — the identity
    /// the evaluation prompt carries so its supporting rows can be queried.
    pub client_order_ids: Vec<String>,
    pub trade_ids: Vec<String>,
}

fn cycle_of(closed: &PositionClosed, cache: &Cache, payload: &str) -> Cycle {
    let mut client_order_ids: Vec<String> = Vec::new();
    let mut trade_ids: Vec<String> = Vec::new();
    if let Some(position) = cache.position(&closed.position_id) {
        for fill in &position.events {
            let client_order_id = fill.client_order_id.to_string();
            if !client_order_ids.contains(&client_order_id) {
                client_order_ids.push(client_order_id);
            }
            trade_ids.push(fill.trade_id.to_string());
        }
    }
    Cycle {
        position_id: closed.position_id.to_string(),
        instrument_id: closed.instrument_id.to_string(),
        opened_at_ns: closed.ts_opened.as_u64() as i64,
        closed_at_ns: closed.ts_closed.unwrap_or(closed.ts_event).as_u64() as i64,
        realized_pnl: closed
            .realized_pnl
            .map_or_else(|| "0".to_string(), |money| money.as_decimal().to_string()),
        currency: closed
            .realized_pnl
            .map_or(closed.currency, |money| money.currency)
            .code
            .to_string(),
        payload: payload.to_owned(),
        client_order_ids,
        trade_ids,
    }
}

/// Inserts the cycle row and answers its id.
pub(crate) fn insert_cycle(
    tx: &Transaction<'_>,
    desk_id: &str,
    cycle: &Cycle,
) -> rusqlite::Result<String> {
    let id = Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO position_cycles \
         (id, desk_id, position_id, instrument_id, opened_at_ns, closed_at_ns, \
          realized_pnl, currency, payload_version, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            desk_id,
            cycle.position_id,
            cycle.instrument_id,
            cycle.opened_at_ns,
            cycle.closed_at_ns,
            cycle.realized_pnl,
            cycle.currency,
            PAYLOAD_VERSION,
            cycle.payload
        ],
    )?;
    Ok(id)
}

/// The cycle's evaluation prompt, in the same unit as the cycle (§6, per D22,
/// D38). R1 delivers nothing: it is born `QUEUED` and stays there until R3.
pub(crate) fn queue_evaluation(
    tx: &Transaction<'_>,
    desk_id: &str,
    cycle_id: &str,
    cycle: &Cycle,
) -> rusqlite::Result<()> {
    let mut fill_ids: Vec<String> = Vec::new();
    {
        let mut by_trade =
            tx.prepare("SELECT id FROM fills WHERE desk_id = ?1 AND trade_id = ?2")?;
        for trade_id in &cycle.trade_ids {
            if let Some(id) = by_trade
                .query_row(params![desk_id, trade_id], |r| r.get::<_, String>(0))
                .optional()?
            {
                fill_ids.push(id);
            }
        }
    }
    let payload = json!({
        "kind": "EVALUATION",
        "cycle_id": cycle_id,
        "instrument_id": cycle.instrument_id,
        "realized_pnl": cycle.realized_pnl,
        "currency": cycle.currency,
        "closed_at_ns": cycle.closed_at_ns,
        "client_order_ids": cycle.client_order_ids,
        "fill_ids": fill_ids,
    });
    tx.execute(
        "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
         VALUES (?1, ?2, 'EVALUATION', 'QUEUED', ?3, ?4)",
        params![
            Uuid::now_v7().to_string(),
            desk_id,
            payload.to_string(),
            now_ns()
        ],
    )?;
    Ok(())
}

fn snapshot_payload(cache: &Cache) -> String {
    let snapshot = BookSnapshot {
        accounts: cache.accounts_all_owned(),
        positions: cache
            .positions_open(None, None, None, None, None)
            .iter()
            .map(|position| position.cloned())
            .collect(),
        orders: cache
            .orders_open(None, None, None, None, None)
            .iter()
            .map(|order| order.cloned())
            .collect(),
    };
    serde_json::to_string(&snapshot).unwrap_or_else(|e| {
        tracing::error!("the desk's book snapshot could not be serialized: {e}");
        String::new()
    })
}

/// Exactly one current snapshot per desk, rewritten in the same unit as the
/// event that changed the book (§5).
fn write_snapshot(tx: &Transaction<'_>, desk_id: &str, payload: &str) -> rusqlite::Result<()> {
    if payload.is_empty() {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO book_snapshots (desk_id, payload_version, payload, written_at_ns) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(desk_id) DO UPDATE SET \
           payload_version = excluded.payload_version, \
           payload = excluded.payload, \
           written_at_ns = excluded.written_at_ns",
        params![desk_id, PAYLOAD_VERSION, payload, now_ns()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Restoration (§4.3, R1-6)
// ---------------------------------------------------------------------------

/// Restores the desk's book at node start: rebuild the account and positions,
/// re-place resting limit orders under their **original** client order
/// identifiers, never from history replay (per D64).
///
/// An absent snapshot is a fresh book and needs nothing done: the sandbox client
/// seeds the desk's cash account from its own starting balances when it connects
/// (§4.1). A payload version this build does not know stops the node rather than
/// trading a book MarketRig cannot account for.
pub(crate) fn restore(store: &Store, desk_id: &str, node: &Node) -> Result<(), NodeError> {
    let desk = desk_id.to_owned();
    let row: Option<(i64, String)> = store
        .call(move |conn| {
            conn.query_row(
                "SELECT payload_version, payload FROM book_snapshots WHERE desk_id = ?1",
                [desk],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
        .map_err(|e| NodeError::new(format!("the desk's book snapshot is unreadable: {e}")))?;
    let Some((version, payload)) = row else {
        return Ok(());
    };
    if version != PAYLOAD_VERSION {
        return Err(NodeError::new(format!(
            "book snapshot (payload version {version}) cannot be restored by this build"
        )));
    }
    let snapshot: BookSnapshot = serde_json::from_str(&payload)
        .map_err(|e| NodeError::new(format!("the desk's book snapshot is unreadable: {e}")))?;
    node.call(move |context| apply(context, snapshot))?
        .map_err(NodeError::new)
}

fn apply(context: &NodeContext, snapshot: BookSnapshot) -> Result<(), String> {
    {
        let mut cache = context.cache.borrow_mut();
        for account in snapshot.accounts {
            cache.add_account(account).map_err(|e| e.to_string())?;
        }
        for position in &snapshot.positions {
            // The opening order is history, not cache state, so the position is
            // added without one; netting position ids are derived from the
            // instrument and strategy, so a later fill finds this one.
            cache
                .add_position_without_order(position, OmsType::Netting)
                .map_err(|e| e.to_string())?;
        }
        for order in &snapshot.orders {
            cache
                .add_order(order.clone(), None, None, true)
                .map_err(|e| e.to_string())?;
        }
        cache.build_index();
    }
    // A resting order lives in the venue's matching engine, which only a running
    // execution engine can put it in, so each one goes back through the ordinary
    // submit path carrying its own accepted history.
    for order in snapshot.orders {
        hand_to_node(context, order, false);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// trade::cycle_and_prompt_atomic, trade::snapshot_restores_book
// (feature SPEC §11)
// ---------------------------------------------------------------------------

#[cfg(test)]
fn count(store: &Store, sql: &'static str) -> i64 {
    store
        .call(move |conn| conn.query_row(sql, [], |r| r.get(0)))
        .unwrap()
}

#[cfg(test)]
#[test]
fn cycle_and_prompt_atomic() {
    let (_dir, store) = crate::store::open_temp();
    let desk = crate::node::seeded_desk(&store, "alpha");
    let cycle = Cycle {
        position_id: "0700.XHKG-MARKETRIG-001".into(),
        instrument_id: "0700.XHKG".into(),
        opened_at_ns: 1_000,
        closed_at_ns: 2_000,
        // Net of fees, the closing event's own figure.
        realized_pnl: "-93.71".into(),
        currency: "HKD".into(),
        payload: r#"{"PositionClosed":{}}"#.into(),
        client_order_ids: vec!["buy-tencent-1".into()],
        trade_ids: vec!["T-1".into()],
    };

    // A fill row the prompt's `fill_ids` must find.
    let seeded = desk.clone();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO fills VALUES \
                 ('f-1', ?1, 'buy-tencent-1', 'T-1', '0700.XHKG', 'BUY', '100', '441.40', \
                  '48.55', 'HKD', 1, '{}', 1500)",
                [seeded],
            )
        })
        .unwrap();

    // A forced failure where the prompt insert belongs — the prompt's own kind
    // CHECK — must leave the cycle behind too.
    let (poisoned, one) = (cycle.clone(), desk.clone());
    let failed = store.unit(move |tx| {
        insert_cycle(tx, &one, &poisoned)?;
        tx.execute(
            "INSERT INTO prompts VALUES ('p-0', ?1, 'NOT_A_KIND', 'QUEUED', '{}', 1)",
            [one.as_str()],
        )
    });
    assert!(failed.is_err(), "the poisoned prompt must fail the unit");
    assert_eq!(count(&store, "SELECT count(*) FROM position_cycles"), 0);
    assert_eq!(count(&store, "SELECT count(*) FROM prompts"), 0);

    // The real pair commits together.
    let (row, two) = (cycle.clone(), desk.clone());
    store
        .unit(move |tx| {
            let cycle_id = insert_cycle(tx, &two, &row)?;
            queue_evaluation(tx, &two, &cycle_id, &row)
        })
        .expect("the cycle and its prompt commit in one unit");

    let (cycle_id, realized, currency): (String, String, String) = store
        .call(|conn| {
            conn.query_row(
                "SELECT id, realized_pnl, currency FROM position_cycles",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!(realized, "-93.71", "the closing event's own net figure");
    assert_eq!(currency, "HKD");

    let (kind, state, payload): (String, String, String) = store
        .call(|conn| {
            conn.query_row("SELECT kind, state, payload FROM prompts", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .unwrap();
    assert_eq!((kind.as_str(), state.as_str()), ("EVALUATION", "QUEUED"));
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap(),
        json!({
            "kind": "EVALUATION",
            "cycle_id": cycle_id,
            "instrument_id": "0700.XHKG",
            "realized_pnl": "-93.71",
            "currency": "HKD",
            "closed_at_ns": 2_000,
            "client_order_ids": ["buy-tencent-1"],
            "fill_ids": ["f-1"],
        }),
        "the §6 prompt payload, field for field"
    );
}

#[cfg(test)]
#[test]
fn snapshot_restores_book() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::feed::{self, MarketState};

    let aapl = catalog::find("AAPL.XNAS").unwrap();
    let (_dir, store) = crate::store::open_temp();
    let (base, hits) = feed::scripted_server(vec![(
        200,
        feed::chart_body("AAPL", "USD", "316.85", 1_788_206_401),
    )]);
    let desk = crate::node::seeded_desk(&store, "alpha");

    let registry = Registry::new(
        store.clone(),
        Arc::new(MarketState::new()),
        Some(base.clone()),
    );
    registry.ensure(&desk).expect("the node starts");
    // The first poll must land before an order can match against it.
    crate::node::within(10, "the first observation", || {
        registry.market().read(aapl, now_ns()).sequence == 1
    });
    assert!(hits.load(Ordering::SeqCst) >= 1);

    // A market buy fills at last, and a limit buy below it rests.
    let (bought, replayed) = submit(
        &store,
        &registry,
        &desk,
        r#"{"action_id":"buy-aapl-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"MARKET","quantity":"10","price":null}"#,
    )
    .expect("the market buy is accepted");
    assert!(!replayed);
    let outcome = bought.outcome.clone().unwrap();
    assert_eq!(outcome["status"], "FILLED", "{outcome}");
    assert_eq!(outcome["client_order_id"], "buy-aapl-1");
    assert_eq!(outcome["filled_quantity"], "10");

    let (rested, _) = submit(
        &store,
        &registry,
        &desk,
        r#"{"action_id":"rest-aapl-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"LIMIT","quantity":"5","price":"200.00"}"#,
    )
    .expect("the limit buy is accepted");
    assert_eq!(rested.outcome.clone().unwrap()["status"], "ACCEPTED");

    let before = (
        count(&store, "SELECT count(*) FROM order_events"),
        count(&store, "SELECT count(*) FROM fills"),
        count(&store, "SELECT count(*) FROM position_events"),
    );
    assert!(before.1 >= 1, "the fill was captured");

    // What the running book says, and what the snapshot holds.
    let live = registry.ensure(&desk).unwrap();
    let (balances, position, resting) = live
        .call(|context| {
            let cache = context.cache.borrow();
            (
                book_balances(&cache),
                cache
                    .positions_open(None, None, None, None, None)
                    .first()
                    .map(|position| (position.id.to_string(), position.quantity.to_string())),
                cache
                    .orders_open(None, None, None, None, None)
                    .iter()
                    .map(|order| order_projection(order))
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
    assert_eq!(position.as_ref().map(|p| p.1.as_str()), Some("10"));
    assert_eq!(resting.len(), 1, "one order rests: {resting:?}");
    assert_eq!(resting[0]["client_order_id"], "rest-aapl-1");
    drop(live);
    registry.stop_all();

    let snapshot_version: i64 = store
        .call(|conn| {
            conn.query_row("SELECT payload_version FROM book_snapshots", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(snapshot_version, PAYLOAD_VERSION);

    // A fresh node over the same store restores the book.
    let restarted = Registry::new(store.clone(), Arc::new(MarketState::new()), Some(base));
    let node = restarted
        .ensure(&desk)
        .expect("the node restores and starts");
    let (restored_balances, restored_position, restored_orders) = node
        .call(|context| {
            let cache = context.cache.borrow();
            (
                book_balances(&cache),
                cache
                    .positions_open(None, None, None, None, None)
                    .first()
                    .map(|position| (position.id.to_string(), position.quantity.to_string())),
                cache
                    .orders_open(None, None, None, None, None)
                    .iter()
                    .map(|order| order_projection(order))
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
    assert_eq!(restored_balances, balances, "balances survive the restart");
    assert_eq!(restored_position, position, "so does the position");
    assert_eq!(
        restored_orders, resting,
        "and the resting order, under its original client order id"
    );

    // The order is genuinely back in the venue's matching engine: cancelling it
    // through the ordinary path closes it.
    cancel(
        &store,
        &restarted,
        &desk,
        "rest-aapl-1",
        r#"{"action_id":"cancel-aapl-1"}"#,
    )
    .expect("the restored order cancels");
    assert!(
        node.call(|context| context
            .cache
            .borrow()
            .orders_open(None, None, None, None, None)
            .is_empty())
            .unwrap()
    );

    // Restoration replays no history: only the cancel's own events are new.
    let after = (
        count(&store, "SELECT count(*) FROM order_events"),
        count(&store, "SELECT count(*) FROM fills"),
        count(&store, "SELECT count(*) FROM position_events"),
    );
    assert_eq!(after.1, before.1, "no fill row was replayed");
    assert_eq!(after.2, before.2, "no position event was replayed");
    assert!(
        after.0 >= before.0 && after.0 <= before.0 + 2,
        "restoration added no order events; the cancel added its own: {before:?} -> {after:?}"
    );
    restarted.stop_all();
}

/// Every venue account's total balance, as decimal text — the comparable shape
/// of a desk's book across a restart.
#[cfg(test)]
fn book_balances(cache: &Cache) -> Vec<(String, String)> {
    let mut balances: Vec<(String, String)> = cache
        .accounts_all_owned()
        .iter()
        .flat_map(|account| {
            use nautilus_model::accounts::Account;
            account
                .balances_total()
                .into_iter()
                .map(|(currency, money)| {
                    (
                        format!("{}-{currency}", account.id()),
                        money.as_decimal().to_string(),
                    )
                })
        })
        .collect();
    balances.sort();
    balances
}
