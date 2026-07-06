//! Writes a sample republisher config with BACnet simulator-aligned points.

use proto_api::Addressing;
use republish_core::config::{config_path, save_to_path, AppConfig, MqttConfig};
use republish_core::model::PointConfig;
use republish_core::network::ipv4_interfaces;

fn bacnet_point(
    device_key: &str,
    device_instance: u32,
    object_type: &str,
    object_instance: u32,
    tag_path: &str,
) -> PointConfig {
    let mut addressing = Addressing::new();
    addressing.insert("device_instance".into(), serde_json::json!(device_instance));
    addressing.insert("object_type".into(), serde_json::json!(object_type));
    addressing.insert("object_instance".into(), serde_json::json!(object_instance));
    addressing.insert("property".into(), serde_json::json!("present_value"));
    PointConfig {
        enabled: true,
        device_key: device_key.to_string(),
        addressing,
        tag_path: tag_path.to_string(),
        poll_interval_secs: 10,
    }
}

fn simulator_points() -> Vec<PointConfig> {
    vec![
        bacnet_point(
            "AHU-L-001",
            10700,
            "analog_input",
            1,
            "AHU-L-001/SupplyAirTemp",
        ),
        bacnet_point(
            "AHU-L-001",
            10700,
            "analog_input",
            2,
            "AHU-L-001/ReturnAirTemp",
        ),
        bacnet_point(
            "AHU-L-001",
            10700,
            "binary_input",
            1,
            "AHU-L-001/SupplyFanStatus",
        ),
        bacnet_point(
            "VAV-OFC-001",
            10900,
            "analog_input",
            1,
            "VAV-OFC-001/RoomTemp",
        ),
        bacnet_point(
            "VAV-OFC-001",
            10900,
            "analog_output",
            1,
            "VAV-OFC-001/DamperPosition",
        ),
        bacnet_point(
            "PLANT-MTR-001",
            10600,
            "analog_input",
            1,
            "PLANT-MTR-001/ActivePower",
        ),
        bacnet_point(
            "PLANT-MTR-001",
            10600,
            "analog_input",
            9,
            "PLANT-MTR-001/TotalEnergy",
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    let path = config_path()?;
    let mut config = AppConfig {
        protocol: "bacnet".to_string(),
        mqtt: MqttConfig {
            use_tls: false,
            port: 1883,
            ..Default::default()
        },
        points: simulator_points(),
        ..Default::default()
    };

    let mut bacnet = Addressing::new();
    bacnet.insert("discover_all_interfaces".into(), serde_json::json!(false));
    bacnet.insert("port".into(), serde_json::json!(0));
    bacnet.insert(
        "broadcast_address".into(),
        serde_json::json!("255.255.255.255"),
    );
    bacnet.insert("bind_failure_policy".into(), serde_json::json!("skip"));

    let interfaces = ipv4_interfaces();
    if let Some(preferred) = interfaces
        .iter()
        .find(|iface| iface.name.starts_with("bridge"))
    {
        bacnet.insert(
            "interface".into(),
            serde_json::json!(preferred.addr.to_string()),
        );
        eprintln!(
            "Set bacnet interface to {} ({}) for Docker/OrbStack reachability",
            preferred.addr, preferred.name
        );
    } else if let Some(first) = interfaces.first() {
        eprintln!(
            "No bridge NIC found; using first interface {} ({})",
            first.addr, first.name
        );
        bacnet.insert(
            "interface".into(),
            serde_json::json!(first.addr.to_string()),
        );
    }
    config.connections.insert("bacnet".to_string(), bacnet);

    save_to_path(&path, &config)?;
    eprintln!(
        "Wrote {} with {} simulator point(s)",
        path.display(),
        config.points.len()
    );
    Ok(())
}
