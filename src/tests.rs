//! Tests. Everything here is read-only with respect to the Mac's settings:
//! scroll tests build synthetic CGEvents in memory (never posted), HID tests
//! only read device properties, and pref tests only call `defaults read`.

use crate::config::{self, Config};
use crate::hid::{self, Kind, Live};
use crate::keys;
use crate::prefs::{Pref, Value};
use crate::tap::{self, Axis, Rules, Wheel};

fn rules(reverse: bool, lines: Option<i64>) -> Rules {
    Rules { reverse, lines, pixels_per_line: 10.0 }
}

fn axis(lines: i64, fixed: f64, points: i64) -> Axis {
    Axis { lines, fixed, points }
}

/// A wheel notch as macOS reports it: vertical axis, horizontal axis.
fn notch(v: Axis, h: Axis) -> Wheel {
    Wheel { continuous: false, axes: [v, h] }
}

const NONE: Axis = Axis { lines: 0, fixed: 0.0, points: 0 };

// ---------- scroll: pure transform ----------

mod scroll {
    use super::*;

    #[test]
    fn trackpad_and_magic_mouse_scrolling_is_never_touched() {
        let trackpad = Wheel { continuous: true, axes: [axis(-2, -2.5, -25), axis(1, 0.5, 5)] };
        for r in [rules(true, None), rules(false, Some(3)), rules(true, Some(3))] {
            assert_eq!(tap::transform(trackpad, &r), trackpad, "{r:?}");
        }
    }

    #[test]
    fn no_rules_is_identity() {
        let w = notch(axis(-1, -3.7, -37), NONE);
        assert_eq!(tap::transform(w, &rules(false, None)), w);
    }

    #[test]
    fn reverse_flips_wheel_down_to_up_and_back() {
        let down = notch(axis(-1, -1.0, -10), NONE);
        let up = notch(axis(1, 1.0, 10), NONE);
        assert_eq!(tap::transform(down, &rules(true, None)), up);
        assert_eq!(tap::transform(up, &rules(true, None)), down);
    }

    #[test]
    fn reverse_keeps_macos_acceleration_magnitude() {
        let fast = notch(axis(-4, -6.25, -62), NONE);
        let out = tap::transform(fast, &rules(true, None));
        assert_eq!(out.axes[0], axis(4, 6.25, 62));
    }

    #[test]
    fn reverse_twice_is_identity() {
        let w = notch(axis(-3, -4.5, -45), axis(1, 0.75, 7));
        let r = rules(true, None);
        assert_eq!(tap::transform(tap::transform(w, &r), &r), w);
    }

    #[test]
    fn linear_gives_same_distance_for_slow_and_fast_spins() {
        let r = rules(false, Some(3));
        let slow = notch(axis(-1, -1.0, -10), NONE);
        let fast = notch(axis(-1, -8.0, -80), NONE);
        let faster = notch(axis(-9, -20.0, -200), NONE);
        let expected = axis(-3, -3.0, -30);
        for w in [slow, fast, faster] {
            assert_eq!(tap::transform(w, &r).axes[0], expected, "{w:?}");
        }
    }

    #[test]
    fn linear_keeps_direction() {
        let r = rules(false, Some(3));
        assert_eq!(tap::transform(notch(axis(1, 2.0, 20), NONE), &r).axes[0], axis(3, 3.0, 30));
        assert_eq!(tap::transform(notch(axis(-1, -2.0, -20), NONE), &r).axes[0], axis(-3, -3.0, -30));
    }

    #[test]
    fn linear_and_reverse_together() {
        let r = rules(true, Some(5));
        let down = notch(axis(-1, -2.0, -20), NONE);
        assert_eq!(tap::transform(down, &r).axes[0], axis(5, 5.0, 50));
    }

    #[test]
    fn pixels_per_line_scales_point_delta() {
        let r = Rules { reverse: false, lines: Some(2), pixels_per_line: 16.0 };
        assert_eq!(tap::transform(notch(axis(1, 1.0, 10), NONE), &r).axes[0].points, 32);
    }

    #[test]
    fn sub_line_movement_uses_fixed_point_sign() {
        // High-resolution wheels can report a fraction of a line with lines == 0.
        let w = notch(axis(0, -0.25, -2), NONE);
        assert_eq!(tap::transform(w, &rules(true, None)).axes[0], axis(0, 0.25, 2));
        assert_eq!(tap::transform(w, &rules(false, Some(3))).axes[0], axis(-3, -3.0, -30));
    }

    #[test]
    fn idle_axis_gets_no_movement_added() {
        let w = notch(axis(-1, -1.0, -10), NONE);
        for r in [rules(true, None), rules(false, Some(3)), rules(true, Some(3))] {
            assert_eq!(tap::transform(w, &r).axes[1], NONE, "{r:?}");
        }
    }

    #[test]
    fn horizontal_tilt_wheel_is_handled_independently() {
        let tilt = notch(NONE, axis(1, 1.5, 15));
        let out = tap::transform(tilt, &rules(true, Some(2)));
        assert_eq!(out.axes[0], NONE);
        assert_eq!(out.axes[1], axis(-2, -2.0, -20));
    }

    #[test]
    fn rules_from_config() {
        let cfg: Config = toml::from_str("[mouse]\nscroll_reverse = true\nscroll_lines = 4").unwrap();
        assert_eq!(Rules::from_config(&cfg.mouse), Rules { reverse: true, lines: Some(4), pixels_per_line: 10.0 });
        assert_eq!(Rules::from_config(&Config::default().mouse), rules(false, None));
    }
}

// ---------- scroll: real CGEvents ----------

mod cg_event {
    use super::*;
    use crate::tap::{EventRef, CGEventGetIntegerValueField, CGEventSetIntegerValueField, IS_CONTINUOUS};
    use core_foundation_sys::base::CFRelease;
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> EventRef;
        fn CGEventGetType(event: EventRef) -> u32;
    }

    const LINE_UNITS: u32 = 1;

    /// An in-memory scroll event, as a mouse wheel would produce. Never posted.
    struct Event(EventRef);

    impl Event {
        fn wheel(vertical: i32, horizontal: i32) -> Event {
            let e = unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null(), LINE_UNITS, 2, vertical, horizontal, 0) };
            assert!(!e.is_null());
            Event(e)
        }
        fn read(&self) -> Wheel {
            unsafe { tap::read_wheel(self.0) }
        }
        fn apply(&self, r: &Rules) {
            unsafe { tap::apply_to_event(self.0, r) }
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0 as _) }
        }
    }

    #[test]
    fn synthetic_wheel_event_looks_like_a_mouse() {
        let e = Event::wheel(-1, 0);
        assert_eq!(unsafe { CGEventGetType(e.0) }, 22);
        let w = e.read();
        assert!(!w.continuous);
        assert_eq!(w.axes[0].lines, -1);
        assert!(w.axes[0].fixed < 0.0 && w.axes[0].points < 0, "{w:?}");
        assert_eq!(w.axes[1], NONE);
    }

    #[test]
    fn reverse_on_a_real_event_flips_every_field() {
        let e = Event::wheel(-1, 0);
        let before = e.read();
        e.apply(&rules(true, None));
        let after = e.read();
        let [b, a] = [before.axes[0], after.axes[0]];
        assert_eq!(a.lines, -b.lines);
        assert_eq!(a.fixed, -b.fixed);
        assert_eq!(a.points, -b.points);
        assert_eq!(after.axes[1], NONE);
    }

    #[test]
    fn linear_on_a_real_event_survives_macos_field_recompute() {
        // macOS recalculates the fixed-point and point deltas when the line
        // delta is written; the written event must still read back exactly.
        for (v, h) in [(-1, 0), (1, 0), (-7, 0), (0, 1), (2, -3)] {
            let e = Event::wheel(v, h);
            let r = rules(true, Some(3));
            let expected = tap::transform(e.read(), &r);
            e.apply(&r);
            assert_eq!(e.read(), expected, "wheel ({v}, {h})");
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let e = Event::wheel(1, 0);
        let w = notch(axis(-4, -4.0, -48), axis(2, 2.0, 24));
        unsafe { tap::write_wheel(e.0, &w) };
        assert_eq!(e.read(), w);
    }

    #[test]
    fn continuous_real_event_is_left_alone() {
        let e = Event::wheel(-2, 1);
        unsafe { CGEventSetIntegerValueField(e.0, IS_CONTINUOUS, 1) };
        let before = e.read();
        e.apply(&rules(true, Some(3)));
        assert_eq!(e.read(), before);
        assert_eq!(unsafe { CGEventGetIntegerValueField(e.0, IS_CONTINUOUS) }, 1);
    }
}

// ---------- pointer / HID ----------

mod pointer {
    use super::*;
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    #[test]
    fn trackpads_and_built_in_input_are_not_treated_as_mice() {
        assert_eq!(hid::classify_pointer("Apple Internal Keyboard / Trackpad", true), Kind::Trackpad);
        assert_eq!(hid::classify_pointer("Magic Trackpad", false), Kind::Trackpad);
        assert_eq!(hid::classify_pointer("Magic Trackpad 2", false), Kind::Trackpad);
        assert_eq!(hid::classify_pointer("anything built in", true), Kind::Trackpad);
    }

    #[test]
    fn external_mice_are_mice() {
        // "USB Receiver" is how a Logitech Bolt/Unifying receiver names itself.
        for name in ["USB Receiver", "MX Master 3S", "USB Optical Mouse", "Razer DeathAdder V3", "Magic Mouse", "(unnamed)"] {
            assert_eq!(hid::classify_pointer(name, false), Kind::Mouse, "{name}");
        }
    }

    #[test]
    fn fixed_point_matches_macos_encoding() {
        // 45056 is the stock HIDMouseAcceleration seen on this Mac (0.6875).
        assert_eq!(hid::fixed(0.6875), 45056);
        assert_eq!(hid::fixed(1.0), 65536);
        assert_eq!(hid::fixed(0.3125), 20480);
        assert_eq!(hid::fixed(3.0), 196608);
    }

    fn mouse_plan(toml_text: &str) -> Vec<(Kind, &'static str, Live)> {
        hid::plan(&toml::from_str(toml_text).unwrap()).unwrap()
    }

    #[test]
    fn linear_is_macos_own_pref_not_a_device_override() {
        // Measured: macOS's toggle is already linear, so hidrc must not force
        // the old -1 curve or a resolution hack on the device.
        assert!(mouse_plan("[mouse]\nlinear = true").is_empty());
        let cfg: Config = toml::from_str("[mouse]\nlinear = true").unwrap();
        let prefs = cfg.prefs().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!((prefs[0].key.as_str(), &prefs[0].value), ("com.apple.mouse.linear", &Value::Bool(true)));
    }

    #[test]
    fn one_to_one_speed_sets_slider_and_live_gain_to_exactly_one() {
        let cfg: Config = toml::from_str("[mouse]\nlinear = true\nspeed = 1.0").unwrap();
        assert_eq!(hid::plan(&cfg).unwrap(), vec![(Kind::Mouse, hid::MOUSE_SPEED, Live::Int(65536))]);
        let prefs = cfg.prefs().unwrap();
        assert!(prefs.iter().any(|p| p.key == "com.apple.mouse.scaling" && p.value == Value::Float(1.0)));
    }

    #[test]
    fn measured_gain_matches_the_slider_value() {
        // hidrc measure on this Mac: slider 0.6875 -> 0.687 points per count.
        assert_eq!(hid::fixed(0.6875), 45056);
        assert!((45056.0 / 65536.0 - 0.687_f64).abs() < 0.001);
    }

    #[test]
    fn mouse_settings_only_target_mice() {
        let plan = mouse_plan("[mouse]\nlinear = true\nspeed = 1.5\nscroll_speed = 0.5\nscroll_reverse = true");
        assert_eq!(
            plan,
            vec![(Kind::Mouse, hid::MOUSE_SPEED, Live::Int(98304)), (Kind::Mouse, hid::WHEEL_SPEED, Live::Int(32768))]
        );
    }

    /// Guards the key choice against real hardware: each connected mouse must
    /// say its acceleration lives in the property hidrc sets.
    #[test]
    fn connected_mice_use_the_acceleration_key_hidrc_sets() {
        let client = hid::Client::new();
        for (kind, s) in &client.devices().list {
            if *kind == Kind::Mouse {
                if let Some(t) = s.string("HIDPointerAccelerationType") {
                    assert_eq!(t, hid::MOUSE_SPEED, "{}", s.name());
                }
                assert!(s.int(hid::MOUSE_SPEED).is_some(), "{} has no {}", s.name(), hid::MOUSE_SPEED);
            }
        }
    }

    #[test]
    fn trackpad_speed_only_targets_trackpads() {
        let cfg: Config = toml::from_str("[trackpad]\nspeed = 2.0").unwrap();
        assert_eq!(hid::plan(&cfg).unwrap(), vec![(Kind::Trackpad, hid::TRACKPAD_SPEED, Live::Int(131072))]);
    }

    #[test]
    fn empty_config_touches_nothing() {
        let cfg = Config::default();
        assert!(hid::plan(&cfg).unwrap().is_empty());
        assert!(cfg.prefs().unwrap().is_empty());
        assert!(!cfg.needs_tap());
    }

    #[test]
    fn keyboard_timings_are_whole_ticks_in_nanoseconds() {
        // Observed on this Mac: KeyRepeat = 2 -> 33333333 ns, InitialKeyRepeat = 25 -> 416666666 ns.
        let cfg: Config = toml::from_str("[keyboard]\nkey_repeat_ms = 33\ninitial_delay_ms = 417").unwrap();
        assert_eq!(
            hid::plan(&cfg).unwrap(),
            vec![
                (Kind::Keyboard, hid::KEY_REPEAT, Live::Int(33_333_333)),
                (Kind::Keyboard, hid::INITIAL_KEY_REPEAT, Live::Int(416_666_666)),
            ]
        );
    }

    #[test]
    fn remap_becomes_user_key_mapping_dictionaries() {
        let cfg: Config = toml::from_str("[keyboard.remap]\ncaps_lock = \"escape\"").unwrap();
        let plan = hid::plan(&cfg).unwrap();
        assert_eq!(plan, vec![(Kind::Keyboard, hid::KEY_MAPPING, Live::Remap(vec![(0x7_0000_0039, 0x7_0000_0029)]))]);

        let cf = hid::to_cf(&plan[0].2);
        let arr = cf.downcast::<CFArray>().expect("array");
        assert_eq!(arr.len(), 1);
        let item = unsafe { CFDictionary::<CFString, CFNumber>::wrap_under_get_rule(*arr.get(0).unwrap() as _) };
        let get = |k: &str| item.find(CFString::new(k)).and_then(|n| n.to_i64());
        assert_eq!(get("HIDKeyboardModifierMappingSrc"), Some(0x7_0000_0039));
        assert_eq!(get("HIDKeyboardModifierMappingDst"), Some(0x7_0000_0029));
    }

    #[test]
    fn empty_remap_clears_mappings() {
        let cfg: Config = toml::from_str("[keyboard.remap]").unwrap();
        assert_eq!(hid::plan(&cfg).unwrap(), vec![(Kind::Keyboard, hid::KEY_MAPPING, Live::Remap(vec![]))]);
    }

    #[test]
    fn bad_remap_key_is_an_error() {
        let cfg: Config = toml::from_str("[keyboard.remap]\ncaps_lok = \"escape\"").unwrap();
        assert!(hid::plan(&cfg).unwrap_err().contains("caps_lok"));
    }

    /// Reads (never writes) this Mac's devices, to check the FFI and the
    /// classification against real hardware.
    #[test]
    fn real_devices_enumerate_and_built_in_trackpad_is_not_a_mouse() {
        let client = hid::Client::new();
        let devices = client.devices();
        assert!(devices.list.iter().any(|(k, _)| *k == Kind::Keyboard), "no keyboard found");
        for (kind, s) in &devices.list {
            if s.name().contains("Internal") && *kind != Kind::Keyboard {
                assert_eq!(*kind, Kind::Trackpad, "{}", s.name());
            }
        }
        // Every connected external, non-trackpad pointer must be classified as a mouse.
        for (kind, s) in &devices.list {
            if *kind != Kind::Keyboard && s.int("Built-In") == Some(0) && !s.name().to_lowercase().contains("trackpad") {
                assert_eq!(*kind, Kind::Mouse, "{}", s.name());
            }
        }
        let repeat = devices.list.iter().find_map(|(k, s)| (*k == Kind::Keyboard).then(|| s.int(hid::KEY_REPEAT)).flatten());
        assert!(repeat.is_some_and(|ns| ns > 1_000_000), "HIDKeyRepeat should be nanoseconds: {repeat:?}");
    }
}

// ---------- config and prefs ----------

mod settings {
    use super::*;

    fn find<'a>(prefs: &'a [Pref], key: &str) -> Vec<&'a Pref> {
        prefs.iter().filter(|p| p.key == key).collect()
    }

    #[test]
    fn typos_are_errors_not_silently_ignored() {
        assert!(toml::from_str::<Config>("[mouse]\nscrol_reverse = true").is_err());
        assert!(toml::from_str::<Config>("[mouce]\nlinear = true").is_err());
    }

    #[test]
    fn key_repeat_converts_to_sixtieth_second_ticks() {
        let cfg: Config = toml::from_str("[keyboard]\nkey_repeat_ms = 33\ninitial_delay_ms = 250").unwrap();
        let prefs = cfg.prefs().unwrap();
        assert_eq!(find(&prefs, "KeyRepeat")[0].value, Value::Int(2));
        assert_eq!(find(&prefs, "InitialKeyRepeat")[0].value, Value::Int(15));
    }

    #[test]
    fn pref_and_live_key_repeat_agree_for_every_ms() {
        // Whatever ms the user writes, the pref ticks and the live ns must
        // describe the same delay, or macOS and the agent would fight.
        for ms in 1..2000 {
            let ticks = config::key_repeat_ticks(ms);
            assert_eq!(hid::ms_to_ns(ms), config::ticks_to_ns(ticks), "{ms} ms");
        }
    }

    /// Against this Mac: a fresh capture must reproduce the key repeat the
    /// keyboards are actually using, to the nanosecond.
    #[test]
    fn captured_key_repeat_matches_live_devices() {
        let cfg: Config = toml::from_str(&crate::capture_text()).unwrap();
        let client = hid::Client::new();
        let devices = client.devices();
        let live = |key| devices.list.iter().find_map(|(k, s)| (*k == Kind::Keyboard).then(|| s.int(key)).flatten());
        if let Some(ms) = cfg.keyboard.key_repeat_ms {
            assert_eq!(Some(hid::ms_to_ns(ms)), live(hid::KEY_REPEAT));
        }
        if let Some(ms) = cfg.keyboard.initial_delay_ms {
            assert_eq!(Some(hid::ms_to_ns(ms)), live(hid::INITIAL_KEY_REPEAT));
        }
    }

    #[test]
    fn key_repeat_never_rounds_to_zero() {
        let cfg: Config = toml::from_str("[keyboard]\nkey_repeat_ms = 1").unwrap();
        assert_eq!(cfg.prefs().unwrap()[0].value, Value::Int(1));
    }

    #[test]
    fn mouse_prefs_use_global_domain() {
        let cfg: Config = toml::from_str("[mouse]\nlinear = true\n[trackpad]\nspeed = 1.25\n[scroll]\nnatural = false").unwrap();
        let prefs = cfg.prefs().unwrap();
        let linear = find(&prefs, "com.apple.mouse.linear")[0];
        assert_eq!((linear.domain.as_str(), &linear.value), ("NSGlobalDomain", &Value::Bool(true)));
        assert_eq!(find(&prefs, "com.apple.trackpad.scaling")[0].value, Value::Float(1.25));
        assert_eq!(find(&prefs, "com.apple.swipescrolldirection")[0].value, Value::Bool(false));
    }

    #[test]
    fn mouse_scroll_reverse_is_not_a_global_pref() {
        // Flipping the global direction would also flip the trackpad.
        let cfg: Config = toml::from_str("[mouse]\nscroll_reverse = true").unwrap();
        assert!(cfg.prefs().unwrap().is_empty());
        assert!(cfg.needs_tap());
    }

    #[test]
    fn tap_to_click_sets_built_in_bluetooth_and_host_prefs() {
        let cfg: Config = toml::from_str("[trackpad]\ntap_to_click = true").unwrap();
        let prefs = cfg.prefs().unwrap();
        let clicking = find(&prefs, "Clicking");
        assert_eq!(clicking.len(), 2);
        assert!(clicking.iter().all(|p| p.value == Value::Bool(true)));
        let host = find(&prefs, "com.apple.mouse.tapBehavior");
        assert!(host[0].current_host);
        assert_eq!(host[0].value, Value::Int(1));
    }

    #[test]
    fn defaults_escape_hatch() {
        let cfg: Config = toml::from_str(
            "[defaults.\"com.apple.dock\"]\nautohide = true\ntilesize = 48\n[defaults.global]\nAppleInterfaceStyle = \"Dark\"",
        )
        .unwrap();
        let prefs = cfg.prefs().unwrap();
        assert_eq!(find(&prefs, "autohide")[0].domain, "com.apple.dock");
        assert_eq!(find(&prefs, "tilesize")[0].value, Value::Int(48));
        assert_eq!(find(&prefs, "AppleInterfaceStyle")[0].domain, "NSGlobalDomain");
    }

    #[test]
    fn defaults_rejects_arrays() {
        let cfg: Config = toml::from_str("[defaults.\"com.x\"]\nk = [1, 2]").unwrap();
        assert!(cfg.prefs().is_err());
    }

    #[test]
    fn captured_config_parses_and_matches_this_mac() {
        let text = crate::capture_text();
        let cfg: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        // Every global setting the capture made active must already hold, so
        // applying a fresh capture changes nothing there.
        for p in cfg.prefs().unwrap().iter().filter(|p| p.domain == "NSGlobalDomain" && !p.current_host) {
            assert!(p.is_applied(), "{} = {} not applied", p.label(), p.value);
        }
    }

    #[test]
    fn config_path_flag_wins() {
        assert_eq!(config::resolve(Some("/x/y.toml".into())), std::path::PathBuf::from("/x/y.toml"));
    }
}

// ---------- key names ----------

mod key_names {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys() {
        assert_eq!(keys::usage("a").unwrap(), 0x7_0000_0004);
        assert_eq!(keys::usage("z").unwrap(), 0x7_0000_001D);
        assert_eq!(keys::usage("1").unwrap(), 0x7_0000_001E);
        assert_eq!(keys::usage("9").unwrap(), 0x7_0000_0026);
        assert_eq!(keys::usage("0").unwrap(), 0x7_0000_0027);
        assert_eq!(keys::usage("f1").unwrap(), 0x7_0000_003A);
        assert_eq!(keys::usage("f12").unwrap(), 0x7_0000_0045);
    }

    #[test]
    fn named_keys_match_hidutil_codes() {
        assert_eq!(keys::usage("caps_lock").unwrap(), 0x7_0000_0039);
        assert_eq!(keys::usage("Caps Lock").unwrap(), 0x7_0000_0039);
        assert_eq!(keys::usage("escape").unwrap(), 0x7_0000_0029);
        assert_eq!(keys::usage("left_control").unwrap(), 0x7_0000_00E0);
        assert_eq!(keys::usage("right_command").unwrap(), 0x7_0000_00E7);
        assert_eq!(keys::usage("fn").unwrap(), 0xFF_0000_0003);
    }

    #[test]
    fn hex_codes() {
        assert_eq!(keys::usage("0x39").unwrap(), 0x7_0000_0039);
        assert_eq!(keys::usage("0x700000039").unwrap(), 0x7_0000_0039);
    }

    #[test]
    fn unknown_names_and_f13_are_errors() {
        assert!(keys::usage("hyper").is_err());
        assert!(keys::usage("f13").is_err());
        assert!(keys::usage("0xZZ").is_err());
    }

    #[test]
    fn pref_values_compare_against_defaults_read_output() {
        assert!(Value::Bool(true).matches("1"));
        assert!(Value::Bool(false).matches("0"));
        assert!(!Value::Bool(true).matches("0"));
        assert!(Value::Int(2).matches("2"));
        assert!(Value::Float(0.6875).matches("0.6875"));
        assert!(Value::Float(1.0).matches("1"));
        assert!(!Value::Float(1.0).matches("1.5"));
        assert!(Value::Str("Dark".into()).matches("Dark"));
    }
}

// ---------- 1:1 measurement analysis ----------

mod measurement {
    use crate::measure::{analyze, Sample};

    const W: f64 = 0.05;

    /// Windows at a spread of speeds, moved through `curve(counts) -> points`.
    fn sweep(curve: impl Fn(f64) -> f64) -> Vec<Sample> {
        let mut out = Vec::new();
        for counts_per_window in [3.0, 6.0, 10.0, 14.0, 20.0, 40.0, 60.0, 70.0, 100.0, 150.0, 240.0, 300.0, 400.0, 600.0] {
            for _ in 0..6 {
                out.push(Sample { counts: counts_per_window, points: curve(counts_per_window) });
            }
        }
        out
    }

    #[test]
    fn exact_one_to_one_is_detected() {
        let r = analyze(&sweep(|c| c), W).unwrap();
        assert!(r.is_linear() && r.is_one_to_one(), "{r:?}");
        assert_eq!(r.bands.len(), 4);
    }

    #[test]
    fn linear_but_not_one_to_one() {
        let r = analyze(&sweep(|c| c * 1.6), W).unwrap();
        assert!(r.is_linear(), "{r:?}");
        assert!(!r.is_one_to_one());
        assert!((r.overall - 1.6).abs() < 1e-9);
    }

    #[test]
    fn macos_style_acceleration_is_detected() {
        // Gain rising with speed, like the default curve.
        let r = analyze(&sweep(|c| c * (0.5 + c / 200.0)), W).unwrap();
        assert!(!r.is_linear(), "{r:?}");
        assert!(r.spread > 2.0);
    }

    #[test]
    fn slight_acceleration_is_still_caught() {
        let r = analyze(&sweep(|c| c * (1.0 + c / 3000.0)), W).unwrap();
        assert!(!r.is_linear(), "{r:?}");
    }

    #[test]
    fn idle_windows_are_ignored() {
        let mut s = sweep(|c| c);
        s.extend(std::iter::repeat(Sample { counts: 0.0, points: 0.0 }).take(500));
        assert!(analyze(&s, W).unwrap().is_one_to_one());
    }

    #[test]
    fn no_movement_gives_no_report() {
        assert!(analyze(&[Sample { counts: 0.0, points: 0.0 }; 10], W).is_none());
    }

    #[test]
    fn sparse_bands_are_dropped() {
        let s: Vec<Sample> = (0..20).map(|_| Sample { counts: 5.0, points: 5.0 }).chain([Sample { counts: 500.0, points: 900.0 }]).collect();
        let r = analyze(&s, W).unwrap();
        assert_eq!(r.bands.len(), 1, "{r:?}");
    }
}
