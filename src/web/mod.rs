//! The `republisherd` web daemon: the desktop GUI's feature set behind an
//! authenticated HTTP API + SSE stream + embedded browser UI, hardened for
//! container deployment on edge/IoT devices.

pub mod api;
pub mod assets;
pub mod auth;
pub mod dto;
pub mod envcfg;
pub mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use republish_core::config::{self, AppConfig};
use republish_core::log::LogLevel;

use auth::{session_from_cookie_header, Auth};
use state::WebState;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

pub struct ServerOptions {
    pub bind: SocketAddr,
    pub config_path: PathBuf,
    pub tls: Option<TlsPaths>,
}

pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl ServerOptions {
    /// Resolve bind/config/TLS from CLI args + environment.
    pub fn from_env(cli_config: Option<PathBuf>, cli_bind: Option<String>) -> Result<Self> {
        let bind_raw = cli_bind
            .or_else(|| std::env::var("REPUBLISHER_BIND").ok())
            .unwrap_or_else(|| "0.0.0.0:8080".to_string());
        let bind: SocketAddr = bind_raw
            .parse()
            .with_context(|| format!("invalid bind address '{bind_raw}'"))?;
        let config_path = cli_config
            .or_else(|| std::env::var("REPUBLISHER_CONFIG").ok().map(PathBuf::from))
            .map(Ok)
            .unwrap_or_else(config::config_path)?;
        let tls = match (
            std::env::var("REPUBLISHER_TLS_CERT").ok(),
            std::env::var("REPUBLISHER_TLS_KEY").ok(),
        ) {
            (Some(cert), Some(key)) => Some(TlsPaths {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            }),
            (None, None) => None,
            _ => anyhow::bail!("REPUBLISHER_TLS_CERT and REPUBLISHER_TLS_KEY must be set together"),
        };
        Ok(Self {
            bind,
            config_path,
            tls,
        })
    }
}

/// Load config from disk (tolerating a missing file), apply env overrides.
fn boot_config(config_path: &std::path::Path) -> Result<(AppConfig, String)> {
    let (mut config, status) = if config_path.exists() {
        match config::load_from_path(config_path) {
            Ok(config) => (config, "Loaded saved configuration".to_string()),
            Err(error) => (
                AppConfig::default(),
                format!("Using defaults; config load failed: {error:#}"),
            ),
        }
    } else {
        (
            AppConfig::default(),
            "Using default configuration".to_string(),
        )
    };
    let notes = envcfg::apply_env_overrides(&mut config)?;
    let status = if notes.is_empty() {
        status
    } else {
        format!("{status}; env overrides: {}", notes.join(", "))
    };
    Ok((config, status))
}

pub async fn serve(options: ServerOptions) -> Result<()> {
    let (config, boot_status) = boot_config(&options.config_path)?;
    let auth = Auth::from_env(options.bind, options.tls.is_some())?;

    let registry = crate::registry::build_registry();
    let mut boot = config;
    if boot.protocol.is_empty() {
        if let Some(first) = registry.ids().first() {
            boot.protocol = first.clone();
        }
    }

    let (state, receiver) = WebState::new(
        registry,
        boot,
        options.config_path.clone(),
        auth,
        boot_status.clone(),
    );
    state::spawn_event_drain(Arc::clone(&state), receiver);
    eprintln!("[republisherd] {boot_status}");
    eprintln!(
        "[republisherd] config: {} · listening on {}{}",
        options.config_path.display(),
        if options.tls.is_some() {
            "https://"
        } else {
            "http://"
        },
        options.bind
    );

    // Turnkey: begin republishing immediately when configured to.
    let autostart = {
        let shared = state.lock();
        shared.config.mqtt.autostart
    };
    if autostart {
        match api::start_republisher(State(Arc::clone(&state))).await {
            Ok(_) => eprintln!("[republisherd] autostart: republisher starting"),
            Err((_, body)) => eprintln!("[republisherd] autostart failed: {}", body.0),
        }
    }

    let app = router(Arc::clone(&state));

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let shutdown_state = Arc::clone(&state);
    tokio::spawn(async move {
        shutdown_signal().await;
        eprintln!("[republisherd] shutdown signal received");
        // Stop the poll→publish worker first so MQTT disconnects cleanly.
        {
            let mut shared = shutdown_state.lock();
            if let Some(stop) = &shared.stop_flag {
                stop.store(true, Ordering::Relaxed);
            }
            shutdown_state.set_status(&mut shared, LogLevel::Info, "Shutting down…");
        }
        shutdown_handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
    });

    match options.tls {
        Some(tls) => {
            let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                .await
                .with_context(|| {
                    format!(
                        "failed to load TLS cert/key ({}, {})",
                        tls.cert.display(),
                        tls.key.display()
                    )
                })?;
            axum_server::bind_rustls(options.bind, rustls)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        None => {
            axum_server::bind(options.bind)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
    }
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
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn router(state: Arc<WebState>) -> Router {
    let api = Router::new()
        .route("/state", get(api::get_state))
        .route("/status", get(api::get_status))
        .route("/capabilities", get(api::get_capabilities))
        .route("/config", get(api::get_config))
        .route("/settings", put(api::put_settings))
        .route("/protocol", post(api::set_protocol))
        .route("/connection", put(api::set_connection))
        .route("/interfaces", get(api::get_interfaces))
        .route("/discover", post(api::discover))
        .route("/devices/{index}/browse", post(api::browse_device))
        .route("/scan-all", post(api::scan_all))
        .route("/poll-once", post(api::poll_once))
        .route(
            "/points",
            get(api::get_points)
                .post(api::save_point)
                .delete(api::clear_points),
        )
        .route(
            "/points/{index}",
            delete(api::delete_point).patch(api::toggle_point),
        )
        .route("/browsed/{index}/add", post(api::add_browsed_point))
        .route("/republisher/start", post(api::start_republisher))
        .route("/republisher/stop", post(api::stop_republisher))
        .route("/logs", get(api::get_logs))
        .route("/events", get(api::sse_events))
        .route(
            "/session",
            get(api::session_info).post(api::login).delete(api::logout),
        );

    Router::new()
        .route("/healthz", get(api::health))
        .route("/", get(assets::index))
        .route("/app.js", get(assets::app_js))
        .route("/style.css", get(assets::style_css))
        .route("/favicon.svg", get(assets::favicon))
        .route("/favicon.png", get(assets::favicon_png))
        .route("/logo.png", get(assets::logo_png))
        .route("/logo-dark.png", get(assets::logo_dark_png))
        .route("/glyph.png", get(assets::glyph_png))
        .nest("/api", api)
        .fallback(|| async { (StatusCode::NOT_FOUND, "not found") })
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ))
        .layer(middleware::from_fn(security_middleware))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Paths reachable without a session: the login shell (static assets), the
/// session endpoints themselves, and the container health probe.
fn is_public(path: &str, method: &Method) -> bool {
    matches!(
        path,
        "/" | "/app.js"
            | "/style.css"
            | "/favicon.svg"
            | "/favicon.png"
            | "/logo.png"
            | "/logo-dark.png"
            | "/glyph.png"
            | "/healthz"
    ) || (path == "/api/session" && matches!(*method, Method::GET | Method::POST | Method::DELETE))
}

async fn auth_middleware(
    State(state): State<Arc<WebState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !state.auth.required || is_public(path, request.method()) {
        return next.run(request).await;
    }
    let authenticated = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(session_from_cookie_header)
        .map(|token| state.auth.check_session(token))
        .unwrap_or(false);
    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "error": "authentication required" })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Security headers on every response + same-origin enforcement on mutations.
/// The CSP is strict: everything comes from the embedded assets, no inline
/// script/style, no external origins.
async fn security_middleware(request: Request, next: Next) -> Response {
    // Cross-site mutation guard: browsers send Origin on cross-origin requests;
    // reject any that doesn't match the Host we're being addressed as. Non-browser
    // clients (curl) omit Origin and pass through — auth still applies.
    let method = request.method().clone();
    if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if let Some(origin) = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
        {
            let origin_host = origin
                .strip_prefix("https://")
                .or_else(|| origin.strip_prefix("http://"))
                .unwrap_or(origin);
            if host.as_deref() != Some(origin_host) {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({ "error": "cross-origin request rejected" })),
                )
                    .into_response();
            }
        }
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
