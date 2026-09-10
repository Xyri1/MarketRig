//! One NautilusTrader node per desk: its own thread, its own paper book, and the
//! polling task that feeds it (root `sdd/SPEC.md` §12.1).
//!
//! Nodes start lazily on a desk's first market-plane use and are never moved
//! between threads, because every NautilusTrader global is thread-local. The
//! [`Registry`] is the daemon's whole view of them: it starts them, hands out
//! handles, and stops them.
//!
//! Contract: `sdd/features/r1-equity-paper-trading/SPEC.md` §4.1, §4.3, per R1-4,
//! R1-6; root `sdd/SPEC.md` §12.1.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
#[cfg(test)]
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nautilus_common::cache::{Cache, CacheView};
use nautilus_common::clients::{DataClient, ExecutionClient};
use nautilus_common::clock::{Clock, TestClock};
use nautilus_common::enums::Environment;
use nautilus_common::factories::{
    ClientConfig, DataClientFactory, SimulatedExecutionClientFactory,
};
use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::logging::logger::LoggerConfig;
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::data::SubscribeQuotes;
use nautilus_core::UnixNanos;
use nautilus_execution::client::core::ExecutionClientCore;
use nautilus_execution::models::fee::{FeeModelAny, MakerTakerFeeModel};
use nautilus_live::node::{LiveNode, LiveNodeHandle, NodeRunMode};
use nautilus_model::data::{Data, QuoteTick};
use nautilus_model::enums::{AccountType, BookType, OmsType};
use nautilus_model::identifiers::{
    AccountId, ClientId, ClientOrderId, InstrumentId, TraderId, Venue,
};
use nautilus_model::instruments::{Equity, InstrumentAny};
use nautilus_model::orders::Order;
use nautilus_model::types::fixed::{HIGH_PRECISION_MODE, PRECISION_BYTES};
use nautilus_model::types::{Currency, Money, Price, Quantity};
use nautilus_portfolio::portfolio::Portfolio;
use nautilus_sandbox::{
    SandboxExecutionClient, SandboxExecutionClientConfig, SandboxExecutionClientFactory,
};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::catalog::{self, Entry, Market};
use crate::cn::{self, CnExec, Exec};
use crate::feed::{
    self, ChartClient, FeedBase, IDLE_INTERVAL, MarketState, Phase, next_delay, phase,
};
use crate::hithink::AShareFeed;
use crate::store::{Store, now_ns};
use crate::trade;

/// The name of the one out-of-tree data client every node registers (§2.1).
const DATA_CLIENT: &str = "MARKETRIG";

/// How long a node may take to reach its running state before the operation that
/// started it gives up (§4.3). Node startup is loopback-free: it builds engines,
/// seeds the account, and connects clients that have nothing to dial.
const START_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// The precision assertion (§1, §4.3, per D39, R1-4): the pinned crates must
/// report the default 64-bit mode, which is what the workspace declares by
/// keeping `high-precision` off every `nautilus-*` entry. It runs first at every
/// node start, before anything reads a price.
pub fn assert_precision() {
    assert_eq!(
        HIGH_PRECISION_MODE, 0,
        "nautilus-* must be built in 64-bit precision mode; \
         a `high-precision` feature reached the graph"
    );
    assert_eq!(
        PRECISION_BYTES, 8,
        "nautilus-* must be built with 8-byte fixed-point values"
    );
}

// ---------------------------------------------------------------------------
// The controlled-clock seam (feature SPEC `a-share-engine` §6)
// ---------------------------------------------------------------------------

/// Seeds every node's clock at a chosen instant, so the CN session, owning-day
/// and deadline decisions — and the stamps NautilusTrader puts on its own
/// events — are the harness's to drive (feature SPEC `a-share-engine` §5.1,
/// §6). Honored only alongside [`crate::store::TEST_DATA_ROOT_ENV`], exactly as
/// [`crate::feed::TEST_QUOTE_URL_ENV`] is.
pub const TEST_CLOCK_ENV: &str = "MARKETRIG_TEST_CLOCK_NS";

/// The instant the seam names, or `None` for the ordinary `LiveClock`.
pub fn test_clock_start_ns() -> Option<u64> {
    resolve_clock_start(
        std::env::var_os(crate::store::TEST_DATA_ROOT_ENV).as_deref(),
        std::env::var(TEST_CLOCK_ENV).ok().as_deref(),
    )
}

/// The seam's rule, apart from the environment: both variables or nothing, and
/// the instant is nanoseconds ([`crate::feed::resolve_base_url`]'s shape).
fn resolve_clock_start(
    test_data_root: Option<&std::ffi::OsStr>,
    test_clock_ns: Option<&str>,
) -> Option<u64> {
    match (test_data_root, test_clock_ns) {
        (Some(_), Some(value)) => value.trim().parse().ok(),
        _ => None,
    }
}

/// Desks registered for controlled time in-process, and the instant their clock
/// starts at. The environment seam above answers for every desk; this map is how
/// one module check controls one desk without disturbing its neighbours in the
/// same test binary, and it exists only in a test build.
#[cfg(test)]
static CONTROLLED: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
fn registered(desk_id: &str) -> Option<u64> {
    CONTROLLED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(desk_id)
        .copied()
}

#[cfg(not(test))]
fn registered(_desk_id: &str) -> Option<u64> {
    None
}

// The clock instance for a desk, on the thread that built its node. [`build`]
// asks for it once for the kernel factory and once per venue for the exec
// factory; every call must answer the *same* clock, which is what this memo
// guarantees.
thread_local! {
    static CLOCKS: RefCell<HashMap<String, Rc<RefCell<TestClock>>>> =
        RefCell::new(HashMap::new());
}

#[cfg(test)]
pub(crate) fn register_controlled(desk_id: &str, start_ns: u64) {
    CONTROLLED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(desk_id.to_owned(), start_ns);
}

#[cfg(test)]
pub(crate) fn unregister_controlled(desk_id: &str) {
    CONTROLLED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(desk_id);
}

/// The seam [`build`] reads: the desk's shared `TestClock`, or `None` for an
/// ordinary desk on a `LiveClock`.
fn controlled_clock(desk_id: &str) -> Option<Rc<RefCell<TestClock>>> {
    let start_ns = registered(desk_id).or_else(test_clock_start_ns)?;
    Some(CLOCKS.with_borrow_mut(|clocks| {
        Rc::clone(clocks.entry(desk_id.to_owned()).or_insert_with(|| {
            let mut clock = TestClock::new();
            clock.advance_time(UnixNanos::from(start_ns), true);
            Rc::new(RefCell::new(clock))
        }))
    }))
}

/// The sandbox factory with the controlled clock injected — the same
/// `SandboxExecutionClient` the daemon builds, differing only in which clock it
/// and its matching engines read.
///
/// The swap is required: `nautilus-sandbox-0.62.0/src/factory.rs:80` hard-codes
/// `LiveClock::default()` for the sandbox client and passes that clock to every
/// `OrderMatchingEngine` it creates, so the kernel clock factory alone would
/// leave every order and fill event stamped with wall-clock time.
#[derive(Debug)]
struct ControlledSandboxFactory(Rc<RefCell<TestClock>>);

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

/// A node failure, which every market-plane operation answers as
/// `MARKET_UNAVAILABLE` (§7, R1-6).
#[derive(Debug)]
pub struct NodeError(String);

impl NodeError {
    pub(crate) fn new(message: impl Into<String>) -> NodeError {
        NodeError(message.into())
    }

    pub fn code(&self) -> &'static str {
        "MARKET_UNAVAILABLE"
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NodeError {}

/// A unit of work for one node's thread. The context is the seam later chunks
/// extend: C12's order submission and C13's cache reads are jobs, not new
/// channels.
type Job = Box<dyn FnOnce(&NodeContext) + Send + 'static>;

/// What a job runs against, on the node thread. Everything here is thread-local
/// NautilusTrader state, which is why it never leaves that thread.
pub struct NodeContext {
    /// The node's own cache — the live authority for orders and positions (R1-5).
    pub cache: Rc<RefCell<Cache>>,
    /// The node's clock, which every order NautilusTrader builds is stamped by.
    pub clock: Rc<RefCell<dyn Clock>>,
    /// The node's portfolio — the component that owns account reservations.
    /// Restoration needs it because `CashAccount::balances_locked` is
    /// `#[serde(skip)]`, so a snapshot cannot carry the per-instrument lock map
    /// and only NautilusTrader can rebuild it (`crate::trade::apply`).
    pub portfolio: Rc<RefCell<Portfolio>>,
    pub trader_id: TraderId,
    /// The node's CN execution state: the release latch and, per CN instrument,
    /// the busy flag, the monotonic publish stamp and readiness
    /// (`crate::cn`). In memory only, touched on this thread alone.
    pub cn: Exec,
}

/// A started desk node: a synchronous way to run work on its thread, and the
/// stop signal its run loop watches.
pub struct Node {
    jobs: UnboundedSender<Job>,
    control: LiveNodeHandle,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("state", &self.control.state())
            .finish_non_exhaustive()
    }
}

impl Node {
    /// Runs `f` on the node thread and waits for its answer — the same shape as
    /// [`crate::store::Store::call`], for the same reason: the state it touches
    /// belongs to one thread.
    pub fn call<T, F>(&self, f: F) -> Result<T, NodeError>
    where
        T: Send + 'static,
        F: FnOnce(&NodeContext) -> T + Send + 'static,
    {
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send(Box::new(move |context| {
                let _ = reply.send(f(context));
            }))
            .map_err(|_| NodeError::new(gone()))?;
        answer.recv().map_err(|_| NodeError::new(gone()))
    }

    /// The node clock's current instant — the one time source every CN decision
    /// reads (feature SPEC `a-share-engine` §5.1).
    pub fn now_ns(&self) -> Result<u64, NodeError> {
        self.call(|context| context.clock.borrow().timestamp_ns().as_u64())
    }

    /// Advances a controlled node clock to `to_ns` and dispatches every time
    /// event it releases, on the node thread; answers the dispatched timer names
    /// in order. A `TestClock` in a live node has no runner draining it, so
    /// nothing fires unless this is called.
    ///
    /// An ordinary node is on a `LiveClock` and answers an error: time is not
    /// the daemon's to move there.
    pub fn advance_to(&self, to_ns: u64) -> Result<Vec<String>, NodeError> {
        self.call(move |context| -> Result<Vec<String>, &'static str> {
            let handlers = {
                let mut clock = context.clock.borrow_mut();
                let test = clock
                    .as_any_mut()
                    .downcast_mut::<TestClock>()
                    .ok_or("this desk's node is not on a controlled clock")?;
                let events = test.advance_time(UnixNanos::from(to_ns), true);
                test.match_handlers(events)
            };
            let mut fired = Vec::new();
            for handler in handlers {
                fired.push(handler.event.name.to_string());
                handler.run();
            }
            Ok(fired)
        })?
        .map_err(NodeError::new)
    }

    /// Signals the run loop to stop and waits for the thread to finish. The
    /// caller bounds the wait (root §4.6); process exit ends whatever is left.
    fn stop_and_join(&self) {
        self.control.stop();
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

fn gone() -> &'static str {
    "The desk's trading node is no longer running."
}

/// Every desk's node, keyed by desk UUID. One per daemon, held by the API state;
/// nothing here is process-global.
pub struct Registry {
    store: Store,
    market: Arc<MarketState>,
    /// The one feed base this run polls, or `None` for no feed at all (§10.1).
    feed_base: Option<FeedBase>,
    nodes: Mutex<HashMap<String, Arc<Node>>>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("polling", &self.feed_base.is_some())
            .finish_non_exhaustive()
    }
}

impl Registry {
    pub fn new(store: Store, market: Arc<MarketState>, feed_base: Option<FeedBase>) -> Registry {
        Registry {
            store,
            market,
            feed_base,
            nodes: Mutex::new(HashMap::new()),
        }
    }

    /// The installation-wide market state every node's polling task feeds
    /// (root §12.2).
    pub fn market(&self) -> &Arc<MarketState> {
        &self.market
    }

    /// The desk's node, started on first use (§4.3, R1-6). A start failure
    /// appends `TRADING_NODE_FAILED`, answers the caller, and leaves the desk
    /// nodeless — so the next market-plane operation retries the start.
    ///
    /// ponytail: one mutex serializes starts across desks, so a slow start
    /// delays another desk's first market-plane call. Node start is loopback-free
    /// and rare; the upgrade path is a per-desk start latch if desk count grows.
    pub fn ensure(&self, desk_id: &str) -> Result<Arc<Node>, NodeError> {
        let mut nodes = self.lock();
        if let Some(node) = nodes.get(desk_id) {
            return Ok(Arc::clone(node));
        }
        match self.start(desk_id) {
            Ok(node) => {
                self.event(desk_id, "TRADING_NODE_STARTED", json!({}));
                nodes.insert(desk_id.to_owned(), Arc::clone(&node));
                tracing::info!(desk_id, "trading node started");
                Ok(node)
            }
            Err(e) => {
                self.event(desk_id, "TRADING_NODE_FAILED", json!({ "error": e.0 }));
                tracing::error!(desk_id, error = %e, "trading node failed to start");
                Err(e)
            }
        }
    }

    /// Stops every node (§4.3). Bounded by the caller's shutdown budget.
    pub fn stop_all(&self) {
        let nodes: Vec<Arc<Node>> = self.lock().drain().map(|(_, node)| node).collect();
        for node in &nodes {
            node.control.stop();
        }
        for node in &nodes {
            node.stop_and_join();
        }
    }

    fn start(&self, desk_id: &str) -> Result<Arc<Node>, NodeError> {
        let (ready, started) = mpsc::channel::<Result<LiveNodeHandle, String>>();
        let (jobs, job_rx) = tokio::sync::mpsc::unbounded_channel();
        let desk = desk_id.to_owned();
        let store = self.store.clone();
        let market = Arc::clone(&self.market);
        let feed_base = self.feed_base.clone();
        let thread = thread::Builder::new()
            .name(format!("marketrig-node-{desk_id}"))
            .spawn(move || node_thread(desk, store, market, feed_base, ready, job_rx))
            .map_err(|e| NodeError::new(format!("The desk's node thread could not start: {e}.")))?;

        let control = started
            .recv()
            .map_err(|_| NodeError::new("The desk's trading node stopped before it started."))?
            .map_err(|e| {
                NodeError::new(format!("The desk's trading node failed to start: {e}."))
            })?;

        // The run loop reaches `Running` only after its engines and clients are
        // connected, which is what makes a started node safe to trade on.
        let deadline = Instant::now() + START_TIMEOUT;
        while !control.is_running() {
            if thread.is_finished() {
                return Err(NodeError::new(
                    "The desk's trading node stopped while starting.",
                ));
            }
            if Instant::now() >= deadline {
                control.stop();
                return Err(NodeError::new(
                    "The desk's trading node did not finish starting in time.",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }

        let node = Arc::new(Node {
            jobs,
            control,
            thread: Mutex::new(Some(thread)),
        });

        // Restoration closes node start (§4.3, R1-6). It runs here, on the
        // running node, because the sandbox seeds the desk's account when its
        // client connects and only a running execution engine can hand a resting
        // order to a venue's matching engine — so the snapshot is applied the
        // moment the node is able to hold it, and before any caller can reach it.
        if let Err(e) = trade::restore(&self.store, desk_id, &node) {
            node.stop_and_join();
            return Err(e);
        }
        if let Err(e) = reconcile_cn(desk_id, &node, &self.market) {
            node.stop_and_join();
            return Err(e);
        }
        // Only now may the feed publish: restoration decided, the expired CN
        // orders are terminal, and the session gate is back up (§5.3).
        node.call(|context| context.cn.borrow_mut().released = true)?;
        Ok(node)
    }

    /// Advances every started node's clock, for the controlled-clock seam's own
    /// route. Answers the instant they were moved to.
    pub fn advance_all(&self, to_ns: u64) -> Result<u64, NodeError> {
        let nodes: Vec<Arc<Node>> = self.lock().values().map(Arc::clone).collect();
        for node in &nodes {
            node.advance_to(to_ns)?;
        }
        Ok(to_ns)
    }

    /// One `operational_events` row for a node lifecycle fact (§5, migration 2).
    fn event(&self, desk_id: &str, kind: &'static str, payload: serde_json::Value) {
        let desk_id = desk_id.to_owned();
        if let Err(e) = self
            .store
            .unit(move |tx| crate::desk::append_event(tx, kind, Some(&desk_id), now_ns(), payload))
        {
            tracing::error!(kind, error = %e, "could not append node lifecycle event");
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Node>>> {
        self.nodes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The CN half of node start, after `trade::restore` and before the feed is let
/// through (feature SPEC `a-share-engine` §5.3): terminate every restored CN
/// order that has outlived its trading day, re-establish the per-instrument
/// session gate the sandbox lost with its matching engines, and discard the
/// temporal baselines.
///
/// The cancel is the same `TradingCommand::CancelOrder` [`trade::cancel`] sends,
/// stamped by the node clock, and it goes out before any quote exists — which is
/// what makes it beat the first crossing observation (F3 (1a), (2)).
fn reconcile_cn(desk_id: &str, node: &Node, market: &MarketState) -> Result<(), NodeError> {
    let now_ns = node.now_ns()?;
    #[cfg(test)]
    let assume_calendar = calendared(desk_id);
    #[cfg(not(test))]
    let _ = desk_id;
    let feed = market
        .hithink()
        .map_or(AShareFeed::Yahoo, |hithink| hithink.a_share_feed());
    let expired: Vec<ClientOrderId> = node.call(move |context| {
        let orders: Vec<nautilus_model::orders::OrderAny> = context
            .cache
            .borrow()
            .orders_open(None, None, None, None, None)
            .into_iter()
            .filter(|order| {
                catalog::find(order.instrument_id().to_string().as_str())
                    .is_some_and(|entry| entry.market == Market::Cn)
                    && cn::expired(order, now_ns)
            })
            .map(|order| order.cloned())
            .collect();
        orders
            .iter()
            .map(|order| {
                trade::cancel_on_node(context.trader_id, order, now_ns);
                order.client_order_id()
            })
            .collect()
    })?;

    for client_order_id in expired {
        settle_closed(node, client_order_id)?;
    }

    // The session gate is per-`OrderMatchingEngine` in-memory state that no
    // snapshot carries (F3 (8)), so every CN instrument is re-gated on every
    // start, whatever the phase. Readiness starts unavailable under *either*
    // feed (feature SPEC §2.1, AE-7): Yahoo CN needs the same confirmed
    // calendar, so only the first successful cycle can open it.
    node.call(move |context| {
        {
            let mut exec = context.cn.borrow_mut();
            exec.reset();
            exec.feed = feed;
            #[cfg(test)]
            {
                exec.assume_calendar = assume_calendar;
            }
            cn::block(&mut exec);
            for entry in feed::cn_entries() {
                cn::republish_status(&mut exec, entry, now_ns);
            }
        }
        // The boundary alert is armed on the node's own clock, so 11:30, 13:00
        // and 14:57 act without a quote (feature SPEC §5.1).
        cn::arm_session_alert(context);
    })?;
    Ok(())
}

/// Waits for one order to reach a terminal state, reading the node's cache
/// between the runner's turns — [`trade::settle`]'s shape, for the one cancel
/// recovery issues itself.
fn settle_closed(node: &Node, client_order_id: ClientOrderId) -> Result<(), NodeError> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let closed = node.call(move |context| {
            context
                .cache
                .borrow()
                .order(&client_order_id)
                .is_none_or(|order| order.is_closed())
        })?;
        if closed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(NodeError::new(format!(
                "the paper book did not terminate the expired order {client_order_id} in time"
            )));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

/// The node's thread: build it here, run it here, never move it (root §12.1).
fn node_thread(
    desk_id: String,
    store: Store,
    market: Arc<MarketState>,
    feed_base: Option<FeedBase>,
    ready: mpsc::Sender<Result<LiveNodeHandle, String>>,
    jobs: UnboundedReceiver<Job>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = ready.send(Err(format!("no runtime for the node thread: {e}")));
            return;
        }
    };
    // A `LocalSet` because the node's cache and message bus are `Rc`-shaped: the
    // polling task and the job loop are its neighbours on this one thread.
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let (mut node, cn) = match build(&desk_id, feed_base, market) {
            Ok(built) => built,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        let context = NodeContext {
            cache: node.kernel().cache(),
            clock: node.kernel().clock(),
            portfolio: Rc::clone(&node.kernel().portfolio),
            trader_id: node.trader_id(),
            cn,
        };
        // The message bus is thread-local, so durable capture is subscribed here,
        // on the node thread, and sees exactly this desk's events (§5).
        trade::install_capture(desk_id.clone(), store, Rc::clone(&context.cache));
        let mut jobs = jobs;
        tokio::task::spawn_local(async move {
            while let Some(job) = jobs.recv().await {
                job(&context);
            }
        });
        if ready.send(Ok(node.handle())).is_err() {
            return;
        }
        // Hosted: the daemon owns SIGINT and the shutdown bound (root §4.6), so
        // the node never installs a signal handler of its own.
        if let Err(e) = node.run_with_mode(NodeRunMode::Hosted).await {
            tracing::error!(desk_id, error = %e, "trading node run loop ended");
        }
    });
}

/// Node start in the §4.3 order: assert precision, build, load the catalog into
/// the cache. The data client is registered on the builder and subscribes the
/// catalog when its engine starts it, on this thread; restoration closes the
/// sequence from [`Registry::start`], once the node is running.
fn build(
    desk_id: &str,
    feed_base: Option<FeedBase>,
    market: Arc<MarketState>,
) -> Result<(LiveNode, Exec), String> {
    assert_precision();

    // Shared by the data client's polling tasks and by every job that runs
    // through `Node::call`: one node, one CN execution state.
    let cn: Exec = Rc::new(RefCell::new(CnExec::new()));

    let trader_id = TraderId::from(format!("MARKETRIG-{desk_id}").as_str());
    let mut logging = LoggerConfig::from_spec("stdout=Off;fileout=Off;is_colored=false")
        .map_err(|e| e.to_string())?;
    // MarketRig's own tracing subscriber is the daemon's diagnostics (per D51);
    // the kernel's logger stays installed but silent.
    logging.bypass_logging = true;

    let mut builder = LiveNode::builder(trader_id, Environment::Sandbox)
        .map_err(|e| e.to_string())?
        .with_name(format!("marketrig-{desk_id}"))
        .with_load_state(false)
        .with_save_state(false)
        .with_logging(logging)
        // Restoration is MarketRig's own, from `book_snapshots` (per D64): there
        // is no venue to reconcile against.
        .with_reconciliation(false)
        .with_timeout_connection(CONNECT_TIMEOUT_SECS)
        .with_delay_post_stop_secs(0)
        .add_data_client(
            Some(DATA_CLIENT.to_owned()),
            Box::new(ChartDataClientFactory),
            Box::new(ChartDataClientConfig {
                feed_base,
                market,
                cn: Rc::clone(&cn),
            }),
        )
        .map_err(|e| e.to_string())?;

    // The controlled-clock seam: only a desk the environment or a module check
    // named gets one; every other desk takes the ordinary `LiveClock` branches
    // below unchanged. `ClockFactory` memoizes the kernel clock and calls the
    // closure again per component clock, so returning clones of one `Rc` makes
    // kernel, components and sandbox share one instance.
    let controlled = controlled_clock(desk_id);
    if let Some(clock) = controlled.clone() {
        builder = builder.with_clock_factory(move || clock.clone() as Rc<RefCell<dyn Clock>>);
    }

    for (venue, market_key) in venues() {
        let exec_factory: Box<dyn SimulatedExecutionClientFactory> = match controlled.clone() {
            Some(clock) => Box::new(ControlledSandboxFactory(clock)),
            None => Box::new(SandboxExecutionClientFactory::new()),
        };
        builder = builder
            .add_simulated_exec_client(
                Some(venue.to_string()),
                exec_factory,
                Box::new(sandbox_config(trader_id, desk_id, venue, market_key)),
            )
            .map_err(|e| e.to_string())?;
    }

    let node = builder.build().map_err(|e| e.to_string())?;
    load_catalog(&node)?;
    Ok((node, cn))
}

/// The catalog as NautilusTrader instruments in the node's cache (§3, §4.3). The
/// per-market fee rate rides each instrument's own fee fields, which the sandbox's
/// explicitly configured maker-taker model then charges (§4.1, R1-4).
fn load_catalog(node: &LiveNode) -> Result<(), String> {
    let cache = node.kernel().cache();
    let ts = UnixNanos::from(now_ns() as u64);
    let mut cache = cache.borrow_mut();
    for entry in catalog::ENTRIES {
        cache
            .add_instrument(equity(entry, ts))
            .map_err(|e| format!("{}: {e}", entry.instrument_id))?;
    }
    Ok(())
}

fn equity(entry: &Entry, ts: UnixNanos) -> InstrumentAny {
    let instrument_id = InstrumentId::from(entry.instrument_id);
    let price_increment = Price::from(entry.price_increment);
    let fee = fee_rate(entry.market);
    InstrumentAny::Equity(Equity::new(
        instrument_id,
        instrument_id.symbol,
        None,
        currency(entry.market),
        price_increment.precision,
        price_increment,
        Some(Quantity::from(entry.lot_size)),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(fee),
        Some(fee),
        None,
        None,
        ts,
        ts,
    ))
}

/// One sandbox execution client per venue (root §12.1): the fee model is set
/// **explicitly**, never left to a default, and the account is a multi-currency
/// cash account with netting positions (§4.1, R1-4).
///
/// ponytail: NautilusTrader resolves a venue's account through the account id's
/// issuer, so a desk's book is one account per venue rather than one account
/// outright; each market's R1-4 seed is split evenly across its venues so the
/// desk-wide totals match the declared figures exactly (the CN 1,000,000 CNY is
/// 500,000 at XSHG and 500,000 at XSHE). A sufficiency refusal is therefore
/// per-venue, which the sandbox's own reason exposes. The upgrade path is one
/// shared account per market if a cross-venue balance ever has to exist.
fn sandbox_config(
    trader_id: TraderId,
    desk_id: &str,
    venue: Venue,
    market: Market,
) -> SandboxExecutionClientConfig {
    SandboxExecutionClientConfig::builder()
        .trader_id(trader_id)
        .account_id(AccountId::from(format!("{venue}-{desk_id}").as_str()))
        .venue(venue)
        .starting_balances(vec![seed(market)])
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Cash)
        .book_type(BookType::L1_MBP)
        .fee_model(FeeModelAny::MakerTaker(MakerTakerFeeModel))
        // The desk's book moves on quotes alone: the feed carries neither trades
        // nor bars (§2.1).
        .bar_execution(false)
        .trade_execution(false)
        .build()
}

/// The venues the catalog spans, first appearance first, each with its market.
fn venues() -> Vec<(Venue, Market)> {
    let mut venues: Vec<(Venue, Market)> = Vec::new();
    for entry in catalog::ENTRIES {
        let venue = InstrumentId::from(entry.instrument_id).venue;
        if !venues.iter().any(|(known, _)| *known == venue) {
            venues.push((venue, entry.market));
        }
    }
    venues
}

/// One venue account's opening cash: the market's R1-4 seed divided evenly by
/// how many catalog venues the market spans, so desk-wide totals are exactly
/// the declared 100,000 USD / 1,000,000 HKD / 1,000,000 CNY.
fn seed(market: Market) -> Money {
    let total = match market {
        Market::Us => 100_000,
        Market::Hk => 1_000_000,
        Market::Cn => 1_000_000,
    };
    let split = venues().iter().filter(|(_, m)| *m == market).count() as i64;
    Money::from(format!("{} {}", total / split, currency(market).code).as_str())
}

/// The declared per-side fee rate by market (§4.1, R1-4): US 0 bp, Hong Kong
/// 11 bp, China A-share 3 bp.
fn fee_rate(market: Market) -> Decimal {
    match market {
        Market::Us => Decimal::ZERO,
        Market::Hk => Decimal::new(11, 4),
        Market::Cn => Decimal::new(3, 4),
    }
}

fn currency(market: Market) -> Currency {
    match market {
        Market::Us => Currency::USD(),
        Market::Hk => Currency::HKD(),
        Market::Cn => Currency::CNY(),
    }
}

// ---------------------------------------------------------------------------
// The data client and its polling task (§2.1, §4.1)
// ---------------------------------------------------------------------------

/// What the node hands its data client: the feed base for this run, and the
/// installation-wide market state every accepted observation advances.
#[derive(Debug)]
struct ChartDataClientConfig {
    feed_base: Option<FeedBase>,
    market: Arc<MarketState>,
    /// The node's CN execution state, whose release latch holds this client's
    /// first publish until recovery has decided (§5.3).
    cn: Exec,
}

impl ClientConfig for ChartDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
struct ChartDataClientFactory;

impl DataClientFactory for ChartDataClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
        clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<ChartDataClientConfig>()
            .ok_or_else(|| anyhow::anyhow!("{name} needs a ChartDataClientConfig"))?;
        let chart = match &config.feed_base {
            Some(base) => Some(ChartClient::new(base.url.clone()).map_err(|e| anyhow::anyhow!(e))?),
            None => None,
        };
        Ok(Box::new(ChartDataClient {
            client_id: ClientId::from(name),
            chart,
            assume_open: config.feed_base.as_ref().is_some_and(|base| base.standin),
            market: Arc::clone(&config.market),
            cache,
            clock,
            cn: Rc::clone(&config.cn),
            subscribed: false,
            connected: false,
        }))
    }

    fn name(&self) -> &str {
        DATA_CLIENT
    }

    fn config_type(&self) -> &str {
        "ChartDataClientConfig"
    }
}

/// MarketRig's own out-of-tree `DataClient` (root §12.1): it subscribes the whole
/// catalog when its engine starts it and publishes through the same data-event
/// sender the shipped venue adapters use.
struct ChartDataClient {
    client_id: ClientId,
    /// `None` keeps this desk off the feed entirely (§10.1): the node still runs,
    /// and every quote stays `UNAVAILABLE`.
    chart: Option<ChartClient>,
    /// A stand-in feed lifts the calendar gate on cadence (§10.1, R1-9): the gate
    /// must tick at any wall-clock hour. Observations still label the real phase.
    ///
    /// ponytail: the test seam therefore polls on a cadence the real feed would
    /// not — a stand-in run never exercises the CLOSED branch of [`next_delay`].
    /// The upgrade path is a controllable clock behind [`phase`] if the gate ever
    /// needs the real phase transitions rather than a bypass.
    assume_open: bool,
    market: Arc<MarketState>,
    cache: CacheView,
    /// The node's own clock — the same instance the sandbox stamps its events
    /// with — which is what a `CN` receipt is stamped by (§5.1).
    clock: Rc<RefCell<dyn Clock>>,
    /// The release latch and the CN execution state (§5.3).
    cn: Exec,
    /// The catalog is subscribed once, when the data engine starts this client.
    subscribed: bool,
    connected: bool,
}

impl std::fmt::Debug for ChartDataClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChartDataClient")
            .field("client_id", &self.client_id)
            .field("subscribed", &self.subscribed)
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait(?Send)]
impl DataClient for ChartDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// The catalog spans four venues, so this client is bound to none of them.
    fn venue(&self) -> Option<Venue> {
        None
    }

    /// Subscribes the catalog (§4.3). The engine calls this on the node thread,
    /// which is where the data-event sender must be taken: it is a thread-local
    /// that panics anywhere else (root §12.1, per D76). A clone of it moves into
    /// each instrument's polling task.
    fn start(&mut self) -> anyhow::Result<()> {
        if self.subscribed {
            return Ok(());
        }
        self.subscribed = true;
        let Some(chart) = self.chart.clone() else {
            tracing::info!("no feed configured; the desk's quotes stay unavailable");
            return Ok(());
        };
        let sender = get_data_event_sender();
        for entry in catalog::ENTRIES {
            // The CN leg is one task for the whole market (feature SPEC
            // `hithink-a-share` §2.2), because a HiThink cycle is one batched
            // request; US and HK keep R1's task per instrument.
            if entry.market == Market::Cn {
                continue;
            }
            tokio::task::spawn_local(poll(
                entry,
                chart.clone(),
                Arc::clone(&self.market),
                self.cache.clone(),
                sender.clone(),
                self.assume_open,
                Rc::clone(&self.cn),
            ));
        }
        tokio::task::spawn_local(poll_cn(
            chart,
            Arc::clone(&self.market),
            self.cache.clone(),
            sender,
            self.assume_open,
            Rc::clone(&self.cn),
            Rc::clone(&self.clock),
        ));
        Ok(())
    }

    /// The polling tasks live and die with the node thread's runtime.
    fn stop(&mut self) -> anyhow::Result<()> {
        self.subscribed = false;
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// There is no session to open — the client polls a stateless endpoint — but
    /// the node's startup and shutdown both wait on this flag, so it is answered
    /// honestly rather than left at the trait's no-op default.
    async fn connect(&mut self) -> anyhow::Result<()> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn is_disconnected(&self) -> bool {
        !self.connected
    }

    /// The catalog is subscribed whole at start, so a per-instrument request is
    /// already satisfied (§3).
    fn subscribe_quotes(&mut self, _cmd: SubscribeQuotes) -> anyhow::Result<()> {
        Ok(())
    }
}

/// One instrument's polling loop (§2.1): once at subscription whatever the phase,
/// then on the cadence [`next_delay`] yields.
async fn poll(
    entry: &'static Entry,
    chart: ChartClient,
    market: Arc<MarketState>,
    cache: CacheView,
    sender: UnboundedSender<DataEvent>,
    assume_open: bool,
    cn: Exec,
) {
    released(&cn).await;
    let instrument_id = InstrumentId::from(entry.instrument_id);
    if poll_once(entry, instrument_id, &chart, &market, &sender)
        .await
        .is_err()
    {
        return;
    }
    loop {
        let cadence_phase = if assume_open {
            Phase::Open
        } else {
            phase(entry.market, now_ns())
        };
        match next_delay(cadence_phase, exposed(&cache, &instrument_id)) {
            Some(delay) => {
                tokio::time::sleep(delay).await;
                if poll_once(entry, instrument_id, &chart, &market, &sender)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // `CLOSED`: nothing is polled, the calendar is re-consulted.
            None => tokio::time::sleep(IDLE_INTERVAL).await,
        }
    }
}

/// The whole `CN` leg's polling loop (feature SPEC `hithink-a-share` §2.2, per
/// HT-2): once at subscription whatever the phase, then on R1's cadence, reading
/// the operator's `a_share_feed` at the top of every cycle so a switch takes
/// effect on the next one with no node restart.
///
/// ponytail: one task and one exposure flag for the whole leg, where R1 gave
/// each instrument its own — the tier collapses to "exposed if any `CN`
/// instrument is exposed", because one HiThink cycle is one request for all of
/// them. The upgrade path is a per-instrument tier map narrowing the batch.
async fn poll_cn(
    chart: ChartClient,
    market: Arc<MarketState>,
    cache: CacheView,
    sender: UnboundedSender<DataEvent>,
    assume_open: bool,
    cn: Exec,
    clock: Rc<RefCell<dyn Clock>>,
) {
    released(&cn).await;
    let entries: Vec<(&'static Entry, InstrumentId)> = feed::cn_entries()
        .map(|entry| (entry, InstrumentId::from(entry.instrument_id)))
        .collect();
    if cn_cycle(&entries, &chart, &market, &sender, &clock, &cache, &cn)
        .await
        .is_err()
    {
        return;
    }
    loop {
        // A HiThink stand-in lifts the calendar gate on cadence exactly as the
        // quote stand-in does (§10.1, feature SPEC `hithink-a-share` §3).
        let standin = assume_open || market.hithink().is_some_and(|h| h.standin);
        let cadence_phase = match (standin, market.hithink()) {
            (true, _) => Phase::Open,
            (false, Some(hithink)) => hithink.cn_phase(now_ns()).0,
            (false, None) => phase(Market::Cn, now_ns()),
        };
        let any_exposed = entries.iter().any(|(_, id)| exposed(&cache, id));
        match next_delay(cadence_phase, any_exposed) {
            Some(delay) => {
                tokio::time::sleep(delay).await;
                if cn_cycle(&entries, &chart, &market, &sender, &clock, &cache, &cn)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            None => tokio::time::sleep(IDLE_INTERVAL).await,
        }
    }
}

/// One `CN` cycle: HiThink's one batched request when the operator chose it and
/// the key still holds, R1's per-instrument Yahoo poll otherwise (§2.2).
///
/// Under HiThink the cycle is also the AE-9 execution loop (`a-share-engine`
/// SPEC §2.5): each accepted item goes through [`cn::CnExec::observe`], whose
/// [`cn::Plan`] is what the node is told — an idle book for an observation that
/// qualifies nothing, a one-sided crossing book for the resting orders it
/// releases, and the instrument's session gate. Yahoo CN keeps R1's synthesized
/// two-sided quote and only gains that gate (§2.1: explicitly simplified).
async fn cn_cycle(
    entries: &[(&'static Entry, InstrumentId)],
    chart: &ChartClient,
    market: &MarketState,
    sender: &UnboundedSender<DataEvent>,
    clock: &Rc<RefCell<dyn Clock>>,
    cache: &CacheView,
    cn: &Exec,
) -> Result<(), ()> {
    let now = || clock.borrow().timestamp_ns().as_u64();
    let list: Vec<&'static Entry> = entries.iter().map(|(entry, _)| *entry).collect();
    let hithink = market.hithink().cloned();
    let feed = hithink
        .as_ref()
        .map_or(AShareFeed::Yahoo, |hithink| hithink.a_share_feed());
    // A switch blocks execution and clears the stale executable state before the
    // new provider's first observation is trusted (§5.3, F5).
    if cn.borrow().feed != feed {
        cn::switch_provider(cn, &list, feed, now());
    }

    // The trading-day list is the *provider's* answer, not the feed's: it gates
    // CN execution under Yahoo exactly as under HiThink (§2.1, AE-7), so it is
    // refreshed before the feed branch, whichever one this cycle takes.
    if let Some(hithink) = &hithink {
        hithink.refresh_calendar_if_due(now() as i64).await;
    }

    let hithink = match hithink.filter(|_| feed == AShareFeed::Hithink) {
        Some(hithink) => hithink,
        None => {
            // Yahoo CN is explicitly simplified, but it is not ungated: the
            // confirmed trading day still decides (§2.1). A desk with no
            // provider at all has no calendar to consult and stays
            // `NO_CALENDAR` — awareness keeps working, execution does not.
            let day = market
                .hithink()
                .map_or(Err(crate::hithink::Reason::NoCalendar), |hithink| {
                    hithink.trading_day(now() as i64)
                });
            for (entry, instrument_id) in entries {
                let at = now();
                {
                    let mut exec = cn.borrow_mut();
                    let day = exec.calendar_verdict(day);
                    let today = cn::shanghai_date(at);
                    let inst = exec.inst(*instrument_id);
                    inst.readiness = day;
                    inst.ready_date = day.is_ok().then_some(today);
                    cn::gate(&mut exec, entry, at);
                }
                poll_once(entry, *instrument_id, chart, market, sender).await?;
            }
            return Ok(());
        }
    };

    // A key rejected mid-run leaves the leg degraded with its last HiThink
    // observation standing; the daemon never switches to Yahoo on its own.
    if !hithink.feed_ready() {
        for entry in &list {
            market.mark_degraded(entry.instrument_id);
        }
        cn::feed_failed(cn, &list, crate::hithink::Reason::FeedLost, now());
        return Ok(());
    }
    let observed = match feed::poll_hithink_observed(&hithink, market).await {
        Ok(observed) => observed,
        Err(reason) => {
            // §2.1: a failed snapshot blocks affected submissions *and* resting
            // fills; recovery re-baselines before anything can match.
            cn::feed_failed(cn, &list, reason, now());
            return Ok(());
        }
    };
    let day = hithink.trading_day(now() as i64);
    for item in observed {
        let evidence = match day {
            Ok(()) => hithink.bar_evidence(item.entry, now() as i64).await,
            Err(reason) => Err(reason),
        };
        // The resting set is read after every await and immediately before the
        // rule runs, so an order admitted in between is baselined, not released.
        let at = now();
        let plan = {
            let resting = cn::resting_limits(
                &cache.borrow(),
                InstrumentId::from(item.entry.instrument_id),
            );
            cn.borrow_mut().observe(&item, evidence, at, &resting)
        };
        cn::execute(item.entry, plan, cache, cn, at).await;
    }
    Ok(())
}

/// Holds a polling task until `Registry::start` has restored the book,
/// terminated the expired CN orders and re-established the session gate (§5.3,
/// F3 (1b): the first poll otherwise beats restoration and fills a prior-day
/// order against data the daemon never had a chance to gate).
///
/// ponytail: a 10 ms poll on a latch rather than a `Notify`, because the latch is
/// set exactly once per node and the wait is a startup cost nobody measures. The
/// upgrade path is a `tokio::sync::Notify` if a node ever re-arms it.
async fn released(cn: &Exec) {
    while !cn.borrow().released {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// One poll. `Err` means the node is gone and the task should end; a feed failure
/// is not an error here, it is degraded health (§2.1).
async fn poll_once(
    entry: &'static Entry,
    instrument_id: InstrumentId,
    chart: &ChartClient,
    market: &MarketState,
    sender: &UnboundedSender<DataEvent>,
) -> Result<(), ()> {
    match chart.fetch(entry.yahoo_symbol).await {
        Ok(quote) => {
            let received_at_ns = now_ns();
            market.accept(entry, &quote, received_at_ns);
            let last = feed::at_precision(quote.price, entry.price_increment);
            let tick = synthesized(
                entry,
                instrument_id,
                &last,
                quote.source_time_ns,
                received_at_ns,
            );
            sender
                .send(DataEvent::Data(Data::Quote(tick)))
                .map_err(|_| ())
        }
        Err(e) => {
            market.mark_degraded(entry.instrument_id);
            tracing::warn!(instrument_id = entry.instrument_id, "feed poll failed: {e}");
            Ok(())
        }
    }
}

/// The synthesized book (§4.1, per D76): both sides the last observation at the
/// instrument's own precision, both sizes one lot. Precision comes from the
/// catalog tick and never from a formatting choice, because the sandbox silently
/// drops a quote whose precision disagrees with its instrument.
fn synthesized(
    entry: &Entry,
    instrument_id: InstrumentId,
    last: &str,
    ts_event_ns: i64,
    received_at_ns: i64,
) -> QuoteTick {
    let last = Price::from(last);
    let lot = Quantity::from(entry.lot_size);
    QuoteTick::new(
        instrument_id,
        last,
        last,
        lot,
        lot,
        UnixNanos::from(ts_event_ns.max(0) as u64),
        UnixNanos::from(received_at_ns.max(0) as u64),
    )
}

/// The desk's exposure to one instrument — an open order or a nonflat position —
/// which is what moves it to the tightened cadence (§2.1, R1-1). Read straight
/// off the node's own cache, on the node's own thread.
fn exposed(cache: &CacheView, instrument_id: &InstrumentId) -> bool {
    let cache = cache.borrow();
    cache.orders_open_count(None, Some(instrument_id), None, None, None) > 0
        || cache.positions_open_count(None, Some(instrument_id), None, None, None) > 0
}

// ---------------------------------------------------------------------------
// node::precision_asserted, node::sender_on_node_thread (feature SPEC §11)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[test]
fn precision_asserted() {
    // The pinned crates must report the default 64-bit mode. This fails the
    // moment a `high-precision` feature reaches the graph through unification —
    // the D76 landmine the workspace's `default-features = false` guards against.
    assert_eq!(HIGH_PRECISION_MODE, 0);
    assert_eq!(PRECISION_BYTES, 8);

    // The same assertion the node runs, and it runs it first: `build` calls this
    // before it touches a builder, so the node started by
    // `sender_on_node_thread` observed it.
    assert_precision();
}

/// Names a desk registered for controlled time in-process. Dropping it
/// unregisters the desk, so a later test reusing the same store never inherits
/// controlled time.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ClockHandle {
    desk_id: String,
}

#[cfg(test)]
impl ClockHandle {
    pub(crate) fn desk_id(&self) -> &str {
        &self.desk_id
    }
}

#[cfg(test)]
impl Drop for ClockHandle {
    fn drop(&mut self) {
        unregister_controlled(&self.desk_id);
    }
}

/// A registry whose `desk_name` desk runs on a `TestClock` seeded at `start_ns`
/// — the in-process form of the [`TEST_CLOCK_ENV`] seam, so one module check
/// controls one desk. The desk row is seeded here; the node starts on the first
/// [`Registry::ensure`].
/// Its desk also assumes today's confirmed trading calendar, because nothing in
/// a module check could install one: see [`cn::CnExec::assume_calendar`]. Use
/// [`controlled_registry_uncalendared`] for the AE-7 checks.
#[cfg(test)]
pub(crate) fn controlled_registry(
    store: &Store,
    feed_base: Option<FeedBase>,
    desk_name: &'static str,
    start_ns: u64,
) -> (Registry, ClockHandle) {
    let (registry, handle) =
        controlled_registry_uncalendared(store, feed_base, desk_name, start_ns);
    CALENDARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(handle.desk_id.clone());
    (registry, handle)
}

/// The same desk with the production calendar rule: CN execution is unavailable
/// until a confirmed same-day trading calendar opens it (§2.1, AE-7).
#[cfg(test)]
pub(crate) fn controlled_registry_uncalendared(
    store: &Store,
    feed_base: Option<FeedBase>,
    desk_name: &'static str,
    start_ns: u64,
) -> (Registry, ClockHandle) {
    let desk_id = seeded_desk(store, desk_name);
    register_controlled(&desk_id, start_ns);
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), feed_base);
    (registry, ClockHandle { desk_id })
}

/// The desks [`controlled_registry`] speaks for. A restart builds a fresh
/// `Registry` over the same desk id, so the flag lives with the desk.
#[cfg(test)]
static CALENDARED: LazyLock<Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

#[cfg(test)]
fn calendared(desk_id: &str) -> bool {
    CALENDARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(desk_id)
}

/// [`Node::advance_to`], panicking — the spike's own vocabulary.
#[cfg(test)]
pub(crate) fn advance(node: &Node, to_ns: u64) -> Vec<String> {
    node.advance_to(to_ns).expect("the node answers")
}

/// [`Node::now_ns`], panicking.
#[cfg(test)]
pub(crate) fn now_ns_of(node: &Node) -> u64 {
    node.now_ns().expect("the node answers")
}

/// A desk row a node can be started against (the `operational_events` foreign
/// key), plus its UUID.
#[cfg(test)]
pub(crate) fn seeded_desk(store: &Store, name: &'static str) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    let row = id.clone();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES (?1, ?2, 'READY', ?3, 1000, 2000)",
                rusqlite::params![row, name, format!("/desks/{name}")],
            )
        })
        .unwrap();
    id
}

#[cfg(test)]
fn events(store: &Store, desk_id: &str) -> Vec<String> {
    let desk = desk_id.to_owned();
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT kind FROM operational_events WHERE desk_id = ?1 \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([desk], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
}

/// Polls `check` until it holds or the bound expires.
#[cfg(test)]
#[track_caller]
pub(crate) fn within(seconds: u64, what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("{what} did not happen within {seconds}s");
}

#[cfg(test)]
#[test]
fn sender_on_node_thread() {
    let aapl = catalog::find("AAPL.XNAS").unwrap();
    let aapl_id = InstrumentId::from(aapl.instrument_id);
    let (_dir, store) = crate::store::open_temp();

    // A local server speaking the chart shape; every instrument's first poll is
    // answered from it, so nothing ever reaches the public endpoint.
    let (base, hits, _) = feed::scripted_server(vec![(
        200,
        feed::chart_body("AAPL", "USD", "316.85", 1_788_206_401),
    )]);

    let desk = seeded_desk(&store, "alpha");
    let registry = Registry::new(
        store.clone(),
        Arc::new(MarketState::new()),
        Some(FeedBase::standin(base.clone())),
    );
    let node = registry.ensure(&desk).expect("the node starts");
    assert_eq!(events(&store, &desk), ["TRADING_NODE_STARTED"]);

    // The sender was taken on the node thread (it panics anywhere else) and a
    // clone reached the polling task: the tick it published landed in the node's
    // own cache, which only the run loop can put it in.
    within(10, "the published tick reaches the node's cache", || {
        node.call(move |context| context.cache.borrow().quote(&aapl_id).is_some())
            .unwrap()
    });
    let tick = node
        .call(move |context| context.cache.borrow().quote(&aapl_id).copied())
        .unwrap()
        .expect("the cached quote");
    assert_eq!(tick.bid_price, tick.ask_price, "the book is synthesized");
    assert_eq!(tick.bid_price, Price::from("316.85"));
    assert_eq!(tick.bid_size, Quantity::from(aapl.lot_size));
    assert_eq!(tick.ask_size, tick.bid_size);

    // And the same accepted observation advanced the installation-wide state.
    let read = registry.market().read(aapl, now_ns());
    assert_eq!(read.sequence, 1);
    assert_eq!(read.last.as_deref(), Some("316.85"));
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);

    // The catalog is in the node's cache under its own precision.
    use nautilus_model::instruments::Instrument;
    let loaded = node
        .call(move |context| context.cache.borrow().instrument(&aapl_id).cloned())
        .unwrap()
        .expect("the catalog is loaded");
    assert_eq!(loaded.price_precision(), 2);
    assert_eq!(loaded.maker_fee(), Decimal::ZERO);

    registry.stop_all();

    // No feed at all (`MARKETRIG_TEST_NO_TRADING`, §10.1): the node still starts,
    // nothing is polled, and the quote stays unavailable.
    let dark = Registry::new(store.clone(), Arc::new(MarketState::new()), None);
    let desk = seeded_desk(&store, "beta");
    let node = dark.ensure(&desk).expect("a node starts without a feed");
    assert_eq!(events(&store, &desk), ["TRADING_NODE_STARTED"]);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        dark.market().read(aapl, now_ns()).health,
        crate::feed::Health::Unavailable
    );
    assert!(
        node.call(move |context| context.cache.borrow().quote(&aapl_id).is_none())
            .unwrap()
    );
    dark.stop_all();

    // A start that fails is evidenced and retryable (§4.3): a book snapshot
    // stamped with a payload version this build does not know stops the node
    // rather than trading a book it cannot account for, and the desk gets its
    // node once the obstacle is gone.
    let desk = seeded_desk(&store, "gamma");
    let snapshot = desk.clone();
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO book_snapshots VALUES (?1, 99, '{}', 3000)",
                [snapshot],
            )
        })
        .unwrap();
    let error = dark.ensure(&desk).expect_err("an unrestorable book stops");
    assert_eq!(error.code(), "MARKET_UNAVAILABLE");
    assert_eq!(events(&store, &desk), ["TRADING_NODE_FAILED"]);

    let cleared = desk.clone();
    store
        .unit(move |tx| tx.execute("DELETE FROM book_snapshots WHERE desk_id = ?1", [cleared]))
        .unwrap();
    dark.ensure(&desk).expect("the next call retries the start");
    assert_eq!(
        events(&store, &desk),
        ["TRADING_NODE_FAILED", "TRADING_NODE_STARTED"]
    );
    dark.stop_all();
}

// ---------------------------------------------------------------------------
// node::clock_seam_is_gated_by_the_data_root, node::the_first_publish_waits_for_recovery,
// node::a_prior_day_cn_order_is_terminated_at_start,
// node::a_same_day_restart_keeps_the_order_resting
// (feature SPEC `a-share-engine` §5.1, §5.3, §6)
// ---------------------------------------------------------------------------

/// 2026-09-09 09:35:00 Asia/Shanghai, inside the CN morning session.
#[cfg(test)]
pub(crate) const CN_0935: u64 = 1_788_917_700_000_000_000;
#[cfg(test)]
pub(crate) const SECOND_NS: u64 = 1_000_000_000;
#[cfg(test)]
pub(crate) const DAY_NS: u64 = 86_400 * SECOND_NS;

/// The one CN instrument the CN checks trade.
#[cfg(test)]
pub(crate) fn moutai() -> &'static Entry {
    catalog::find("600519.XSHG").expect("the CN catalog entry")
}

/// Publishes one book through the production publisher, on the node thread.
#[cfg(test)]
pub(crate) fn publish(
    node: &Node,
    entry: &'static Entry,
    bid: (&str, u32),
    ask: (&str, u32),
    ts_ns: u64,
) {
    let bid = (bid.0.parse::<Decimal>().unwrap(), Decimal::from(bid.1));
    let ask = (ask.0.parse::<Decimal>().unwrap(), Decimal::from(ask.1));
    node.call(move |_| cn::publish_quote(entry, bid, ask, ts_ns))
        .expect("the node answers");
    within(10, "the published book reaches the node", || {
        node.call(move |context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from(entry.instrument_id))
                .is_some_and(|quote| quote.ts_event.as_u64() == ts_ns)
        })
        .unwrap()
    });
}

#[cfg(test)]
pub(crate) fn order_status(node: &Node, client_order_id: &str) -> Option<String> {
    let id = ClientOrderId::from(client_order_id);
    node.call(move |context| {
        context
            .cache
            .borrow()
            .order(&id)
            .map(|order| order.status().to_string())
    })
    .expect("the node answers")
}

/// The desk's captured event kinds for one client order id, oldest first.
#[cfg(test)]
pub(crate) fn kinds(store: &Store, desk_id: &str, client_order_id: &str) -> Vec<String> {
    let (desk, order) = (desk_id.to_owned(), client_order_id.to_owned());
    store
        .call(move |conn| {
            conn.prepare(
                "SELECT kind FROM order_events WHERE desk_id = ?1 AND client_order_id = ?2 \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map(rusqlite::params![desk, order], |r| r.get(0))?
            .collect()
        })
        .expect("the order events read")
}

#[cfg(test)]
pub(crate) fn fill_count(store: &Store) -> i64 {
    store
        .call(|conn| conn.query_row("SELECT count(*) FROM fills", [], |r| r.get(0)))
        .expect("the fills count")
}

/// The venue account's balance as `total|locked|free`, and how many entries its
/// per-instrument lock map holds — the reservation accounting a terminal event
/// has to move (R1).
#[cfg(test)]
pub(crate) fn reservation(node: &Node, venue: &str, desk_id: &str) -> (String, usize) {
    let account_id = AccountId::from(format!("{venue}-{desk_id}").as_str());
    node.call(move |context| {
        let cache = context.cache.borrow();
        let account = cache.account(&account_id).expect("the venue account");
        let balance = account
            .balances()
            .values()
            .copied()
            .next()
            .expect("one balance");
        let locked = match &*account {
            nautilus_model::accounts::AccountAny::Cash(cash) => cash.balances_locked.len(),
            other => panic!("the sandbox account is a cash account, got {other:?}"),
        };
        (
            format!("{}|{}|{}", balance.total, balance.locked, balance.free),
            locked,
        )
    })
    .expect("the node answers")
}

/// Rests one CN limit buy through the production submit path.
#[cfg(test)]
pub(crate) fn rest_buy(
    store: &Store,
    registry: &Registry,
    desk_id: &str,
    action_id: &str,
    price: &str,
) {
    let body = format!(
        r#"{{"action_id":"{action_id}","instrument_id":"600519.XSHG",
            "side":"BUY","type":"LIMIT","quantity":"100","price":"{price}"}}"#
    );
    let (record, _) =
        trade::submit(store, registry, desk_id, &body, &trade::Source::Session).expect("accepted");
    assert_eq!(
        record.outcome.clone().unwrap()["status"],
        "ACCEPTED",
        "{action_id} must rest, not fill"
    );
}

/// A second registry over the same store, whose node's clock starts at
/// `start_ns` — a restart at a chosen instant, which is what a day-lifetime
/// decision has to be judged at.
#[cfg(test)]
pub(crate) fn restart_at(store: &Store, desk_id: &str, start_ns: u64) -> (Registry, Arc<Node>) {
    register_controlled(desk_id, start_ns);
    let registry = Registry::new(store.clone(), Arc::new(MarketState::new()), None);
    let node = registry.ensure(desk_id).expect("the node restores");
    (registry, node)
}

#[cfg(test)]
#[test]
fn clock_seam_is_gated_by_the_data_root() {
    use std::ffi::OsStr;
    let root = OsStr::new("/tmp/scratch");
    assert_eq!(
        resolve_clock_start(Some(root), Some("1788917700000000000")),
        Some(CN_0935)
    );
    assert_eq!(
        resolve_clock_start(None, Some("1788917700000000000")),
        None,
        "the instant alone never controls a node"
    );
    assert_eq!(resolve_clock_start(Some(root), None), None);
    assert_eq!(resolve_clock_start(Some(root), Some("soon")), None);
    assert_eq!(TEST_CLOCK_ENV, "MARKETRIG_TEST_CLOCK_NS");
}

/// The clock seam on a **real** node: the kernel and the sandbox share the
/// injected `TestClock`, the daemon advances it, and every stamp NautilusTrader
/// puts on an order — which is what `order_events.occurred_at_ns` is — is the
/// injected instant (feature SPEC §6; F0).
#[cfg(test)]
#[test]
fn clock_seam_drives_node() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "clock-seam", CN_0935);
    let node = registry.ensure(handle.desk_id()).expect("the node starts");
    assert_eq!(node.now_ns().unwrap(), CN_0935);

    // Advancing is the daemon's call, and the events it releases are dispatched
    // on the node thread. The sandbox's own expiry-sweep timer fires, which is
    // the proof that the sandbox shares this clock instance.
    let at = CN_0935 + 60 * SECOND_NS;
    let fired = node.advance_to(at).expect("the controlled clock advances");
    assert_eq!(node.now_ns().unwrap(), at);
    assert!(
        fired.iter().any(|t| t.ends_with("-sandbox-expiry-sweep")),
        "{fired:?}"
    );

    publish(&node, moutai(), ("1700.00", 100), ("1700.00", 100), at);
    let (record, _) = trade::submit(
        &store,
        &registry,
        handle.desk_id(),
        r#"{"action_id":"clock-buy-1","instrument_id":"600519.XSHG",
            "side":"BUY","type":"MARKET","quantity":"100","price":null}"#,
        &trade::Source::Session,
    )
    .expect("the market buy is accepted");
    assert_eq!(record.outcome.clone().unwrap()["status"], "FILLED");

    let stamps: Vec<i64> = store
        .call(|conn| {
            conn.prepare("SELECT occurred_at_ns FROM order_events")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .unwrap();
    assert!(
        !stamps.is_empty() && stamps.iter().all(|ns| *ns == at as i64),
        "every stored order event carries the injected instant {at}: {stamps:?}"
    );
    registry.stop_all();
}

/// §5.3's first half: the feed publishes nothing until restoration has decided.
/// Without the latch the first poll wins the race and fills the restored order
/// against data the daemon never gated (F3 (1b), 3 restarts out of 3).
#[cfg(test)]
#[test]
fn the_first_publish_waits_for_recovery() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "cn-hold", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let node = registry.ensure(&desk_id).expect("the node starts");
    publish(&node, moutai(), ("1700.00", 100), ("1700.00", 100), CN_0935);
    rest_buy(&store, &registry, &desk_id, "cn-hold-1", "1600.00");
    registry.stop_all();

    // The restart's feed answers a crossing 1500.00 to whatever it is asked.
    let (base, _hits, _requests) = feed::scripted_server(vec![(
        200,
        feed::chart_body("600519.SS", "CNY", "1500.00", 1_788_917_700),
    )]);
    register_controlled(&desk_id, CN_0935);
    let registry = Registry::new(
        store.clone(),
        Arc::new(MarketState::new()),
        Some(FeedBase::standin(base)),
    );
    let node = registry.ensure(&desk_id).expect("the node restores");

    // `ensure` returns with the book restored and no market data at all: the
    // poller was still holding its first publish.
    assert_eq!(
        order_status(&node, "cn-hold-1").as_deref(),
        Some("ACCEPTED")
    );
    assert_eq!(fill_count(&store), 0, "nothing filled during recovery");

    // And the feed does resume: the same crossing quote lands afterwards.
    within(10, "the released feed publishes", || {
        node.call(|context| {
            context
                .cache
                .borrow()
                .quote(&InstrumentId::from(moutai().instrument_id))
                .is_some()
        })
        .unwrap()
    });
    registry.stop_all();
}

/// §5.3: a restored CN order that has outlived its trading day is terminated
/// exactly once, under its original identifier, before any quote exists — and
/// its reservation is released (R1 S1, S4).
#[cfg(test)]
#[test]
fn a_prior_day_cn_order_is_terminated_at_start() {
    for (name, restart_ns) in [
        ("cn-nextday", CN_0935 + DAY_NS),
        ("cn-past-deadline", CN_0935 + 21_300 * SECOND_NS),
    ] {
        let (_dir, store) = crate::store::open_temp();
        let (registry, handle) = controlled_registry(&store, None, name, CN_0935);
        let desk_id = handle.desk_id().to_owned();
        let node = registry.ensure(&desk_id).expect("the node starts");
        publish(&node, moutai(), ("1700.00", 100), ("1700.00", 100), CN_0935);
        rest_buy(&store, &registry, &desk_id, "cn-expired-1", "1600.00");
        assert_eq!(
            reservation(&node, "XSHG", &desk_id).0,
            "500000.00 CNY|160000.00 CNY|340000.00 CNY"
        );
        registry.stop_all();

        let (registry, node) = restart_at(&store, &desk_id, restart_ns);
        assert_eq!(
            order_status(&node, "cn-expired-1").as_deref(),
            Some("CANCELED"),
            "{name}: recovery terminated the expired order before returning"
        );
        assert_eq!(
            kinds(&store, &desk_id, "cn-expired-1"),
            [
                "OrderInitialized",
                "OrderSubmitted",
                "OrderAccepted",
                "OrderCanceled"
            ],
            "{name}: exactly one terminal event under the original id"
        );
        assert_eq!(
            reservation(&node, "XSHG", &desk_id),
            ("500000.00 CNY|0.00 CNY|500000.00 CNY".to_owned(), 0),
            "{name}: the reservation is released"
        );

        // The crossing quote the restart was racing fills nothing now.
        publish(
            &node,
            moutai(),
            ("1500.00", 100),
            ("1500.00", 100),
            restart_ns + SECOND_NS,
        );
        assert_eq!(fill_count(&store), 0, "{name}");
        let history = trade::history_orders(&store, &desk_id).expect("the history reads");
        assert_eq!(history.len(), 1, "{name}: {history:?}");
        assert_eq!(history[0]["status"], "CANCELED");
        registry.stop_all();

        // And a second restart is idempotent: nothing to cancel, nothing added.
        let (registry, node) = restart_at(&store, &desk_id, restart_ns + 60 * SECOND_NS);
        assert_eq!(
            kinds(&store, &desk_id, "cn-expired-1").len(),
            4,
            "{name}: no duplicate terminal event"
        );
        assert_eq!(reservation(&node, "XSHG", &desk_id).1, 0, "{name}");
        assert_eq!(
            trade::history_orders(&store, &desk_id)
                .expect("the history reads")
                .len(),
            1,
            "{name}: one chain"
        );
        registry.stop_all();
    }
}

/// §5.1: the same trading day's order survives a lunch-break restart. It is not
/// terminated, it keeps resting, and once the session is open again it fills on
/// a crossing quote under its own identifier.
#[cfg(test)]
#[test]
fn a_same_day_restart_keeps_the_order_resting() {
    let (_dir, store) = crate::store::open_temp();
    let (registry, handle) = controlled_registry(&store, None, "cn-lunch", CN_0935);
    let desk_id = handle.desk_id().to_owned();
    let node = registry.ensure(&desk_id).expect("the node starts");
    publish(&node, moutai(), ("1700.00", 100), ("1700.00", 100), CN_0935);
    rest_buy(&store, &registry, &desk_id, "cn-lunch-1", "1600.00");
    registry.stop_all();

    // 12:10, inside the lunch break: the day is not over, so nothing expires,
    // and the session gate comes back closed.
    let lunch = CN_0935 + 9_300 * SECOND_NS;
    let (registry, node) = restart_at(&store, &desk_id, lunch);
    assert_eq!(
        order_status(&node, "cn-lunch-1").as_deref(),
        Some("ACCEPTED")
    );
    assert_eq!(
        node.call(|context| {
            context
                .cache
                .borrow()
                .instrument_status(&InstrumentId::from(moutai().instrument_id))
                .map(|cached| cached.action)
        })
        .unwrap(),
        Some(nautilus_model::enums::MarketStatusAction::Pause),
        "the lunch restart re-gates the instrument paused — the day is not over"
    );
    assert_eq!(
        reservation(&node, "XSHG", &desk_id),
        ("500000.00 CNY|160000.00 CNY|340000.00 CNY".to_owned(), 1),
        "the reservation came back and was rebuilt"
    );

    // 13:05, the afternoon session. Reopening the gate is the poller's job; the
    // order then fills on the first crossing quote, once, under its own id.
    let afternoon = CN_0935 + 12_600 * SECOND_NS;
    node.advance_to(afternoon).expect("the clock advances");
    node.call(move |context| {
        let ts = context
            .cn
            .borrow_mut()
            .stamp(InstrumentId::from(moutai().instrument_id), afternoon);
        cn::publish_status(moutai(), cn::status_for(afternoon, true), ts);
    })
    .unwrap();
    publish(
        &node,
        moutai(),
        ("1500.00", 100),
        ("1500.00", 100),
        afternoon + SECOND_NS,
    );
    within(10, "the resting order fills", || {
        order_status(&node, "cn-lunch-1").as_deref() == Some("FILLED")
    });
    assert_eq!(fill_count(&store), 1);
    assert_eq!(
        kinds(&store, &desk_id, "cn-lunch-1"),
        [
            "OrderInitialized",
            "OrderSubmitted",
            "OrderAccepted",
            "OrderFilled"
        ],
        "restoration replayed nothing"
    );
    registry.stop_all();
}
