//! The built-in plugin registry (REQ-PLUG-001): every plugin compiled in by its feature, then
//! filtered by `[plugins] enabled` at runtime.
use femux_plugin::Plugin;

/// Every plugin compiled into this binary.
#[allow(clippy::vec_init_then_push)]
pub fn compiled() -> Vec<Plugin> {
    let mut out: Vec<Plugin> = Vec::new();
    #[cfg(feature = "agents")]
    out.push(femux_plugin_agents::plugin());
    #[cfg(feature = "infobar")]
    out.push(femux_plugin_infobar::plugin());
    #[cfg(feature = "notify")]
    out.push(femux_plugin_notify::plugin());
    #[cfg(feature = "git")]
    out.push(femux_plugin_git::plugin());
    #[cfg(feature = "clipboard")]
    out.push(femux_plugin_clipboard::plugin());
    #[cfg(feature = "graphics")]
    out.push(femux_plugin_graphics::plugin());
    #[cfg(feature = "shell-integration")]
    out.push(femux_plugin_shell_integration::plugin());
    #[cfg(feature = "broadcast")]
    out.push(femux_plugin_broadcast::plugin());
    #[cfg(feature = "which-key")]
    out.push(femux_plugin_which_key::plugin());
    #[cfg(feature = "recorder")]
    out.push(femux_plugin_recorder::plugin());
    #[cfg(feature = "hosts")]
    out.push(femux_plugin_hosts::plugin());
    #[cfg(feature = "theme")]
    out.push(femux_plugin_theme::plugin());
    #[cfg(feature = "launcher")]
    out.push(femux_plugin_launcher::plugin());
    out
}

/// The compiled plugins named in `enabled`, in registry order.
pub fn enabled(enabled: &[String]) -> Vec<Plugin> {
    compiled()
        .into_iter()
        .filter(|p| enabled.iter().any(|e| e == p.name))
        .collect()
}
