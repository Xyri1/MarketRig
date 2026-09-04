pub mod api;
pub mod catalog;
pub mod claude;
pub mod codex;
pub mod daemon;
pub mod desk;
pub mod dispatch;
pub mod exec;
pub mod feed;
pub mod log;
pub mod memory;
pub mod node;
pub mod runtime;
pub mod schedule;
pub mod session;
pub mod store;
pub mod terminal;
pub mod trade;
pub mod trigger;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// The daemon entry point: §4.1 startup, serve the §6 routes, §4.2 shutdown.
pub fn run() -> ExitCode {
    let mut startup = match start() {
        Ok(s) => s,
        Err((code, message)) => {
            eprintln!("error: {code}: {message}");
            return ExitCode::FAILURE;
        }
    };
    // The feed's seam variables are read once here and passed down, like the data
    // roots (R1 feature SPEC §10.1).
    let feed_base = feed::feed_base_from_env();
    match serve(&mut startup, feed_base) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("serve failed: {e}");
            eprintln!("error: INTERNAL: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<daemon::Startup, (&'static str, String)> {
    // The precision the whole build depends on, asserted before the daemon does
    // anything at all — a node start is far too late to learn a `high-precision`
    // feature reached the graph (per D39, R1-4).
    node::assert_precision();
    let roots = store::Roots::from_env().map_err(|e| ("INTERNAL", e.to_string()))?;
    roots
        .create_dirs()
        .map_err(|e| ("INTERNAL", e.to_string()))?;
    log::init(&roots.logs).map_err(|e| ("INTERNAL", e.to_string()))?;
    daemon::start(roots).map_err(|e| {
        tracing::error!(code = e.code(), "startup failed: {e}");
        (e.code(), e.to_string())
    })
}

fn serve(startup: &mut daemon::Startup, feed_base: Option<feed::FeedBase>) -> std::io::Result<()> {
    // The credential store is named once, before any route can reach it (R4
    // feature SPEC §3, per R4-2). Under the test seam the store is a file in
    // the relocated root, so the platform one is never touched.
    let memory = Arc::new(memory::Memory::new(
        startup.store.clone(),
        startup.roots.clone(),
    )?);
    if !memory.seam {
        memory::set_platform_store();
    }
    let std_listener = startup
        .listener
        .take()
        .ok_or_else(|| std::io::Error::other("the listener was already taken"))?;
    std_listener.set_nonblocking(true)?;
    let registry = Arc::new(node::Registry::new(
        startup.store.clone(),
        Arc::new(feed::MarketState::new()),
        feed_base,
    ));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let (quit_tx, mut quit_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (shut_tx, shut_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::select! {
                _ = quit_rx.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            let _ = shut_tx.send(true);
        });
        // R2 feature SPEC §3.1 and §4.3: the scheduler and the executor wake
        // on mutations and acceptances, and both stop on the shutdown watch.
        let scheduler_wake = Arc::new(tokio::sync::Notify::new());
        let exec_wake = Arc::new(tokio::sync::Notify::new());
        // The terminal manager both adapters spawn through (R3 feature SPEC §3)
        // and the dispatcher's two evidence streams (§6.2).
        let (terminals, terminal_exits) = terminal::Manager::new();
        let (adapter_events, adapter_rx) = tokio::sync::mpsc::unbounded_channel();
        let channels = Arc::new(claude::Channels::default());
        let search_path = runtime::search_path();
        let test_data_root =
            std::env::var_os(store::TEST_DATA_ROOT_ENV).map(std::path::PathBuf::from);
        let mcp_path = claude::sibling("marketrig-mcp")
            .unwrap_or_else(|| std::path::PathBuf::from("marketrig-mcp"));
        let codex = codex::Codex::new(
            startup.store.clone(),
            startup.roots.clone(),
            terminals.clone(),
            adapter_events.clone(),
            search_path.clone(),
            mcp_path,
            test_data_root,
            startup.daemon_uuid.clone(),
        );
        let dispatcher = dispatch::Dispatcher::new(
            startup.store.clone(),
            dispatch::Adapters {
                codex: Arc::new(codex.clone()),
                claude: Arc::new(claude::Claude::new(
                    startup.store.clone(),
                    daemon::launch_dir(&startup.roots),
                    search_path.clone(),
                    terminals.clone(),
                    channels.clone(),
                    adapter_events,
                )),
            },
            startup.daemon_uuid.clone(),
        );
        let router = api::router(api::ApiState {
            store: startup.store.clone(),
            desks_home: startup.roots.desks.clone(),
            daemon_uuid: startup.daemon_uuid.clone(),
            credential: startup.credential.clone(),
            started_at_ns: startup.started_at_ns,
            quit: quit_tx,
            registry: registry.clone(),
            scheduler_wake: scheduler_wake.clone(),
            search_path,
            terminals: terminals.clone(),
            channels,
            dispatch: dispatcher.clone(),
            memory: memory.clone(),
        });
        let mut graceful = shut_rx.clone();
        let serving = tokio::spawn(
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = graceful.changed().await;
                })
                .into_future(),
        );
        let scheduler = tokio::spawn(schedule::run(
            startup.store.clone(),
            startup.started_at_ns,
            scheduler_wake,
            exec_wake.clone(),
            shut_rx.clone(),
        ));
        let executor = tokio::spawn(exec::run(
            startup.store.clone(),
            startup.roots.clone(),
            startup.daemon_uuid.clone(),
            exec_wake,
            shut_rx.clone(),
        ));
        let dispatcher_task = tokio::spawn(dispatch::run(
            dispatcher.clone(),
            adapter_rx,
            terminal_exits,
            shut_rx.clone(),
        ));
        let mut began = shut_rx.clone();
        let _ = began.changed().await;
        // §4.2: bounded end to end at 5 seconds, after which the daemon exits
        // anyway. The desks' trading nodes stop inside that same budget.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let _ = tokio::time::timeout_at(deadline, serving).await;
        // The executor returns only after every running group is terminated
        // and recorded `QUIT` (R2 feature SPEC §4.4).
        let _ = tokio::time::timeout_at(deadline, executor).await;
        let _ = tokio::time::timeout_at(deadline, scheduler).await;
        // The dispatcher stops before the terminals do, so their exits are the
        // Quit path's to record, not an activation failure's (§6.2, §7).
        let _ = tokio::time::timeout_at(deadline, dispatcher_task).await;
        // Terminals first, then the app-server, then the desks' trading nodes
        // (§4.1, §4.2); every row that was still open ends `QUIT`.
        let _ = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || terminals.shutdown_all()),
        )
        .await;
        let _ = tokio::time::timeout_at(deadline, codex.stop()).await;
        // The memory child goes after the terminals and the app-server, inside
        // the same bound (R4 feature SPEC §2.3).
        let _ = tokio::time::timeout_at(deadline, memory.stop_child()).await;
        let _ = dispatcher.quit_rows();
        let _ = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || registry.stop_all()),
        )
        .await;
        startup.shutdown()
    })
}
