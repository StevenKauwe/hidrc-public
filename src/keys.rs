//! Key names for `[keyboard.remap]`, as HID usages in the form macOS's
//! UserKeyMapping (and `hidutil`) expects: (usage page << 32) | usage.

const KEYBOARD_PAGE: u64 = 0x7_0000_0000;

pub fn usage(name: &str) -> Result<u64, String> {
    let n = name.trim().to_lowercase().replace(['-', ' '], "_");
    if let Some(hex) = n.strip_prefix("0x") {
        let v = u64::from_str_radix(hex, 16).map_err(|_| format!("bad hex key code {name:?}"))?;
        return Ok(if v > 0xFFFF { v } else { KEYBOARD_PAGE | v });
    }
    let bytes = n.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        return Ok(KEYBOARD_PAGE | (0x04 + (bytes[0] - b'a') as u64));
    }
    if bytes.len() == 1 && bytes[0].is_ascii_digit() {
        let d = (bytes[0] - b'0') as u64;
        return Ok(KEYBOARD_PAGE | if d == 0 { 0x27 } else { 0x1D + d });
    }
    if let Some(f) = n.strip_prefix('f').and_then(|s| s.parse::<u64>().ok()) {
        if (1..=12).contains(&f) {
            return Ok(KEYBOARD_PAGE | (0x39 + f));
        }
    }
    let u = match n.as_str() {
        "return" | "enter" => 0x28,
        "escape" | "esc" => 0x29,
        "backspace" | "delete" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "minus" => 0x2D,
        "equal" => 0x2E,
        "left_bracket" => 0x2F,
        "right_bracket" => 0x30,
        "backslash" => 0x31,
        "semicolon" => 0x33,
        "quote" => 0x34,
        "grave" | "backtick" => 0x35,
        "comma" => 0x36,
        "period" => 0x37,
        "slash" => 0x38,
        "caps_lock" => 0x39,
        "forward_delete" => 0x4C,
        "right" | "right_arrow" => 0x4F,
        "left" | "left_arrow" => 0x50,
        "down" | "down_arrow" => 0x51,
        "up" | "up_arrow" => 0x52,
        "non_us_backslash" | "section" => 0x64,
        "left_control" | "control" => 0xE0,
        "left_shift" | "shift" => 0xE1,
        "left_option" | "option" => 0xE2,
        "left_command" | "command" => 0xE3,
        "right_control" => 0xE4,
        "right_shift" => 0xE5,
        "right_option" => 0xE6,
        "right_command" => 0xE7,
        "fn" => return Ok(0xFF_0000_0003),
        _ => return Err(format!("unknown key {name:?} (use a name like caps_lock, or a hex usage like 0x39)")),
    };
    Ok(KEYBOARD_PAGE | u)
}
