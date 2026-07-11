//! Headless web-GUI republisher daemon for containerized/edge deployment.
//!
//! Boot is turnkey: configuration comes from `/data/config.toml` (or
//! `REPUBLISHER_CONFIG`) plus `REPUBLISHER_*` env overrides; with
//! `REPUBLISHER_AUTOSTART=true` publishing begins immediately, otherwise the
//! daemon serves the web GUI and waits.

use std::path::PathBuf;
use std::process::ExitCode;

use republisher::web::{serve, ServerOptions};

const USAGE: &str = "\
republisherd — NETIX republisher web daemon

USAGE:
    republisherd [--config <path>] [--bind <addr:port>]
    republisherd healthcheck
    republisherd --help | --version

ENVIRONMENT:
    REPUBLISHER_BIND                 listen address (default 0.0.0.0:8080)
    REPUBLISHER_CONFIG               config.toml path (default: OS config dir)
    REPUBLISHER_TLS_CERT/_TLS_KEY    serve HTTPS from this PEM cert/key pair
    REPUBLISHER_ADMIN_PASSWORD       web GUI admin password (plain)
    REPUBLISHER_ADMIN_PASSWORD_HASH  web GUI admin password (argon2 PHC)
    REPUBLISHER_AUTH=disabled        disable auth (loopback binds only)
    REPUBLISHER_AUTOSTART            start republishing at boot (true/false)
    REPUBLISHER_PROTOCOL             active protocol (bacnet/modbus/opcua)
    REPUBLISHER_MQTT_*               broker overrides (HOST, PORT, TLS,
                                     USERNAME, PASSWORD, CLIENT_ID,
                                     TOPIC_PREFIX, HEALTH_TOPIC, RETAIN,
                                     KEEP_ALIVE_SECS, PAYLOAD_FORMAT,
                                     DEVICE_TOPIC_PREFIX, CA_CERT,
                                     CLIENT_CERT, CLIENT_KEY,
                                     CLIENT_KEY_PASSPHRASE)
    REPUBLISHER_CONNECTION_JSON      JSON object merged into the active
                                     protocol's connection settings
    REPUBLISHER_POINTS_JSON          JSON array of points merged at boot
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut config: Option<PathBuf> = None;
    let mut bind: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "healthcheck" => return healthcheck(),
            "--config" => match args.next() {
                Some(value) => config = Some(PathBuf::from(value)),
                None => return usage_error("--config requires a path"),
            },
            "--bind" => match args.next() {
                Some(value) => bind = Some(value),
                None => return usage_error("--bind requires an address"),
            },
            "--help" | "-h" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("republisherd {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => return usage_error(&format!("unknown argument '{other}'")),
        }
    }

    let options = match ServerOptions::from_env(config, bind) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("republisherd: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("republisherd: failed to start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("republisherd: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("republisherd: {message}\n\n{USAGE}");
    ExitCode::FAILURE
}

/// Container HEALTHCHECK entrypoint: no shell or curl exists in the image, so
/// the binary probes itself. Plain HTTP gets a real /healthz round trip; with
/// TLS enabled a successful TCP connect to the listener suffices.
fn healthcheck() -> ExitCode {
    use std::io::{Read, Write};

    let bind = std::env::var("REPUBLISHER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let port = bind.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
    let Some(port) = port else {
        eprintln!("healthcheck: cannot parse port from REPUBLISHER_BIND '{bind}'");
        return ExitCode::FAILURE;
    };
    let addr = format!("127.0.0.1:{port}");
    let timeout = std::time::Duration::from_secs(3);
    let stream = std::net::TcpStream::connect_timeout(
        &addr.parse().expect("loopback address always parses"),
        timeout,
    );
    let mut stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("healthcheck: connect {addr}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let tls_enabled = std::env::var("REPUBLISHER_TLS_CERT").is_ok();
    if tls_enabled {
        // Listener is up; a TLS handshake is out of scope for a static probe.
        return ExitCode::SUCCESS;
    }

    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if let Err(error) =
        stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
    {
        eprintln!("healthcheck: write: {error}");
        return ExitCode::FAILURE;
    }
    let mut response = String::new();
    if let Err(error) = stream.read_to_string(&mut response) {
        eprintln!("healthcheck: read: {error}");
        return ExitCode::FAILURE;
    }
    if response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200") {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "healthcheck: unexpected response: {}",
            response.lines().next().unwrap_or("<empty>")
        );
        ExitCode::FAILURE
    }
}
