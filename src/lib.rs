//! Shared library for the NETIX republisher binaries: the desktop GUI
//! (`republisher`, feature `gui`) and the headless web-GUI daemon
//! (`republisherd`, feature `web`) both drive the same protocol registry and
//! `republish-core` worker engine.

pub mod registry;

#[cfg(feature = "web")]
pub mod web;
