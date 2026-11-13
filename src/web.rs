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
use crate::docker::{self, ContainerLive, DfView, NewContainer};
use crate::i18n::{self, Lang};
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
        .route("/containers/new", get(container_new_page).post(container_create))
        .route("/containers/{id}", get(container_page))
        .route("/containers/{id}/{action}", post(container_action))
        .route("/images", get(images_page))
        .route("/images/inspect", get(image_page))
        .route("/images/pull", post(image_pull))
        .route("/images/remove", post(image_remove))
        .route("/images/prune", post(image_prune))
        .route("/metrics", get(metrics_page))
        .route("/logs", get(logs_page))
        .route("/maintenance", get(maintenance_page))
        .route("/maintenance/prune/{kind}", post(prune_action))
        .route("/maintenance/db/{action}", post(db_action))
        .route("/api/live", get(api_live))
        .route("/api/metrics/live", get(api_metrics_live))
        .route("/api/containers/{id}/stats", get(api_container_stats))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/login", get(login_page).post(login_post))
        .route("/logout", post(logout))
        .route("/language/{lang}", get(language_switch))
        .route("/healthz", get(healthz))
        .route("/static/app.css", get(asset_css))
        .route("/static/app.js", get(asset_js))
        .route("/static/favicon.svg", get(asset_favicon))
        .route("/static/fonts/manrope-latin.woff2", get(asset_manrope))
        .route(
            "/static/fonts/space-grotesk-latin.woff2",
            get(asset_space_grotesk),
        )
        .route(
            "/static/fonts/jetbrains-mono-latin.woff2",
            get(asset_jetbrains_mono),
        )
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
                "default-src 'self'; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'",
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

async fn asset_manrope() -> Response {
    font_asset(include_bytes!("../assets/fonts/manrope-latin.woff2"))
}

async fn asset_space_grotesk() -> Response {
    font_asset(include_bytes!("../assets/fonts/space-grotesk-latin.woff2"))
}

async fn asset_jetbrains_mono() -> Response {
    font_asset(include_bytes!("../assets/fonts/jetbrains-mono-latin.woff2"))
}

fn font_asset(body: &'static [u8]) -> Response {
    let mut response = Response::new(axum::body::Body::from(axum::body::Bytes::from_static(
        body,
    )));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("font/woff2"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
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
    let lang = Lang::from_headers(&headers);
    render_login(&state, lang, None)
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
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
        let mut response = render_login(
            &state,
            lang,
            Some(&i18n::t(lang, "login_error_invalid")),
        );
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

async fn language_switch(
    Path(lang): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let selected = Lang::parse(&lang).unwrap_or(Lang::Id);
    let next = safe_next_path(query.get("next").map(String::as_str), &headers);
    let mut response = Redirect::to(&next).into_response();
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000",
        i18n::COOKIE,
        selected.code()
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

fn safe_next_path(value: Option<&str>, headers: &HeaderMap) -> String {
    if let Some(value) = value {
        let value = value.trim();
        if value.starts_with('/') && !value.starts_with("//") && !value.contains(['\r', '\n']) {
            return value.to_string();
        }
    }
    if let Some(referer) = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
    {
        let path = if let Some(index) = referer.find("://") {
            let rest = &referer[index + 3..];
            rest.find('/').map(|slash| &rest[slash..])
        } else if referer.starts_with('/') {
            Some(referer)
        } else {
            None
        };
        if let Some(path) = path {
            if path.starts_with('/') && !path.starts_with("//") {
                return path.to_string();
            }
        }
    }
    "/".into()
}

async fn not_found(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let lang = Lang::from_headers(&headers);
    render_error(
        &state,
        lang,
        StatusCode::NOT_FOUND,
        &i18n::t(lang, "err_not_found_title"),
        &i18n::t(lang, "err_not_found_body"),
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

fn render_login(state: &AppState, lang: Lang, error: Option<&str>) -> Response {
    let context = json!({
        "error": error,
        "version": VERSION,
        "lang": lang.code(),
        "lang_other": lang.other().code(),
        "lang_other_short": lang.other().short(),
        "t": i18n::dict(lang),
    });
    match state.renderer.render("login.html", &context) {
        Ok(html) => no_store(Html(html).into_response()),
        Err(error) => {
            error!(target: "workamon::web", "gagal render halaman login: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                i18n::t(lang, "err_render_login"),
            )
                .into_response()
        }
    }
}

fn render_page(
    state: &AppState,
    lang: Lang,
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
    map.insert("lang".into(), json!(lang.code()));
    map.insert("lang_other".into(), json!(lang.other().code()));
    map.insert("lang_other_short".into(), json!(lang.other().short()));
    map.insert("t".into(), i18n::dict(lang));
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
                i18n::t(lang, "err_render_page"),
            )
                .into_response()
        }
    }
}

fn render_error(
    state: &AppState,
    lang: Lang,
    status: StatusCode,
    title: &str,
    message: &str,
) -> Response {
    let context = json!({
        "status": status.as_u16(),
        "title": title,
        "message": message,
        "version": VERSION,
        "lang": lang.code(),
        "lang_other": lang.other().code(),
        "lang_other_short": lang.other().short(),
        "t": i18n::dict(lang),
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
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let live = state.live.read().await.clone();
    let history = state.db.recent_system(60).unwrap_or_default();
    let (logs, _) = state.db.logs(None, None, 1, 6).unwrap_or_default();

    let system = live.system.clone().unwrap_or_default();
    let counts = container_counts(&live.containers);
    let containers: Vec<Value> = live
        .containers
        .iter()
        .take(6)
        .map(|container| container_view(container, lang))
        .collect();
    let log_views: Vec<Value> = logs.iter().map(|log| log_view(log, lang)).collect();

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
        "system": system_view(&system, &live, lang),
        "counts": counts,
        "containers": containers,
        "logs": log_views,
        "cpu_chart": line_chart(&cpu_points, 520, 150, Some(0.0), Some(100.0), "percent", "chart-cpu", lang),
        "mem_chart": line_chart(&mem_points, 520, 150, Some(0.0), Some(100.0), "percent", "chart-mem", lang),
        "disk_chart": line_chart(&disk_points, 1000, 150, Some(0.0), Some(100.0), "percent", "chart-disk", lang),
        "sample_count": history.len(),
        "has_data": live.system.is_some(),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "dashboard.html",
        "dashboard",
        &i18n::t(lang, "dashboard_title"),
        context,
    )
}

async fn containers_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let live = state.live.read().await.clone();
    let containers: Vec<Value> = live
        .containers
        .iter()
        .map(|container| container_view(container, lang))
        .collect();
    let context = json!({
        "containers": containers,
        "counts": container_counts(&live.containers),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "sample_secs": state.cfg.sample_secs,
        "container_sample_secs": state.cfg.container_sample_secs,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "containers.html",
        "containers",
        &i18n::t(lang, "containers_title"),
        context,
    )
}

#[derive(Deserialize, Default)]
struct CreateContainerForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    image: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    env: String,
    #[serde(default)]
    ports: String,
    #[serde(default)]
    volumes: String,
    #[serde(default)]
    restart: String,
    #[serde(default)]
    start: Option<String>,
}

fn create_form_view(form: &CreateContainerForm) -> Value {
    json!({
        "name": form.name,
        "image": form.image,
        "command": form.command,
        "env": form.env,
        "ports": form.ports,
        "volumes": form.volumes,
        "restart": if form.restart.is_empty() { "no" } else { form.restart.as_str() },
        "start": form.start.is_some(),
    })
}

async fn container_new_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    render_new_container(
        &state,
        lang,
        &CreateContainerForm::default(),
        flash_from_query(&query),
    )
    .await
}

async fn render_new_container(
    state: &AppState,
    lang: Lang,
    form: &CreateContainerForm,
    flash: Option<Flash>,
) -> Response {
    let mut image_names = Vec::new();
    let mut docker_ok = state.docker.is_some();
    let mut docker_error = state.docker_startup_error.clone();
    if let Some(docker) = &state.docker {
        match tokio::time::timeout(Duration::from_secs(10), docker::list_images(docker)).await {
            Ok(Ok(images)) => {
                let mut names: Vec<String> = images
                    .iter()
                    .flat_map(|image| image.repo_tags.clone())
                    .filter(|tag| tag != "<none>:<none>")
                    .collect();
                names.sort();
                names.dedup();
                image_names = names;
                docker_ok = true;
                docker_error = None;
            }
            Ok(Err(error)) => {
                docker_error = Some(format!("{error:#}"));
            }
            Err(_) => {
                docker_error = Some(i18n::t(lang, "msg_image_list_timeout"));
            }
        }
    }

    let context = json!({
        "form": create_form_view(form),
        "images": image_names,
        "docker_ok": docker_ok,
        "docker_error": docker_error,
        "flash": flash_value(flash),
    });
    render_page(
        state,
        lang,
        "container_new.html",
        "containers",
        &i18n::t(lang, "new_container_title"),
        context,
    )
}

async fn container_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CreateContainerForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);

    let name = form.name.trim().to_string();
    let image = form.image.trim().to_string();
    if name.is_empty() || image.is_empty() {
        return render_new_container(
            &state,
            lang,
            &form,
            Some(Flash {
                kind: "error".into(),
                message: i18n::t(lang, "msg_container_name_required"),
            }),
        )
        .await;
    }
    if !valid_container_name(&name) {
        return render_new_container(
            &state,
            lang,
            &form,
            Some(Flash {
                kind: "error".into(),
                message: i18n::t(lang, "msg_container_name_invalid"),
            }),
        )
        .await;
    }

    let cmd = split_command(&form.command);
    let env = match parse_env_lines(&form.env, lang) {
        Ok(values) => values,
        Err(message) => {
            return render_new_container(
                &state,
                lang,
                &form,
                Some(Flash {
                    kind: "error".into(),
                    message,
                }),
            )
            .await;
        }
    };
    let ports = match parse_port_lines(&form.ports, lang) {
        Ok(values) => values,
        Err(message) => {
            return render_new_container(
                &state,
                lang,
                &form,
                Some(Flash {
                    kind: "error".into(),
                    message,
                }),
            )
            .await;
        }
    };
    let binds = match parse_volume_lines(&form.volumes, lang) {
        Ok(values) => values,
        Err(message) => {
            return render_new_container(
                &state,
                lang,
                &form,
                Some(Flash {
                    kind: "error".into(),
                    message,
                }),
            )
            .await;
        }
    };

    let Some(docker) = state.docker.clone() else {
        return render_new_container(
            &state,
            lang,
            &form,
            Some(Flash {
                kind: "error".into(),
                message: i18n::t(lang, "msg_docker_unavailable"),
            }),
        )
        .await;
    };

    let spec = NewContainer {
        name: name.clone(),
        image,
        cmd,
        env,
        ports,
        binds,
        restart: form.restart.clone(),
        start: form.start.is_some(),
    };

    match tokio::time::timeout(Duration::from_secs(60), docker::create_container(&docker, &spec))
        .await
    {
        Ok(Ok(id)) => {
            info!(
                target: "workamon::container",
                "container {} dibuat sebagai {}",
                name,
                id
            );
            redirect_msg(
                &format!("/containers/{}", util::url_encode(&id)),
                "ok",
                &i18n::tf(lang, "msg_container_create_ok", &[("name", name)]),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::container", "gagal membuat container {name}: {error:#}");
            render_new_container(
                &state,
                lang,
                &form,
                Some(Flash {
                    kind: "error".into(),
                    message: i18n::tf(
                        lang,
                        "msg_container_create_failed",
                        &[("error", format!("{error:#}"))],
                    ),
                }),
            )
            .await
        }
        Err(_) => {
            warn!(target: "workamon::container", "pembuatan container {name} melewati batas waktu");
            render_new_container(
                &state,
                lang,
                &form,
                Some(Flash {
                    kind: "error".into(),
                    message: i18n::t(lang, "msg_container_create_timeout"),
                }),
            )
            .await
        }
    }
}

fn valid_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    name.len() <= 128
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

/// Pisahkan command seperti shell sederhana: spasi di luar kutip dianggap
/// pemisah, sedangkan kutip tunggal/ganda dan backslash dipertahankan.
fn split_command(value: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => escaped = true,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn parse_env_lines(value: &str, lang: Lang) -> Result<Vec<String>, String> {
    let mut env = Vec::new();
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some((key, _)) = line.split_once('=') else {
            return Err(i18n::t(lang, "msg_container_env_invalid"));
        };
        if key.trim().is_empty() {
            return Err(i18n::t(lang, "msg_container_env_invalid"));
        }
        env.push(line.to_string());
    }
    Ok(env)
}

fn parse_port_lines(value: &str, lang: Lang) -> Result<Vec<(String, String)>, String> {
    let mut ports = Vec::new();
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some((host, container)) = line.split_once(':') else {
            return Err(i18n::t(lang, "msg_container_port_invalid"));
        };
        let host = host.trim();
        let container = container.trim();
        if host.is_empty() || container.is_empty() {
            return Err(i18n::t(lang, "msg_container_port_invalid"));
        }
        let (port, protocol) = match container.split_once('/') {
            Some((port, protocol)) => (port.trim(), protocol.trim()),
            None => (container, "tcp"),
        };
        if port.parse::<u16>().is_err()
            || host.parse::<u16>().is_err()
            || !matches!(protocol, "tcp" | "udp" | "sctp")
        {
            return Err(i18n::t(lang, "msg_container_port_invalid"));
        }
        ports.push((format!("{port}/{protocol}"), host.to_string()));
    }
    Ok(ports)
}

fn parse_volume_lines(value: &str, lang: Lang) -> Result<Vec<String>, String> {
    let mut binds = Vec::new();
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
            return Err(i18n::t(lang, "msg_container_volume_invalid"));
        }
        if !parts[1].trim().starts_with('/') {
            return Err(i18n::t(lang, "msg_container_volume_invalid"));
        }
        binds.push(line.to_string());
    }
    Ok(binds)
}

async fn container_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let Some(docker) = state.docker.clone() else {
        return render_error(
            &state,
            lang,
            StatusCode::SERVICE_UNAVAILABLE,
            &i18n::t(lang, "err_docker_unavailable_title"),
            &state
                .docker_startup_error
                .clone()
                .unwrap_or_else(|| i18n::t(lang, "err_docker_unavailable_body")),
        );
    };

    let inspect = match tokio::time::timeout(Duration::from_secs(8), docker::inspect(&docker, &id))
        .await
    {
        Ok(Ok(inspect)) => inspect,
        Ok(Err(error)) => {
            return render_error(
                &state,
                lang,
                StatusCode::NOT_FOUND,
                &i18n::t(lang, "err_container_not_found_title"),
                &format!("{error:#}"),
            )
        }
        Err(_) => {
            return render_error(
                &state,
                lang,
                StatusCode::GATEWAY_TIMEOUT,
                &i18n::t(lang, "err_docker_timeout_title"),
                &i18n::t(lang, "err_docker_timeout_body"),
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
    let container = container_view(&live_container, lang);
    let info = inspect_view(&inspect, &name, &container_id, &live, lang);

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
        Ok(Err(error)) => i18n::tf(
            lang,
            "msg_container_logs_failed",
            &[("error", format!("{error:#}"))],
        ),
        Err(_) => i18n::t(lang, "msg_container_logs_timeout"),
    };

    let context = json!({
        "c": container,
        "info": info,
        "container_id": container_id,
        "logs": logs,
        "history_count": history.len(),
        "cpu_chart": line_chart(&cpu_points, 520, 150, Some(0.0), None, "percent", "chart-cpu", lang),
        "mem_chart": line_chart(
            &mem_points,
            520,
            150,
            Some(0.0),
            if mem_limit > 0 { Some(mem_limit as f64) } else { None },
            "bytes",
            "chart-mem",
            lang,
        ),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "container.html",
        "containers",
        &format!("{} {name}", i18n::t(lang, "containers_title")),
        context,
    )
}

async fn images_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let mut images = Vec::new();
    let mut docker_ok = state.docker.is_some();
    let mut docker_error = state.docker_startup_error.clone();

    if let Some(docker) = &state.docker {
        match tokio::time::timeout(Duration::from_secs(12), docker::list_images(docker)).await {
            Ok(Ok(list)) => {
                images = list;
                docker_ok = true;
                docker_error = None;
            }
            Ok(Err(error)) => {
                docker_ok = false;
                docker_error = Some(format!("{error:#}"));
            }
            Err(_) => {
                docker_ok = false;
                docker_error = Some(i18n::t(lang, "msg_image_list_timeout"));
            }
        }
    }

    images.sort_by(|a, b| b.created.cmp(&a.created));
    let total_size: i64 = images.iter().map(|image| image.size.max(0)).sum();
    let dangling = images.iter().filter(|image| is_dangling(image)).count();
    let image_views: Vec<Value> = images
        .iter()
        .map(|image| image_view(image, lang))
        .collect();

    let context = json!({
        "images": image_views,
        "total": images.len(),
        "dangling": dangling,
        "total_size_text": util::fmt_bytes(total_size.max(0) as u64),
        "docker_ok": docker_ok,
        "docker_error": docker_error,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "images.html",
        "images",
        &i18n::t(lang, "images_title"),
        context,
    )
}

async fn image_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let reference = query.get("ref").cloned().unwrap_or_default();
    let Some(docker) = state.docker.clone() else {
        return render_error(
            &state,
            lang,
            StatusCode::SERVICE_UNAVAILABLE,
            &i18n::t(lang, "err_docker_unavailable_title"),
            &state
                .docker_startup_error
                .clone()
                .unwrap_or_else(|| i18n::t(lang, "err_docker_unavailable_body")),
        );
    };
    if reference.trim().is_empty() {
        return render_error(
            &state,
            lang,
            StatusCode::NOT_FOUND,
            &i18n::t(lang, "err_image_not_found_title"),
            &i18n::t(lang, "msg_image_reference_required"),
        );
    }

    let inspect = match tokio::time::timeout(
        Duration::from_secs(10),
        docker::inspect_image(&docker, &reference),
    )
    .await
    {
        Ok(Ok(inspect)) => inspect,
        Ok(Err(error)) => {
            return render_error(
                &state,
                lang,
                StatusCode::NOT_FOUND,
                &i18n::t(lang, "err_image_not_found_title"),
                &format!("{error:#}"),
            )
        }
        Err(_) => {
            return render_error(
                &state,
                lang,
                StatusCode::GATEWAY_TIMEOUT,
                &i18n::t(lang, "err_docker_timeout_title"),
                &i18n::t(lang, "err_docker_timeout_body"),
            )
        }
    };

    let live = state.live.read().await.clone();
    let detail = image_detail_view(&inspect, &reference, &live, lang);
    let context = json!({
        "image": detail,
        "reference": reference,
        "docker_ok": true,
        "docker_error": Value::Null,
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "image.html",
        "images",
        &i18n::t(lang, "image_detail_title"),
        context,
    )
}

#[derive(Deserialize)]
struct ConfirmForm {
    #[serde(default)]
    confirm: Option<String>,
}

#[derive(Deserialize)]
struct ImageForm {
    reference: String,
    #[serde(default)]
    force: Option<String>,
}

async fn image_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ImageForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let reference = form.reference.trim().to_string();
    if reference.is_empty() {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_image_reference_required"),
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_docker_unavailable"),
        );
    };

    match tokio::time::timeout(
        Duration::from_secs(300),
        docker::pull_image(&docker, &reference),
    )
    .await
    {
        Ok(Ok(_)) => {
            info!(target: "workamon::image", "image {reference} berhasil ditarik");
            redirect_msg(
                "/images",
                "ok",
                &i18n::tf(
                    lang,
                    "msg_image_pull_ok",
                    &[("reference", reference.clone())],
                ),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::image", "gagal menarik image {reference}: {error:#}");
            redirect_msg(
                "/images",
                "error",
                &i18n::tf(
                    lang,
                    "msg_image_pull_failed",
                    &[("error", format!("{error:#}"))],
                ),
            )
        }
        Err(_) => {
            warn!(target: "workamon::image", "penarikan image {reference} melewati batas waktu");
            redirect_msg(
                "/images",
                "error",
                &i18n::t(lang, "msg_image_pull_timeout"),
            )
        }
    }
}

async fn image_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ImageForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let reference = form.reference.trim().to_string();
    if reference.is_empty() {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_image_reference_required"),
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_docker_unavailable"),
        );
    };
    let force = form.force.as_deref() == Some("yes");

    match tokio::time::timeout(
        Duration::from_secs(90),
        docker::remove_image(&docker, &reference, force),
    )
    .await
    {
        Ok(Ok(())) => {
            info!(
                target: "workamon::image",
                "image {reference} dihapus (force={force})"
            );
            redirect_msg(
                "/images",
                "ok",
                &i18n::tf(
                    lang,
                    "msg_image_remove_ok",
                    &[("reference", reference.clone())],
                ),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::image", "gagal menghapus image {reference}: {error:#}");
            redirect_msg(
                "/images",
                "error",
                &i18n::tf(
                    lang,
                    "msg_image_remove_failed",
                    &[("error", format!("{error:#}"))],
                ),
            )
        }
        Err(_) => {
            warn!(target: "workamon::image", "penghapusan image {reference} melewati batas waktu");
            redirect_msg(
                "/images",
                "error",
                &i18n::t(lang, "msg_image_remove_timeout"),
            )
        }
    }
}

async fn image_prune(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    if form.confirm.as_deref() != Some("yes") {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_confirm_invalid"),
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(
            "/images",
            "error",
            &i18n::t(lang, "msg_docker_unavailable"),
        );
    };

    match tokio::time::timeout(Duration::from_secs(120), docker::prune(&docker, "images")).await {
        Ok(Ok(report)) => {
            info!(
                target: "workamon::image",
                "prune image: {} objek, {} dibebaskan",
                report.deleted,
                util::fmt_bytes(report.reclaimed)
            );
            redirect_msg(
                "/images",
                "ok",
                &i18n::tf(
                    lang,
                    "msg_image_prune_ok",
                    &[
                        ("deleted", report.deleted.to_string()),
                        ("reclaimed", util::fmt_bytes(report.reclaimed)),
                    ],
                ),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::image", "prune image gagal: {error:#}");
            redirect_msg(
                "/images",
                "error",
                &i18n::tf(
                    lang,
                    "msg_image_prune_failed",
                    &[("error", format!("{error:#}"))],
                ),
            )
        }
        Err(_) => {
            warn!(target: "workamon::image", "prune image melewati batas waktu");
            redirect_msg(
                "/images",
                "error",
                &i18n::t(lang, "msg_image_prune_timeout"),
            )
        }
    }
}

fn is_dangling(image: &bollard::models::ImageSummary) -> bool {
    image.repo_tags.is_empty()
        || image
            .repo_tags
            .iter()
            .all(|tag| tag == "<none>:<none>" || tag == "<none>")
}

fn image_reference(image: &bollard::models::ImageSummary) -> String {
    image
        .repo_tags
        .iter()
        .find(|tag| tag.as_str() != "<none>:<none>" && tag.as_str() != "<none>")
        .cloned()
        .unwrap_or_else(|| image.id.clone())
}

fn short_image_id(id: &str) -> String {
    let trimmed = id.strip_prefix("sha256:").unwrap_or(id);
    trimmed.chars().take(12).collect()
}

fn image_view(image: &bollard::models::ImageSummary, lang: Lang) -> Value {
    let tags: Vec<String> = image
        .repo_tags
        .iter()
        .filter(|tag| tag.as_str() != "<none>:<none>" && tag.as_str() != "<none>")
        .cloned()
        .collect();
    let dangling = is_dangling(image);
    json!({
        "id": image.id,
        "id_short": short_image_id(&image.id),
        "reference": image_reference(image),
        "tags": tags,
        "tag_text": if dangling {
            i18n::t(lang, "image_untagged")
        } else {
            tags.join(", ")
        },
        "dangling": dangling,
        "size_text": util::fmt_bytes(image.size.max(0) as u64),
        "created_text": if image.created > 0 { util::fmt_datetime(lang, image.created) } else { "—".into() },
        "created_ago": if image.created > 0 { util::time_ago(lang, image.created) } else { "—".into() },
        "containers": image.containers.max(0),
    })
}

fn image_detail_view(
    inspect: &bollard::models::ImageInspect,
    reference: &str,
    live: &LiveSnapshot,
    lang: Lang,
) -> Value {
    let tags: Vec<String> = inspect.repo_tags.clone().unwrap_or_default();
    let config = inspect.config.as_ref();
    let env: Vec<String> = config.and_then(|config| config.env.clone()).unwrap_or_default();
    let entrypoint = config
        .and_then(|config| config.entrypoint.clone())
        .unwrap_or_default()
        .join(" ");
    let cmd = config
        .and_then(|config| config.cmd.clone())
        .unwrap_or_default()
        .join(" ");
    let exposed_ports: Vec<String> = config
        .and_then(|config| config.exposed_ports.clone())
        .unwrap_or_default();
    let layers = inspect
        .root_fs
        .as_ref()
        .and_then(|root| root.layers.as_ref())
        .map(Vec::len)
        .unwrap_or(0);
    let digests: Vec<String> = inspect.repo_digests.clone().unwrap_or_default();
    let image_id = inspect.id.clone().unwrap_or_else(|| reference.to_string());
    let used_by: Vec<Value> = live
        .containers
        .iter()
        .filter(|container| {
            container.image == reference
                || container.image == image_id
                || tags.iter().any(|tag| tag == &container.image)
        })
        .map(|container| {
            json!({
                "id": container.id,
                "name": container.name,
                "state_text": match container.state.as_str() {
                    "running" => i18n::t(lang, "state_running"),
                    "exited" => i18n::t(lang, "state_exited"),
                    "created" => i18n::t(lang, "state_created"),
                    "restarting" => i18n::t(lang, "state_restarting"),
                    "paused" => i18n::t(lang, "state_paused"),
                    "dead" => i18n::t(lang, "state_dead"),
                    other => other.to_string(),
                },
            })
        })
        .collect();

    json!({
        "id": image_id,
        "id_short": short_image_id(&image_id),
        "reference": reference,
        "tags": tags,
        "tag_text": if tags.is_empty() { i18n::t(lang, "image_untagged") } else { tags.join(", ") },
        "digests": digests,
        "size_text": util::fmt_bytes(inspect.size.unwrap_or(0).max(0) as u64),
        "created_text": fmt_docker_date(&inspect.created),
        "architecture": inspect.architecture.clone().unwrap_or_else(|| "—".into()),
        "os": inspect.os.clone().unwrap_or_else(|| "—".into()),
        "author": inspect.author.clone().filter(|value| !value.is_empty()).unwrap_or_else(|| "—".into()),
        "entrypoint": if entrypoint.is_empty() { "—".to_string() } else { util::truncate_chars(&entrypoint, 180) },
        "cmd": if cmd.is_empty() { "—".to_string() } else { util::truncate_chars(&cmd, 180) },
        "env": env,
        "exposed_ports": exposed_ports,
        "layers": layers,
        "used_by": used_by,
    })
}

async fn metrics_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let range = parse_range(query.get("range").map(String::as_str));
    let live = state.live.read().await.clone();
    let is_live = range.key == "live";
    let since = util::now() - range.span_secs;
    let history = if is_live {
        let mut rows = live
            .history
            .iter()
            .filter(|row| row.ts >= since)
            .cloned()
            .collect::<Vec<_>>();
        if rows.is_empty() {
            rows = state
                .db
                .recent_system(500)
                .unwrap_or_default()
                .into_iter()
                .filter(|row| row.ts >= since)
                .collect();
        }
        rows
    } else {
        state
            .db
            .system_bucketed(since, range.bucket_secs)
            .unwrap_or_default()
    };
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
                "label": range_label(lang, candidate.key),
                "active": candidate.key == range.key,
            })
        })
        .collect();

    let recent = if is_live {
        let mut rows = history.clone();
        rows.reverse();
        rows.truncate(50);
        rows
    } else {
        state.db.recent_system(50).unwrap_or_default()
    };
    let rows: Vec<Value> = recent
        .into_iter()
        .rev()
        .map(|row| {
            json!({
                "ts": row.ts,
                "time_text": util::fmt_datetime(lang, row.ts),
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
        "live": is_live,
        "live_window": range.span_secs,
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
            "range_label": range_label(lang, range.key),
        },
        "cpu_chart": line_chart(&cpu_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-cpu", lang),
        "mem_chart": line_chart(&mem_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-mem", lang),
        "disk_chart": line_chart(&disk_points, 1000, 190, Some(0.0), Some(100.0), "percent", "chart-disk", lang),
        "containers_chart": line_chart(&container_points, 1000, 190, Some(0.0), None, "count", "chart-count", lang),
        "rows": rows,
        "db": db_view(&db_stats, &state.cfg, lang),
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "metrics.html",
        "metrics",
        &i18n::t(lang, "metrics_title"),
        context,
    )
}

async fn logs_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
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
    let row_views: Vec<Value> = rows.iter().map(|log| log_view(log, lang)).collect();

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
    render_page(
        &state,
        lang,
        "logs.html",
        "logs",
        &i18n::t(lang, "logs_title"),
        context,
    )
}

async fn maintenance_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = Lang::from_headers(&headers);
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
        "db": db_view(&db_stats, &state.cfg, lang),
        "docker_ok": live.docker_ok,
        "docker_error": live.docker_error,
        "df": df.as_ref().map(df_view),
        "flash": flash_value(flash_from_query(&query)),
    });
    render_page(
        &state,
        lang,
        "maintenance.html",
        "maintenance",
        &i18n::t(lang, "maintenance_title"),
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
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Query(query): Query<ActionQuery>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    let back_path = if query.back.as_deref() == Some("detail") {
        format!("/containers/{}", util::url_encode(&id))
    } else {
        "/containers".to_string()
    };

    if !matches!(action.as_str(), "start" | "stop" | "restart" | "delete") {
        return redirect_msg(
            &back_path,
            "error",
            &i18n::t(lang, "msg_container_action_unknown"),
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(
            &back_path,
            "error",
            &i18n::t(lang, "msg_docker_unavailable"),
        );
    };

    if action == "delete" {
        return match tokio::time::timeout(
            Duration::from_secs(45),
            docker::remove_container(&docker, &id),
        )
        .await
        {
            Ok(Ok(())) => {
                info!(target: "workamon::container", "container {id} dihapus");
                redirect_msg("/containers", "ok", &i18n::t(lang, "msg_container_remove_ok"))
            }
            Ok(Err(error)) => {
                warn!(target: "workamon::container", "gagal menghapus container {id}: {error:#}");
                redirect_msg(
                    &back_path,
                    "error",
                    &i18n::tf(
                        lang,
                        "msg_container_remove_failed",
                        &[("error", format!("{error:#}"))],
                    ),
                )
            }
            Err(_) => {
                warn!(target: "workamon::container", "penghapusan container {id} melewati batas waktu");
                redirect_msg(
                    &back_path,
                    "error",
                    &i18n::t(lang, "msg_container_remove_timeout"),
                )
            }
        };
    }

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
                &i18n::tf(
                    lang,
                    "msg_container_action_ok",
                    &[("action", action_label(lang, &action))],
                ),
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
                &i18n::tf(
                    lang,
                    "msg_container_action_failed",
                    &[
                        ("action", action_label(lang, &action)),
                        ("error", format!("{error:#}")),
                    ],
                ),
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
                &i18n::t(lang, "msg_container_action_timeout"),
            )
        }
    }
}

async fn prune_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
    if form.confirm.as_deref() != Some("yes") {
        return redirect_msg(
            "/maintenance",
            "error",
            &i18n::t(lang, "msg_confirm_invalid"),
        );
    }
    if !matches!(
        kind.as_str(),
        "containers" | "images" | "volumes" | "build" | "networks"
    ) {
        return render_error(
            &state,
            lang,
            StatusCode::NOT_FOUND,
            &i18n::t(lang, "err_prune_unknown_title"),
            &i18n::t(lang, "err_prune_unknown_body"),
        );
    }
    let Some(docker) = state.docker.clone() else {
        return redirect_msg(
            "/maintenance",
            "error",
            &i18n::t(lang, "msg_docker_unavailable"),
        );
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
                &i18n::tf(
                    lang,
                    "msg_prune_ok",
                    &[
                        ("kind", prune_label(lang, &kind)),
                        ("deleted", report.deleted.to_string()),
                        ("reclaimed", util::fmt_bytes(report.reclaimed)),
                    ],
                ),
            )
        }
        Ok(Err(error)) => {
            warn!(target: "workamon::maintenance", "prune {kind} gagal: {error:#}");
            redirect_msg(
                "/maintenance",
                "error",
                &i18n::tf(
                    lang,
                    "msg_prune_failed",
                    &[
                        ("kind", prune_label(lang, &kind)),
                        ("error", format!("{error:#}")),
                    ],
                ),
            )
        }
        Err(_) => {
            warn!(target: "workamon::maintenance", "prune {kind} melewati batas waktu");
            redirect_msg(
                "/maintenance",
                "error",
                &i18n::tf(
                    lang,
                    "msg_prune_timeout",
                    &[("kind", prune_label(lang, &kind))],
                ),
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
    headers: HeaderMap,
    Path(action): Path<String>,
    Form(form): Form<DbActionForm>,
) -> Response {
    let lang = Lang::from_headers(&headers);
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
                    &i18n::tf(
                        lang,
                        "msg_db_trim_ok",
                        &[
                            (
                                "deleted",
                                (report.deleted_system
                                    + report.deleted_containers
                                    + report.deleted_logs)
                                    .to_string(),
                            ),
                            ("before", util::fmt_bytes(report.before_bytes)),
                            ("after", util::fmt_bytes(report.after_bytes)),
                        ],
                    ),
                )
            }
            Err(error) => {
                warn!(target: "workamon::maintenance", "pangkas database gagal: {error:#}");
                redirect_msg(
                    "/maintenance",
                    "error",
                    &i18n::tf(
                        lang,
                        "msg_db_trim_failed",
                        &[("error", format!("{error:#}"))],
                    ),
                )
            }
        },
        "vacuum" => match state.db.vacuum() {
            Ok(()) => {
                info!(target: "workamon::maintenance", "VACUUM database selesai");
                redirect_msg(
                    "/maintenance",
                    "ok",
                    &i18n::t(lang, "msg_db_vacuum_ok"),
                )
            }
            Err(error) => {
                warn!(target: "workamon::maintenance", "VACUUM gagal: {error:#}");
                redirect_msg(
                    "/maintenance",
                    "error",
                    &i18n::tf(
                        lang,
                        "msg_db_vacuum_failed",
                        &[("error", format!("{error:#}"))],
                    ),
                )
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
                        &i18n::tf(
                            lang,
                            "msg_db_logs_cleanup_ok",
                            &[
                                ("deleted", deleted.to_string()),
                                ("days", days.to_string()),
                            ],
                        ),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "hapus log lama gagal: {error:#}");
                    redirect_msg(
                        "/maintenance",
                        "error",
                        &i18n::tf(
                            lang,
                            "msg_db_logs_cleanup_failed",
                            &[("error", format!("{error:#}"))],
                        ),
                    )
                }
            }
        }
        "clear-logs" => {
            if form.confirm.as_deref() != Some("yes") {
                return redirect_msg(
                    "/maintenance",
                    "error",
                    &i18n::t(lang, "msg_confirm_invalid"),
                );
            }
            match state.db.clear_logs() {
                Ok(deleted) => {
                    info!(target: "workamon::maintenance", "menghapus semua log: {deleted} baris");
                    redirect_msg(
                        "/maintenance",
                        "ok",
                        &i18n::tf(
                            lang,
                            "msg_db_logs_clear_ok",
                            &[("deleted", deleted.to_string())],
                        ),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "kosongkan log gagal: {error:#}");
                    redirect_msg(
                        "/maintenance",
                        "error",
                        &i18n::tf(
                            lang,
                            "msg_db_logs_clear_failed",
                            &[("error", format!("{error:#}"))],
                        ),
                    )
                }
            }
        }
        "clear-metrics" => {
            if form.confirm.as_deref() != Some("yes") {
                return redirect_msg(
                    "/maintenance",
                    "error",
                    &i18n::t(lang, "msg_confirm_invalid"),
                );
            }
            match state.db.clear_metrics() {
                Ok(deleted) => {
                    info!(target: "workamon::maintenance", "menghapus semua metrik: {deleted} baris");
                    redirect_msg(
                        "/maintenance",
                        "ok",
                        &i18n::tf(
                            lang,
                            "msg_db_metrics_clear_ok",
                            &[("deleted", deleted.to_string())],
                        ),
                    )
                }
                Err(error) => {
                    warn!(target: "workamon::maintenance", "kosongkan metrik gagal: {error:#}");
                    redirect_msg(
                        "/maintenance",
                        "error",
                        &i18n::tf(
                            lang,
                            "msg_db_metrics_clear_failed",
                            &[("error", format!("{error:#}"))],
                        ),
                    )
                }
            }
        }
        _ => render_error(
            &state,
            lang,
            StatusCode::NOT_FOUND,
            &i18n::t(lang, "err_unknown_action_title"),
            &i18n::t(lang, "err_unknown_action_body"),
        ),
    }
}

async fn api_live(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let lang = Lang::from_headers(&headers);
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
        "ts_text": util::fmt_datetime(lang, live.last_update),
        "cpu_pct": round1(sample.cpu_pct),
        "cpu_text": util::fmt_pct(sample.cpu_pct),
        "mem_pct": round1(mem_pct),
        "mem_pct_text": util::fmt_pct(mem_pct),
        "mem_text": format!("{} / {}", util::fmt_bytes(sample.mem_used), util::fmt_bytes(sample.mem_total)),
        "disk_pct": round1(disk_pct),
        "disk_pct_text": util::fmt_pct(disk_pct),
        "disk_text": format!("{} / {}", util::fmt_bytes(sample.disk_used), util::fmt_bytes(sample.disk_total)),
        "uptime_text": util::fmt_duration(lang, live.uptime_secs),
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

async fn api_metrics_live(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let lang = Lang::from_headers(&headers);
    let window = query
        .get("window")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1_800)
        .clamp(60, 86_400);
    let since = util::now() - window;
    let mut history = {
        let live = state.live.read().await;
        live.history.clone()
    };
    if history.is_empty() {
        history = state.db.recent_system(500).unwrap_or_default();
    }
    let history: Vec<SystemSample> = history
        .into_iter()
        .filter(|row| row.ts >= since)
        .collect();

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
    let last = history.last();
    let disk_now = last
        .map(|row| util::percent(row.disk_used, row.disk_total))
        .unwrap_or(0.0);
    let disk_now_text = last
        .map(|row| {
            format!(
                "{} / {}",
                util::fmt_bytes(row.disk_used),
                util::fmt_bytes(row.disk_total)
            )
        })
        .unwrap_or_else(|| "—".into());
    let containers_now = last.map(|row| row.containers_running).unwrap_or(0);
    let last_update = last.map(|row| row.ts).unwrap_or_else(util::now);

    let points: Vec<Value> = history
        .iter()
        .map(|row| {
            json!({
                "ts": row.ts,
                "cpu": round1(row.cpu_pct),
                "mem": round1(util::percent(row.mem_used, row.mem_total)),
                "disk": round1(util::percent(row.disk_used, row.disk_total)),
                "containers": row.containers_running,
                "load": round1(row.load1),
                "temp": row.temp_c.map(round1),
            })
        })
        .collect();

    Json(json!({
        "window": window,
        "points": points,
        "summary": {
            "cpu_avg_text": util::fmt_pct(cpu_avg),
            "cpu_max_text": util::fmt_pct(cpu_max),
            "mem_avg_text": util::fmt_pct(mem_avg),
            "mem_max_text": util::fmt_pct(mem_max),
            "disk_now_text": disk_now_text,
            "disk_now_pct": util::fmt_pct(disk_now),
            "containers_now": containers_now,
            "sample_count": history.len(),
            "last_update_text": util::fmt_datetime(lang, last_update),
        }
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
    span_secs: i64,
    bucket_secs: i64,
}

const RANGES: [RangeSpec; 5] = [
    RangeSpec {
        key: "live",
        span_secs: 1_800,
        bucket_secs: 5,
    },
    RangeSpec {
        key: "1h",
        span_secs: 3_600,
        bucket_secs: 60,
    },
    RangeSpec {
        key: "6h",
        span_secs: 21_600,
        bucket_secs: 300,
    },
    RangeSpec {
        key: "24h",
        span_secs: 86_400,
        bucket_secs: 900,
    },
    RangeSpec {
        key: "7d",
        span_secs: 604_800,
        bucket_secs: 3_600,
    },
];

fn parse_range(value: Option<&str>) -> RangeSpec {
    RANGES
        .iter()
        .copied()
        .find(|range| Some(range.key) == value)
        .unwrap_or(RANGES[0])
}

fn range_label(lang: Lang, key: &str) -> String {
    let translation_key = match key {
        "live" => "range_live",
        "1h" => "range_1h",
        "6h" => "range_6h",
        "7d" => "range_7d",
        _ => "range_24h",
    };
    i18n::t(lang, translation_key)
}

fn system_view(sample: &SystemSample, live: &LiveSnapshot, lang: Lang) -> Value {
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
        "uptime_text": util::fmt_duration(lang, live.uptime_secs),
        "hostname": if live.hostname.is_empty() { "unknown".to_string() } else { live.hostname.clone() },
        "os": live.os,
        "ts": sample.ts,
        "ts_text": util::fmt_datetime(lang, sample.ts),
    })
}

fn container_view(container: &ContainerLive, lang: Lang) -> Value {
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
        "running" => i18n::t(lang, "state_running"),
        "exited" => i18n::t(lang, "state_exited"),
        "created" => i18n::t(lang, "state_created"),
        "restarting" => i18n::t(lang, "state_restarting"),
        "paused" => i18n::t(lang, "state_paused"),
        "dead" => i18n::t(lang, "state_dead"),
        "removing" => i18n::t(lang, "state_removing"),
        other => other.to_string(),
    };
    let health_text = match container.health.as_deref() {
        Some("healthy") => i18n::t(lang, "health_healthy"),
        Some("unhealthy") => i18n::t(lang, "health_unhealthy"),
        Some("starting") => i18n::t(lang, "health_starting"),
        _ => i18n::t(lang, "health_none"),
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
        "created_text": if container.created > 0 { util::fmt_datetime(lang, container.created) } else { "—".into() },
        "created_ago": if container.created > 0 { util::time_ago(lang, container.created) } else { "—".into() },
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
    lang: Lang,
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
        Some("healthy") => i18n::t(lang, "health_healthy"),
        Some("unhealthy") => i18n::t(lang, "health_unhealthy"),
        Some("starting") => i18n::t(lang, "health_starting"),
        _ => i18n::t(lang, "health_none"),
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

fn log_view(log: &LogRow, lang: Lang) -> Value {
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
        "time_text": util::fmt_datetime(lang, log.ts),
        "ago": util::time_ago(lang, log.ts),
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

fn db_view(stats: &DbStats, config: &Config, lang: Lang) -> Value {
    json!({
        "size_text": util::fmt_bytes(stats.size_bytes),
        "max_text": format!("{} MB", config.db_max_mb()),
        "size_pct": round1(util::percent(stats.size_bytes, stats.max_bytes)),
        "system_rows": stats.system_rows,
        "container_rows": stats.container_rows,
        "log_rows": stats.log_rows,
        "oldest_text": stats.oldest_ts.map(|ts| util::fmt_datetime(lang, ts)).unwrap_or_else(|| "—".into()),
        "newest_text": stats.newest_ts.map(|ts| util::fmt_datetime(lang, ts)).unwrap_or_else(|| "—".into()),
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

fn action_label(lang: Lang, action: &str) -> String {
    let key = match action {
        "start" => "action_start",
        "stop" => "action_stop",
        "restart" => "action_restart",
        "delete" => "action_delete",
        _ => return action.to_string(),
    };
    i18n::t(lang, key)
}

fn prune_label(lang: Lang, kind: &str) -> String {
    let key = match kind {
        "containers" => "prune_kind_containers",
        "images" => "prune_kind_images",
        "volumes" => "prune_kind_volumes",
        "build" => "prune_kind_build",
        "networks" => "prune_kind_networks",
        _ => return kind.to_string(),
    };
    i18n::t(lang, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_commands() {
        assert_eq!(
            split_command("/bin/sh -c \"sleep 3600\""),
            vec!["/bin/sh", "-c", "sleep 3600"]
        );
        assert_eq!(split_command("echo 'hello world'"), vec!["echo", "hello world"]);
    }

    #[test]
    fn validates_container_names() {
        assert!(valid_container_name("web"));
        assert!(valid_container_name("web-1.test"));
        assert!(!valid_container_name(""));
        assert!(!valid_container_name("-web"));
        assert!(!valid_container_name("web/1"));
    }

    #[test]
    fn parses_container_forms() {
        assert_eq!(
            parse_env_lines("A=1\nB=2", Lang::Id).expect("env valid"),
            vec!["A=1", "B=2"]
        );
        assert!(parse_env_lines("INVALID", Lang::Id).is_err());
        assert_eq!(
            parse_port_lines("8080:80\n53:53/udp", Lang::Id).expect("port valid"),
            vec![
                ("80/tcp".to_string(), "8080".to_string()),
                ("53/udp".to_string(), "53".to_string())
            ]
        );
        assert!(parse_port_lines("not-a-port:80", Lang::Id).is_err());
        assert_eq!(
            parse_volume_lines("/srv/data:/data:ro", Lang::Id).expect("volume valid"),
            vec!["/srv/data:/data:ro"]
        );
        assert!(parse_volume_lines("data", Lang::Id).is_err());
    }
}
