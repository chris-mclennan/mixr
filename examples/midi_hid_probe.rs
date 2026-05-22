//! Run with: `cargo run --example midi_hid_probe`
//!
//! Lists every MIDI input port + every HID device the running user
//! can see, with VID/PID. Used once to identify a connected DJ
//! controller's identifiers so they can be wired into the MIDI
//! map (`presets/*.midi-map.json`) and the HID decoder scaffold
//! (`src/hid.rs`).
//!
//! No mixr deps loaded — just midir + hidapi. Side-effect free
//! (no listening, no command writes).

fn main() {
    println!("=== MIDI INPUTS ===");
    match midir::MidiInput::new("mixr-probe") {
        Ok(input) => {
            let ports = input.ports();
            if ports.is_empty() {
                println!("  (no MIDI inputs detected)");
            } else {
                for port in &ports {
                    let name = input.port_name(port).unwrap_or_else(|_| "?".into());
                    println!("  • {name}");
                }
            }
        }
        Err(e) => println!("  midir init failed: {e}"),
    }

    println!();
    println!("=== HID DEVICES ===");
    match hidapi::HidApi::new() {
        Ok(api) => {
            let devices: Vec<_> = api.device_list().collect();
            if devices.is_empty() {
                println!("  (no HID devices detected — try with sudo or grant permission)");
            } else {
                for d in devices {
                    let vid = d.vendor_id();
                    let pid = d.product_id();
                    let manuf = d.manufacturer_string().unwrap_or("");
                    let prod = d.product_string().unwrap_or("");
                    // Filter to interesting candidates (common DJ-controller vendors)
                    let interesting = manuf.to_lowercase().contains("numark")
                        || manuf.to_lowercase().contains("inmusic")
                        || manuf.to_lowercase().contains("pioneer")
                        || manuf.to_lowercase().contains("native instruments")
                        || prod.to_lowercase().contains("mixstream")
                        || prod.to_lowercase().contains("ddj")
                        || prod.to_lowercase().contains("traktor");
                    let prefix = if interesting { "★" } else { " " };
                    println!("  {prefix} VID={vid:#06x} PID={pid:#06x}  {manuf:<20} {prod}");
                }
            }
        }
        Err(e) => println!("  hidapi init failed: {e}"),
    }
}
