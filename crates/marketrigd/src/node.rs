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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nautilus_common::cache::{Cache, CacheView};
use nautilus_common::clients::DataClient;
use nautilus_common::clock::Clock;
use nautilus_common::enums::Environment;
use nautilus_common::factories::{ClientConfig, DataClientFactory};
use nautilus_common::live::runner::get_data_event_sender;
use nautilus_common::logging::logger::LoggerConfig;
use nautilus_common::messages::DataEvent;
use nautilus_common::messages::data::SubscribeQuotes;
use nautilus_core::UnixNanos;
use nautilus_execution::models::fee::{FeeModelAny, MakerTakerFeeModel};
use nautilus_live::node::{LiveNode, LiveNodeHandle, NodeRunMode};
use nautilus_model::data::{Data, QuoteTick};
use nautilus_model::enums::{AccountType, BookType, OmsType};
use nautilus_model::identifiers::{AccountId, ClientId, InstrumentId, TraderId, Venue};
use nautilus_model::instruments::{Equity, InstrumentAny};
use nautilus_model::types::fixed::{HIGH_PRECISION_MODE, PRECISION_BYTES};
use nautilus_model::types::{Currency, Money, Price, Quantity};
use nautilus_sandbox::{SandboxExecutionClientConfig, SandboxExecutionClientFactory};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::catalog::{self, Entry, Market};
use crate::feed::{
    self, ChartClient, FeedBase, IDLE_INTERVAL, MarketState, Phase, next_delay, phase,
};
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
    pub trader_id: TraderId,
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
        Ok(node)
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
        let mut node = match build(&desk_id, feed_base, market) {
            Ok(node) => node,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        let context = NodeContext {
            cache: node.kernel().cache(),
            clock: node.kernel().clock(),
            trader_id: node.trader_id(),
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
) -> Result<LiveNode, String> {
    assert_precision();

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
            Box::new(ChartDataClientConfig { feed_base, market }),
        )
        .map_err(|e| e.to_string())?;

    for (venue, market_key) in venues() {
        builder = builder
            .add_simulated_exec_client(
                Some(venue.to_string()),
                Box::new(SandboxExecutionClientFactory::new()),
                Box::new(sandbox_config(trader_id, desk_id, venue, market_key)),
            )
            .map_err(|e| e.to_string())?;
    }

    let node = builder.build().map_err(|e| e.to_string())?;
    load_catalog(&node)?;
    Ok(node)
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
        _clock: Rc<RefCell<dyn Clock>>,
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
    assume_open: bool,
    market: Arc<MarketState>,
    cache: CacheView,
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
            tokio::task::spawn_local(poll(
                entry,
                chart.clone(),
                Arc::clone(&self.market),
                self.cache.clone(),
                sender.clone(),
                self.assume_open,
            ));
        }
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
) {
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
            let tick = synthesized(entry, instrument_id, &quote, received_at_ns);
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
    quote: &feed::ChartQuote,
    received_at_ns: i64,
) -> QuoteTick {
    let last = Price::from(feed::at_precision(quote.price, entry.price_increment));
    let lot = Quantity::from(entry.lot_size);
    QuoteTick::new(
        instrument_id,
        last,
        last,
        lot,
        lot,
        UnixNanos::from(quote.source_time_ns.max(0) as u64),
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
    let (base, hits) = feed::scripted_server(vec![(
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
