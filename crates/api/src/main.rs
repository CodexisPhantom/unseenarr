use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `api openapi <file>` writes the OpenAPI document and exits (`just gen`).
    let args: Vec<String> = std::env::args().collect();
    if let [_, command, file] = args.as_slice()
        && command == "openapi"
    {
        std::fs::write(file, api::openapi().to_pretty_json()? + "\n")?;
        return Ok(());
    }

    let data_dir = PathBuf::from(std::env::var("UNSEENARR_DATA_DIR").unwrap_or("/config".into()));
    std::fs::create_dir_all(&data_dir)?;
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let log_level =
        api::logs::parse_level_filter(std::env::var("UNSEENARR_LOG_LEVEL").ok().as_deref());
    let log_format =
        api::logs::parse_log_format(std::env::var("UNSEENARR_LOG_FORMAT").ok().as_deref());
    api::logs::init_tracing(logs_dir.clone(), log_level, log_format);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "unseenarr starting");

    let db = db::connect(&data_dir).await?;
    tracing::info!(data_dir = %data_dir.display(), "database ready");

    // Files adopted before the parser existed have quality `Unknown`.
    let backfill = db::backfill::run(&db).await?;
    if backfill.updated > 0 {
        tracing::info!(files = backfill.updated, "quality set on adopted files");
    }
    for path in &backfill.unparsed {
        tracing::warn!(%path, "quality of adopted file not parsed, stays Unknown");
    }

    let secret = api::auth::load_secret(std::env::var("UNSEENARR_SECRET").ok(), &data_dir)?;
    let base_url = std::env::var("UNSEENARR_BASE_URL").ok();
    let state = api::AppState::new(db, &secret, base_url.as_deref())
        .await?
        .with_logs_dir(logs_dir);
    api::logs::prune_expired_on_startup(&state).await;
    api::downloads::reconcile_on_startup(&state).await;
    api::downloads::spawn(state.clone());

    let bind = std::env::var("UNSEENARR_BIND").unwrap_or("0.0.0.0:7979".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(
        listener,
        api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    tracing::info!("stopped");
    Ok(())
}

/// Resolves on Ctrl+C, and on SIGTERM (what `docker stop` sends). The process
/// is PID 1 in the container, where SIGTERM does nothing without a handler.
async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
