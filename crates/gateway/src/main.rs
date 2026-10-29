use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listener");
    let state = pennyrouter_gateway::AppState::from_env();
    // Catalog freshness (Iter 10): reload the snapshot periodically when
    // routing is configured. The task dies with the process at shutdown.
    if let Some(router) = state.router_handle() {
        let interval = pennyrouter_gateway::reload_interval_from_env();
        if !interval.is_zero() {
            eprintln!("gateway: catalog reload every {}s", interval.as_secs());
            pennyrouter_gateway::spawn_reload_task(router, interval, state.metrics_handle());
        }
    }
    axum::serve(listener, pennyrouter_gateway::app_with_state(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve");
}

/// Wait for Ctrl-C or SIGTERM (Iter 9): in-flight requests drain before
/// the process exits, so deploys stop dropping them mid-flight.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install sigterm handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
