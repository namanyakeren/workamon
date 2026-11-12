use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_PASSWORD: &str = "workamon";

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: String,
    pub username: String,
    pub password: String,
    pub db_path: PathBuf,
    pub db_max_bytes: u64,
    pub sample_secs: u64,
    pub container_sample_secs: u64,
    pub host_root: PathBuf,
    pub docker_enabled: bool,
    pub session_hours: i64,
    pub log_retention_days: i64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let addr = env::var("WORKAMON_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let username = env::var("WORKAMON_USER").unwrap_or_else(|_| "admin".into());
        let password = env::var("WORKAMON_PASSWORD").unwrap_or_else(|_| DEFAULT_PASSWORD.into());
        let db_path = PathBuf::from(
            env::var("WORKAMON_DB").unwrap_or_else(|_| "data/workamon.db".into()),
        );
        let db_max_mb = env::var("WORKAMON_DB_MAX_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10)
            .clamp(1, 512);
        let sample_secs = env::var("WORKAMON_SAMPLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(5, 3600);
        let container_sample_secs = env::var("WORKAMON_CONTAINER_SAMPLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300)
            .clamp(sample_secs, 86_400);
        let host_root = PathBuf::from(
            env::var("WORKAMON_HOST_ROOT").unwrap_or_else(|_| "/".into()),
        );
        let docker_enabled = env::var("WORKAMON_DOCKER")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"))
            .unwrap_or(true);
        let session_hours = env::var("WORKAMON_SESSION_HOURS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(168)
            .clamp(1, 24 * 365);
        let log_retention_days = env::var("WORKAMON_LOG_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(14)
            .clamp(1, 3650);

        if username.trim().is_empty() {
            anyhow::bail!("WORKAMON_USER tidak boleh kosong");
        }
        if password.is_empty() {
            anyhow::bail!("WORKAMON_PASSWORD tidak boleh kosong");
        }

        Ok(Self {
            addr,
            username,
            password,
            db_path,
            db_max_bytes: db_max_mb * 1024 * 1024,
            sample_secs,
            container_sample_secs,
            host_root,
            docker_enabled,
            session_hours,
            log_retention_days,
        })
    }

    pub fn db_max_mb(&self) -> u64 {
        self.db_max_bytes / (1024 * 1024)
    }

    /// Folder host yang benar-benar ada. Di container biasanya `/host`, saat
    /// dijalankan langsung di Raspberry Pi cukup `/`.
    pub fn resolved_host_root(&self) -> PathBuf {
        if self.host_root.is_dir() {
            self.host_root.clone()
        } else {
            PathBuf::from("/")
        }
    }

    pub fn uses_default_password(&self) -> bool {
        self.password == DEFAULT_PASSWORD
    }

    pub fn validate_db_dir(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("gagal membuat folder database {}", parent.display()))?;
        }
        Ok(())
    }
}
