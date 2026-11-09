//! The config file schema. Every setting is optional: a setting that is
//! absent is left exactly as macOS has it.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::prefs::{Pref, Value};

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keyboard: Keyboard,
    pub mouse: Mouse,
    pub trackpad: Trackpad,
    pub scroll: Scroll,
    /// Escape hatch: any other preference, as `[defaults."<domain>"] key = value`.
    pub defaults: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Keyboard {
    /// Time between repeats while a key is held.
    pub key_repeat_ms: Option<u32>,
    /// Time a key must be held before it starts repeating.
    pub initial_delay_ms: Option<u32>,
    /// true: holding a letter shows the accent picker. false: it repeats.
    pub press_and_hold: Option<bool>,
    /// true: F1-F12 act as standard function keys without holding fn.
    pub fn_keys_standard: Option<bool>,
    /// Key remapping, e.g. caps_lock = "escape". An empty table clears all remaps.
    pub remap: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Mouse {
    /// Turn pointer acceleration off: macOS's own toggle in System Settings >
    /// Mouse > Advanced. Verified linear on this Mac with `hidrc measure`.
    pub linear: Option<bool>,
    /// Tracking speed, 0.0-3.0 (the System Settings slider). With `linear =
    /// true` it is a plain multiplier: 1.0 = one mouse count per point (1:1).
    pub speed: Option<f64>,
    /// Scroll wheel speed, 0.0-1.0.
    pub scroll_speed: Option<f64>,
    /// Reverse the wheel direction for mice only (trackpad keeps `scroll.natural`).
    pub scroll_reverse: Option<bool>,
    /// Scroll a fixed number of lines per wheel notch, without wheel acceleration.
    pub scroll_lines: Option<i64>,
    /// Points per line for `scroll_lines` in pixel-based apps. Default 10.
    pub pixels_per_line: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Trackpad {
    /// Tracking speed, 0.0-3.0.
    pub speed: Option<f64>,
    pub tap_to_click: Option<bool>,
    pub three_finger_drag: Option<bool>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Scroll {
    /// "Natural" scrolling. Global in macOS: applies to mice too unless
    /// `mouse.scroll_reverse` flips them back.
    pub natural: Option<bool>,
}

const GLOBAL: &str = "NSGlobalDomain";
const TRACKPAD_DOMAINS: [&str; 2] = [
    "com.apple.AppleMultitouchTrackpad",
    "com.apple.driver.AppleBluetoothMultitouch.trackpad",
];

/// Preferences that only take effect after logging out and back in.
pub const NEEDS_RELOGIN: [&str; 5] = [
    // System Settings applies this one live through a private call; written
    // with `defaults` it is picked up at the next login.
    "com.apple.mouse.linear",
    "com.apple.swipescrolldirection",
    "Clicking",
    "TrackpadThreeFingerDrag",
    "com.apple.mouse.tapBehavior",
];

/// macOS stores key repeat timings in ticks of 1/60 s, not the often-quoted
/// 15 ms: KeyRepeat = 2 shows up on devices as HIDKeyRepeat = 33333333 ns.
pub const KEY_REPEAT_UNIT_MS: f64 = 1000.0 / 60.0;

/// Milliseconds to the nearest whole tick (at least one).
pub fn key_repeat_ticks(ms: u32) -> i64 {
    (ms as f64 / KEY_REPEAT_UNIT_MS).round().max(1.0) as i64
}

/// The exact nanoseconds macOS uses for a tick count.
pub fn ticks_to_ns(ticks: i64) -> i64 {
    ticks * 1_000_000_000 / 60
}

impl Config {
    /// The preference writes this config asks for.
    pub fn prefs(&self) -> Result<Vec<Pref>, String> {
        let mut out = Vec::new();
        let mut global = |key: &str, value: Option<Value>| {
            if let Some(v) = value {
                out.push(Pref::new(GLOBAL, key, v));
            }
        };
        let units = |ms: u32| Value::Int(key_repeat_ticks(ms));

        let k = &self.keyboard;
        global("KeyRepeat", k.key_repeat_ms.map(units));
        global("InitialKeyRepeat", k.initial_delay_ms.map(units));
        global("ApplePressAndHoldEnabled", k.press_and_hold.map(Value::Bool));
        global("com.apple.keyboard.fnState", k.fn_keys_standard.map(Value::Bool));
        global("com.apple.mouse.linear", self.mouse.linear.map(Value::Bool));
        global("com.apple.mouse.scaling", self.mouse.speed.map(Value::Float));
        global("com.apple.scrollwheel.scaling", self.mouse.scroll_speed.map(Value::Float));
        global("com.apple.trackpad.scaling", self.trackpad.speed.map(Value::Float));
        global("com.apple.swipescrolldirection", self.scroll.natural.map(Value::Bool));

        if let Some(tap) = self.trackpad.tap_to_click {
            for d in TRACKPAD_DOMAINS {
                out.push(Pref::new(d, "Clicking", Value::Bool(tap)));
            }
            out.push(Pref::new(GLOBAL, "com.apple.mouse.tapBehavior", Value::Int(tap as i64)).current_host());
        }
        if let Some(drag) = self.trackpad.three_finger_drag {
            for d in TRACKPAD_DOMAINS {
                out.push(Pref::new(d, "TrackpadThreeFingerDrag", Value::Bool(drag)));
            }
        }

        for (domain, keys) in &self.defaults {
            let domain = match domain.as_str() {
                "-g" | "global" | "NSGlobalDomain" => GLOBAL,
                d => d,
            };
            for (key, value) in keys {
                let v = Value::from_toml(value).map_err(|e| format!("defaults.{domain}.{key}: {e}"))?;
                out.push(Pref::new(domain, key, v));
            }
        }
        Ok(out)
    }

    /// Whether the scroll event tap is needed.
    pub fn needs_tap(&self) -> bool {
        self.mouse.scroll_reverse == Some(true) || self.mouse.scroll_lines.is_some()
    }
}

/// `--config PATH`, else $HIDRC_CONFIG, else ./config.toml, else ~/.config/hidrc/config.toml.
pub fn resolve(flag: Option<String>) -> PathBuf {
    if let Some(p) = flag.or_else(|| std::env::var("HIDRC_CONFIG").ok()) {
        return PathBuf::from(p);
    }
    let local = PathBuf::from("config.toml");
    if local.exists() {
        return std::fs::canonicalize(local).unwrap();
    }
    PathBuf::from(std::env::var("HOME").expect("HOME not set")).join(".config/hidrc/config.toml")
}

pub fn load(path: &PathBuf) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!("{}: {e}\n  (create one with: hidrc capture > config.toml)", path.display())
    })?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}
