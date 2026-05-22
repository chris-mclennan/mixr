//! Persistent cross-session DJ memory.
//!
//! Lives at `~/.mixr/dj_memory.json`. Records explicit user feedback on
//! mixes (the `+` / `-` hotkeys) so that future sessions can lean on
//! what worked and avoid what didn't. Load on DJ startup, inject the
//! top-N entries into the system prompt, append on rating.
//!
//! This is plain text (JSON as prose) rather than a vector store —
//! Claude reads the entries directly. Small, interpretable, and the
//! user can edit the file by hand if they want to tune its behavior.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// How many entries each of `good` / `bad` keeps. Older entries drop
/// when the file is rewritten so the memory stays bounded and the
/// prompt cost doesn't drift over a long career.
pub const MEMORY_CAP: usize = 50;

/// How many entries each half we inject into the prompt. Less than
/// MEMORY_CAP so the prompt cost is stable regardless of total size.
pub const PROMPT_INJECT_LIMIT: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DjMemory {
    /// Mixes the user rated positively (`+` hotkey) — "do more of this."
    #[serde(default)]
    pub good: Vec<MixEntry>,
    /// Mixes the user rated negatively (`-` hotkey) — "don't repeat."
    #[serde(default)]
    pub bad: Vec<MixEntry>,
}

/// One curated mix. Fields are deliberately minimal — Claude reads the
/// JSON as prose, so flat keys + compact values keep token cost down.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixEntry {
    /// "artist A → artist B" or "titleA → titleB". Free-form so the
    /// user can edit for readability without breaking parsing.
    pub pair: String,
    /// BPM of the outgoing and incoming tracks — tuple [out, in].
    #[serde(default)]
    pub bpm: Option<[f64; 2]>,
    /// Camelot keys "9A → 8A" or similar.
    #[serde(default)]
    pub key: Option<String>,
    /// Transition type the engine picked for this mix.
    #[serde(default)]
    pub transition: Option<String>,
    /// User-facing note ("nailed the drop", "off by a beat").
    #[serde(default)]
    pub note: Option<String>,
    /// Unix timestamp of when the rating was recorded.
    #[serde(default)]
    pub rated_at: Option<i64>,
}

fn memory_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".mixr/dj_memory.json")
}

impl DjMemory {
    /// Read from disk. Missing / unparseable file → empty memory (first
    /// session, or user nuked the file). Never panics — we'd rather
    /// start fresh than crash the DJ.
    pub fn load() -> Self {
        let path = memory_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default(); };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("dj_memory.json parse failed ({e}) — starting fresh");
            Self::default()
        })
    }

    /// Write the current memory to disk. Trims each list to
    /// `MEMORY_CAP` newest-first before writing so the file size
    /// stays bounded. Errors are logged but not fatal.
    pub fn save(&mut self) {
        self.trim();
        let path = memory_path();
        if let Ok(text) = serde_json::to_string_pretty(self)
            && let Err(e) = std::fs::write(&path, text) {
                tracing::warn!("dj_memory.json write failed: {e}");
            }
    }

    fn trim(&mut self) {
        if self.good.len() > MEMORY_CAP {
            let drop = self.good.len() - MEMORY_CAP;
            self.good.drain(0..drop);
        }
        if self.bad.len() > MEMORY_CAP {
            let drop = self.bad.len() - MEMORY_CAP;
            self.bad.drain(0..drop);
        }
    }

    /// Record a positive rating. Appends to `good`; oldest drops on save().
    pub fn remember_good(&mut self, entry: MixEntry) {
        self.good.push(entry);
    }

    /// Record a negative rating.
    pub fn remember_bad(&mut self, entry: MixEntry) {
        self.bad.push(entry);
    }

    /// Remove the most-recent entry from `good` or `bad` whose
    /// `rated_at` timestamp matches. Used by the rating-undo flow:
    /// when the user toggles off a rating (or flips good→bad), the
    /// existing entry is yanked from memory so we don't accumulate
    /// stale or contradictory feedback. Returns true if an entry was
    /// removed.
    pub fn unremember_by_rated_at(&mut self, timestamp: i64, was_good: bool) -> bool {
        let bucket = if was_good { &mut self.good } else { &mut self.bad };
        if let Some(pos) = bucket.iter().rposition(|e| e.rated_at == Some(timestamp)) {
            bucket.remove(pos);
            true
        } else {
            false
        }
    }

    /// Short prose summary for injection into the DJ system prompt.
    /// Limited to the last `PROMPT_INJECT_LIMIT` of each category so
    /// the prompt token cost is stable. Empty string when memory is
    /// empty — the prompt composer checks this to avoid dangling
    /// "MEMORY:" headers.
    pub fn prompt_summary(&self) -> String {
        if self.good.is_empty() && self.bad.is_empty() { return String::new(); }
        let good: Vec<String> = self.good.iter().rev().take(PROMPT_INJECT_LIMIT)
            .map(|e| Self::format_entry(e, "+")).collect();
        let bad: Vec<String> = self.bad.iter().rev().take(PROMPT_INJECT_LIMIT)
            .map(|e| Self::format_entry(e, "−")).collect();
        let mut parts = Vec::new();
        if !good.is_empty() {
            parts.push(format!("GOOD MIXES (do more of): {}", good.join("; ")));
        }
        if !bad.is_empty() {
            parts.push(format!("BAD MIXES (avoid): {}", bad.join("; ")));
        }
        parts.join(" | ")
    }

    fn format_entry(e: &MixEntry, prefix: &str) -> String {
        let mut s = format!("{prefix} {}", e.pair);
        if let Some([a, b]) = e.bpm { s.push_str(&format!(" ({a:.0}→{b:.0}bpm)")); }
        if let Some(k) = &e.key { s.push_str(&format!(" {k}")); }
        if let Some(note) = &e.note { s.push_str(&format!(" — {note}")); }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pair: &str) -> MixEntry {
        MixEntry {
            pair: pair.into(), bpm: Some([128.0, 130.0]),
            key: Some("9A→8A".into()), transition: Some("BeatMatched".into()),
            note: Some("clean".into()), rated_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn empty_memory_produces_empty_summary() {
        let m = DjMemory::default();
        assert!(m.prompt_summary().is_empty(),
            "no entries → no summary so the prompt doesn't carry a dangling header");
    }

    #[test]
    fn summary_mentions_both_lists_when_nonempty() {
        let mut m = DjMemory::default();
        m.remember_good(entry("A → B"));
        m.remember_bad(entry("C → D"));
        let s = m.prompt_summary();
        assert!(s.contains("GOOD MIXES"));
        assert!(s.contains("BAD MIXES"));
        assert!(s.contains("A → B"));
        assert!(s.contains("C → D"));
    }

    #[test]
    fn summary_respects_prompt_inject_limit() {
        let mut m = DjMemory::default();
        for i in 0..PROMPT_INJECT_LIMIT + 5 {
            m.remember_good(entry(&format!("t{i} → t{i}'")));
        }
        let s = m.prompt_summary();
        // The first overflow entry (t0) must not appear — rev().take(N)
        // keeps the N most-recent.
        assert!(!s.contains("t0 →"),
            "oldest entries past PROMPT_INJECT_LIMIT should be dropped");
        assert!(s.contains(&format!("t{} →", PROMPT_INJECT_LIMIT + 4)),
            "most-recent entry must appear");
    }

    #[test]
    fn trim_bounds_memory_to_cap() {
        let mut m = DjMemory::default();
        for i in 0..MEMORY_CAP + 10 {
            m.remember_good(entry(&format!("g{i}")));
            m.remember_bad(entry(&format!("b{i}")));
        }
        m.trim();
        assert_eq!(m.good.len(), MEMORY_CAP);
        assert_eq!(m.bad.len(), MEMORY_CAP);
        // Oldest drained, newest retained.
        assert_eq!(m.good.first().map(|e| e.pair.as_str()), Some("g10"));
    }

    #[test]
    fn format_entry_is_compact() {
        let e = entry("A → B");
        let line = DjMemory::format_entry(&e, "+");
        // Format stays on one line (no newlines) — prompts don't want
        // to break in the middle of the list.
        assert!(!line.contains('\n'));
        assert!(line.starts_with("+ A → B"));
    }
}
