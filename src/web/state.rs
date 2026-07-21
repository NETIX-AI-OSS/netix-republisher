//! Shared daemon state and the worker-event drain.
//!
//! This is the headless twin of the desktop `RepublisherApp`: the same
//! `republish-core` worker feeds a crossbeam channel; here a dedicated thread
//! drains it into [`Shared`] and fans out JSON deltas to SSE subscribers.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use proto_api::Capabilities;
use republish_core::config::{self, AppConfig};
use republish_core::log::LogLevel;
use republish_core::model::{
    now_millis, DiscoveredDevice, DiscoveredPoint, PointIdentity, PointStatus,
};
use republish_core::protocol::RepublishRegistry;
use republish_core::worker::{
    RepublisherLifecycle, WorkerChannel, WorkerEvent, WorkerReceiver, WorkerSender,
};
use serde::Serialize;

use super::auth::Auth;
use super::dto;

/// Same ring capacities as the desktop GUI.
const LOG_CAPACITY: usize = 500;
const RECENT_SAMPLE_CAPACITY: usize = 200;
/// SSE fan-out buffer; slow subscribers skip missed events.
const EVENT_BUFFER: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    pub seq: u64,
    pub ts_ms: i64,
    pub level: &'static str,
    pub message: String,
}

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "info",
        LogLevel::Warning => "warning",
        LogLevel::Error => "error",
    }
}

/// Mutable state behind one lock — mirrors the desktop app's fields.
pub struct Shared {
    pub config: AppConfig,
    pub devices: Vec<DiscoveredDevice>,
    pub browsed: Vec<DiscoveredPoint>,
    pub scan_progress: Option<(usize, usize)>,
    pub statuses: HashMap<PointIdentity, PointStatus>,
    pub recent_samples: VecDeque<dto::SampleDto>,
    pub logs: VecDeque<LogRecord>,
    pub log_seq: u64,
    pub lifecycle: RepublisherLifecycle,
    pub stop_flag: Option<Arc<AtomicBool>>,
    pub published_total: usize,
    pub status_line: String,
}

pub struct WebState {
    pub registry: RepublishRegistry,
    pub caps: HashMap<String, Capabilities>,
    pub protocol_ids: Vec<String>,
    pub config_path: PathBuf,
    pub worker_tx: WorkerSender,
    pub events: tokio::sync::broadcast::Sender<String>,
    pub auth: Auth,
    inner: Mutex<Shared>,
}

impl WebState {
    pub fn new(
        registry: RepublishRegistry,
        config: AppConfig,
        config_path: PathBuf,
        auth: Auth,
        boot_status: String,
    ) -> (Arc<Self>, WorkerReceiver) {
        let caps: HashMap<String, Capabilities> = registry
            .capabilities()
            .into_iter()
            .map(|caps| (caps.id.to_string(), caps))
            .collect();
        let protocol_ids = registry.ids();
        let channel = WorkerChannel::new();
        let (events, _) = tokio::sync::broadcast::channel(EVENT_BUFFER);
        let mut shared = Shared {
            config,
            devices: Vec::new(),
            browsed: Vec::new(),
            scan_progress: None,
            statuses: HashMap::new(),
            recent_samples: VecDeque::new(),
            logs: VecDeque::new(),
            log_seq: 0,
            lifecycle: RepublisherLifecycle::Stopped,
            stop_flag: None,
            published_total: 0,
            status_line: boot_status.clone(),
        };
        push_log(&mut shared, LogLevel::Info, boot_status);
        let state = Arc::new(Self {
            registry,
            caps,
            protocol_ids,
            config_path,
            worker_tx: channel.sender,
            events,
            auth,
            inner: Mutex::new(shared),
        });
        (state, channel.receiver)
    }

    pub fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Broadcast one SSE JSON payload; no-op without subscribers.
    pub fn emit(&self, value: serde_json::Value) {
        let _ = self.events.send(value.to_string());
    }

    /// Record a status-line message (mirrors the desktop `save_status`).
    pub fn set_status(&self, shared: &mut Shared, level: LogLevel, message: impl Into<String>) {
        let message = message.into();
        shared.status_line = message.clone();
        let record = push_log(shared, level, message);
        self.emit(serde_json::json!({ "type": "log", "record": record, "status_line": shared.status_line }));
    }

    /// Validate + persist the config (mirrors the desktop `save_config`).
    /// Returns Err with the human-readable reason on validation/save failure.
    pub fn save_config(&self, shared: &mut Shared) -> Result<(), String> {
        if let Err(error) = shared.config.validate() {
            let message = format!("Config invalid: {error}");
            self.set_status(shared, LogLevel::Error, message.clone());
            return Err(message);
        }
        match config::save_to_path(&self.config_path, &shared.config) {
            Ok(()) => {
                self.set_status(shared, LogLevel::Info, "Configuration saved");
                Ok(())
            }
            Err(error) => {
                let message = format!("Save failed: {error:#}");
                self.set_status(shared, LogLevel::Error, message.clone());
                Err(message)
            }
        }
    }

    pub fn points_json(&self, shared: &Shared) -> serde_json::Value {
        let points: Vec<dto::PointDto> = shared
            .config
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let identity = PointIdentity::from_point(point);
                dto::point_dto(
                    index,
                    point,
                    &shared.config.mqtt,
                    shared.statuses.get(&identity),
                )
            })
            .collect();
        serde_json::json!(points)
    }

    pub fn emit_points(&self, shared: &Shared) {
        self.emit(serde_json::json!({ "type": "points", "points": self.points_json(shared) }));
    }

    pub fn status_json(&self, shared: &Shared) -> serde_json::Value {
        let stale = shared.statuses.values().filter(|s| s.stale).count();
        serde_json::json!({
            "status_line": shared.status_line,
            "protocol": shared.config.protocol,
            "lifecycle": dto::lifecycle_dto(&shared.lifecycle),
            "points": shared.config.points.len(),
            "devices": shared.devices.len(),
            "published_total": shared.published_total,
            "stale_points": stale,
            "scan_progress": shared.scan_progress.map(|(current, total)| serde_json::json!({"current": current, "total": total})),
            "config_path": self.config_path.display().to_string(),
        })
    }

    /// Full-state snapshot used by the UI to hydrate in one round trip.
    pub fn snapshot_json(&self, shared: &Shared) -> serde_json::Value {
        let capabilities: Vec<dto::CapabilitiesDto> = self
            .protocol_ids
            .iter()
            .filter_map(|id| self.caps.get(id))
            .map(Into::into)
            .collect();
        let devices: Vec<dto::DeviceDto> = shared
            .devices
            .iter()
            .enumerate()
            .map(|(index, device)| dto::device_dto(index, device))
            .collect();
        let browsed: Vec<dto::BrowsedPointDto> = shared
            .browsed
            .iter()
            .enumerate()
            .map(|(index, point)| dto::browsed_dto(index, point))
            .collect();
        serde_json::json!({
            "type": "snapshot",
            "status": self.status_json(shared),
            "capabilities": capabilities,
            "config": dto::redacted_config(&shared.config),
            "devices": devices,
            "browsed": browsed,
            "points": self.points_json(shared),
            "recent_samples": shared.recent_samples,
            "logs": shared.logs,
        })
    }
}

fn push_log(shared: &mut Shared, level: LogLevel, message: impl Into<String>) -> LogRecord {
    if shared.logs.len() >= LOG_CAPACITY {
        shared.logs.pop_front();
    }
    shared.log_seq += 1;
    let record = LogRecord {
        seq: shared.log_seq,
        ts_ms: now_millis(),
        level: level_str(level),
        message: message.into(),
    };
    shared.logs.push_back(record.clone());
    record
}

/// Drain worker events on a dedicated thread for the daemon's lifetime.
/// Mirrors the desktop `drain_worker_events`, then fans each change out as an
/// SSE delta.
pub fn spawn_event_drain(state: Arc<WebState>, receiver: WorkerReceiver) {
    std::thread::Builder::new()
        .name("worker-event-drain".into())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                apply_event(&state, event);
            }
        })
        .expect("failed to spawn worker-event drain thread");
}

fn apply_event(state: &WebState, event: WorkerEvent) {
    let mut shared = state.lock();
    match event {
        WorkerEvent::Log(level, message) => {
            let record = push_log(&mut shared, level, message);
            state.emit(serde_json::json!({ "type": "log", "record": record }));
        }
        WorkerEvent::Devices(outcome) => {
            shared.devices = outcome.devices;
            for warning in outcome.warnings {
                let record = push_log(&mut shared, LogLevel::Warning, warning);
                state.emit(serde_json::json!({ "type": "log", "record": record }));
            }
            let devices: Vec<dto::DeviceDto> = shared
                .devices
                .iter()
                .enumerate()
                .map(|(index, device)| dto::device_dto(index, device))
                .collect();
            state.emit(serde_json::json!({ "type": "devices", "devices": devices }));
        }
        WorkerEvent::Points(points) => {
            shared.browsed = points;
            let browsed: Vec<dto::BrowsedPointDto> = shared
                .browsed
                .iter()
                .enumerate()
                .map(|(index, point)| dto::browsed_dto(index, point))
                .collect();
            state.emit(serde_json::json!({ "type": "browsed", "points": browsed }));
        }
        WorkerEvent::ScanProgress { current, total, .. } => {
            shared.scan_progress = Some((current, total));
            state.emit(
                serde_json::json!({ "type": "scan_progress", "current": current, "total": total }),
            );
        }
        WorkerEvent::BulkTagImport(result) => {
            shared.config.points = result.points;
            shared.scan_progress = None;
            let _ = state.save_config(&mut shared);
            state.set_status(
                &mut shared,
                LogLevel::Info,
                format!(
                    "Bulk import: {} added, {} updated",
                    result.added, result.updated
                ),
            );
            state.emit_points(&shared);
        }
        WorkerEvent::Samples(samples) => {
            let mut changed: HashMap<String, serde_json::Value> = HashMap::new();
            let mut sample_dtos = Vec::with_capacity(samples.len());
            for sample in samples {
                let identity = PointIdentity::from_point(&sample.point);
                let status = shared.statuses.entry(identity).or_default();
                status.record_sample(&sample);
                let key = dto::identity_key(&sample.point);
                changed.insert(
                    key,
                    serde_json::to_value(dto::status_dto(Some(status))).unwrap_or_default(),
                );
                let dto = dto::sample_dto(&sample);
                shared.recent_samples.push_front(dto);
                sample_dtos.push(dto::sample_dto(&sample));
            }
            while shared.recent_samples.len() > RECENT_SAMPLE_CAPACITY {
                shared.recent_samples.pop_back();
            }
            state.emit(serde_json::json!({ "type": "samples", "samples": sample_dtos, "statuses": changed }));
        }
        WorkerEvent::Failures(failures) => {
            let mut changed: HashMap<String, serde_json::Value> = HashMap::new();
            for failure in failures {
                let identity = PointIdentity::from_point(&failure.point);
                let key = dto::identity_key(&failure.point);
                let status = shared.statuses.entry(identity).or_default();
                status.record_read_failure(failure.error);
                changed.insert(
                    key,
                    serde_json::to_value(dto::status_dto(Some(status))).unwrap_or_default(),
                );
            }
            state.emit(serde_json::json!({ "type": "statuses", "statuses": changed }));
        }
        WorkerEvent::PublishStatus(stats) => {
            shared.published_total += stats.published;
            state.emit(
                serde_json::json!({ "type": "stats", "published_total": shared.published_total }),
            );
        }
        WorkerEvent::PointPublish { identity, error } => {
            // Addressing-only key, matching dto::identity_key so the UI joins
            // statuses to points across a device_key rename.
            let key = identity
                .addressing
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            let status = shared.statuses.entry(identity).or_default();
            match error {
                None => status.record_publish_success(),
                Some(message) => status.record_publish_failure(message),
            }
            let value = serde_json::to_value(dto::status_dto(Some(status))).unwrap_or_default();
            state.emit(serde_json::json!({ "type": "statuses", "statuses": { key: value } }));
        }
        WorkerEvent::Lifecycle(lifecycle) => {
            if let RepublisherLifecycle::Failed(ref error) = lifecycle {
                let record = push_log(
                    &mut shared,
                    LogLevel::Error,
                    format!("Republisher failed: {error}"),
                );
                state.emit(serde_json::json!({ "type": "log", "record": record }));
            }
            shared.lifecycle = lifecycle;
            state.emit(serde_json::json!({ "type": "lifecycle", "lifecycle": dto::lifecycle_dto(&shared.lifecycle) }));
        }
        WorkerEvent::Finished(message) => {
            let record = push_log(&mut shared, LogLevel::Info, message);
            state.emit(serde_json::json!({ "type": "log", "record": record }));
        }
    }
}
