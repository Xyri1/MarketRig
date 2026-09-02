pub mod api;
pub mod catalog;
pub mod daemon;
pub mod desk;
pub mod feed;
pub mod log;
pub mod node;
pub mod schedule;
pub mod store;
pub mod trade;
pub mod trigger;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// The daemon entry point: §4.1 startup, serve the §6 routes, §4.2 shutdown.
pub fn run() -> ExitCode {
    let startup = match start() {
        Ok(s) => s,
        Err((code, message)) => {
            eprintln!("error: {code}: {message}");
            return ExitCode::FAILURE;
        }
    };
    // The feed's seam variables are read once here and passed down, like the data
    // roots (R1 feature SPEC §10.1).
    let feed_base = feed::feed_base_from_env();
    match serve(&startup, feed_base) {
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

fn serve(startup: &daemon::Startup, feed_base: Option<feed::FeedBase>) -> std::io::Result<()> {
    let std_listener = startup.listener.try_clone()?;
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
        let router = api::router(api::ApiState {
            store: startup.store.clone(),
            desks_home: startup.roots.desks.clone(),
            daemon_uuid: startup.daemon_uuid.clone(),
            credential: startup.credential.clone(),
            started_at_ns: startup.started_at_ns,
            quit: quit_tx,
            registry: registry.clone(),
        });
        let mut graceful = shut_rx.clone();
        let serving = tokio::spawn(
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = graceful.changed().await;
                })
                .into_future(),
        );
        let mut began = shut_rx.clone();
        let _ = began.changed().await;
        // §4.2: bounded end to end at 5 seconds, after which the daemon exits
        // anyway. The desks' trading nodes stop inside that same budget.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let _ = tokio::time::timeout_at(deadline, serving).await;
        let _ = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || registry.stop_all()),
        )
        .await;
        startup.shutdown()
    })
}
