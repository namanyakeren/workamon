use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::{error, info, warn};

use crate::config::{Config, VERSION};
use crate::db::{DbStats, LogRow, SystemSample};
use crate::docker::{self, ContainerLive, DfView};
use crate::render::line_chart;
use crate::state::{
    clear_session_cookie, constant_time_eq, cookie_value, require_auth, session_cookie, AppState,
    LiveSnapshot,
};
use crate::util;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/", get(dashboard))
        .route("/containers", get(containers_page))
        .route("/containers/{id}", get(container_page))
        .route("/containers/{id}/{action}", post(container_action))
        .route("/metrics", get(metrics_page))
        .route("/logs", get(logs_page))
        .route("/maintenance", get(maintenance_page))
        .route("/maintenance/prune/{kind}", post(prune_action))
        .route("/maintenance/db/{action}", post(db_action))
        .route("/api/live", get(api_live))
        .route("/api/containers/{id}/stats", get(api_container_stats))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/login", get(login_page).post(login_post))
        .route("/logout", post(logout))
        .route("/healthz", get(healthz))
        .route("/static/app.css", get(asset_css))
        .route("/static/app.js", get(asset_js))
        .route("/static/favicon.svg", get(asset_favicon))
        .merge(protected)
        .fallback(not_found)
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static(
                "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'",
            ),
        ))
        .with_state(state)
}

async fn asset_css() -> Response {
    asset(
        include_str!("../assets/app.css"),
        "text/css; charset=utf-8",
    )
}

async fn asset_js() -> Response {
    asset(
        include_str!("../assets/app.js"),
        "text/javascript; charset=utf-8",
    )
}

async fn asset_favicon() -> Response {
    asset(include_str!("../assets/favicon.svg"), "image/svg+xml")
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

async fn login_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, "workamon_session") {
        if state.sessions.validate(&token) {
            return Redirect::to("/").into_response();
        }
    }
    render_login(&state, None)
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login_post(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    let user_ok = constant_time_eq(&form.username, &state.cfg.username);
    let password_ok = constant_time_eq(&form.password, &state.cfg.password);
    if user_ok && password_ok {
        let token = state.sessions.create();
        info!(target: "workamon::auth", "login berhasil untuk user {}", state.cfg.username);
        let mut response = Redirect::to("/").into_response();
        if let Ok(value) =
            HeaderValue::from_str(&session_cookie(&token, state.sessions.ttl_secs()))
        {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        response
    } else {
        warn!(
            target: "workamon::auth",
            "login gagal untuk username {:?}",
            util::truncate_chars(&form.username, 64)
        );
        let mut response = render_login(&state, Some("Username atau password salah."));
        *response.status_mut() = StatusCode::UNAUTHORIZED;
        response
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, "workamon_session") {
        state.sessions.revoke(&token);
    }
    info!(target: "workamon::auth", "logout");
    let mut response = Redirect::to("/login").into_response();
    if let Ok(value) = HeaderValue::from_str(&clear_session_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn not_found(State(state): State<AppState>) -> Response {
    render_error(
        &state,
        StatusCode::NOT_FOUND,
        "Halaman tidak ditemukan",
        "Alamat yang diminta tidak tersedia. Periksa kembali tautan atau kembali ke ringkasan.",
    )
}

async fn healthz(State(state): State<AppState>) -> Response {
    match state.db.stats() {
        Ok(stats) => Json(json!({
            "status": "ok",
            "version": VERSION,
            "db_bytes": stats.size_bytes,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "error": format!("{error:#}") })),
        )
            .into_response(),
    }
}

fn render_login(state: &AppState, error: Option<&str>) -> Response {
    let context = json!({
        "error": error,
        "version": VERSION,
    });
    match state.renderer.render("login.html", &context) {
        Ok(html) => no_store(Html(html).into_response()),
        Err(error) => {
            error!(target: "workamon::web", "gagal render halaman login: {error:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Gagal memuat halaman login.").into_response()
        }
    }
}

fn render_page(
    state: &AppState,
    template: &str,
    active: &str,
    title: &str,
    extra: Value,
) -> Response {
    let mut map = match extra {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    map.insert("active".into(), json!(active));
    map.insert("page_title".into(), json!(title));
    map.insert("username".into(), json!(state.cfg.username));
    map.insert("version".into(), json!(VERSION));
    map.insert("db_max_mb".into(), json!(state.cfg.db_max_mb()));
    map.insert("default_creds".into(), json!(state.cfg.uses_default_password()));
    map.entry("docker_ok").or_insert(json!(false));
    map.entry("docker_error").or_insert(Value::Null);
    map.entry("flash").or_insert(Value::Null);

    let context = Value::Object(map);
    match state.renderer.render(template, &context) {
        Ok(html) => no_store(Html(html).into_response()),
        Err(error) => {
            error!(target: "workamon::web", "gagal render template {template}: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Terjadi kesalahan saat merender halaman.",
            )
                .into_response()
        }
    }
}

fn render_error(state: &AppState, status: StatusCode, title: &str, message: &str) -> Response {
    let context = json!({
        "status": status.as_u16(),
        "title": title,
        "message": message,
    });
    match state.renderer.render("error.html", &context) {
        Ok(html) => no_store((status, Html(html)).into_response()),
        Err(error) => {
            error!(target: "workamon::web", "gagal render halaman error: {error:#}");
            (status, format!("{title}: {message}")).into_response()
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Clone)]
struct Flash {
    kind: String,
    message: String,
}

fn flash_from_query(query: &HashMap<String, String>) -> Option<Flash> {
    let message = query.get("msg")?.clone();
    let kind = match query.get("kind").map(String::as_str) {
        Some("warn") => "warn",
        Some("error") => "error",
        _ => "ok",
    };
    Some(Flash {
        kind: kind.into(),
        message,
    })
}

fn flash_value(flash: Option<Flash>) -> Value {
    match flash {
        Some(flash) => json!({ "kind": flash.kind, "message": flash.message }),
        None => Value::Null,
    }
}

fn redirect_msg(path: &str, kind: &str, message: &str) -> Response {
    let separator = if path.contains('?') { '&' } else { '?' };
    Redirect::to(&format!(
        "{path}{separator}msg={}&kind={kind}",
        util::url_encode(message)
    ))
    .into_response()
}

async fn dashboard(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let live = state.live.read().await.clone();
    let history = state.db.recent_system(60).unwrap_or_default();
    let (logs, _) = state.db.logs(None, None, 1, 6).unwrap_or_default();

    let system = live.system.clone().unwrap_or_default();
    let counts = container_counts(&live.containers);
    let containers: Vec<Value> = live
        .containers
        .iter()
        .take(6)
        .map(container_view)
        .collect();
    let log_views: Vec<Value> = logs.iter().map(log_view).collect();

    let cpu_points: Vec<(i64, f64)> = history.iter().map(|row| (row.ts, row.cpu_pct)).collect();
    let mem_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, util::percent(row.mem_used, row.mem_total)))
        .collect();
    let disk_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, util::percent(row.disk_used, row.disk_total)))
        .collect();

    let context = json!({
        "system": system_view(&system, &live),
        "counts": counts,
        "containers": containers,
        "logs": log_views,
        "cpu_chart": line_chart(&cpu_points, 520, 150, Some(0.0), Some(100.0), "percent", "chart-cpu"),
        "mem_chart": line_chart(&mem_points, 520, 150, Some(0.0), Some(100.0), "percent", "chart-mem"),
        "disk_chart": line_chart(&disk_points, 1000, 150, Some(0.0), Some(100.0), "percent", "chart-disk"),
        "sample_count": history.len(),
        "has_data": live.system.is_some(),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(&state, "dashboard.html", "dashboard", "Ringkasan", context)
}

async fn containers_page(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let live = state.live.read().await.clone();
    let containers: Vec<Value> = live.containers.iter().map(container_view).collect();
    let context = json!({
        "containers": containers,
        "counts": container_counts(&live.containers),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "sample_secs": state.cfg.sample_secs,
        "container_sample_secs": state.cfg.container_sample_secs,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(&state, "containers.html", "containers", "Container", context)
}

async fn container_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(docker) = state.docker.clone() else {
        return render_error(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "Docker tidak tersedia",
            &state
                .docker_startup_error
                .clone()
                .unwrap_or_else(|| "Socket Docker tidak dapat diakses.".into()),
        );
    };

    let inspect = match tokio::time::timeout(Duration::from_secs(8), docker::inspect(&docker, &id))
        .await
    {
        Ok(Ok(inspect)) => inspect,
        Ok(Err(error)) => {
            return render_error(
                &state,
                StatusCode::NOT_FOUND,
                "Container tidak ditemukan",
                &format!("{error:#}"),
            )
        }
        Err(_) => {
            return render_error(
                &state,
                StatusCode::GATEWAY_TIMEOUT,
                "Docker terlalu lama merespons",
                "Permintaan inspect container melewati batas waktu.",
            )
        }
    };

    let live = state.live.read().await.clone();
    let container_id = inspect.id.clone().unwrap_or_else(|| id.clone());
    let name = inspect
        .name
        .clone()
        .unwrap_or_else(|| container_id.clone())
        .trim_start_matches('/')
        .to_string();
    let cached = live
        .containers
        .iter()
        .find(|container| container.id == container_id || container.name == name)
        .cloned();
    let live_container = cached.unwrap_or_else(|| fallback_container(&inspect, &name, &container_id));
    let container = container_view(&live_container);
    let info = inspect_view(&inspect, &name, &container_id, &live);

    let since = util::now() - 86_400;
    let history = state
        .db
        .container_history(&container_id, since, 300)
        .unwrap_or_default();
    let cpu_points: Vec<(i64, f64)> = history.iter().map(|row| (row.ts, row.cpu_pct)).collect();
    let mem_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, row.mem_used as f64))
        .collect();
    let mem_limit = history
        .iter()
        .map(|row| row.mem_limit)
        .max()
        .unwrap_or(0);

    let logs = match tokio::time::timeout(
        Duration::from_secs(8),
        docker::container_logs(&docker, &container_id, 150),
    )
    .await
    {
        Ok(Ok(logs)) => logs,
        Ok(Err(error)) => format!("Gagal mengambil log container: {error:#}"),
        Err(_) => "Log container tidak tersedia: permintaan melewati batas waktu.".into(),
    };

    let context = json!({
        "c": container,
        "info": info,
        "container_id": container_id,
        "logs": logs,
        "history_count": history.len(),
        "cpu_chart": line_chart(&cpu_points, 520, 150, Some(0.0), None, "percent", "chart-cpu"),
        "mem_chart": line_chart(
            &mem_points,
            520,
            150,
            Some(0.0),
            if mem_limit > 0 { Some(mem_limit as f64) } else { None },
            "bytes",
            "chart-mem",
        ),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        "container.html",
        "containers",
        &format!("Container {name}"),
        context,
    )
}

async fn metrics_page(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let range = parse_range(query.get("range").map(String::as_str));
    let since = util::now() - range.span_secs;
    let history = state
        .db
        .system_bucketed(since, range.bucket_secs)
        .unwrap_or_default();
    let live = state.live.read().await.clone();
    let db_stats = match state.db.stats() {
        Ok(stats) => stats,
        Err(error) => {
            error!(target: "workamon::db", "gagal membaca statistik database: {error:#}");
            DbStats {
                size_bytes: 0,
                max_bytes: state.cfg.db_max_bytes,
                system_rows: 0,
                container_rows: 0,
                log_rows: 0,
                oldest_ts: None,
                newest_ts: None,
            }
        }
    };

    let cpu_avg = average(history.iter().map(|row| row.cpu_pct)).unwrap_or(0.0);
    let cpu_max = history
        .iter()
        .map(|row| row.cpu_pct)
        .fold(0.0_f64, f64::max);
    let mem_values: Vec<f64> = history
        .iter()
        .map(|row| util::percent(row.mem_used, row.mem_total))
        .collect();
    let mem_avg = average(mem_values.iter().copied()).unwrap_or(0.0);
    let mem_max = mem_values.iter().copied().fold(0.0_f64, f64::max);
    let disk_now = history
        .last()
        .map(|row| util::percent(row.disk_used, row.disk_total))
        .unwrap_or_else(|| {
            live.system
                .as_ref()
                .map(|row| util::percent(row.disk_used, row.disk_total))
                .unwrap_or(0.0)
        });
    let disk_now_text = history
        .last()
        .map(|row| {
            format!(
                "{} / {}",
                util::fmt_bytes(row.disk_used),
                util::fmt_bytes(row.disk_total)
            )
        })
        .or_else(|| {
            live.system.as_ref().map(|row| {
                format!(
                    "{} / {}",
                    util::fmt_bytes(row.disk_used),
                    util::fmt_bytes(row.disk_total)
                )
            })
        })
        .unwrap_or_else(|| "—".into());
    let containers_now = history
        .last()
        .map(|row| row.containers_running)
        .or_else(|| live.system.as_ref().map(|row| row.containers_running))
        .unwrap_or(0);

    let cpu_points: Vec<(i64, f64)> = history.iter().map(|row| (row.ts, row.cpu_pct)).collect();
    let mem_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, util::percent(row.mem_used, row.mem_total)))
        .collect();
    let disk_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, util::percent(row.disk_used, row.disk_total)))
        .collect();
    let container_points: Vec<(i64, f64)> = history
        .iter()
        .map(|row| (row.ts, row.containers_running as f64))
        .collect();

    let ranges: Vec<Value> = RANGES
        .iter()
        .map(|candidate| {
            json!({
                "key": candidate.key,
                "label": candidate.label,
                "active": candidate.key == range.key,
            })
        })
        .collect();

    let recent = state.db.recent_system(50).unwrap_or_default();
    let rows: Vec<Value> = recent
        .into_iter()
        .rev()
        .map(|row| {
            json!({
                "ts": row.ts,
                "time_text": util::fmt_datetime(row.ts),
                "cpu_text": util::fmt_pct(row.cpu_pct),
                "mem_text": util::fmt_pct(util::percent(row.mem_used, row.mem_total)),
                "disk_text": util::fmt_pct(util::percent(row.disk_used, row.disk_total)),
                "load_text": format!("{:.2}", row.load1),
                "temp_text": row.temp_c.map(|value| format!("{value:.1} °C")).unwrap_or_else(|| "—".into()),
                "containers_text": format!("{}/{}", row.containers_running, row.containers_total),
            })
        })
        .collect();

    let context = json!({
        "ranges": ranges,
        "summary": {
            "cpu_avg_text": util::fmt_pct(cpu_avg),
            "cpu_max_text": util::fmt_pct(cpu_max),
            "mem_avg_text": util::fmt_pct(mem_avg),
            "mem_max_text": util::fmt_pct(mem_max),
            "disk_now_text": disk_now_text,
            "disk_now_pct": util::fmt_pct(disk_now),
            "disk_path": live.disk_path,
            "containers_now": containers_now,
            "containers_running_now": containers_now,
            "sample_count": history.len(),
            "range_label": range.label,
        },
        "cpu_chart": line_chart(&cpu_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-cpu"),
        "mem_chart": line_chart(&mem_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-mem"),
        "disk_chart": line_chart(&disk_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-disk"),
        "containers_chart": line_chart(&container_points, 1000, 190, Some(0.0), None, "count", "chart-count"),
        "rows": rows,
        "db": db_view(&db_stats, &state.cfg),
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(&state, "metrics.html", "metrics", "Metrik", context)
}

async fn logs_page(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let requested_level = query
        .get("level")
        .map(String::as_str)
        .unwrap_or("ALL")
        .to_ascii_uppercase();
    let level = if matches!(requested_level.as_str(), "INFO" | "WARN" | "ERROR") {
        requested_level.clone()
    } else {
        "ALL".into()
    };
    let search = query.get("q").cloned().unwrap_or_default();
    let requested_page = query
        .get("page")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let per_page = 100;

    let level_filter = if level == "ALL" {
        None
    } else {
        Some(level.as_str())
    };
    let (mut rows, total) = state
        .db
        .logs(level_filter, Some(search.as_str()), requested_page, per_page)
        .unwrap_or_default();
    let total_pages = (((total.max(0) as usize) + per_page - 1) / per_page).max(1);
    let page = requested_page.min(total_pages);
    if page != requested_page {
        (rows, _) = state
            .db
            .logs(level_filter, Some(search.as_str()), page, per_page)
            .unwrap_or_default();
    }

    let prev_link = if page > 1 {
        Some(logs_link(&level, &search, page - 1))
    } else {
        None
    };
    let next_link = if page < total_pages {
        Some(logs_link(&level, &search, page + 1))
    } else {
        None
    };
    let row_views: Vec<Value> = rows.iter().map(log_view).collect();

    let context = json!({
        "rows": row_views,
        "level": level,
        "query": search,
        "page": page,
        "total": total,
        "total_pages": total_pages,
        "prev_link": prev_link,
        "next_link": next_link,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(&state, "logs.html", "logs", "Log", context)
}

async fn maintenance_page(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let live = state.live.read().await.clone();
    let db_stats = match state.db.stats() {
        Ok(stats) => stats,
        Err(error) => {
            error!(target: "workamon::db", "gagal membaca statistik database: {error:#}");
            DbStats {
                size_bytes: 0,
                max_bytes: state.cfg.db_max_bytes,
                system_rows: 0,
                container_rows: 0,
                log_rows: 0,
                oldest_ts: None,
                newest_ts: None,
            }
        }
    };

    let df = if let Some(docker) = &state.docker {
        match tokio::time::timeout(Duration::from_secs(10), docker::df(docker)).await {
            Ok(Ok(df)) => Some(df),
            Ok(Err(error)) => {
                warn!(target: "workamon::docker", "gagal membaca disk usage Docker: {error:#}");
                None
            }
            Err(_) => {
                warn!(target: "workamon::docker", "disk usage Docker melewati batas waktu");
                None
            }
        }
    } else {
        None
    };

    let context = json!({
        "disk": disk_view(&live),
        "db": db_view(&db_stats, &state.cfg),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "df": df.as_ref().map(df_view),
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        "maintenance.html",
        "maintenance",
        "Pemeliharaan",
        context,
    )
}

#[derive(Deserialize)]
struct ActionQuery {
    #[serde(default)]
    back: Option<String>,
}

async fn container_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<ActionQuery>,
) -> Response {
    let back_path = if query.back.as_deref() == Some("detail") {
        format!("/containers/{}", util::url_encode(&id))
    } else {
        "/containers".to_string()
    };

    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return redirect_msg(&back_path, "error", "Aksi container tidak dikenal.");
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(&back_path, "error", "Docker tidak tersedia.");
    };

    match tokio::time::timeout(Duration::from_secs(45), docker::action(&docker, &id, &action)).await
    {
        Ok(Ok(())) => {
            info!(
                target: "workamon::container",
                "container {} berhasil di-{}",
                id,
                action
            );
            redirect_msg(
                &back_path,
                "ok",
                &format!("Container berhasil di-{}.", action_label(&action)),
            )
        }
        Ok(Err(error)) => {
            warn!(
                target: "workamon::container",
                "gagal {} container {}: {error:#}",
                action,
                id
            );
            redirect_msg(
                &back_path,
                "error",
                &format!("Gagal {} container: {error:#}", action_label(&action)),
            )
        }
        Err(_) => {
            warn!(
                target: "workamon::container",
                "aksi {} container {} melewati batas waktu",
                action,
                id
            );
            redirect_msg(
                &back_path,
                "error",
                "Aksi container melewati batas waktu.",
            )
        }
    }
}

#[derive(Deserialize)]
struct ConfirmForm {
    #[serde(default)]
    confirm: Option<String>,
}

async fn prune_action(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    if form.confirm.as_deref() != Some("yes") {
        return redirect_msg("/maintenance", "error", "Konfirmasi prune tidak valid.");
    }
    if !matches!(
        kind.as_str(),
        "containers" | "images" | "volumes" | "build" | "networks"
    ) {
        return render_error(
            &state,
            StatusCode::NOT_FOUND,
            "Jenis prune tidak dikenal",
            "Pilih salah satu jenis pembersihan Docker yang tersedia.",
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg("/maintenance", "error", "Docker tidak tersedia.");
    };

    match tokio::time::timeout(Duration::from_secs(120), docker::prune(&docker, &kind)).await {
        Ok(Ok(report)) => {
            info!(
                target: "workamon::maintenance",
                "prune {}: {} objek, {} dibebaskan",
                kind,
                report.deleted,
                util::fmt_bytes(report.reclaimed)
            );
            redirect_msg(
                "/maintenance",
                "ok",
                &format!(
                    "Prune {} selesai: {} objek dihapus, {} dibebaskan.",
                    prune_label(&kind),
                    report.deleted,
                    util::fmt_bytes(report.reclaimed)
                ),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::maintenance", "prune {kind} gagal: {error:#}");
            redirect_msg(
                "/maintenance",
                "error",
                &format!("Prune {} gagal: {error:#}", prune_label(&kind)),
            )
        }
        Err(_) => {
            warn!(target: "workamon::maintenance", "prune {kind} melewati batas waktu");
            redirect_msg(
                "/maintenance",
                "error",
                &format!("Prune {} melewati batas waktu.", prune_label(&kind)),
            )
        }
    }
}

#[derive(Deserialize)]
struct DbActionForm {
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    days: Option<i64>,
}

async fn db_action(
    State(state): State<AppState>,
    Path(action): Path<String>,
    Form(form): Form<DbActionForm>,
) -> Response {
    match action.as_str() {
        "trim" => match state.db.trim_to_limit() {
            Ok(report) => {
                info!(
                    target: "workamon::maintenance",
                    "pangkas database manual: {} metrik sistem, {} metrik container, {} log ({} -> {})",
                    report.deleted_system,
                    report.deleted_containers,
                    report.deleted_logs,
                    util::fmt_bytes(report.before_bytes),
                    util::fmt_bytes(report.after_bytes)
                );
                redirect_msg(
                    "/maintenance",
                    "ok",
                    &format!(
                        "Pangkas selesai: {} baris dihapus, ukuran {} menjadi {}.",
                        report.deleted_system + report.deleted_containers + report.deleted_logs,
                        util::fmt_bytes(report.before_bytes),
                        util::fmt_bytes(report.after_bytes)
                    ),
                )
            }
            Err(error) => {
                warn!(target: "workamon::maintenance", "pangkas database gagal: {error:#}");
                redirect_msg(
                    "/maintenance",
                    "error",
                    &format!("Pangkas database gagal: {error:#}"),
                )
            }
        },
        "vacuum" => match state.db.vacuum() {
            Ok(()) => {
                info!(target: "workamon::maintenance", "VACUUM database selesai");
                redirect_msg("/maintenance", "ok", "VACUUM selesai. File database sudah dirapikan.")
            }
            Err(error) => {
                warn!(target: "workamon::maintenance", "VACUUM gagal: {error:#}");
                redirect_msg("/maintenance", "error", &format!("VACUUM gagal: {error:#}"))
            }
        },
        "cleanup-logs" => {
            let days = form
                .days
                .unwrap_or(state.cfg.log_retention_days)
                .clamp(1, 3650);
            let cutoff = util::now() - days * 86_400;
            match state.db.cleanup_logs_before(cutoff) {
                Ok(deleted) => {
                    info!(
                        target: "workamon::maintenance",
                        "menghapus {deleted} log lebih lama dari {days} hari"
                    );
                    redirect_msg(
                        "/maintenance",
                        "ok",
                        &format!("{deleted} log lebih lama dari {days} hari dihapus."),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "hapus log lama gagal: {error:#}");
                    redirect_msg(
                        "/maintenance",
                        "error",
                        &format!("Gagal menghapus log lama: {error:#}"),
                    )
                }
            }
        }
        "clear-logs" => {
            if form.confirm.as_deref() != Some("yes") {
                return redirect_msg("/maintenance", "error", "Konfirmasi tidak valid.");
            }
            match state.db.clear_logs() {
                Ok(deleted) => {
                    info!(target: "workamon::maintenance", "menghapus semua log: {deleted} baris");
                    redirect_msg(
                        "/maintenance",
                        "ok",
                        &format!("{deleted} log dihapus."),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "kosongkan log gagal: {error:#}");
                    redirect_msg("/maintenance", "error", &format!("Gagal menghapus log: {error:#}"))
                }
            }
        }
        "clear-metrics" => {
            if form.confirm.as_deref() != Some("yes") {
                return redirect_msg("/maintenance", "error", "Konfirmasi tidak valid.");
            }
            match state.db.clear_metrics() {
                Ok(deleted) => {
                    info!(target: "workamon::maintenance", "menghapus semua metrik: {deleted} baris");
                    redirect_msg(
                        "/maintenance",
                        "ok",
                        &format!("{deleted} baris metrik dihapus."),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "kosongkan metrik gagal: {error:#}");
                    redirect_msg(
                        "/maintenance",
                        "error",
                        &format!("Gagal menghapus metrik: {error:#}"),
                    )
                }
            }
        }
        _ => render_error(
            &state,
            StatusCode::NOT_FOUND,
            "Aksi database tidak dikenal",
            "Aksi pemeliharaan yang diminta tidak tersedia.",
        ),
    }
}

async fn api_live(State(state): State<AppState>) -> Json<Value> {
    let live = state.live.read().await;
    let sample = live.system.clone().unwrap_or_default();
    let mem_pct = util::percent(sample.mem_used, sample.mem_total);
    let disk_pct = util::percent(sample.disk_used, sample.disk_total);
    let total = live.containers.len();
    let running = live
        .containers
        .iter()
        .filter(|container| container.state == "running")
        .count();
    let unhealthy = live
        .containers
        .iter()
        .filter(|container| container.health.as_deref() == Some("unhealthy"))
        .count();

    Json(json!({
        "ts": live.last_update,
        "ts_text": util::fmt_datetime(live.last_update),
        "cpu_pct": round1(sample.cpu_pct),
        "cpu_text": util::fmt_pct(sample.cpu_pct),
        "mem_pct": round1(mem_pct),
        "mem_pct_text": util::fmt_pct(mem_pct),
        "mem_text": format!("{} / {}", util::fmt_bytes(sample.mem_used), util::fmt_bytes(sample.mem_total)),
        "disk_pct": round1(disk_pct),
        "disk_pct_text": util::fmt_pct(disk_pct),
        "disk_text": format!("{} / {}", util::fmt_bytes(sample.disk_used), util::fmt_bytes(sample.disk_total)),
        "uptime_text": util::fmt_duration(live.uptime_secs),
        "load_text": format!("{:.2} / {:.2} / {:.2}", sample.load1, sample.load5, sample.load15),
        "temp_text": sample.temp_c.map(|value| format!("{value:.1} °C")).unwrap_or_else(|| "—".into()),
        "containers_running": running,
        "containers_stopped": total.saturating_sub(running),
        "containers_unhealthy": unhealthy,
        "containers_total": total,
        "docker_ok": live.docker_ok,
        "docker_status": if live.docker_ok { "online" } else { "offline" },
        "docker_error": live.docker_error,
    }))
}

async fn api_container_stats(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let live = state.live.read().await;
    let Some(container) = live
        .containers
        .iter()
        .find(|container| container.id == id || container.name == id)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "container not found" })),
        )
            .into_response();
    };

    let mem_pct = if container.mem_limit > 0 {
        util::percent(container.mem_used, container.mem_limit)
    } else {
        0.0
    };
    Json(json!({
        "cpu_pct": round1(container.cpu_pct),
        "cpu": util::fmt_pct(container.cpu_pct),
        "mem_pct_value": round1(mem_pct),
        "mem_pct": util::fmt_pct(mem_pct),
        "mem": if container.mem_limit > 0 {
            format!("{} / {}", util::fmt_bytes(container.mem_used), util::fmt_bytes(container.mem_limit))
        } else {
            util::fmt_bytes(container.mem_used)
        },
        "net": format!("↓ {} · ↑ {}", util::fmt_bytes(container.net_rx), util::fmt_bytes(container.net_tx)),
    }))
    .into_response()
}

#[derive(Clone, Copy)]
struct RangeSpec {
    key: &'static str,
    label: &'static str,
    span_secs: i64,
    bucket_secs: i64,
}

const RANGES: [RangeSpec; 4] = [
    RangeSpec {
        key: "1h",
        label: "1 jam",
        span_secs: 3_600,
        bucket_secs: 60,
    },
    RangeSpec {
        key: "6h",
        label: "6 jam",
        span_secs: 21_600,
        bucket_secs: 300,
    },
    RangeSpec {
        key: "24h",
        label: "24 jam",
        span_secs: 86_400,
        bucket_secs: 900,
    },
    RangeSpec {
        key: "7d",
        label: "7 hari",
        span_secs: 604_800,
        bucket_secs: 3_600,
    },
];

fn parse_range(value: Option<&str>) -> RangeSpec {
    RANGES
        .iter()
        .copied()
        .find(|range| Some(range.key) == value)
        .unwrap_or(RANGES[2])
}

fn system_view(sample: &SystemSample, live: &LiveSnapshot) -> Value {
    let mem_pct = util::percent(sample.mem_used, sample.mem_total);
    let disk_pct = util::percent(sample.disk_used, sample.disk_total);
    json!({
        "cpu_pct": round1(sample.cpu_pct),
        "cpu_text": util::fmt_pct(sample.cpu_pct),
        "cpu_count": live.cpu_count,
        "load1_text": format!("{:.2}", sample.load1),
        "temp_text": sample.temp_c.map(|value| format!("{value:.1} °C")).unwrap_or_else(|| "—".into()),
        "mem_pct": round1(mem_pct),
        "mem_pct_text": util::fmt_pct(mem_pct),
        "mem_text": format!("{} / {}", util::fmt_bytes(sample.mem_used), util::fmt_bytes(sample.mem_total)),
        "swap_text": format!("{} / {}", util::fmt_bytes(sample.swap_used), util::fmt_bytes(sample.swap_total)),
        "disk_pct": round1(disk_pct),
        "disk_pct_text": util::fmt_pct(disk_pct),
        "disk_text": format!("{} / {}", util::fmt_bytes(sample.disk_used), util::fmt_bytes(sample.disk_total)),
        "disk_path": live.disk_path,
        "uptime_text": util::fmt_duration(live.uptime_secs),
        "hostname": if live.hostname.is_empty() { "unknown".to_string() } else { live.hostname.clone() },
        "os": live.os,
        "ts": sample.ts,
        "ts_text": util::fmt_datetime(sample.ts),
    })
}

fn container_view(container: &ContainerLive) -> Value {
    let state_class = match container.state.as_str() {
        "running" => "ok",
        "restarting" | "paused" => "warn",
        "dead" => "danger",
        _ => "idle",
    };
    let health_class = match container.health.as_deref() {
        Some("healthy") => "ok",
        Some("unhealthy") => "danger",
        Some("starting") => "warn",
        _ => "none",
    };
    let mem_pct = if container.mem_limit > 0 {
        util::percent(container.mem_used, container.mem_limit)
    } else {
        0.0
    };
    let state_text = match container.state.as_str() {
        "running" => "berjalan",
        "exited" => "berhenti",
        "created" => "dibuat",
        "restarting" => "restart",
        "paused" => "jeda",
        "dead" => "mati",
        "removing" => "dihapus",
        other => other,
    };
    let health_text = match container.health.as_deref() {
        Some("healthy") => "sehat",
        Some("unhealthy") => "bermasalah",
        Some("starting") => "memulai",
        _ => "—",
    };

    json!({
        "id": container.id.clone(),
        "id_short": util::short_id(&container.id),
        "name": container.name.clone(),
        "image": container.image.clone(),
        "image_short": util::truncate_chars(&container.image, 44),
        "state": container.state.clone(),
        "state_text": state_text,
        "state_class": state_class,
        "status": container.status.clone(),
        "health": health_text,
        "health_class": health_class,
        "running": container.state == "running",
        "cpu_pct": round1(container.cpu_pct),
        "cpu_text": util::fmt_pct(container.cpu_pct),
        "mem_pct": round1(mem_pct),
        "mem_pct_text": util::fmt_pct(mem_pct),
        "mem_text": if container.mem_limit > 0 {
            format!("{} / {}", util::fmt_bytes(container.mem_used), util::fmt_bytes(container.mem_limit))
        } else {
            util::fmt_bytes(container.mem_used)
        },
        "net_text": format!("↓ {} · ↑ {}", util::fmt_bytes(container.net_rx), util::fmt_bytes(container.net_tx)),
        "ports": util::truncate_chars(&container.ports, 60),
        "created_text": if container.created > 0 { util::fmt_datetime(container.created) } else { "—".into() },
        "created_ago": if container.created > 0 { util::time_ago(container.created) } else { "—".into() },
    })
}

fn fallback_container(
    inspect: &bollard::models::ContainerInspectResponse,
    name: &str,
    id: &str,
) -> ContainerLive {
    let state = inspect.state.as_ref();
    let state_text = state
        .and_then(|state| state.status)
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".into());
    let health = state
        .and_then(|state| state.health.as_ref())
        .and_then(|health| health.status)
        .map(|status| status.to_string())
        .filter(|status| !status.is_empty() && status != "none");
    ContainerLive {
        id: id.to_string(),
        name: name.to_string(),
        image: inspect
            .config
            .as_ref()
            .and_then(|config| config.image.clone())
            .or_else(|| inspect.image.clone())
            .unwrap_or_else(|| "—".into()),
        state: state_text,
        status: state
            .and_then(|state| state.status)
            .map(|status| status.to_string())
            .unwrap_or_default(),
        health,
        created: 0,
        ports: "—".into(),
        ..Default::default()
    }
}

fn inspect_view(
    inspect: &bollard::models::ContainerInspectResponse,
    name: &str,
    id: &str,
    live: &LiveSnapshot,
) -> Value {
    let state = inspect.state.as_ref();
    let health = state
        .and_then(|state| state.health.as_ref())
        .and_then(|health| health.status)
        .map(|status| status.to_string())
        .filter(|status| !status.is_empty() && status != "none");
    let health_class = match health.as_deref() {
        Some("healthy") => "ok",
        Some("unhealthy") => "danger",
        Some("starting") => "warn",
        _ => "none",
    };
    let health_text = match health.as_deref() {
        Some("healthy") => "sehat",
        Some("unhealthy") => "bermasalah",
        Some("starting") => "memulai",
        _ => "—",
    };

    let mut command_parts = Vec::new();
    if let Some(path) = &inspect.path {
        command_parts.push(path.clone());
    }
    if let Some(args) = &inspect.args {
        command_parts.extend(args.clone());
    }
    let command = if command_parts.is_empty() {
        inspect
            .config
            .as_ref()
            .and_then(|config| config.cmd.as_ref())
            .map(|cmd| cmd.join(" "))
            .unwrap_or_else(|| "—".into())
    } else {
        command_parts.join(" ")
    };

    let image = inspect
        .config
        .as_ref()
        .and_then(|config| config.image.clone())
        .or_else(|| inspect.image.clone())
        .unwrap_or_else(|| "—".into());

    let ports = live
        .containers
        .iter()
        .find(|container| container.id == id || container.name == name)
        .map(|container| container.ports.clone())
        .unwrap_or_else(|| "—".into());

    let networks = inspect
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .map(|networks| {
            networks
                .iter()
                .map(|(network, endpoint)| match &endpoint.ip_address {
                    Some(ip) if !ip.is_empty() => format!("{network} ({ip})"),
                    _ => network.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|networks| !networks.is_empty())
        .unwrap_or_else(|| "—".into());

    let mounts: Vec<Value> = inspect
        .mounts
        .as_ref()
        .map(|mounts| {
            mounts
                .iter()
                .take(8)
                .map(|mount| {
                    json!({
                        "source": mount.source.clone().unwrap_or_else(|| "—".into()),
                        "destination": mount.destination.clone().unwrap_or_else(|| "—".into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let started = state.and_then(|state| state.started_at.clone());

    json!({
        "id": id,
        "id_short": util::short_id(id),
        "name": name,
        "image": image,
        "command": util::truncate_chars(&command, 180),
        "created_text": fmt_docker_date(&inspect.created),
        "started_text": fmt_docker_date(&started),
        "restart_count": inspect.restart_count.unwrap_or(0),
        "health": health_text,
        "health_class": health_class,
        "ports": ports,
        "networks": networks,
        "mounts": mounts,
    })
}

fn fmt_docker_date(value: &Option<String>) -> String {
    match value {
        Some(raw) if !raw.is_empty() && !raw.starts_with("0001") => raw
            .replace('T', " ")
            .chars()
            .take(19)
            .collect(),
        _ => "—".into(),
    }
}

fn log_view(log: &LogRow) -> Value {
    let level_class = match log.level.as_str() {
        "ERROR" => "error",
        "WARN" => "warn",
        "INFO" => "info",
        _ => "debug",
    };
    json!({
        "id": log.id,
        "ts": log.ts,
        "level": log.level.clone(),
        "level_class": level_class,
        "target": log.target.clone(),
        "message": log.message.clone(),
        "time_text": util::fmt_datetime(log.ts),
        "ago": util::time_ago(log.ts),
    })
}

fn container_counts(containers: &[ContainerLive]) -> Value {
    let total = containers.len();
    let running = containers
        .iter()
        .filter(|container| container.state == "running")
        .count();
    let unhealthy = containers
        .iter()
        .filter(|container| container.health.as_deref() == Some("unhealthy"))
        .count();
    json!({
        "total": total,
        "running": running,
        "stopped": total.saturating_sub(running),
        "unhealthy": unhealthy,
    })
}

fn disk_view(live: &LiveSnapshot) -> Value {
    let sample = live.system.clone().unwrap_or_default();
    let percent = util::percent(sample.disk_used, sample.disk_total);
    json!({
        "used_text": util::fmt_bytes(sample.disk_used),
        "total_text": util::fmt_bytes(sample.disk_total),
        "pct": round1(percent),
        "pct_text": util::fmt_pct(percent),
        "path": if live.disk_path.is_empty() { "/".to_string() } else { live.disk_path.clone() },
    })
}

fn db_view(stats: &DbStats, config: &Config) -> Value {
    json!({
        "size_text": util::fmt_bytes(stats.size_bytes),
        "max_text": format!("{} MB", config.db_max_mb()),
        "size_pct": round1(util::percent(stats.size_bytes, stats.max_bytes)),
        "system_rows": stats.system_rows,
        "container_rows": stats.container_rows,
        "log_rows": stats.log_rows,
        "oldest_text": stats.oldest_ts.map(util::fmt_datetime).unwrap_or_else(|| "—".into()),
        "newest_text": stats.newest_ts.map(util::fmt_datetime).unwrap_or_else(|| "—".into()),
        "retention_days": config.log_retention_days,
        "sample_secs": config.sample_secs,
        "container_sample_secs": config.container_sample_secs,
    })
}

fn df_view(df: &DfView) -> Value {
    json!({
        "images_size_text": util::fmt_bytes(df.images_size),
        "images_count": df.images_count,
        "images_reclaimable_text": util::fmt_bytes(df.images_reclaimable),
        "containers_size_text": util::fmt_bytes(df.containers_size),
        "containers_count": df.containers_count,
        "containers_reclaimable_text": util::fmt_bytes(df.containers_reclaimable),
        "volumes_size_text": util::fmt_bytes(df.volumes_size),
        "volumes_count": df.volumes_count,
        "volumes_reclaimable_text": util::fmt_bytes(df.volumes_reclaimable),
        "build_size_text": util::fmt_bytes(df.build_size),
        "build_count": df.build_count,
        "build_reclaimable_text": util::fmt_bytes(df.build_reclaimable),
    })
}

fn logs_link(level: &str, query: &str, page: usize) -> String {
    format!(
        "/logs?level={}&q={}&page={page}",
        util::url_encode(level),
        util::url_encode(query)
    )
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0_f64, 0usize), |(sum, count), value| {
        (sum + value, count + 1)
    });
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn action_label(action: &str) -> &str {
    match action {
        "start" => "jalankan",
        "stop" => "hentikan",
        "restart" => "restart",
        _ => action,
    }
}

fn prune_label(kind: &str) -> &str {
    match kind {
        "containers" => "container",
        "images" => "image",
        "volumes" => "volume",
        "build" => "build cache",
        "networks" => "network",
        _ => kind,
    }
}
