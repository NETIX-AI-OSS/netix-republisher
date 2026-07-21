//! Turnkey environment bootstrap: every deployment-relevant setting can come
//! from `REPUBLISHER_*` env vars so the container starts ready on an IoT device
//! with no interactive setup. Env values override the persisted config at every
//! boot (env wins), but are only written to disk when the operator saves from
//! the GUI — secrets follow the existing `remember_secrets` rule.

use anyhow::{bail, Context, Result};
use republish_core::config::{AppConfig, PayloadFormat};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_bool(key: &str) -> Result<Option<bool>> {
    match env(key) {
        None => Ok(None),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            other => bail!("{key}: expected a boolean, got '{other}'"),
        },
    }
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match env(key) {
        None => Ok(None),
        Some(raw) => raw
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|error| anyhow::anyhow!("{key}: {error}")),
    }
}

/// Apply `REPUBLISHER_*` overrides to a loaded (or default) config.
/// Returns human-readable notes about what was overridden.
pub fn apply_env_overrides(config: &mut AppConfig) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let mut note = |message: String| notes.push(message);

    if let Some(protocol) = env("REPUBLISHER_PROTOCOL") {
        config.protocol = protocol.trim().to_string();
        note(format!("protocol = {} (env)", config.protocol));
    }

    if let Some(host) = env("REPUBLISHER_MQTT_HOST") {
        config.mqtt.host = host;
        note("mqtt.host (env)".into());
    }
    if let Some(port) = env_parse::<u16>("REPUBLISHER_MQTT_PORT")? {
        config.mqtt.port = port;
        note("mqtt.port (env)".into());
    }
    if let Some(tls) = env_bool("REPUBLISHER_MQTT_TLS")? {
        config.mqtt.use_tls = tls;
        note("mqtt.use_tls (env)".into());
    }
    if let Some(client_id) = env("REPUBLISHER_MQTT_CLIENT_ID") {
        config.mqtt.client_id = client_id;
        note("mqtt.client_id (env)".into());
    }
    if let Some(prefix) = env("REPUBLISHER_MQTT_TOPIC_PREFIX") {
        config.mqtt.topic_prefix = prefix;
        note("mqtt.topic_prefix (env)".into());
    }
    if let Some(topic) = env("REPUBLISHER_MQTT_HEALTH_TOPIC") {
        config.mqtt.health_topic = topic;
        note("mqtt.health_topic (env)".into());
    }
    if let Some(username) = env("REPUBLISHER_MQTT_USERNAME") {
        config.mqtt.username = Some(username);
        note("mqtt.username (env)".into());
    }
    if let Some(password) = env("REPUBLISHER_MQTT_PASSWORD") {
        config.mqtt.password = Some(password);
        note("mqtt.password (env)".into());
    }
    // `password_env` names *another* variable that holds the secret, so the
    // password stays out of both the config file and this process's direct env
    // inventory. Resolve it immediately (env indirection is the supported path).
    if let Some(var) = env("REPUBLISHER_MQTT_PASSWORD_ENV") {
        config.mqtt.password_env = Some(var.clone());
        if config.mqtt.resolve_password_env() {
            note(format!("mqtt.password (via ${var})"));
        } else {
            note(format!("mqtt.password_env = {var} (env, unresolved)"));
        }
    }
    if let Some(path) = env("REPUBLISHER_MQTT_CA_CERT") {
        config.mqtt.ca_cert_path = Some(path);
        note("mqtt.ca_cert_path (env)".into());
    }
    if let Some(path) = env("REPUBLISHER_MQTT_CLIENT_CERT") {
        config.mqtt.client_cert_path = Some(path);
        note("mqtt.client_cert_path (env)".into());
    }
    if let Some(path) = env("REPUBLISHER_MQTT_CLIENT_KEY") {
        config.mqtt.client_key_path = Some(path);
        note("mqtt.client_key_path (env)".into());
    }
    if let Some(passphrase) = env("REPUBLISHER_MQTT_CLIENT_KEY_PASSPHRASE") {
        config.mqtt.client_key_passphrase = Some(passphrase);
        note("mqtt.client_key_passphrase (env)".into());
    }
    if let Some(retain) = env_bool("REPUBLISHER_MQTT_RETAIN")? {
        config.mqtt.retain = retain;
        note("mqtt.retain (env)".into());
    }
    if let Some(keep_alive) = env_parse::<u64>("REPUBLISHER_MQTT_KEEP_ALIVE_SECS")? {
        config.mqtt.keep_alive_secs = keep_alive.max(1);
        note("mqtt.keep_alive_secs (env)".into());
    }
    if let Some(format) = env("REPUBLISHER_MQTT_PAYLOAD_FORMAT") {
        config.mqtt.payload_format = match format.trim().to_ascii_lowercase().as_str() {
            "scalar" => PayloadFormat::Scalar,
            "netix_envelope" | "envelope" => PayloadFormat::NetixEnvelope,
            other => bail!(
                "REPUBLISHER_MQTT_PAYLOAD_FORMAT: expected 'scalar' or 'netix_envelope', got '{other}'"
            ),
        };
        note("mqtt.payload_format (env)".into());
    }
    if let Some(prefix) = env("REPUBLISHER_MQTT_DEVICE_TOPIC_PREFIX") {
        config.mqtt.device_topic_prefix = prefix;
        note("mqtt.device_topic_prefix (env)".into());
    }
    if let Some(autostart) = env_bool("REPUBLISHER_AUTOSTART")? {
        config.mqtt.autostart = autostart;
        note(format!("mqtt.autostart = {autostart} (env)"));
    }
    // Runtime discover-then-poll: with no enabled points the worker discovers
    // devices and builds an identity-faithful point set in memory instead of
    // looping forever publishing nothing (RCA #2). Top-level config flag, sibling
    // to `autostart`.
    if let Some(discover_on_start) = env_bool("REPUBLISHER_DISCOVER_ON_START")? {
        config.discover_on_start = discover_on_start;
        note(format!("discover_on_start = {discover_on_start} (env)"));
    }

    if let Some(raw) = env("REPUBLISHER_CONNECTION_JSON") {
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)
            .context("REPUBLISHER_CONNECTION_JSON is not a JSON object")?;
        if config.protocol.is_empty() {
            bail!("REPUBLISHER_CONNECTION_JSON requires REPUBLISHER_PROTOCOL (or a configured protocol)");
        }
        let connection = config.connection_mut();
        for (key, value) in object {
            connection.insert(key, value);
        }
        note("connection (env JSON merge)".into());
    }

    if let Some(raw) = env("REPUBLISHER_POINTS_JSON") {
        let imported: Vec<republish_core::model::PointConfig> = serde_json::from_str(&raw)
            .context("REPUBLISHER_POINTS_JSON is not a PointConfig array")?;
        let merged = republish_core::import::merge_imported_points(&config.points, &imported);
        note(format!(
            "points (env JSON merge: {} added, {} updated)",
            merged.added, merged.updated
        ));
        config.points = merged.points;
    }

    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests mutate process state; keep them in one test so they cannot
    // race each other under the parallel test runner.
    #[test]
    fn env_overrides_apply_and_validate() {
        let vars = [
            ("REPUBLISHER_PROTOCOL", "bacnet"),
            ("REPUBLISHER_MQTT_HOST", "broker.example"),
            ("REPUBLISHER_MQTT_PORT", "1883"),
            ("REPUBLISHER_MQTT_TLS", "false"),
            ("REPUBLISHER_MQTT_PAYLOAD_FORMAT", "netix_envelope"),
            ("REPUBLISHER_AUTOSTART", "true"),
            ("REPUBLISHER_DISCOVER_ON_START", "true"),
            (
                "REPUBLISHER_CONNECTION_JSON",
                r#"{"port": 0, "discover_all_interfaces": true}"#,
            ),
            (
                "REPUBLISHER_POINTS_JSON",
                r#"[{"device_key":"AHU-1","tag_path":"AHU-1/Temp","addressing":{"device_instance":10700}}]"#,
            ),
        ];
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        let mut config = AppConfig::default();
        let notes = apply_env_overrides(&mut config).expect("overrides apply");
        for (key, _) in vars {
            std::env::remove_var(key);
        }

        assert_eq!(config.protocol, "bacnet");
        assert_eq!(config.mqtt.host, "broker.example");
        assert_eq!(config.mqtt.port, 1883);
        assert!(!config.mqtt.use_tls);
        assert_eq!(config.mqtt.payload_format, PayloadFormat::NetixEnvelope);
        assert!(config.mqtt.autostart);
        assert!(config.discover_on_start);
        assert_eq!(
            config.connection().get("discover_all_interfaces"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(config.points.len(), 1);
        assert_eq!(config.points[0].tag_path, "AHU-1/Temp");
        assert!(!notes.is_empty());

        // Bad values fail loudly (turnkey boots must not silently misconfigure).
        std::env::set_var("REPUBLISHER_MQTT_PORT", "not-a-port");
        let error = apply_env_overrides(&mut AppConfig::default()).unwrap_err();
        std::env::remove_var("REPUBLISHER_MQTT_PORT");
        assert!(error.to_string().contains("REPUBLISHER_MQTT_PORT"));

        // password_env indirection: name a *different* var holding the secret;
        // the password is resolved from it and never taken from the config file.
        std::env::set_var("REPUBLISHER_MQTT_PASSWORD_ENV", "MY_BROKER_SECRET_VAR");
        std::env::set_var("MY_BROKER_SECRET_VAR", "indirect-secret");
        let mut config = AppConfig::default();
        apply_env_overrides(&mut config).expect("overrides apply");
        std::env::remove_var("REPUBLISHER_MQTT_PASSWORD_ENV");
        std::env::remove_var("MY_BROKER_SECRET_VAR");
        assert_eq!(config.mqtt.password_env.as_deref(), Some("MY_BROKER_SECRET_VAR"));
        assert_eq!(config.mqtt.password.as_deref(), Some("indirect-secret"));
    }
}
