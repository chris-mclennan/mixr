//! HID controller integration — scaffolding.
//!
//! Most DJ controllers expose two modes:
//!   - **MIDI** (class-compliant): generic, parsed by `crate::midi`
//!   - **HID** (vendor-specific): higher resolution, finer control,
//!     supports LED feedback, jog-wheel raw-touch detection, etc.
//!
//! HID reports are device-specific binary blobs — there's no universal
//! parser like MIDI's CC/note model. Each supported controller needs
//! its own decoder that maps report bytes → semantic events. This
//! module provides:
//!   - Device discovery (list connected HID devices by VID/PID)
//!   - Report-listener thread skeleton
//!   - A `Decoder` trait per supported controller (one impl per device)
//!   - Hook into the same `Action` enum from `midi.rs` so HID-decoded
//!     events dispatch through the same IPC path
//!
//! Right now: only the discovery + listener skeleton is wired. The
//! per-controller decoders get added when we have hardware in hand
//! to capture report bytes (Mixstream Pro Go Plus is the first
//! target).

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

/// Device identifier — VID/PID pair. Matched at discovery time
/// against the connected HID device list to pick the right decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub vendor: u16,
    pub product: u16,
}

impl DeviceId {
    /// Numark Mixstream Pro Go Plus. **Placeholder values** — the
    /// real VID/PID needs to be confirmed by inspecting `lsusb` /
    /// `ioreg -p IOUSB` on a connected unit (see also the
    /// device-id helper below). Used as a marker until the
    /// controller arrives tomorrow.
    pub const NUMARK_MIXSTREAM_PRO_GO_PLUS: DeviceId = DeviceId {
        vendor: 0x15E4,
        product: 0x0000,
    };
}

/// One HID report from a device + the device that produced it.
/// Decoders consume these and translate to events for dispatch.
#[derive(Debug, Clone)]
pub struct Report {
    pub device: DeviceId,
    pub bytes: Vec<u8>,
}

/// Translates a device's HID reports into mixr `Action`s. One impl
/// per supported controller. Stateful — implementations track
/// previous-report state to detect transitions (button press vs
/// hold), debounce noisy controls, decode jog-wheel direction from
/// successive deltas, etc.
pub trait Decoder: Send {
    /// Convert a single report into zero or more (action, value)
    /// pairs to dispatch. Value semantics match `midi::Action::
    /// to_ipc_command`'s `value` parameter (0..=127 for continuous,
    /// >0 for note-on, 0 for release).
    fn decode(&mut self, report: &[u8]) -> Vec<(crate::midi::Action, u32)>;
}

/// Listener state — same shape as MIDI's. The TUI doesn't need to
/// distinguish HID vs MIDI: bindings are by `Action`, the source
/// is just an input stream.
#[derive(Debug, Default)]
pub struct ListenerState {
    pub last_device: Option<DeviceId>,
    pub last_report: Option<Vec<u8>>,
    pub dispatch_active: bool,
}

/// Spawn the HID listener thread. Walks the connected HID device
/// list, opens any with a known decoder, and routes reports through
/// the per-device decoder + dispatch path. No-op when no supported
/// devices are connected (most users).
pub fn spawn_listener() -> Arc<Mutex<ListenerState>> {
    let state = Arc::new(Mutex::new(ListenerState {
        dispatch_active: true,
        ..Default::default()
    }));
    let state_for_thread = state.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_listener(state_for_thread) {
            tracing::debug!("HID listener exited: {e}");
        }
    });
    state
}

fn run_listener(_state: Arc<Mutex<ListenerState>>) -> anyhow::Result<()> {
    let api = hidapi::HidApi::new()?;
    let devices: Vec<_> = api.device_list().collect();
    tracing::info!("HID: scanning {} connected devices", devices.len());

    // Per-device decoder dispatch lives here once decoders exist.
    // For now we just enumerate so users can see what's connected
    // (helpful for figuring out the Mixstream's VID/PID tomorrow).
    for dev in &devices {
        let vid = dev.vendor_id();
        let pid = dev.product_id();
        let name = dev.product_string().unwrap_or("?");
        let manufacturer = dev.manufacturer_string().unwrap_or("?");
        tracing::debug!("HID device: {manufacturer} {name} (VID={vid:#06x} PID={pid:#06x})");
    }

    // No decoders wired yet — return cleanly so the listener thread
    // doesn't burn a core. When the Mixstream decoder lands, this
    // becomes a per-device read loop.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_equality() {
        let a = DeviceId {
            vendor: 0x1234,
            product: 0x5678,
        };
        let b = DeviceId {
            vendor: 0x1234,
            product: 0x5678,
        };
        let c = DeviceId {
            vendor: 0x1234,
            product: 0x9999,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
