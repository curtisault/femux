//! `hosts` plugin: arrives in 0.6 (REQ-PLUG-004). The crate exists so the feature flag and
//! the registry entry are stable from the start.

pub fn plugin() -> femux_plugin::Plugin {
    femux_plugin::Plugin::new("hosts")
}
