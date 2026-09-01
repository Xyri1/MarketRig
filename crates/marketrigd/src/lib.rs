pub mod api;
pub mod catalog;
pub mod daemon;
pub mod desk;
pub mod feed;
pub mod log;
pub mod store;

use std::process::ExitCode;
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
    match serve(&startup) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("serve failed: {e}");
            eprintln!("error: INTERNAL: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<daemon::Startup, (&'static str, String)> {
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

fn serve(startup: &daemon::Startup) -> std::io::Result<()> {
    let std_listener = startup.listener.try_clone()?;
    std_listener.set_nonblocking(true)?;
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
        // §4.2: bounded end to end at 5 seconds, after which the daemon exits anyway.
        let _ = tokio::time::timeout(Duration::from_secs(5), serving).await;
        startup.shutdown()
    })
}
