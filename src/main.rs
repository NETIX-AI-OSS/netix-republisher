//! Generic industrial-protocol MQTT republisher.
//!
//! One binary discovers/browses/polls and republishes from any registered
//! protocol, selected in the UI. Protocol adapters are compiled in here and
//! registered with the [`RepublishRegistry`]; adding a protocol is a one-line
//! `register_republish` call.
#![cfg_attr(windows, windows_subsystem = "windows")]

use republish_core::RepublishRegistry;

/// Build the registry with every compiled-in republisher adapter.
fn build_registry() -> RepublishRegistry {
    let mut registry = RepublishRegistry::new();
    proto_bacnet::register_republish(&mut registry);
    proto_modbus::register_republish(&mut registry);
    proto_opcua::register_republish(&mut registry);
    registry
}

fn main() {
    if let Err(error) = republish_core::run(build_registry) {
        eprintln!("Republisher error: {error}");
        std::process::exit(1);
    }
}
