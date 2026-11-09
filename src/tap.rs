//! Scroll-wheel rewriting through a CGEventTap. Wheel events from mice are
//! "non-continuous"; trackpad and Magic Mouse scrolling is continuous and is
//! passed through untouched.
//!
//! The rewrite itself (`transform`) is a pure function over a `Wheel` value;
//! `apply_to_event` moves real CGEvent fields in and out of it.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::runloop::{
    kCFRunLoopCommonModes, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

use crate::config::Mouse;

pub type EventRef = *mut c_void;
type MachPortRef = *mut c_void;
pub type TapCallback = extern "C" fn(*mut c_void, u32, EventRef, *mut c_void) -> EventRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> MachPortRef;
    fn CGEventTapEnable(tap: MachPortRef, enable: bool);
    pub fn CGEventGetIntegerValueField(event: EventRef, field: u32) -> i64;
    pub fn CGEventSetIntegerValueField(event: EventRef, field: u32, value: i64);
    pub fn CGEventGetDoubleValueField(event: EventRef, field: u32) -> f64;
    pub fn CGEventSetDoubleValueField(event: EventRef, field: u32, value: f64);
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: MachPortRef, order: isize) -> *mut c_void;
}

const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const DEFAULT_OPTIONS: u32 = 0;
const SCROLL_WHEEL: u32 = 22;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

pub const IS_CONTINUOUS: u32 = 88;
/// (line delta, fixed-point line delta, point delta) for the vertical and horizontal axes.
pub const AXES: [(u32, u32, u32); 2] = [(11, 93, 96), (12, 94, 97)];

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Axis {
    pub lines: i64,
    pub fixed: f64,
    pub points: i64,
}

/// The scroll fields of one event: [vertical, horizontal].
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Wheel {
    pub continuous: bool,
    pub axes: [Axis; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    pub reverse: bool,
    pub lines: Option<i64>,
    pub pixels_per_line: f64,
}

impl Rules {
    pub fn from_config(m: &Mouse) -> Rules {
        Rules {
            reverse: m.scroll_reverse.unwrap_or(false),
            lines: m.scroll_lines,
            pixels_per_line: m.pixels_per_line.unwrap_or(10.0),
        }
    }
}

/// Rewrites a mouse-wheel event. Continuous (trackpad / Magic Mouse) events
/// are returned unchanged, and so is any axis with no movement.
pub fn transform(w: Wheel, r: &Rules) -> Wheel {
    if w.continuous {
        return w;
    }
    let mut out = w;
    for axis in out.axes.iter_mut() {
        // Not f64::signum: it returns 1.0 for 0.0, which would scroll an idle axis.
        let dir = if axis.lines != 0 {
            axis.lines.signum()
        } else if axis.fixed > 0.0 {
            1
        } else if axis.fixed < 0.0 {
            -1
        } else {
            0
        };
        if dir == 0 {
            continue;
        }
        let flip = if r.reverse { -1 } else { 1 };
        match r.lines {
            Some(n) => {
                let lines = dir * flip * n;
                *axis = Axis {
                    lines,
                    fixed: lines as f64,
                    points: (lines as f64 * r.pixels_per_line).round() as i64,
                };
            }
            None if r.reverse => {
                *axis = Axis { lines: -axis.lines, fixed: -axis.fixed, points: -axis.points };
            }
            None => {}
        }
    }
    out
}

pub unsafe fn read_wheel(event: EventRef) -> Wheel {
    let mut w = Wheel { continuous: CGEventGetIntegerValueField(event, IS_CONTINUOUS) != 0, ..Default::default() };
    for (axis, (line, fixed, point)) in w.axes.iter_mut().zip(AXES) {
        *axis = Axis {
            lines: CGEventGetIntegerValueField(event, line),
            fixed: CGEventGetDoubleValueField(event, fixed),
            points: CGEventGetIntegerValueField(event, point),
        };
    }
    w
}

pub unsafe fn write_wheel(event: EventRef, w: &Wheel) {
    for (axis, (line, fixed, point)) in w.axes.iter().zip(AXES) {
        // Setting the line delta makes macOS recompute the other two fields,
        // so it goes first and the others are overwritten after it.
        CGEventSetIntegerValueField(event, line, axis.lines);
        CGEventSetDoubleValueField(event, fixed, axis.fixed);
        CGEventSetIntegerValueField(event, point, axis.points);
    }
}

/// Applies `rules` to a real scroll event in place.
pub unsafe fn apply_to_event(event: EventRef, rules: &Rules) {
    let before = read_wheel(event);
    let after = transform(before, rules);
    if after != before {
        write_wheel(event, &after);
    }
}

static RULES: OnceLock<Rules> = OnceLock::new();
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Checks Accessibility permission; with `prompt`, macOS shows its grant dialog.
pub fn trusted(prompt: bool) -> bool {
    let opts = CFDictionary::from_CFType_pairs(&[(
        CFString::new("AXTrustedCheckOptionPrompt"),
        CFBoolean::from(prompt),
    )]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) }
}

extern "C" fn callback(_proxy: *mut c_void, kind: u32, event: EventRef, _info: *mut c_void) -> EventRef {
    if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER_INPUT {
        let tap = TAP.load(Ordering::Relaxed);
        if !tap.is_null() {
            unsafe { CGEventTapEnable(tap, true) };
        }
        return event;
    }
    if kind == SCROLL_WHEEL {
        if let Some(rules) = RULES.get() {
            unsafe { apply_to_event(event, rules) };
        }
    }
    event
}

/// Installs the tap and runs the current thread's run loop forever.
pub fn run(rules: Rules) -> Result<(), String> {
    let _ = RULES.set(rules);
    let tap = unsafe {
        CGEventTapCreate(
            SESSION_EVENT_TAP,
            HEAD_INSERT,
            DEFAULT_OPTIONS,
            1u64 << SCROLL_WHEEL,
            callback,
            std::ptr::null_mut(),
        )
    };
    if tap.is_null() {
        return Err("CGEventTapCreate failed (is Accessibility permission granted?)".into());
    }
    TAP.store(tap, Ordering::Relaxed);
    unsafe {
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source as _, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        CFRunLoopRun();
    }
    Ok(())
}
