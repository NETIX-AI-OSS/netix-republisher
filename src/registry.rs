//! The one place protocols are compiled in; adding a protocol is a one-line
//! `register_republish` call shared by every binary.

use republish_core::RepublishRegistry;

/// Build the registry with every compiled-in republisher adapter.
pub fn build_registry() -> RepublishRegistry {
    let mut registry = RepublishRegistry::new();
    proto_bacnet::register_republish(&mut registry);
    proto_modbus::register_republish(&mut registry);
    proto_opcua::register_republish(&mut registry);
    registry
}
