//! The CLI's view of `config.toml`: parsing lives in `femux-config`; this turns it into what
//! the client and the daemon take.

use std::time::Duration;

use femux_client::config::{BarPosition, Borders, ClientConfig};
pub use femux_config::{Config, config_dir, home, state_dir};

/// The client's settings from the config file.
pub fn client(c: &Config) -> ClientConfig {
    ClientConfig {
        prefix: c.input.prefix.clone(),
        prefixless_nav: c.input.prefixless_nav,
        mouse: c.input.mouse,
        which_key_delay: Duration::from_millis(c.ui.which_key_delay_ms),
        borders: match c.layout.borders {
            femux_config::Borders::Single => Borders::Single,
            femux_config::Borders::Heavy => Borders::Heavy,
            femux_config::Borders::Double => Borders::Double,
            femux_config::Borders::Spaces => Borders::Spaces,
        },
        running_indicator: c.bar.running_indicator.clone(),
        theme: c.theme.name.clone(),
        keys_prefix: c.keys.prefix.clone(),
        keys_root: c.keys.root.clone(),
        keys_copy: c.keys.copy.clone(),
        bar: match c.bar.position {
            femux_config::BarPosition::Top => BarPosition::Top,
            femux_config::BarPosition::Bottom => BarPosition::Bottom,
        },
        bar_modules: c.bar.modules.clone(),
        bar_interval: Duration::from_millis(c.bar.interval_ms),
        panel_open: c.panel.open,
        panel_width: c.panel.width,
        panel_sections: c.panel.sections.clone(),
        kitty_graphics: c.terminal.kitty_graphics,
        sixel: c.terminal.sixel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_feeds_the_client() {
        let text = include_str!("../../../spec/config.example.toml");
        let cc = client(&Config::parse(text).unwrap());
        assert_eq!(cc.which_key_delay, Duration::from_millis(400));
        assert!(cc.mouse);
        assert_eq!(
            cc.keys_prefix.get("-").map(String::as_str),
            Some("split down")
        );
    }
}
