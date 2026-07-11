//! JSON DTOs for the web API. `republish-core`'s runtime types (capabilities,
//! discovery results, samples, statuses) are not serde types, so the web layer
//! mirrors them here; the shapes are the contract with the embedded browser UI.

use proto_api::{Addressing, BrowseKind, Capabilities, DiscoveryKind, FieldKind, FieldSpec};
use republish_core::config::{AppConfig, MqttConfig};
use republish_core::model::{
    json_scalar, DiscoveredDevice, DiscoveredPoint, PointConfig, PointIdentity, PointSample,
    PointStatus,
};
use republish_core::topic::telemetry_topic;
use republish_core::worker::RepublisherLifecycle;
use serde::Serialize;

#[derive(Serialize)]
pub struct FieldSpecDto {
    pub key: String,
    pub label: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl From<&FieldSpec> for FieldSpecDto {
    fn from(spec: &FieldSpec) -> Self {
        let (kind, options) = match &spec.kind {
            FieldKind::Text => ("text", None),
            FieldKind::U32 => ("u32", None),
            FieldKind::Bool => ("bool", None),
            FieldKind::Enum(options) => ("enum", Some(options.clone())),
            FieldKind::Secret => ("secret", None),
        };
        Self {
            key: spec.key.clone(),
            label: spec.label.clone(),
            kind,
            options,
            default: spec.default.clone(),
            help: spec.help.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct CapabilitiesDto {
    pub id: &'static str,
    pub display_name: &'static str,
    pub discovery: &'static str,
    pub discovery_label: &'static str,
    pub browse: &'static str,
    pub connection_fields: Vec<FieldSpecDto>,
    pub addressing_fields: Vec<FieldSpecDto>,
    pub default_port: u16,
}

impl From<&Capabilities> for CapabilitiesDto {
    fn from(caps: &Capabilities) -> Self {
        let (discovery, discovery_label) = match caps.discovery {
            DiscoveryKind::Broadcast => ("broadcast", "Discover (broadcast)"),
            DiscoveryKind::EndpointQuery => ("endpoint_query", "Query endpoints"),
            DiscoveryKind::SubnetScan => ("subnet_scan", "Scan subnet"),
            DiscoveryKind::ManualOnly => ("manual", "Discover"),
        };
        let browse = match caps.browse {
            BrowseKind::ObjectList => "object_list",
            BrowseKind::AddressSpace => "address_space",
            BrowseKind::RegisterScan => "register_scan",
            BrowseKind::None => "none",
        };
        Self {
            id: caps.id,
            display_name: caps.display_name,
            discovery,
            discovery_label,
            browse,
            connection_fields: caps.connection_fields.iter().map(Into::into).collect(),
            addressing_fields: caps.addressing_fields.iter().map(Into::into).collect(),
            default_port: caps.default_port,
        }
    }
}

#[derive(Serialize)]
pub struct DeviceDto {
    pub index: usize,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<u32>,
    pub address: String,
    pub detail: String,
}

pub fn device_dto(index: usize, device: &DiscoveredDevice) -> DeviceDto {
    DeviceDto {
        index,
        key: device.key.clone(),
        instance: device.instance,
        address: device.address.clone(),
        detail: device.detail.clone(),
    }
}

#[derive(Serialize)]
pub struct BrowsedPointDto {
    pub index: usize,
    pub device_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_display: Option<String>,
    pub addressing: Addressing,
    pub addressing_display: String,
    pub suggested_tag_path: String,
}

pub fn browsed_dto(index: usize, point: &DiscoveredPoint) -> BrowsedPointDto {
    BrowsedPointDto {
        index,
        device_key: point.device_key.clone(),
        name: point.name.clone(),
        description: point.description.clone(),
        units: point.units.clone(),
        value: point.value.as_ref().map(|v| v.as_json_value()),
        value_display: point.value.as_ref().map(|v| v.to_string()),
        addressing: point.addressing.clone(),
        addressing_display: addressing_display(&point.addressing),
        suggested_tag_path: point.suggested_tag_path.clone(),
    }
}

pub fn addressing_display(addressing: &Addressing) -> String {
    addressing
        .iter()
        .map(|(k, v)| format!("{k}={}", json_scalar(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Stable string key for a point identity, used by the UI to join statuses to
/// points and samples.
pub fn identity_key(point: &PointConfig) -> String {
    let identity = PointIdentity::from_point(point);
    let addressing = identity
        .addressing
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|{addressing}", identity.device_key)
}

#[derive(Serialize)]
pub struct PointStatusDto {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_value_display: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sample_ms: Option<i64>,
    pub stale: bool,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_publish_error: Option<String>,
}

/// Mirrors the desktop's status chip precedence: publish error > read error >
/// stale > ok; absent status renders as unknown.
pub fn status_dto(status: Option<&PointStatus>) -> PointStatusDto {
    match status {
        None => PointStatusDto {
            state: "unknown",
            last_value: None,
            last_value_display: None,
            last_sample_ms: None,
            stale: true,
            consecutive_failures: 0,
            last_error: None,
            last_publish_error: None,
        },
        Some(status) => {
            let state = if status.last_publish_error.is_some() {
                "publish_error"
            } else if status.last_error.is_some() {
                "read_error"
            } else if status.stale {
                "stale"
            } else {
                "ok"
            };
            PointStatusDto {
                state,
                last_value: status.last_value.as_ref().map(|v| v.as_json_value()),
                last_value_display: status.last_value.as_ref().map(|v| v.to_string()),
                last_sample_ms: status.last_sample_ms,
                stale: status.stale,
                consecutive_failures: status.consecutive_failures,
                last_error: status.last_error.clone(),
                last_publish_error: status.last_publish_error.clone(),
            }
        }
    }
}

#[derive(Serialize)]
pub struct PointDto {
    pub index: usize,
    pub identity: String,
    pub enabled: bool,
    pub device_key: String,
    pub tag_path: String,
    pub poll_interval_secs: u64,
    pub addressing: Addressing,
    pub addressing_display: String,
    pub display_name: String,
    pub topic: String,
    pub status: PointStatusDto,
}

pub fn point_dto(
    index: usize,
    point: &PointConfig,
    mqtt: &MqttConfig,
    status: Option<&PointStatus>,
) -> PointDto {
    PointDto {
        index,
        identity: identity_key(point),
        enabled: point.enabled,
        device_key: point.device_key.clone(),
        tag_path: point.tag_path.clone(),
        poll_interval_secs: point.poll_interval_secs,
        addressing: point.addressing.clone(),
        addressing_display: addressing_display(&point.addressing),
        display_name: point.display_name(),
        topic: telemetry_topic(mqtt, point),
        status: status_dto(status),
    }
}

#[derive(Serialize)]
pub struct SampleDto {
    pub identity: String,
    pub device_key: String,
    pub tag_path: String,
    pub display_name: String,
    pub topic: String,
    pub value: serde_json::Value,
    pub value_display: String,
    pub timestamp_ms: i64,
}

pub fn sample_dto(sample: &PointSample) -> SampleDto {
    SampleDto {
        identity: identity_key(&sample.point),
        device_key: sample.point.device_key.clone(),
        tag_path: sample.point.tag_path.clone(),
        display_name: sample.point.display_name(),
        topic: sample.topic.clone(),
        value: sample.value.as_json_value(),
        value_display: sample.value.to_string(),
        timestamp_ms: sample.timestamp_ms,
    }
}

pub fn lifecycle_dto(lifecycle: &RepublisherLifecycle) -> serde_json::Value {
    let (state, error) = match lifecycle {
        RepublisherLifecycle::Starting => ("starting", None),
        RepublisherLifecycle::Running => ("running", None),
        RepublisherLifecycle::Stopping => ("stopping", None),
        RepublisherLifecycle::Stopped => ("stopped", None),
        RepublisherLifecycle::Failed(error) => ("failed", Some(error.clone())),
    };
    serde_json::json!({ "state": state, "error": error })
}

/// The saved config with write-only secrets removed, plus flags telling the UI
/// which secrets are currently set.
pub fn redacted_config(config: &AppConfig) -> serde_json::Value {
    let mut clone = config.clone();
    clone.mqtt.password = None;
    clone.mqtt.client_key_passphrase = None;
    serde_json::json!({
        "config": clone,
        "secrets": {
            "password_set": config.mqtt.password.is_some(),
            "client_key_passphrase_set": config.mqtt.client_key_passphrase.is_some(),
        },
    })
}
