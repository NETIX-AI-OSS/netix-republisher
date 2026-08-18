//! Shared library for the NETIX republisher binaries (desktop GUI `republisher` and headless daemon `republisherd`), both driving the same protocol registry and `republish-core` engine.

pub mod registry;

#[cfg(feature = "web")]
pub mod web;
