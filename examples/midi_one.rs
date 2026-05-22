//! Run with: `cargo run --example midi_one`
//!
//! Captures MIDI events for 8 seconds, then prints a summary of
//! distinct (kind, channel, controller#) tuples observed and which
//! one moved the most. Designed for one-control-at-a-time mapping.
//!
//! Output:
//!   • CC ch=0 #14   ← crossfader (saw 134 events)
//!   • NoteOn ch=0 note=11 ← play button (saw 1 event)
//!
//! Each run = move ONE thing on the controller.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct Key { kind: &'static str, channel: u8, control: u8 }

fn main() {
    let input = midir::MidiInput::new("mixr-one").expect("midir init");
    let port = input.ports().into_iter().next().expect("no MIDI input");
    let name = input.port_name(&port).unwrap_or_default();
    eprintln!("Listening on: {name}");
    eprintln!("Move ONE control now (30s capture)...\n");

    let counts: Arc<Mutex<HashMap<Key, (u32, Vec<u8>)>>> = Arc::new(Mutex::new(HashMap::new()));
    let counts_cb = counts.clone();
    let _conn = input.connect(&port, "mixr-one", move |_, msg, _| {
        if msg.is_empty() { return; }
        let status = msg[0];
        let kind = status & 0xF0;
        let channel = status & 0x0F;
        let (label, control, val): (&'static str, u8, u8) = match kind {
            0xB0 => ("CC", msg.get(1).copied().unwrap_or(0), msg.get(2).copied().unwrap_or(0)),
            0x90 => {
                let v = msg.get(2).copied().unwrap_or(0);
                if v == 0 { ("NoteOff", msg.get(1).copied().unwrap_or(0), 0) }
                else      { ("NoteOn",  msg.get(1).copied().unwrap_or(0), v) }
            }
            0x80 => ("NoteOff", msg.get(1).copied().unwrap_or(0), 0),
            0xE0 => ("PitchBend", 0, msg.get(2).copied().unwrap_or(0)),
            _ => return,
        };
        let key = Key { kind: label, channel, control };
        let mut map = counts_cb.lock().unwrap();
        let entry = map.entry(key).or_insert((0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 4 { entry.1.push(val); }
    }, ()).expect("connect");

    std::thread::sleep(Duration::from_secs(30));

    let map = counts.lock().unwrap();
    if map.is_empty() {
        eprintln!("No events captured — did you move a control?");
        return;
    }
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    eprintln!("\nEvents observed (most → least frequent):\n");
    for (key, (count, samples)) in entries {
        let label = match key.kind {
            "CC" => format!("CC        ch={:<2} cc={:<3}", key.channel, key.control),
            "NoteOn" => format!("NoteOn    ch={:<2} note={:<3}", key.channel, key.control),
            "NoteOff" => format!("NoteOff   ch={:<2} note={:<3}", key.channel, key.control),
            other => format!("{other} ch={}", key.channel),
        };
        let sample_str: String = samples.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(",");
        eprintln!("  {label}   ({count:>4} events, sample values: {sample_str})");
    }
}
