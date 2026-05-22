//! Run with: `cargo run --example midi_capture`
//!
//! Connects to the first MIDI input port and prints every event for
//! 60 seconds. Used to map a controller's physical layout: move a
//! fader / press a button, see the (channel, controller#, note#)
//! triple it produces, then write that into the JSON preset.
//!
//! No mixr deps. Self-contained. Press Ctrl+C to stop early.

use std::time::Duration;

fn main() {
    let input = midir::MidiInput::new("mixr-capture").expect("midir init");
    let ports = input.ports();
    let port = ports.first().expect("no MIDI input ports — connect a controller in computer mode");
    let name = input.port_name(port).unwrap_or_else(|_| "?".into());
    eprintln!("Capturing from: {name}");
    eprintln!("Move faders / knobs / press buttons. Captures for 60s. Ctrl+C to stop.\n");

    let _conn = input.connect(port, "mixr-capture", |stamp_us, msg, _| {
        // Decode the standard MIDI status byte.
        if msg.is_empty() { return; }
        let status = msg[0];
        let kind = status & 0xF0;
        let channel = status & 0x0F;
        let stamp_ms = stamp_us / 1000;
        match kind {
            0x80 => {
                let note = msg.get(1).copied().unwrap_or(0);
                let vel = msg.get(2).copied().unwrap_or(0);
                println!("  [{stamp_ms:>10} ms]  NOTE OFF  ch={channel:<2} note={note:<3} vel={vel}");
            }
            0x90 => {
                let note = msg.get(1).copied().unwrap_or(0);
                let vel = msg.get(2).copied().unwrap_or(0);
                let label = if vel == 0 { "NOTE OFF (vel=0)" } else { "NOTE ON " };
                println!("  [{stamp_ms:>10} ms]  {label}  ch={channel:<2} note={note:<3} vel={vel}");
            }
            0xB0 => {
                let cc = msg.get(1).copied().unwrap_or(0);
                let val = msg.get(2).copied().unwrap_or(0);
                println!("  [{stamp_ms:>10} ms]  CC        ch={channel:<2} cc={cc:<3}  val={val}");
            }
            0xE0 => {
                let lo = msg.get(1).copied().unwrap_or(0) as u16;
                let hi = msg.get(2).copied().unwrap_or(0) as u16;
                let bend = ((hi << 7) | lo) as i32 - 0x2000;
                println!("  [{stamp_ms:>10} ms]  PITCH BEND ch={channel:<2} value={bend:+}");
            }
            _ => {
                println!("  [{stamp_ms:>10} ms]  RAW       {:02x?}", msg);
            }
        }
    }, ()).expect("connect");

    std::thread::sleep(Duration::from_secs(60));
    eprintln!("\n(60s elapsed — capture done)");
}
