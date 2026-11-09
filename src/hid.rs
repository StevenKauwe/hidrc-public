//! Live settings applied per device through the IOHIDEventSystemClient API
//! (the same mechanism System Settings and `hidutil` use). These take effect
//! immediately; the matching `defaults` preferences make them stick.
//!
//! What to set (`plan`) is computed as plain data so it can be tested; `apply`
//! turns it into CoreFoundation values and pushes it to each device.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{kCFAllocatorDefault, CFAllocatorRef, CFEqual, CFRelease, CFTypeRef};
use core_foundation_sys::string::CFStringRef;
use std::ffi::c_void;

use crate::config::{self, Config};
use crate::keys;

type ClientRef = *mut c_void;
type ServiceRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDEventSystemClientCreateSimpleClient(alloc: CFAllocatorRef) -> ClientRef;
    fn IOHIDEventSystemClientCopyServices(client: ClientRef) -> CFArrayRef;
    fn IOHIDServiceClientConformsTo(service: ServiceRef, page: u32, usage: u32) -> u8;
    fn IOHIDServiceClientCopyProperty(service: ServiceRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientSetProperty(service: ServiceRef, key: CFStringRef, value: CFTypeRef) -> u8;
}

/// Tracking speed (16.16), the System Settings slider. Mice report this key
/// as their HIDPointerAccelerationType. With macOS's own "Pointer
/// acceleration" toggle off (`com.apple.mouse.linear`), it is a plain
/// multiplier: measured on this Mac, 0.6875 gave 0.687 points per count at
/// every speed, so 1.0 is exact 1:1.
pub const MOUSE_SPEED: &str = "HIDMouseAcceleration";
pub const WHEEL_SPEED: &str = "HIDMouseScrollAcceleration";
pub const TRACKPAD_SPEED: &str = "HIDTrackpadAcceleration";
pub const KEY_REPEAT: &str = "HIDKeyRepeat";
pub const INITIAL_KEY_REPEAT: &str = "HIDInitialKeyRepeat";
pub const FKEY_MODE: &str = "HIDFKeyMode";
pub const KEY_MAPPING: &str = "UserKeyMapping";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Trackpad,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Live {
    Int(i64),
    /// (source usage, destination usage) pairs.
    Remap(Vec<(u64, u64)>),
}

/// HID speeds are 16.16 fixed point.
pub fn fixed(v: f64) -> i64 {
    (v * 65536.0).round() as i64
}

/// Rounded to whole 1/60 s ticks so the live value matches the pref exactly.
pub fn ms_to_ns(ms: u32) -> i64 {
    config::ticks_to_ns(config::key_repeat_ticks(ms))
}

/// External mouse, as opposed to a trackpad or a laptop's built-in input.
pub fn classify_pointer(product: &str, built_in: bool) -> Kind {
    if built_in || product.to_lowercase().contains("trackpad") {
        Kind::Trackpad
    } else {
        Kind::Mouse
    }
}

/// The properties to set on each kind of device. (`mouse.linear` has no live
/// property: it is macOS's own preference, see config.rs.)
pub fn plan(cfg: &Config) -> Result<Vec<(Kind, &'static str, Live)>, String> {
    let mut out = Vec::new();
    let k = &cfg.keyboard;
    if let Some(ms) = k.key_repeat_ms {
        out.push((Kind::Keyboard, KEY_REPEAT, Live::Int(ms_to_ns(ms))));
    }
    if let Some(ms) = k.initial_delay_ms {
        out.push((Kind::Keyboard, INITIAL_KEY_REPEAT, Live::Int(ms_to_ns(ms))));
    }
    if let Some(std) = k.fn_keys_standard {
        out.push((Kind::Keyboard, FKEY_MODE, Live::Int(std as i64)));
    }
    if let Some(remap) = &k.remap {
        let pairs = remap
            .iter()
            .map(|(src, dst)| Ok((keys::usage(src)?, keys::usage(dst)?)))
            .collect::<Result<Vec<_>, String>>()
            .map_err(|e| format!("keyboard.remap: {e}"))?;
        out.push((Kind::Keyboard, KEY_MAPPING, Live::Remap(pairs)));
    }
    let m = &cfg.mouse;
    if let Some(s) = m.speed {
        out.push((Kind::Mouse, MOUSE_SPEED, Live::Int(fixed(s))));
    }
    if let Some(s) = m.scroll_speed {
        out.push((Kind::Mouse, WHEEL_SPEED, Live::Int(fixed(s))));
    }
    if let Some(s) = cfg.trackpad.speed {
        out.push((Kind::Trackpad, TRACKPAD_SPEED, Live::Int(fixed(s))));
    }
    Ok(out)
}

pub(crate) fn to_cf(v: &Live) -> CFType {
    match v {
        Live::Int(i) => CFNumber::from(*i).as_CFType(),
        Live::Remap(pairs) => {
            let src = CFString::new("HIDKeyboardModifierMappingSrc");
            let dst = CFString::new("HIDKeyboardModifierMappingDst");
            let items: Vec<CFDictionary<CFString, CFNumber>> = pairs
                .iter()
                .map(|(s, d)| {
                    CFDictionary::from_CFType_pairs(&[
                        (src.clone(), CFNumber::from(*s as i64)),
                        (dst.clone(), CFNumber::from(*d as i64)),
                    ])
                })
                .collect();
            CFArray::from_CFTypes(&items).as_CFType()
        }
    }
}

pub struct Client {
    raw: ClientRef,
}

impl Client {
    pub fn new() -> Client {
        let raw = unsafe { IOHIDEventSystemClientCreateSimpleClient(kCFAllocatorDefault) };
        assert!(!raw.is_null(), "IOHIDEventSystemClientCreateSimpleClient failed");
        Client { raw }
    }

    /// Keyboards, mice and trackpads currently connected.
    pub fn devices(&self) -> Devices {
        let arr = unsafe { IOHIDEventSystemClientCopyServices(self.raw) };
        let mut list = Vec::new();
        if !arr.is_null() {
            for i in 0..unsafe { CFArrayGetCount(arr) } {
                let raw = unsafe { CFArrayGetValueAtIndex(arr, i) };
                let conforms = |usage| unsafe { IOHIDServiceClientConformsTo(raw, 0x01, usage) } != 0;
                let s = Service { raw };
                // Some receivers expose one service that is both keyboard and mouse.
                if conforms(0x06) {
                    list.push((Kind::Keyboard, s));
                }
                if conforms(0x02) {
                    list.push((classify_pointer(&s.name(), s.int("Built-In").unwrap_or(0) != 0), s));
                }
            }
        }
        Devices { arr, list }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { CFRelease(self.raw as CFTypeRef) }
    }
}

/// Owns the copied service array so the borrowed service refs stay valid.
pub struct Devices {
    arr: CFArrayRef,
    pub list: Vec<(Kind, Service)>,
}

impl Drop for Devices {
    fn drop(&mut self) {
        if !self.arr.is_null() {
            unsafe { CFRelease(self.arr as CFTypeRef) }
        }
    }
}

#[derive(Clone, Copy)]
pub struct Service {
    raw: ServiceRef,
}

impl Service {
    fn prop(&self, key: &str) -> Option<CFType> {
        let k = CFString::new(key);
        let r = unsafe { IOHIDServiceClientCopyProperty(self.raw, k.as_concrete_TypeRef()) };
        (!r.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(r) })
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.prop(key)?.downcast::<CFString>().map(|s| s.to_string())
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        let v = self.prop(key)?;
        if let Some(b) = v.downcast::<CFBoolean>() {
            return Some(bool::from(b) as i64);
        }
        v.downcast::<CFNumber>()?.to_i64()
    }

    pub fn name(&self) -> String {
        self.string("Product").unwrap_or_else(|| "(unnamed)".into())
    }

    /// Sets `key` only when it differs, so re-applying is a no-op.
    /// Returns None when already set, else whether the set succeeded.
    fn ensure(&self, key: &str, value: &CFType) -> Option<bool> {
        if let Some(cur) = self.prop(key) {
            if unsafe { CFEqual(cur.as_CFTypeRef(), value.as_CFTypeRef()) } != 0 {
                return None;
            }
        }
        let k = CFString::new(key);
        Some(unsafe { IOHIDServiceClientSetProperty(self.raw, k.as_concrete_TypeRef(), value.as_CFTypeRef()) != 0 })
    }
}

/// Pushes the planned properties to every matching device. Returns log lines
/// for the devices that changed.
pub fn apply(client: &Client, plan: &[(Kind, &'static str, Live)]) -> Vec<String> {
    let values: Vec<(Kind, &str, CFType)> = plan.iter().map(|(k, key, v)| (*k, *key, to_cf(v))).collect();
    let mut log = Vec::new();
    for (kind, s) in &client.devices().list {
        let changes: Vec<String> = values
            .iter()
            .filter(|(k, _, _)| k == kind)
            .filter_map(|(_, key, v)| {
                s.ensure(key, v).map(|ok| format!("{key}{}", if ok { "" } else { " FAILED" }))
            })
            .collect();
        if !changes.is_empty() {
            log.push(format!("{kind:?} {}: set {}", s.name(), changes.join(", ")));
        }
    }
    log
}
