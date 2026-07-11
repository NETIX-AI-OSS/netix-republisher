//! Generic industrial-protocol MQTT republisher (desktop GUI).
//!
//! One binary discovers/browses/polls and republishes from any registered
//! protocol, selected in the UI. Protocol adapters are compiled in via
//! [`republisher::registry::build_registry`].
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if let Err(error) = republish_core::run(republisher::registry::build_registry) {
        eprintln!("Republisher error: {error}");
        std::process::exit(1);
    }
}
