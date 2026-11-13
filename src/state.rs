use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use rand::RngExt;
use serde_json::json;

use crate::config::Config;
use crate::db::Db;
use crate::docker::ContainerLive;
use crate::host::{HostAccess, NetworkSample, NetworkSnapshot, ServiceLive};
use crate::render::Renderer;
use crate::util;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub docker: Option<bollard::Docker>,
    pub host: HostAccess,
    pub live: Arc<tokio::sync::RwLock<LiveSnapshot>>,
    pub sessions: Sessions,
    pub renderer: Arc<Renderer>,
    pub docker_startup_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LiveSnapshot {
    pub system: Option<crate::db::SystemSample>,
    pub history: Vec<crate::db::SystemSample>,
    pub containers: Vec<ContainerLive>,
    pub services: Vec<ServiceLive>,
    pub services_ok: bool,
    pub services_error: Option<String>,
    pub network: Option<NetworkSnapshot>,
    pub network_history: Vec<NetworkSample>,
    pub network_ok: bool,
    pub network_error: Option<String>,
    pub docker_ok: bool,
    pub docker_error: Option<String>,
    pub last_update: i64,
    pub hostname: String,
    pub os: String,
    pub cpu_count: usize,
    pub uptime_secs: u64,
    pub disk_path: String,
}

#[derive(Clone)]
pub struct Sessions {
    inner: Arc<Mutex<HashMap<String, i64>>>,
    ttl_secs: i64,
}

impl Sessions {
    pub fn new(hours: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl_secs: hours.max(1) * 3_600,
        }
    }

    pub fn create(&self) -> String {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut sessions = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = util::now();
        sessions.retain(|_, expiry| *expiry > now);
        sessions.insert(token.clone(), now + self.ttl_secs);
        token
    }

    pub fn validate(&self, token: &str) -> bool {
        let mut sessions = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = util::now();
        match sessions.get(token).copied() {
            Some(expiry) if expiry > now => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn revoke(&self, token: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token);
    }

    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }
}

pub async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let token = cookie_value(request.headers(), "workamon_session");
    if let Some(token) = token {
        if state.sessions.validate(&token) {
            return next.run(request).await;
        }
    }

    if request.uri().path().starts_with("/api/") {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim() == name {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

pub fn session_cookie(token: &str, max_age_secs: i64) -> String {
    format!(
        "workamon_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        max_age_secs.max(0)
    )
}

pub fn clear_session_cookie() -> String {
    "workamon_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0".into()
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}
