//! Generic industrial-protocol MQTT republisher (desktop GUI): discovers/browses/polls and republishes from any registered protocol selected in the UI.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if let Err(error) = republish_core::run(republisher::registry::build_registry) {
        eprintln!("Republisher error: {error}");
        std::process::exit(1);
    }
}
