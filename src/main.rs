mod config;
mod db;
mod docker;
mod logging;
mod monitor;
mod render;
mod state;
mod util;
mod web;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::config::{Config, VERSION};
use crate::db::Db;
use crate::render::Renderer;
use crate::state::{AppState, LiveSnapshot, Sessions};

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::from_env().context("konfigurasi tidak valid")?;
    cfg.validate_db_dir()?;

    let db = Db::open(&cfg.db_path, cfg.db_max_bytes)
        .with_context(|| format!("gagal menyiapkan database {}", cfg.db_path.display()))?;

    let log_receiver = logging::init("workamon=info,tower_http=warn,warn");
    let log_task = tokio::spawn(logging::run_writer(db.clone(), log_receiver));

    let renderer = Renderer::new().context("gagal memuat template")?;

    let (docker, docker_startup_error) = docker::connect(cfg.docker_enabled).await;
    match &docker_startup_error {
        Some(message) => warn!(target: "workamon::docker", "{message}"),
        None => info!(target: "workamon::docker", "terhubung ke Docker daemon"),
    }

    let started_at = util::now();
    let disk_path = cfg.resolved_host_root().display().to_string();
    let addr = cfg.addr.clone();
    let session_hours = cfg.session_hours;

    let state = AppState {
        cfg: Arc::new(cfg),
        db: db.clone(),
        docker,
        live: Arc::new(tokio::sync::RwLock::new(LiveSnapshot {
            last_update: started_at,
            docker_ok: false,
            docker_error: docker_startup_error.clone(),
            disk_path,
            ..Default::default()
        })),
        sessions: Sessions::new(session_hours),
        renderer: Arc::new(renderer),
        docker_startup_error,
    };

    if state.cfg.uses_default_password() {
        warn!(
            target: "workamon::auth",
            "password default masih dipakai; set WORKAMON_PASSWORD sebelum membuka akses dari jaringan"
        );
    }

    info!(
        target: "workamon::startup",
        "Workamon v{VERSION} berjalan di http://{addr} (database {}, batas {} MB)",
        state.cfg.db_path.display(),
        state.cfg.db_max_mb()
    );

    tokio::spawn(monitor::run(state.clone()));

    let app = web::router(state.clone());
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("gagal membuka alamat {addr}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server berhenti karena kesalahan")?;

    info!(target: "workamon::startup", "Workamon berhenti");
    log_task.abort();
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(target: "workamon::startup", "gagal memasang handler SIGTERM: {error}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
