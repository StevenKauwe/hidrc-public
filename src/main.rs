mod config;
mod hid;
mod keys;
mod measure;
mod prefs;
mod tap;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use config::Config;
use hid::Kind;

const LABEL: &str = "dev.hidrc";

const USAGE: &str = "hidrc: your macOS keyboard, mouse and trackpad settings as a config file

USAGE:
  hidrc capture > config.toml   Write the current settings out as a config
  hidrc plan                    Show what apply would change
  hidrc apply                   Apply the config now
  hidrc install                 Run the background agent at login (needed for
                                per-device mouse settings, remaps, scroll rewriting)
  hidrc uninstall               Remove the agent
  hidrc status                  Devices, permission and agent state
  hidrc measure [SECONDS]       Measure whether the mouse is really linear / 1:1
  hidrc run                     Run the agent in the foreground

Config: --config PATH, $HIDRC_CONFIG, ./config.toml, or ~/.config/hidrc/config.toml";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flag = args.iter().position(|a| a == "--config").map(|i| {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        v
    });
    let path = config::resolve(flag);
    let result = match args.first().map(String::as_str).unwrap_or("") {
        "capture" => capture(),
        "plan" => config::load(&path).and_then(|c| plan(&c).map(|_| ())),
        "apply" => config::load(&path).and_then(|c| apply(&c)),
        "run" => config::load(&path).and_then(run),
        "status" => status(&path),
        "install" => install(&path),
        "uninstall" => uninstall(),
        "measure" => measure::run(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20)),
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("hidrc: {e}");
        std::process::exit(1);
    }
}

fn run(cfg: Config) -> Result<(), String> {
    let live = hid::plan(&cfg)?;
    eprintln!("hidrc: running with {} live setting(s), scroll tap: {}", live.len(), cfg.needs_tap());

    // Devices come and go (Bluetooth sleep, replugging) and macOS pushes its
    // own values on connect, so re-apply periodically. Unchanged devices are
    // skipped, so this is cheap.
    std::thread::spawn(move || {
        let client = hid::Client::new();
        loop {
            for line in hid::apply(&client, &live) {
                eprintln!("hidrc: {line}");
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });

    if !cfg.needs_tap() {
        loop {
            std::thread::park();
        }
    }
    if !tap::trusted(true) {
        eprintln!("hidrc: waiting for Accessibility permission (System Settings > Privacy & Security > Accessibility)");
        while !tap::trusted(false) {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    eprintln!("hidrc: scroll tap active");
    tap::run(tap::Rules::from_config(&cfg.mouse))
}

/// Prints differences between the config and this Mac. Returns the number of
/// preferences that would change.
fn plan(cfg: &Config) -> Result<usize, String> {
    let mut changes = 0;
    println!("Preferences:");
    for p in cfg.prefs()? {
        if p.is_applied() {
            println!("  = {} = {}", p.label(), p.value);
        } else {
            changes += 1;
            let cur = p.current().unwrap_or_else(|| "(unset)".into());
            println!("  ~ {}: {cur} -> {}", p.label(), p.value);
        }
    }
    println!("Live device settings (applied by apply and kept by the agent):");
    for (kind, key, value) in hid::plan(cfg)? {
        println!("  {kind:?} {key} = {value:?}");
    }
    if cfg.needs_tap() {
        println!("Scroll rewriting (agent): {:?}", tap::Rules::from_config(&cfg.mouse));
    }
    println!("{changes} preference(s) to change.");
    Ok(changes)
}

fn apply(cfg: &Config) -> Result<(), String> {
    let mut relogin = Vec::new();
    for p in cfg.prefs()? {
        if !p.is_applied() {
            p.write()?;
            println!("set {} = {}", p.label(), p.value);
            if config::NEEDS_RELOGIN.contains(&p.key.as_str()) {
                relogin.push(p.key.clone());
            }
        }
    }
    for line in hid::apply(&hid::Client::new(), &hid::plan(cfg)?) {
        println!("{line}");
    }
    if agent_loaded() {
        launchctl(&["kickstart", "-k", &target()])?;
        println!("restarted agent");
    } else if cfg.needs_tap() || cfg.keyboard.remap.is_some() {
        println!("note: run `hidrc install` so mouse, remap and scroll settings survive replugging and reboots");
    }
    if !relogin.is_empty() {
        println!("note: log out and back in for: {}", relogin.join(", "));
    }
    Ok(())
}

fn line(out: &mut String, key: &str, value: Option<String>, effective: Option<String>, help: &str) {
    match value {
        Some(v) => writeln!(out, "{key} = {v}  # {help}"),
        None => writeln!(out, "# {key} = {}  # {help} (not set; macOS default)", effective.unwrap_or("?".into())),
    }
    .unwrap();
}

fn capture() -> Result<(), String> {
    print!("{}", capture_text());
    Ok(())
}

/// The current settings as a config file.
fn capture_text() -> String {
    let g = |k: &str| prefs::read(false, "NSGlobalDomain", k);
    let tp = |k: &str| prefs::read(false, "com.apple.AppleMultitouchTrackpad", k);
    let as_bool = |v: String| (v == "1").to_string();
    let as_float = |v: String| v.parse::<f64>().map(|f| format!("{f:?}")).unwrap_or(v);
    let as_ms = |v: String| v.parse::<f64>().map(|u| ((u * config::KEY_REPEAT_UNIT_MS).round() as i64).to_string()).unwrap_or(v);

    let client = hid::Client::new();
    let devices = client.devices();
    let hid_value = |kind: Kind, key: &str| devices.list.iter().filter(|(k, _)| *k == kind).find_map(|(_, s)| s.int(key));
    let hid_ms = |key| hid_value(Kind::Keyboard, key).map(|ns| (ns / 1_000_000).to_string());
    let hid_speed = |kind, key| hid_value(kind, key).map(|v| format!("{:?}", v as f64 / 65536.0));

    let mut o = String::from("# hidrc config, captured from this Mac. Commented lines are left as macOS has them.\n\n");
    o += "[keyboard]\n";
    line(&mut o, "key_repeat_ms", g("KeyRepeat").map(as_ms), hid_ms(hid::KEY_REPEAT), "time between repeats; macOS rounds to 1/60 s ticks, fastest in System Settings is 33");
    line(&mut o, "initial_delay_ms", g("InitialKeyRepeat").map(as_ms), hid_ms(hid::INITIAL_KEY_REPEAT), "hold time before repeating starts; shortest in System Settings is 250");
    line(&mut o, "press_and_hold", g("ApplePressAndHoldEnabled").map(as_bool), Some("true".into()), "false = holding a key repeats instead of showing accents");
    line(&mut o, "fn_keys_standard", g("com.apple.keyboard.fnState").map(as_bool), Some("false".into()), "true = F1-F12 without holding fn");
    o += "\n# [keyboard.remap]\n# caps_lock = \"escape\"\n\n";
    o += "[mouse]\n";
    line(&mut o, "linear", g("com.apple.mouse.linear").map(as_bool), Some("false".into()), "true = no pointer acceleration");
    line(&mut o, "speed", g("com.apple.mouse.scaling").map(as_float), hid_speed(Kind::Mouse, hid::MOUSE_SPEED), "tracking speed 0.0-3.0; with linear = true, 1.0 = exact 1:1");
    line(&mut o, "scroll_speed", g("com.apple.scrollwheel.scaling").map(as_float), hid_speed(Kind::Mouse, hid::WHEEL_SPEED), "wheel speed 0.0-1.0");
    o += "# scroll_reverse = true  # flip wheel direction on mice only (trackpad keeps scroll.natural)\n";
    o += "# scroll_lines = 3  # fixed lines per wheel notch, no wheel acceleration\n\n";
    o += "[trackpad]\n";
    line(&mut o, "speed", g("com.apple.trackpad.scaling").map(as_float), hid_speed(Kind::Trackpad, hid::TRACKPAD_SPEED), "tracking speed 0.0-3.0");
    line(&mut o, "tap_to_click", tp("Clicking").map(as_bool), Some("false".into()), "");
    line(&mut o, "three_finger_drag", tp("TrackpadThreeFingerDrag").map(as_bool), Some("false".into()), "");
    o += "\n[scroll]\n";
    line(&mut o, "natural", g("com.apple.swipescrolldirection").map(as_bool), Some("true".into()), "\"natural\" direction; global for trackpad and mouse");
    o += "\n# Any other preference: [defaults.\"<domain>\"] key = value\n# [defaults.\"com.apple.dock\"]\n# autohide = true\n";
    o
}

fn status(path: &PathBuf) -> Result<(), String> {
    let client = hid::Client::new();
    println!("Devices:");
    for (kind, s) in &client.devices().list {
        let props: Vec<String> = match kind {
            Kind::Keyboard => [hid::KEY_REPEAT, hid::INITIAL_KEY_REPEAT, hid::FKEY_MODE].iter().map(|k| format!("{k}={:?}", s.int(k))).collect(),
            Kind::Mouse => [hid::MOUSE_SPEED, hid::WHEEL_SPEED].iter().map(|k| format!("{k}={:?}", s.int(k))).collect(),
            Kind::Trackpad => vec![format!("{}={:?}", hid::TRACKPAD_SPEED, s.int(hid::TRACKPAD_SPEED))],
        };
        println!("  {:<9} {:<36} {}", format!("{kind:?}"), s.name(), props.join(" "));
    }
    println!("Accessibility granted: {}", tap::trusted(false));
    println!("Config: {}", path.display());
    println!("Agent loaded: {}", agent_loaded());
    Ok(())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME not set"))
}

fn bin_path() -> PathBuf {
    home().join(".local/bin/hidrc")
}

fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn target() -> String {
    extern "C" {
        fn getuid() -> u32;
    }
    format!("gui/{}/{LABEL}", unsafe { getuid() })
}

fn agent_loaded() -> bool {
    Command::new("launchctl").args(["print", &target()]).output().is_ok_and(|o| o.status.success())
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    let status = Command::new("launchctl").args(args).status().map_err(|e| e.to_string())?;
    status.success().then_some(()).ok_or(format!("launchctl {} failed", args.join(" ")))
}

fn install(path: &PathBuf) -> Result<(), String> {
    config::load(path)?;
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let bin = bin_path();
    std::fs::create_dir_all(bin.parent().unwrap()).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if exe != bin {
        // Remove first so the running agent's binary is replaced, not rewritten in place.
        let _ = std::fs::remove_file(&bin);
        std::fs::copy(&exe, &bin).map_err(|e| format!("copy to {}: {e}", bin.display()))?;
    }
    let log = home().join("Library/Logs/hidrc.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>run</string><string>--config</string><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        bin.display(),
        path.display(),
        log.display()
    );
    std::fs::create_dir_all(plist_path().parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(plist_path(), plist).map_err(|e| e.to_string())?;
    let domain = target().rsplit_once('/').unwrap().0.to_string();
    let _ = Command::new("launchctl").args(["bootout", &target()]).output();
    launchctl(&["bootstrap", &domain, plist_path().to_str().unwrap()])?;
    println!("Installed {} and started {LABEL}, using {}", bin.display(), path.display());
    println!("Log: {}", log.display());
    println!("If the config uses mouse scroll settings, allow hidrc under System Settings > Privacy & Security > Accessibility.");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let _ = Command::new("launchctl").args(["bootout", &target()]).output();
    let _ = std::fs::remove_file(plist_path());
    let _ = std::fs::remove_file(bin_path());
    println!("Removed {LABEL}. Preferences written by apply are kept; key remaps clear on next login.");
    Ok(())
}
