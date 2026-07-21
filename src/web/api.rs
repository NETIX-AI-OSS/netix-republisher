//! REST + SSE handlers. Each endpoint mirrors one desktop GUI action so the
//! web UI has strict feature parity with the iced app.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use proto_api::{Addressing, FieldKind, FieldSpec};
use republish_core::config::{PayloadFormat, UiTheme};
use republish_core::log::LogLevel;
use republish_core::model::PointConfig;
use republish_core::network::ipv4_interfaces;
use republish_core::worker::{
    spawn_browse, spawn_discovery, spawn_poll_once, spawn_republisher, spawn_scan_all_objects,
    RepublisherLifecycle,
};
use serde::Deserialize;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use super::auth::session_from_cookie_header;
use super::state::WebState;

type ApiError = (StatusCode, Json<serde_json::Value>);

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
}

fn conflict(message: impl Into<String>) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": message.into() })),
    )
}

fn not_found(message: impl Into<String>) -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": message.into() })),
    )
}

fn ok() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

// ---- capability field coercion (mirrors the desktop build_addressing) ----

/// Convert UI string values into a typed Addressing map per the field specs.
pub fn build_addressing(specs: &[FieldSpec], values: &BTreeMap<String, String>) -> Addressing {
    let mut addressing = Addressing::new();
    for spec in specs {
        let raw = values.get(&spec.key).cloned().unwrap_or_default();
        let value = match &spec.kind {
            FieldKind::U32 => raw
                .trim()
                .parse::<u64>()
                .map(|n| serde_json::json!(n))
                .unwrap_or_else(|_| serde_json::json!(raw)),
            FieldKind::Bool => serde_json::json!(raw == "true"),
            _ => serde_json::json!(raw),
        };
        addressing.insert(spec.key.clone(), value);
    }
    addressing
}

// ---- read endpoints ----

pub async fn health() -> &'static str {
    "ok"
}

pub async fn get_state(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let shared = state.lock();
    Json(state.snapshot_json(&shared))
}

pub async fn get_status(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let shared = state.lock();
    Json(state.status_json(&shared))
}

pub async fn get_capabilities(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let capabilities: Vec<super::dto::CapabilitiesDto> = state
        .protocol_ids
        .iter()
        .filter_map(|id| state.caps.get(id))
        .map(Into::into)
        .collect();
    Json(serde_json::json!({ "protocols": capabilities }))
}

pub async fn get_config(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let shared = state.lock();
    Json(super::dto::redacted_config(&shared.config))
}

pub async fn get_points(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let shared = state.lock();
    Json(state.points_json(&shared))
}

pub async fn get_interfaces(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let interfaces: Vec<serde_json::Value> = ipv4_interfaces()
        .into_iter()
        .map(|iface| serde_json::json!({ "name": iface.name, "addr": iface.addr.to_string() }))
        .collect();
    {
        let mut shared = state.lock();
        state.set_status(
            &mut shared,
            LogLevel::Info,
            format!("Refreshed {} network interface(s)", interfaces.len()),
        );
    }
    Json(serde_json::json!({ "interfaces": interfaces }))
}

pub async fn get_logs(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let shared = state.lock();
    Json(serde_json::json!({ "logs": shared.logs }))
}

pub async fn sse_events(
    State(state): State<Arc<WebState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let snapshot = {
        let shared = state.lock();
        state.snapshot_json(&shared).to_string()
    };
    let live = BroadcastStream::new(state.events.subscribe()).filter_map(
        |item: Result<String, BroadcastStreamRecvError>| match item {
            Ok(payload) => Some(Ok(Event::default().data(payload))),
            // A lagged subscriber just skips; the UI re-syncs from deltas.
            Err(BroadcastStreamRecvError::Lagged(_)) => None,
        },
    );
    let stream = tokio_stream::once(Ok(Event::default().data(snapshot))).chain(live);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---- protocol / connection ----

#[derive(Deserialize)]
pub struct ProtocolReq {
    pub id: String,
}

pub async fn set_protocol(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ProtocolReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.caps.contains_key(&req.id) {
        return Err(bad_request(format!("unknown protocol '{}'", req.id)));
    }
    let mut shared = state.lock();
    shared.config.protocol = req.id;
    shared.devices.clear();
    shared.browsed.clear();
    let config = super::dto::redacted_config(&shared.config);
    state.emit(serde_json::json!({ "type": "config", "config": config }));
    state.emit(serde_json::json!({ "type": "devices", "devices": [] }));
    state.emit(serde_json::json!({ "type": "browsed", "points": [] }));
    Ok(ok())
}

#[derive(Deserialize)]
pub struct ConnectionReq {
    /// Raw string field values keyed by FieldSpec key, as edited in the UI.
    pub values: BTreeMap<String, String>,
}

pub async fn set_connection(
    State(state): State<Arc<WebState>>,
    Json(req): Json<ConnectionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let caps = state
        .caps
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?
        .clone();
    let addressing = build_addressing(&caps.connection_fields, &req.values);
    *shared.config.connection_mut() = addressing;
    state.save_config(&mut shared).map_err(bad_request)?;
    let config = super::dto::redacted_config(&shared.config);
    state.emit(serde_json::json!({ "type": "config", "config": config }));
    Ok(ok())
}

// ---- discovery / browse / scan / poll ----

pub async fn discover(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let factory = state
        .registry
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?;
    shared.devices.clear();
    state.set_status(&mut shared, LogLevel::Info, "Discovering…");
    state.emit(serde_json::json!({ "type": "devices", "devices": [] }));
    spawn_discovery(state.worker_tx.clone(), factory, shared.config.connection());
    Ok(ok())
}

pub async fn browse_device(
    State(state): State<Arc<WebState>>,
    Path(index): Path<usize>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let device = shared
        .devices
        .get(index)
        .cloned()
        .ok_or_else(|| not_found("no such discovered device"))?;
    let factory = state
        .registry
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?;
    shared.browsed.clear();
    state.set_status(
        &mut shared,
        LogLevel::Info,
        format!("Browsing {}…", device.key),
    );
    state.emit(serde_json::json!({ "type": "browsed", "points": [] }));
    spawn_browse(
        state.worker_tx.clone(),
        factory,
        shared.config.connection(),
        device,
    );
    Ok(ok())
}

pub async fn scan_all(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    if shared.devices.is_empty() {
        state.set_status(
            &mut shared,
            LogLevel::Warning,
            "Discover devices before scan-all",
        );
        return Err(bad_request("discover devices before scan-all"));
    }
    let factory = state
        .registry
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?;
    let devices = shared.devices.clone();
    let existing = shared.config.points.clone();
    shared.scan_progress = Some((0, devices.len()));
    state.set_status(&mut shared, LogLevel::Info, "Scanning all objects…");
    state
        .emit(serde_json::json!({ "type": "scan_progress", "current": 0, "total": devices.len() }));
    spawn_scan_all_objects(
        state.worker_tx.clone(),
        factory,
        shared.config.connection(),
        devices,
        existing,
    );
    Ok(ok())
}

pub async fn poll_once(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let factory = state
        .registry
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?;
    spawn_poll_once(
        state.worker_tx.clone(),
        factory,
        shared.config.connection(),
        shared.config.mqtt.clone(),
        shared.config.points.clone(),
    );
    state.set_status(&mut shared, LogLevel::Info, "Polling once…");
    Ok(ok())
}

// ---- points ----

#[derive(Deserialize)]
pub struct PointReq {
    /// Present when editing an existing point; absent when adding.
    pub index: Option<usize>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub device_key: String,
    #[serde(default)]
    pub tag_path: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Raw string values keyed by addressing FieldSpec key.
    #[serde(default)]
    pub addressing: BTreeMap<String, String>,
}

fn default_enabled() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    10
}

pub async fn save_point(
    State(state): State<Arc<WebState>>,
    Json(req): Json<PointReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let caps = state
        .caps
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("select a protocol first"))?
        .clone();
    let point = PointConfig {
        enabled: req.enabled,
        device_key: req.device_key.trim().to_string(),
        addressing: build_addressing(&caps.addressing_fields, &req.addressing),
        tag_path: req.tag_path.trim().to_string(),
        poll_interval_secs: req.poll_interval_secs.max(1),
    };
    match req.index {
        Some(index) => {
            let Some(slot) = shared.config.points.get_mut(index) else {
                return Err(not_found("no such point"));
            };
            *slot = point;
        }
        None => shared.config.points.push(point),
    }
    state.save_config(&mut shared).map_err(bad_request)?;
    state.emit_points(&shared);
    Ok(ok())
}

#[derive(Deserialize)]
pub struct ToggleReq {
    pub enabled: bool,
}

pub async fn toggle_point(
    State(state): State<Arc<WebState>>,
    Path(index): Path<usize>,
    Json(req): Json<ToggleReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let Some(point) = shared.config.points.get_mut(index) else {
        return Err(not_found("no such point"));
    };
    point.enabled = req.enabled;
    state.save_config(&mut shared).map_err(bad_request)?;
    state.emit_points(&shared);
    Ok(ok())
}

pub async fn delete_point(
    State(state): State<Arc<WebState>>,
    Path(index): Path<usize>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    if index >= shared.config.points.len() {
        return Err(not_found("no such point"));
    }
    shared.config.points.remove(index);
    state.save_config(&mut shared).map_err(bad_request)?;
    state.emit_points(&shared);
    Ok(ok())
}

pub async fn clear_points(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let removed = shared.config.points.len();
    shared.config.points.clear();
    shared.statuses.clear();
    state.save_config(&mut shared).map_err(bad_request)?;
    state.set_status(
        &mut shared,
        LogLevel::Info,
        format!("Cleared {removed} configured point(s)."),
    );
    state.emit_points(&shared);
    Ok(Json(serde_json::json!({ "ok": true, "removed": removed })))
}

pub async fn add_browsed_point(
    State(state): State<Arc<WebState>>,
    Path(index): Path<usize>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let Some(found) = shared.browsed.get(index).cloned() else {
        return Err(not_found("no such browsed point"));
    };
    let point = PointConfig {
        enabled: true,
        device_key: found.device_key,
        addressing: found.addressing,
        tag_path: found.suggested_tag_path,
        poll_interval_secs: 10,
    };
    // Upsert by identity, exactly like the desktop add-from-browse.
    let identity = republish_core::model::PointIdentity::from_point(&point);
    if let Some(existing) = shared
        .config
        .points
        .iter_mut()
        .find(|p| republish_core::model::PointIdentity::from_point(p) == identity)
    {
        *existing = point;
    } else {
        shared.config.points.push(point);
    }
    state.save_config(&mut shared).map_err(bad_request)?;
    state.set_status(&mut shared, LogLevel::Info, "Added point from browse");
    state.emit_points(&shared);
    Ok(ok())
}

// ---- settings ----

#[derive(Deserialize)]
pub struct SettingsReq {
    pub mqtt: MqttSettingsReq,
    #[serde(default)]
    pub ui: Option<UiSettingsReq>,
}

#[derive(Deserialize)]
pub struct UiSettingsReq {
    pub theme: UiTheme,
}

#[derive(Deserialize)]
pub struct MqttSettingsReq {
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    pub client_id: String,
    pub topic_prefix: String,
    pub health_topic: String,
    #[serde(default)]
    pub username: Option<String>,
    /// Absent/null = keep the stored secret; "" = clear; other = replace.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    #[serde(default)]
    pub client_cert_path: Option<String>,
    #[serde(default)]
    pub client_key_path: Option<String>,
    /// Same keep/clear/replace semantics as `password`.
    #[serde(default)]
    pub client_key_passphrase: Option<String>,
    pub remember_secrets: bool,
    pub retain: bool,
    pub keep_alive_secs: u64,
    pub payload_format: PayloadFormat,
    pub device_topic_prefix: String,
    pub autostart: bool,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

/// Write-only secret update: None keeps the current value, "" clears it.
fn apply_secret(current: &mut Option<String>, update: Option<String>) {
    if let Some(value) = update {
        *current = (!value.is_empty()).then_some(value);
    }
}

pub async fn put_settings(
    State(state): State<Arc<WebState>>,
    Json(req): Json<SettingsReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    let mqtt = &mut shared.config.mqtt;
    mqtt.host = req.mqtt.host;
    mqtt.port = req.mqtt.port;
    mqtt.use_tls = req.mqtt.use_tls;
    mqtt.client_id = req.mqtt.client_id;
    mqtt.topic_prefix = req.mqtt.topic_prefix;
    mqtt.health_topic = req.mqtt.health_topic;
    mqtt.username = non_empty(req.mqtt.username);
    apply_secret(&mut mqtt.password, req.mqtt.password);
    mqtt.ca_cert_path = non_empty(req.mqtt.ca_cert_path);
    mqtt.client_cert_path = non_empty(req.mqtt.client_cert_path);
    mqtt.client_key_path = non_empty(req.mqtt.client_key_path);
    apply_secret(
        &mut mqtt.client_key_passphrase,
        req.mqtt.client_key_passphrase,
    );
    mqtt.remember_secrets = req.mqtt.remember_secrets;
    mqtt.retain = req.mqtt.retain;
    mqtt.keep_alive_secs = req.mqtt.keep_alive_secs.max(1);
    mqtt.payload_format = req.mqtt.payload_format;
    mqtt.device_topic_prefix = req.mqtt.device_topic_prefix;
    mqtt.autostart = req.mqtt.autostart;
    if let Some(ui) = req.ui {
        shared.config.ui.theme = ui.theme;
    }
    state.save_config(&mut shared).map_err(bad_request)?;
    let config = super::dto::redacted_config(&shared.config);
    state.emit(serde_json::json!({ "type": "config", "config": config }));
    Ok(ok())
}

// ---- republisher lifecycle ----

pub async fn start_republisher(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    if matches!(
        shared.lifecycle,
        RepublisherLifecycle::Starting
            | RepublisherLifecycle::Running
            | RepublisherLifecycle::Stopping
    ) {
        return Err(conflict("republisher is already running"));
    }
    if let Err(error) = shared.config.validate() {
        let message = format!("Cannot start: {error}");
        state.set_status(&mut shared, LogLevel::Error, message.clone());
        return Err(bad_request(message));
    }
    let factory = state
        .registry
        .get(&shared.config.protocol)
        .ok_or_else(|| bad_request("no protocol selected"))?;
    let stop = Arc::new(AtomicBool::new(false));
    shared.stop_flag = Some(Arc::clone(&stop));
    shared.published_total = 0;
    shared.acked_total = 0;
    spawn_republisher(
        state.worker_tx.clone(),
        factory,
        shared.config.connection(),
        shared.config.mqtt.clone(),
        shared.config.points.clone(),
        stop,
    );
    state.set_status(&mut shared, LogLevel::Info, "Republisher starting…");
    Ok(ok())
}

pub async fn stop_republisher(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut shared = state.lock();
    if let Some(stop) = &shared.stop_flag {
        stop.store(true, Ordering::Relaxed);
        state.set_status(&mut shared, LogLevel::Info, "Stopping republisher…");
    }
    Ok(ok())
}

// ---- session ----

#[derive(Deserialize)]
pub struct LoginReq {
    pub password: String,
}

pub async fn session_info(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let authenticated = !state.auth.required
        || headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(session_from_cookie_header)
            .map(|token| state.auth.check_session(token))
            .unwrap_or(false);
    Json(serde_json::json!({
        "auth_required": state.auth.required,
        "authenticated": authenticated,
    }))
}

pub async fn login(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LoginReq>,
) -> impl IntoResponse {
    if !state.auth.required {
        return (StatusCode::OK, HeaderMap::new(), ok()).into_response();
    }
    if state.auth.throttled(peer.ip()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many attempts; retry later" })),
        )
            .into_response();
    }
    if !state.auth.verify_password(&req.password) {
        state.auth.record_failure(peer.ip());
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid password" })),
        )
            .into_response();
    }
    let token = state.auth.create_session();
    let mut headers = HeaderMap::new();
    if let Ok(cookie) = state.auth.session_cookie(&token).parse() {
        headers.insert(header::SET_COOKIE, cookie);
    }
    (StatusCode::OK, headers, ok()).into_response()
}

pub async fn logout(State(state): State<Arc<WebState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(session_from_cookie_header)
    {
        state.auth.remove_session(token);
    }
    let mut response_headers = HeaderMap::new();
    if let Ok(cookie) = state.auth.clear_cookie().parse() {
        response_headers.insert(header::SET_COOKIE, cookie);
    }
    (StatusCode::OK, response_headers, ok())
}
