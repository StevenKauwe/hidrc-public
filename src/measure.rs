//! `hidrc measure`: is pointer movement really 1:1?
//!
//! Counts what the mouse sends (raw HID X/Y counts) and what macOS does with
//! it (cursor deltas, in points) in 50 ms windows while you move the mouse.
//! If acceleration is off, points per count is the same at every speed; if
//! that ratio is 1.0, one count moves the cursor one point (Windows' "6/11").

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFAllocatorRef, CFTypeRef};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::runloop::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef,
    CFRunLoopRunInMode,
};
use core_foundation_sys::string::CFStringRef;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::tap::{self, EventRef};

type ManagerRef = *mut c_void;
type ValueRef = *mut c_void;
type ElementRef = *mut c_void;
type DeviceRef = *mut c_void;
type ValueCallback = extern "C" fn(*mut c_void, i32, *mut c_void, ValueRef);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(alloc: CFAllocatorRef, options: u32) -> ManagerRef;
    fn IOHIDManagerSetDeviceMatching(m: ManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerRegisterInputValueCallback(m: ManagerRef, cb: ValueCallback, ctx: *mut c_void);
    fn IOHIDManagerScheduleWithRunLoop(m: ManagerRef, rl: CFRunLoopRef, mode: CFStringRef);
    fn IOHIDManagerOpen(m: ManagerRef, options: u32) -> i32;
    fn IOHIDValueGetElement(v: ValueRef) -> ElementRef;
    fn IOHIDValueGetIntegerValue(v: ValueRef) -> isize;
    fn IOHIDElementGetUsagePage(e: ElementRef) -> u32;
    fn IOHIDElementGetUsage(e: ElementRef) -> u32;
    fn IOHIDElementGetDevice(e: ElementRef) -> DeviceRef;
    fn IOHIDDeviceGetProperty(d: DeviceRef, key: CFStringRef) -> CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: *mut c_void, order: isize) -> *mut c_void;
}

const MOUSE_MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const OTHER_DRAGGED: u32 = 27;
const DELTA_X: u32 = 4;
const DELTA_Y: u32 = 5;
const LISTEN_ONLY: u32 = 1;
const WINDOW: Duration = Duration::from_millis(50);

/// (raw counts x, y) and (cursor points x, y) accumulated in the current window.
static COUNTS: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
static POINTS: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

/// One window: distance in raw counts and in cursor points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub counts: f64,
    pub points: f64,
}

#[derive(Debug, PartialEq)]
pub struct Band {
    pub label: &'static str,
    pub windows: usize,
    pub ratio: f64,
}

#[derive(Debug)]
pub struct Report {
    pub bands: Vec<Band>,
    pub overall: f64,
    /// Fastest band's ratio over slowest band's; 1.0 means no acceleration.
    pub spread: f64,
}

impl Report {
    pub fn is_linear(&self) -> bool {
        (self.spread - 1.0).abs() < 0.05
    }
    pub fn is_one_to_one(&self) -> bool {
        self.is_linear() && (self.overall - 1.0).abs() < 0.03
    }
}

/// Speed bands in counts per second.
const BANDS: [(&str, f64, f64); 4] = [
    ("slow   (<300 counts/s)", 0.0, 300.0),
    ("medium (300-1500)", 300.0, 1500.0),
    ("fast   (1500-5000)", 1500.0, 5000.0),
    ("flick  (>5000)", 5000.0, f64::INFINITY),
];

/// Groups windows by speed and computes points-per-count in each band.
/// Bands with fewer than 5 windows are left out as too noisy.
pub fn analyze(samples: &[Sample], window_secs: f64) -> Option<Report> {
    let moving: Vec<&Sample> = samples.iter().filter(|s| s.counts >= 1.0 && s.points > 0.0).collect();
    let total_c: f64 = moving.iter().map(|s| s.counts).sum();
    if total_c == 0.0 {
        return None;
    }
    let overall = moving.iter().map(|s| s.points).sum::<f64>() / total_c;
    let bands: Vec<Band> = BANDS
        .iter()
        .filter_map(|&(label, lo, hi)| {
            let inside: Vec<&&Sample> =
                moving.iter().filter(|s| (lo..hi).contains(&(s.counts / window_secs))).collect();
            let c: f64 = inside.iter().map(|s| s.counts).sum();
            (inside.len() >= 5).then(|| Band { label, windows: inside.len(), ratio: inside.iter().map(|s| s.points).sum::<f64>() / c })
        })
        .collect();
    let spread = match (bands.first(), bands.last()) {
        (Some(slow), Some(fast)) => fast.ratio / slow.ratio,
        _ => 1.0,
    };
    Some(Report { bands, overall, spread })
}

extern "C" fn on_value(_ctx: *mut c_void, _result: i32, _sender: *mut c_void, value: ValueRef) {
    unsafe {
        let el = IOHIDValueGetElement(value);
        if IOHIDElementGetUsagePage(el) != 0x01 {
            return;
        }
        let axis = IOHIDElementGetUsage(el);
        if axis != 0x30 && axis != 0x31 {
            return;
        }
        // Only the external mouse: ignore the built-in trackpad.
        let key = CFString::new("Built-In");
        let built_in = IOHIDDeviceGetProperty(IOHIDElementGetDevice(el), key.as_concrete_TypeRef());
        if !built_in.is_null() {
            let v = core_foundation::base::CFType::wrap_under_get_rule(built_in);
            let yes = v.downcast::<CFBoolean>().map(bool::from).or_else(|| v.downcast::<CFNumber>().and_then(|n| n.to_i64()).map(|n| n != 0));
            if yes == Some(true) {
                return;
            }
        }
        let n = IOHIDValueGetIntegerValue(value) as f64;
        let mut c = COUNTS.lock().unwrap();
        if axis == 0x30 {
            c.0 += n;
        } else {
            c.1 += n;
        }
    }
}

extern "C" fn on_move(_proxy: *mut c_void, kind: u32, event: EventRef, _info: *mut c_void) -> EventRef {
    if matches!(kind, MOUSE_MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED) {
        let (dx, dy) = unsafe { (tap::CGEventGetDoubleValueField(event, DELTA_X), tap::CGEventGetDoubleValueField(event, DELTA_Y)) };
        let mut p = POINTS.lock().unwrap();
        p.0 += dx;
        p.1 += dy;
    }
    event
}

fn take(m: &Mutex<(f64, f64)>) -> f64 {
    let (x, y) = std::mem::take(&mut *m.lock().unwrap());
    x.hypot(y)
}

pub fn run(seconds: u64) -> Result<(), String> {
    unsafe {
        let rl = CFRunLoopGetCurrent();

        let mgr = IOHIDManagerCreate(kCFAllocatorDefault, 0);
        let matching = CFDictionary::from_CFType_pairs(&[
            (CFString::new("DeviceUsagePage"), CFNumber::from(1)),
            (CFString::new("DeviceUsage"), CFNumber::from(2)),
        ]);
        IOHIDManagerSetDeviceMatching(mgr, matching.as_concrete_TypeRef());
        IOHIDManagerRegisterInputValueCallback(mgr, on_value, std::ptr::null_mut());
        IOHIDManagerScheduleWithRunLoop(mgr, rl, kCFRunLoopDefaultMode);
        let rc = IOHIDManagerOpen(mgr, 0);
        if rc != 0 {
            return Err(format!(
                "could not open the mouse (IOReturn {rc:#x}). Allow your terminal under System Settings > Privacy & Security > Input Monitoring, then retry."
            ));
        }

        let mask = [MOUSE_MOVED, LEFT_DRAGGED, RIGHT_DRAGGED, OTHER_DRAGGED].iter().fold(0u64, |m, t| m | 1 << t);
        let port = tap::CGEventTapCreate(1, 0, LISTEN_ONLY, mask, on_move, std::ptr::null_mut());
        if port.is_null() {
            return Err("could not watch cursor movement. Allow your terminal under System Settings > Privacy & Security > Input Monitoring, then retry.".into());
        }
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
        CFRunLoopAddSource(rl, src as _, kCFRunLoopCommonModes);

        println!("Measuring for {seconds}s. Use only the mouse (not the trackpad). Mix very slow");
        println!("moves, normal moves and fast flicks; stay away from screen edges if you can.");
        let _ = (take(&COUNTS), take(&POINTS));
        let mut samples = Vec::new();
        let end = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < end {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, WINDOW.as_secs_f64(), 0);
            samples.push(Sample { counts: take(&COUNTS), points: take(&POINTS) });
        }

        let Some(r) = analyze(&samples, WINDOW.as_secs_f64()) else {
            return Err("no mouse movement seen".into());
        };
        println!("\nPoints of cursor movement per mouse count:");
        for b in &r.bands {
            println!("  {:<24} {:>6.3}   ({} samples)", b.label, b.ratio, b.windows);
        }
        println!("  {:<24} {:>6.3}", "overall", r.overall);
        println!();
        if r.bands.len() < 2 {
            println!("Not enough range of speeds to judge acceleration; try again with slower and faster moves.");
        } else if r.is_linear() {
            println!("Linear: no acceleration (fast/slow ratio {:.3}).", r.spread);
        } else {
            println!("ACCELERATED: fast moves go {:.2}x further per count than slow ones.", r.spread);
        }
        if r.is_one_to_one() {
            println!("1:1: one mouse count moves the cursor one point.");
        } else if r.is_linear() {
            let client = crate::hid::Client::new();
            let current = client.devices().list.iter().find_map(|(k, s)| {
                (*k == crate::hid::Kind::Mouse).then(|| s.int(crate::hid::MOUSE_SPEED)).flatten()
            });
            match current.map(|v| v as f64 / 65536.0) {
                Some(speed) => println!(
                    "Gain is {:.3}, not 1.0. For 1:1 set `speed = {:.4}` under [mouse] (now {speed}).",
                    r.overall,
                    speed / r.overall
                ),
                None => println!("Gain is {:.3}, not 1.0.", r.overall),
            }
        } else {
            println!("Turn acceleration off first: `linear = true` under [mouse], then log out and back in.");
        }
        Ok(())
    }
}
