# hidrc

Your macOS keyboard, mouse and trackpad settings as one config file in git,
plus a small Rust agent for the things macOS can't do on its own (the useful
parts of LinearMouse, Karabiner-lite remaps).

```toml
[keyboard]
key_repeat_ms = 33         # 2 ticks of 1/60 s, the fastest System Settings offers
initial_delay_ms = 250
press_and_hold = false

[keyboard.remap]
caps_lock = "escape"

[mouse]
linear = true              # no pointer acceleration (macOS's own toggle)
speed = 1.0                # with linear: 1 mouse count = 1 point, i.e. Windows 6/11
scroll_reverse = true      # traditional wheel direction on the mouse...
scroll_lines = 3           # ...with a fixed 3 lines per notch

[trackpad]
tap_to_click = true

[scroll]
natural = true             # ...while the trackpad stays "natural"
```

Every setting is optional. Anything not in the file is left as macOS has it.

## Quick start

With [just](https://github.com/casey/just) (`cargo install just`):

```sh
just capture   # write your current settings to config.toml
# edit config.toml
just plan      # what would change
just install   # apply, and run the agent at login
just           # list everything else (test, status, logs, ...)
```

Or directly: `cargo build --release`, then `target/release/hidrc capture > config.toml`,
`hidrc plan`, `hidrc apply`, `hidrc install`.

`install` copies the binary to `~/.local/bin/hidrc` and starts a LaunchAgent
pointing at **this checkout's** `config.toml`, so keep the repo somewhere stable.
After editing the config, run `hidrc apply` (it restarts the agent).

## How each setting is applied

| Setting | Persistent pref (`defaults`) | Live, per device (agent) |
|---|---|---|
| `keyboard.key_repeat_ms`, `initial_delay_ms` | `KeyRepeat`, `InitialKeyRepeat` (1/60 s ticks) | `HIDKeyRepeat`, `HIDInitialKeyRepeat` (ns) |
| `keyboard.press_and_hold` | `ApplePressAndHoldEnabled` (relaunch apps) | |
| `keyboard.fn_keys_standard` | `com.apple.keyboard.fnState` | `HIDFKeyMode` |
| `keyboard.remap` | | `UserKeyMapping` (same as `hidutil`) |
| `mouse.linear` | `com.apple.mouse.linear`, macOS's own toggle (log out/in when set by hidrc) | |
| `mouse.speed` | `com.apple.mouse.scaling` | `HIDMouseAcceleration`; with linear on, a plain multiplier: **1.0 = exact 1:1** |
| `mouse.scroll_speed` | `com.apple.scrollwheel.scaling` | `HIDMouseScrollAcceleration` |
| `mouse.scroll_reverse`, `scroll_lines` | | CGEventTap on wheel events (needs Accessibility) |
| `trackpad.speed` | `com.apple.trackpad.scaling` | `HIDTrackpadAcceleration` |
| `trackpad.tap_to_click`, `three_finger_drag` | trackpad domains (log out/in) | |
| `scroll.natural` | `com.apple.swipescrolldirection` (log out/in) | |
| `[defaults."<domain>"]` | anything else | |

Mouse settings only go to external mice. The built-in trackpad and Magic
Trackpad are never treated as mice. Scroll rewriting only touches wheel-notch
(non-continuous) events, so trackpad and Magic Mouse scrolling pass through.

## Tests

```sh
cargo test
```

The tests never change your settings. They cover the scroll rewrite logic,
run real in-memory `CGEvent`s through the same code path the agent uses
(including macOS's habit of recomputing deltas when a field is written), check
device classification against your actual hardware (read-only), and check
config/pref/key-code mapping and that a fresh `capture` round-trips.

## Permissions

Mouse scroll settings need **Accessibility** permission for the agent:
System Settings > Privacy & Security > Accessibility. After a rebuild and
reinstall, macOS may treat it as a new app; remove and re-add it if scrolling
stops being rewritten.

## Is it really 1:1?

`just measure` (needs Input Monitoring for your terminal) compares the raw
counts your mouse sends with how far the cursor moves, at slow, medium, fast
and flick speeds. Measured on a MacBook with the built-in "Pointer
acceleration" toggle off and the slider at its default:

```
slow   0.705   medium 0.684   fast 0.683   flick 0.688   ->  linear, gain 0.687
```

So macOS's own toggle really is linear, and the gain is just the slider value
(0.6875). `speed = 1.0` is exact 1:1: one count moves the cursor one point
(two physical pixels on a Retina display).
