use super::api::{ClaudeAPI, ToolCall};
use super::memory::{DjMemory, MixEntry};
use crate::config::{ClaudeDjMode, ClaudeDjSettings, DjStyle, Strictness};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Which operational mode a DJ call is running in. Set by the app on
/// each trigger based on engine state — during an active crossfade
/// we want a slim prompt + tool set focused on phase/phrase alignment;
/// otherwise the full prep prompt for track selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallMode {
    /// Prep/selection: full browse/queue tool set, curation prompt,
    /// memory, recent-sources block. Used between mixes.
    #[default]
    Prep,
    /// Performance: slim prompt, mix-critical tools only. Used while
    /// the engine is in Crossfading state so Claude can correct phase
    /// and phrase alignment without reloading the full curation
    /// context every call. Token-optimized.
    Performance,
}

/// Claude DJ — AI-powered DJ that browses Beatport and controls the mix.
pub struct ClaudeDJ {
    api: ClaudeAPI,
    pub enabled: bool,
    last_call: Option<Instant>,
    min_interval: Duration,
    conversation: Vec<Value>,
    pub log: Vec<LogEntry>,
    pub user_prompt: Option<String>,
    /// Tool-call rounds in the current trigger chain. Reset on `trigger()`,
    /// incremented in `continue_with_results()`. Capped at `MAX_ROUNDS` —
    /// when hit, the DJ stops the chain and logs loudly so you can decide
    /// whether to raise the cap later.
    round: u32,
    /// True between `trigger()` returning a non-empty tool list and the
    /// follow-up `continue_with_results()` returning empty (chain done).
    /// Callers consult `is_busy()` before firing a new trigger so we
    /// don't pile up overlapping conversations and burn rate-limit
    /// tokens by cancel-and-restart cycles.
    chain_in_progress: bool,
    /// Short FIFO of breadcrumbs the DJ has visited this session. Fed into
    /// the system prompt so the model can steer away from re-querying the
    /// same chart/genre. Bounded by MEMORY_LEN.
    recent_screens: VecDeque<String>,
    /// Short FIFO of track titles the DJ has recently queued. Used to
    /// diversify selections — pairs with the "2-3 per chart" guideline.
    recent_queued: VecDeque<String>,
    /// Behavior tunables — mirror of `AppConfig.claude_dj`. Kept here so
    /// the DJ can compose its system prompt without a callback into the
    /// app. Updated by the app whenever the user changes settings.
    settings: ClaudeDjSettings,
    /// Operational mode for the *next* API call — flipped by the
    /// caller on every trigger_dj based on engine state. Stored on
    /// self so continue_with_results uses the same branch for the
    /// rest of the chain.
    call_mode: CallMode,
    /// Cross-session training memory — loaded from ~/.mixr/dj_memory.json
    /// at construction, re-saved on each rating. Summarized into the
    /// system prompt so Claude carries curated "what worked" forward.
    memory: DjMemory,
}

/// How many entries of session memory (browsed screens, queued tracks)
/// the DJ carries forward. Short enough to stay negligible in token
/// cost; long enough to break out of a browse loop.
const MEMORY_LEN: usize = 8;

/// Hard ceiling on tool-call rounds per trigger chain. Exceeding this logs
/// at INFO and aborts the chain. Lower bound: each round resends the full
/// conversation as input, so cumulative tokens grow O(N²). At 10 rounds
/// with ~3KB tool_result payloads (truncated below) we stay well under
/// the 50k input-tokens/min Anthropic limit on Haiku.
pub const MAX_ROUNDS: u32 = 10;

/// Higher round cap when manual mode is on. Beatmatching iterates
/// read_phase → adjust_tempo → nudge → read_phase, which needs more
/// rounds per trigger than auto-DJ track selection. Still bounded so
/// a runaway chain can't wedge forever.
pub const MAX_ROUNDS_MANUAL: u32 = 20;

/// Minimum seconds between API calls in auto/assist mode.
pub const MIN_INTERVAL_AUTO_SECS: u64 = 2;

/// Shorter interval in manual mode so the beatmatch loop can iterate
/// faster. Rate-limit backoff on 429 still kicks in and can push this
/// back up, so the cap is a floor, not a ceiling.
pub const MIN_INTERVAL_MANUAL_SECS: u64 = 1;

/// Per tool_result body cap. browse_screen and search_tracks otherwise
/// return ~1.5KB of item lists each — and those bodies appear in every
/// subsequent round's input. Truncating to the first ~500 chars keeps
/// the conversational signal (the DJ usually only acts on the first
/// few items anyway) while collapsing the token tail.
const TOOL_RESULT_MAX_CHARS: usize = 500;

/// Aggressive cap applied retroactively to tool_result bodies older than
/// `TOOL_RESULT_KEEP_RECENT` rounds. Each round re-sends the full
/// conversation as input, so without retro-compaction the cumulative
/// input tokens grow O(N²) even with the fresh-round cap above. Older
/// tool outputs are almost never re-consulted by the model once it has
/// moved on, so we squash them to a signature string that preserves
/// the tool id/pairing without the body.
const TOOL_RESULT_STALE_MAX_CHARS: usize = 80;

/// How many most-recent tool_result user-messages keep their full body
/// before retro-compaction kicks in.
const TOOL_RESULT_KEEP_RECENT: usize = 2;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub entry_type: LogEntryType,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogEntryType {
    Action,
    Track,
    Phase,
    Flow,
    User,
    Error,
    Info,
}

impl ClaudeDJ {
    #[allow(dead_code)] // used in #[cfg(test)] only; production uses from_key_file()
    pub fn new(api_key: String) -> Self {
        Self {
            api: ClaudeAPI::new(api_key),
            enabled: false,
            last_call: None,
            min_interval: Duration::from_secs(2),
            conversation: Vec::new(),
            log: Vec::new(),
            user_prompt: None,
            round: 0,
            chain_in_progress: false,
            recent_screens: VecDeque::new(),
            recent_queued: VecDeque::new(),
            settings: ClaudeDjSettings::default(),
            memory: DjMemory::load(),
            call_mode: CallMode::Prep,
        }
    }

    pub fn from_key_file() -> Option<Self> {
        ClaudeAPI::from_key_file().ok().map(|api| Self {
            api,
            enabled: false,
            last_call: None,
            min_interval: Duration::from_secs(2),
            conversation: Vec::new(),
            log: Vec::new(),
            user_prompt: None,
            round: 0,
            chain_in_progress: false,
            recent_screens: VecDeque::new(),
            recent_queued: VecDeque::new(),
            settings: ClaudeDjSettings::default(),
            memory: DjMemory::load(),
            call_mode: CallMode::Prep,
        })
    }

    /// Append a "good mix" entry and persist. No-op when the persistent
    /// memory has been disabled in settings — the user can opt out
    /// without losing the ability to hit the rating hotkeys.
    pub fn rate_good(&mut self, entry: MixEntry) {
        if !self.settings.memory_enabled {
            return;
        }
        self.memory.remember_good(entry);
        self.memory.save();
    }

    pub fn rate_bad(&mut self, entry: MixEntry) {
        if !self.settings.memory_enabled {
            return;
        }
        self.memory.remember_bad(entry);
        self.memory.save();
    }

    /// Remove a previously-saved rating. Used by the toggle-undo flow
    /// when the user hits the same rating key twice. Returns true if
    /// an entry was found and removed. No-op when memory is disabled.
    pub fn unrate(&mut self, rated_at: i64, was_good: bool) -> bool {
        if !self.settings.memory_enabled {
            return false;
        }
        let removed = self.memory.unremember_by_rated_at(rated_at, was_good);
        if removed {
            self.memory.save();
        }
        removed
    }

    #[cfg(test)]
    pub(crate) fn inject_memory_for_test(&mut self, m: DjMemory) {
        self.memory = m;
    }

    /// Overwrite the current behavior settings — called by the app when
    /// the user toggles a Claude DJ option. Cheap, synchronous; next
    /// trigger picks up the new prompt composition.
    pub fn apply_settings(&mut self, settings: ClaudeDjSettings) {
        let was_manual = self.settings.mode == ClaudeDjMode::Manual;
        let now_manual = settings.mode == ClaudeDjMode::Manual;
        self.settings = settings;
        // Mode transition resets the adaptive rate-limit floor to the
        // appropriate default — without this, switching from auto→manual
        // leaves the slower 2s interval in place even though manual
        // wants to iterate faster for beatmatching. 429-backoff still
        // dominates if it's active.
        if was_manual != now_manual {
            self.min_interval = Duration::from_secs(self.base_interval_secs());
        }
    }

    /// Round cap for the current mode. Manual mode gets a higher ceiling
    /// since beatmatching iterates more than track selection. Centralized
    /// here so both `trigger` and `continue_with_results` check the
    /// same limit.
    fn current_round_cap(&self) -> u32 {
        // Route by call_mode (Prep vs Performance), not settings.mode
        // (Auto vs Manual). Prep always wants the tight cap because
        // track selection needs only 5-8 rounds; letting it run 20
        // blows the rate limit on schema-accumulation. Performance
        // gets the higher cap because beatmatching during a sweep
        // legitimately needs multiple read_alignment → nudge cycles.
        match self.call_mode {
            CallMode::Performance => MAX_ROUNDS_MANUAL,
            CallMode::Prep => MAX_ROUNDS,
        }
    }

    /// Default min-interval for the current mode. Returned in seconds so
    /// callers can compose a Duration. Not affected by 429 backoff —
    /// that's a runtime multiplier on top.
    fn base_interval_secs(&self) -> u64 {
        if self.settings.mode == ClaudeDjMode::Manual {
            MIN_INTERVAL_MANUAL_SECS
        } else {
            MIN_INTERVAL_AUTO_SECS
        }
    }

    /// Set the operational mode for the next API call. Called by the
    /// app right before trigger() / continue_with_results() so the
    /// right prompt + tool set composes. Does not affect an
    /// in-flight chain.
    pub fn set_call_mode(&mut self, m: CallMode) {
        self.call_mode = m;
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }
    pub fn disable(&mut self) {
        self.enabled = false;
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_prompt(&mut self, prompt: String) {
        self.add_log(LogEntryType::User, format!("Direction: {prompt}"));
        self.user_prompt = Some(prompt);
    }

    pub fn log_entries(&self) -> &[LogEntry] {
        &self.log
    }

    /// Classify a tool call by name into the appropriate log entry type.
    fn classify_tool(name: &str) -> LogEntryType {
        match name {
            "queue_track" | "queue_all" | "search_tracks" | "browse_screen" | "select_item"
            | "go_back" | "skip_track" => LogEntryType::Track,
            "read_phase" | "read_alignment" | "nudge" | "adjust_tempo" | "jump_beats"
            | "set_crossfader" | "sweep_crossfader" => LogEntryType::Phase,
            _ => LogEntryType::Action,
        }
    }

    pub fn add_log(&mut self, entry_type: LogEntryType, message: String) {
        tracing::info!("ClaudeDJ: {message}");
        // Dashboard log panel shows the last 4 entries in a narrow column.
        // A long model reasoning paragraph wraps badly and pushes older
        // entries offscreen. Cap at ~180 chars (roughly 2 lines at 88
        // cols) with an ellipsis — full text is preserved in mixr.log
        // via the tracing::info above.
        const DASHBOARD_LOG_CAP: usize = 180;
        let message = if message.chars().count() > DASHBOARD_LOG_CAP {
            let head: String = message.chars().take(DASHBOARD_LOG_CAP).collect();
            format!("{head}…")
        } else {
            message
        };
        self.log.push(LogEntry {
            entry_type,
            message,
        });
        if self.log.len() > 200 {
            self.log.drain(0..50);
        }
    }

    pub fn can_call(&self) -> bool {
        match self.last_call {
            Some(last) => last.elapsed() >= self.min_interval,
            None => true,
        }
    }

    /// True while the DJ is mid-chain (waiting for app-side tool execution
    /// and a follow-up `continue_with_results`). Triggers should suppress
    /// during this window — overlapping triggers cancel-and-restart the
    /// chain, which inflates the conversation and exhausts the input
    /// token rate limit.
    pub fn is_busy(&self) -> bool {
        self.chain_in_progress
    }

    fn handle_rate_limit(&mut self) {
        self.min_interval = (self.min_interval * 2).min(Duration::from_secs(60));
        // Wipe the conversation so the next trigger isn't replaying
        // 30+ KB of accumulated tool_use/result history that just blew
        // the token budget. Loses some session context but stops the
        // bleed — fresh trigger starts clean.
        let cleared = self.conversation.len();
        self.conversation.clear();
        self.chain_in_progress = false;
        self.round = 0;
        self.add_log(
            LogEntryType::Info,
            format!(
                "Rate limited, backoff {}s, cleared {} conversation messages",
                self.min_interval.as_secs(),
                cleared
            ),
        );
    }

    fn reset_rate_limit(&mut self) {
        self.min_interval = Duration::from_secs(self.base_interval_secs());
    }

    /// Route to the appropriate system prompt for the current call
    /// mode. Performance mode keeps the prompt under ~400 tokens so
    /// per-call input cost stays low during the phase-correction
    /// loop — which can fire several times within one minute.
    fn system_prompt(&self) -> String {
        match self.call_mode {
            CallMode::Performance => self.system_prompt_performance(),
            CallMode::Prep => self.system_prompt_prep(),
        }
    }

    fn system_prompt_performance(&self) -> String {
        // Slim, focused. No memory, no recent sources, no curation
        // style guidance — none of it matters mid-mix. Claude's job
        // here is one thing: keep the phase and phrase aligned until
        // the sweep completes.
        "You are in PERFORMANCE MODE — an active crossfade is running. \
         The crossfader move is on autopilot via sweep_crossfader. \
         Your job: watch phase + phrase, nudge if they drift. \
         \
         TOOLS (performance): read_alignment, read_phase, nudge, \
         jump_beats, adjust_tempo, set_eq, set_filter, set_crossfader, \
         sweep_crossfader. Do NOT browse or queue — there is a PREP \
         mode for that between mixes. \
         \
         Rules (use beat_phase_fraction for BPM-independent thresholds): \
         - |ms| < 10 / fraction < 0.02: do NOTHING. Engine handles it. \
         - |ms| 10-30 / fraction 0.02-0.06: nudge 2-3 times SAME direction (positive = nudge -1, \
           negative = nudge +1). Do NOT read_alignment between nudges — stale for ~1 beat. \
         - |ms| > 30 / fraction > 0.06: nudge 4-5 times same direction. Still nudge, NOT jump_beats — \
           jump is a hard skip that glitches when both decks are live. \
         - beat_in_bar mismatch: call jump_beats — bar alignment IS worth the glitch. \
         - bar_in_phrase mismatch with imminent drop: usually accept, \
           unless the gap > 4 bars (then consider jump_beats by bars). \
         - NEVER nudge opposite to what the phase says. \
         - EQ kill: set_eq on one deck's lows around midpoint for a \
           bass-swap feel. \
         \
         Keep replies 1 short sentence + 1-2 tool calls. Do not \
         restate the context. If phase is tight and no action is \
         needed, reply 'aligned' and stop.".into()
    }

    /// Build the per-turn dynamic state block. This is appended to the
    /// user's trigger message on each call instead of baked into the
    /// system prompt — otherwise any session-state change (new queued
    /// track, new browsed screen, new memory entry) would invalidate
    /// Anthropic's prompt cache and force a full prefix rewrite every
    /// call. Empty when nothing is set yet.
    fn dynamic_state_block(&self) -> String {
        let recent_browse = if self.recent_screens.is_empty() {
            "none".into()
        } else {
            self.recent_screens
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(" → ")
        };
        let recent_queued = if self.recent_queued.is_empty() {
            "none".into()
        } else {
            self.recent_queued
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        let memory_summary = if self.settings.memory_enabled {
            let s = self.memory.prompt_summary();
            if s.is_empty() {
                String::new()
            } else {
                format!("\nMEMORY: {s}")
            }
        } else {
            String::new()
        };
        let user_dir = self
            .user_prompt
            .as_ref()
            .map(|p| format!("\nUSER DIRECTION: {p}"))
            .unwrap_or_default();
        format!(
            "\n\n--- session state ---\n\
             RECENT SOURCES: {recent_browse}\n\
             RECENTLY QUEUED: {recent_queued}{memory_summary}{user_dir}"
        )
    }

    fn system_prompt_prep(&self) -> String {
        // IMPORTANT: this prompt must stay stable across a session so
        // Anthropic's prompt cache actually hits. Dynamic state
        // (recent_screens, recent_queued, memory_summary, user_prompt)
        // moves into the per-turn user message via
        // `dynamic_state_block()` — otherwise every browse/queue would
        // invalidate the cache and force a full prefix rewrite at 1.25×
        // cost per call. Only settings-level knobs (style, strictness,
        // mode) remain here; those change rarely and re-cache is fine.

        // Compose style-specific digging guidance. Pure prompt flavor —
        // doesn't hard-gate anything, just tilts track selection.
        let style_guidance = match self.settings.style {
            DjStyle::Underground => {
                "DIG DEEP — think like an underground DJ: \
                prefer DJ-curated charts over generic Top 100, when using Top 100 pick \
                from tracks 20-100, search for labels you discover."
            }
            DjStyle::Mainstream => {
                "COMMERCIAL mode — Top 10 and Top 100 are fine, favor \
                recognizable tracks, don't dig too deep."
            }
            DjStyle::Exploratory => {
                "EXPLORE — cross-genre, break typical rules, favor \
                unexpected picks that reward the listener. Surprises are welcome."
            }
        };

        let camelot_rule = match self.settings.camelot_strictness {
            Strictness::Strict => {
                "Camelot key compatibility is REQUIRED: same key, \
                ±1 number same letter, or same number A↔B. Never queue a key-clashing track."
            }
            Strictness::Prefer => {
                "Prefer Camelot-compatible keys (same, ±1, or A↔B swap) \
                but a deliberate key jump is fine if it serves the set."
            }
            Strictness::Off => "",
        };

        let bpm_rule = match self.settings.bpm_gap_strictness {
            Strictness::Strict => "BPM gap must stay under 8% — never queue outside that.",
            Strictness::Prefer => {
                "Prefer BPM gaps under 8%; larger gaps are ok when paired \
                with an EchoOut-style transition."
            }
            Strictness::Off => "",
        };

        let round_cap = self.current_round_cap();

        let mode_guidance = match self.settings.mode {
            ClaudeDjMode::Auto => {
                "MODE: Auto. The engine handles beatmatching, crossfade \
                curves, and deck swaps. You pick tracks and monitor phase; nudge or adjust_tempo \
                only when the engine asks for help."
            }
            ClaudeDjMode::Assist => {
                "MODE: Assist. Auto drives the mix; you comment and \
                suggest but don't move faders. Flag issues in your reasoning text — the user is \
                watching the DJ log."
            }
            ClaudeDjMode::Manual => {
                "MODE: MANUAL. You are driving the mix. Physical decks \
                A and B are yours to load, preview, beatmatch, and crossfade. Use load_to_deck \
                to cue up the next track on the idle deck, preview_deck to audition in the \
                monitor bus, adjust tempo + nudge to match, seek_deck to a cue point, then \
                play_deck + set_crossfader to start the mix. The engine still runs phase-sync \
                math and the downbeat-align seek — that's safety rail, not autopilot. Read \
                `read_alignment` to check beat_in_bar + bar_in_phrase; `jump_beats` fixes \
                off-by-N errors without touching tempo. \
                \
                CRITICAL: When the engine enters Crossfading state, the crossfader does NOT \
                move on its own. Call sweep_crossfader(target, bars) ONCE with the correct \
                target — engine paces the move over bars of wall time. A is playing → \
                target=+1. B is playing → target=-1. (The direction FLIPS every mix because \
                the playing deck swaps.) Do NOT call set_crossfader five times in a row — \
                those all execute in milliseconds and produce a hard cut, not a sweep. \
                Use set_crossfader only for a snap or nudge. \
                \
                After sweep_crossfader starts the move, the crossfader is on autopilot — \
                BUT phase can still drift during the sweep. You will get mid-mix phase-check \
                triggers at ~30% and ~70% progress. On each check: call read_alignment. \
                \
                MID-MIX PHASE CORRECTION RULES (use beat_phase_fraction for BPM-independent decisions): \
                - If beat_phase_fraction < 0.02 (or |phase_ms| < 10): do NOTHING. The engine's rate corrector handles small drift automatically. \
                - If beat_phase_fraction 0.02-0.06 (or |phase_ms| 10-30): nudge 2-3 times in the SAME direction (negative phase = nudge +1, positive = nudge -1). \
                  Wait — do NOT read_alignment between nudges (data is stale for ~1 beat after a nudge). \
                - If beat_phase_fraction > 0.06 (or |phase_ms| > 30): nudge 4-5 times in the same direction. Still use nudge, NOT jump_beats — \
                  jump_beats is a hard position skip that causes an audible glitch when both decks are live. \
                - NEVER nudge opposite to what the phase says. If phase_ms is +15, nudge -1 (slow the incoming). \
                  If phase_ms is -15, nudge +1 (speed the incoming). \
                - If beat_in_bar is mismatched, call jump_beats to fix the bar alignment — that IS worth the glitch. \
                The sweep doesn't need your help; the phase does."
            }
        };

        format!(
            "You are an AI DJ running a live electronic music set on Beatport streaming. \
            Your job: select tracks, manage energy flow, and keep the set cohesive. \
            Think like a skilled DJ — read the energy, build tension, create moments. \
            \
            {mode_guidance} \
            \
            TOOL BUDGET: You have ~{round_cap} rounds per trigger. Each round re-sends the full \
            conversation as input, so exploratory browse/go_back loops are expensive and \
            cause rate-limit errors. COMMIT after the first useful browse — if you see \
            a decent track, queue it rather than digging further. Never call browse_screen twice \
            on the same screen — the first call returned everything you need. \
            \
            BROWSING: browse_screen shows breadcrumb + 20 items. select_item drills in, \
            go_back returns. search_tracks is for ARTIST or TRACK TITLE only — \
            queries containing genre names ('house', 'techno', etc.), BPM numbers, \
            or Camelot keys (3A, 9B…) will be REFUSED with an error. Beatport's text \
            search matches track titles, not metadata. For genres use the browse \
            tree; BPM/key compatibility is reported inline when you queue a track. \
            Tree: Discover > Genres > [Genre] > Charts or Top 100 > [Tracks]. \
            \
            {style_guidance} \
            - Never queue more than 2-3 tracks from the same chart. \
            - Diversify — each trigger's user message carries RECENT SOURCES + \
            RECENTLY QUEUED; check those and pick something different. \
            - Queue 1 track at a time. Verify BPM/key compatibility before queueing. \
            {camelot_rule} {bpm_rule} \
            \
            ENERGY ARC: Context shows Session: N min (phase). Shape energy \
            to the arc — low energy in warmup, build through the middle, peak \
            around 30-75 min, wind down after. Don't redline the opening or \
            flatline the peak. \
            \
            Keep reasoning brief (1-2 sentences) then take action."
        )
    }

    /// Append a browse screen to the session memory. Deduplicated against
    /// the most-recent entry (same-screen refresh doesn't bloat the list)
    /// and bounded to MEMORY_LEN.
    pub fn remember_browse(&mut self, breadcrumb: String) {
        if self.recent_screens.back() == Some(&breadcrumb) {
            return;
        }
        self.recent_screens.push_back(breadcrumb);
        while self.recent_screens.len() > MEMORY_LEN {
            self.recent_screens.pop_front();
        }
    }

    /// Record a queued track title so the next trigger can steer away
    /// from repeating artists/labels too tightly.
    pub fn remember_queued(&mut self, title: String) {
        self.recent_queued.push_back(title);
        while self.recent_queued.len() > MEMORY_LEN {
            self.recent_queued.pop_front();
        }
    }

    /// Run a triggered DJ call — returns tool calls for the app to execute.
    /// The app calls this, executes the tools, then calls `continue_with_results`.
    pub async fn trigger(
        &mut self,
        context: &str,
        trigger_reason: &str,
    ) -> Result<Vec<ToolCall>, anyhow::Error> {
        if !self.enabled || !self.can_call() {
            return Ok(vec![]);
        }

        // Fresh trigger chain — reset the round counter.
        self.round = 0;
        self.add_log(LogEntryType::Info, format!("Trigger: {trigger_reason}"));

        // Before pushing a new user trigger, close any dangling tool_use
        // blocks from a prior chain that was interrupted (e.g. a tool
        // execution failed to fire `continue_with_results`). Anthropic's
        // API rejects any message list where a `tool_use` id isn't
        // followed by a matching `tool_result`.
        self.close_dangling_tool_use();

        // Bound conversation size so it doesn't grow unbounded across a
        // long session. Trim respects tool_use/tool_result pairing so we
        // never cut mid-handshake.
        self.trim_conversation();

        // Build messages
        // Dynamic state (recent sources, memory, user direction) goes
        // HERE in the user message — not the system prompt. This keeps
        // the system prompt byte-stable across a session so Anthropic's
        // prompt cache actually reuses the prefix instead of rewriting
        // it at 1.25× cost every call.
        let dyn_state = self.dynamic_state_block();
        let user_msg = format!("{context}{dyn_state}\n\nTRIGGER: {trigger_reason}");
        self.conversation
            .push(serde_json::json!({"role": "user", "content": user_msg}));

        self.last_call = Some(Instant::now());
        let result = self
            .api
            .ask_with_tools(
                &self.system_prompt(),
                &self.conversation,
                &Self::tool_definitions_for(self.call_mode),
            )
            .await;

        match result {
            Ok(resp) => {
                self.reset_rate_limit();
                if let Some(ref text) = resp.text {
                    self.add_log(LogEntryType::Flow, text.clone());
                }
                // Add assistant response to conversation
                let mut content = Vec::new();
                if let Some(ref text) = resp.text {
                    content.push(serde_json::json!({"type": "text", "text": text}));
                }
                for tc in &resp.tool_calls {
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.input
                    }));
                    self.add_log(
                        Self::classify_tool(&tc.name),
                        format!("→ {}({})", tc.name, tc.input),
                    );
                }
                self.conversation
                    .push(serde_json::json!({"role": "assistant", "content": content}));
                // Mark chain in-progress only if Claude actually wants more
                // tool calls. An empty tool_calls list means "I'm done" —
                // chain ends here, next trigger is free to fire.
                self.chain_in_progress = !resp.tool_calls.is_empty();
                Ok(resp.tool_calls)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("429") {
                    self.handle_rate_limit();
                }
                if msg.contains("400") {
                    self.conversation.clear();
                    tracing::info!("ClaudeDJ: HTTP 400, cleared conversation to recover");
                }
                self.add_log(LogEntryType::Error, format!("API error: {msg}"));
                // Remove the user message we added (if conversation wasn't cleared)
                if !self.conversation.is_empty() {
                    self.conversation.pop();
                }
                self.chain_in_progress = false;
                Err(e)
            }
        }
    }

    /// Continue conversation after tool results are provided.
    pub async fn continue_with_results(
        &mut self,
        results: Vec<(String, String)>, // (tool_use_id, result_text)
    ) -> Result<Vec<ToolCall>, anyhow::Error> {
        if results.is_empty() {
            return Ok(vec![]);
        }

        self.round += 1;
        // `>` so round 15 is the last one that actually runs; at round 16
        // we short-circuit. Previously `>=` stopped at round 15, giving an
        // effective cap of 14 productive rounds.
        let cap = self.current_round_cap();
        if self.round > cap {
            // Chain exceeded the cap — stop calling the API and log loudly
            // so we can review overruns and decide whether to raise the
            // mode's limit. Auto = 10, Manual = 20 (beatmatch needs more).
            let msg = format!(
                "Tool-round cap hit ({}/{} mode={:?}) — stopping chain. \
                 Review mixr.log and consider raising MAX_ROUNDS / \
                 MAX_ROUNDS_MANUAL in src/claude/dj.rs if legitimate.",
                self.round, cap, self.settings.mode
            );
            self.add_log(LogEntryType::Error, msg.clone());
            tracing::info!("ClaudeDJ: {msg}");
            self.chain_in_progress = false;
            return Ok(vec![]);
        }

        // Before appending this round's results, retro-compact older
        // tool_result bodies. Each round's *input* to the API is the
        // full conversation so far — without this step a 10-round chain
        // sends the first round's tool outputs 10 times at 500 chars
        // each. Squashing stale bodies to a signature keeps the chain
        // under the 50k input-tokens/min limit.
        self.compact_old_tool_results();

        // Add tool results to conversation. Bodies are truncated so a
        // chatty tool (browse_screen returning 20 items) can't blow the
        // input-token budget on the next round — see TOOL_RESULT_MAX_CHARS.
        let content: Vec<Value> = results
            .iter()
            .map(|(id, text)| {
                let truncated: String = if text.chars().count() > TOOL_RESULT_MAX_CHARS {
                    let head: String = text.chars().take(TOOL_RESULT_MAX_CHARS).collect();
                    format!("{head}…[truncated]")
                } else {
                    text.clone()
                };
                serde_json::json!({"type": "tool_result", "tool_use_id": id, "content": truncated})
            })
            .collect();
        self.conversation
            .push(serde_json::json!({"role": "user", "content": content}));

        self.last_call = Some(Instant::now());
        let result = self
            .api
            .ask_with_tools(
                &self.system_prompt(),
                &self.conversation,
                &Self::tool_definitions_for(self.call_mode),
            )
            .await;

        match result {
            Ok(resp) => {
                self.reset_rate_limit();
                if let Some(ref text) = resp.text {
                    self.add_log(LogEntryType::Flow, text.clone());
                }
                let mut content = Vec::new();
                if let Some(ref text) = resp.text {
                    content.push(serde_json::json!({"type": "text", "text": text}));
                }
                for tc in &resp.tool_calls {
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.input
                    }));
                    self.add_log(
                        Self::classify_tool(&tc.name),
                        format!("→ {}({})", tc.name, tc.input),
                    );
                }
                self.conversation
                    .push(serde_json::json!({"role": "assistant", "content": content}));
                // Same logic as trigger(): chain continues only while
                // Claude is still asking for tool calls.
                self.chain_in_progress = !resp.tool_calls.is_empty();
                Ok(resp.tool_calls)
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("429") {
                    self.handle_rate_limit();
                }
                if msg.contains("400") {
                    self.conversation.clear();
                    tracing::info!("ClaudeDJ: HTTP 400, cleared conversation to recover");
                }
                self.add_log(LogEntryType::Error, format!("API error: {msg}"));
                if !self.conversation.is_empty() {
                    self.conversation.pop();
                }
                self.chain_in_progress = false;
                Err(e)
            }
        }
    }

    /// Keep conversation bounded. Finds a *safe* cut point that doesn't
    /// separate a `tool_use` block from its matching `tool_result` —
    /// Anthropic rejects messages with dangling ids, and blind
    /// `drain(0..20)` used to cause the 400 errors we saw in the wild.
    pub fn trim_conversation(&mut self) {
        if self.conversation.len() <= 40 {
            return;
        }
        // Safe cut = first user message after index 20 whose content is
        // a plain string (i.e., the start of a fresh trigger, not a
        // tool_result-carrying user message).
        let cut = self
            .conversation
            .iter()
            .enumerate()
            .skip(20)
            .find(|(_, msg)| {
                msg.get("role").and_then(|r| r.as_str()) == Some("user")
                    && msg.get("content").is_some_and(|c| c.is_string())
            })
            .map(|(i, _)| i);
        if let Some(idx) = cut {
            self.conversation.drain(0..idx);
        }
    }

    /// Synthesize `tool_result: cancelled` entries for any `tool_use`
    /// blocks in the most-recent assistant message that weren't followed
    /// by matching tool_results. Called at the start of each fresh
    /// trigger so an interrupted prior chain (e.g. tool execution
    /// failed to fire DjContinue) can't poison the next API call with
    /// dangling ids.
    fn close_dangling_tool_use(&mut self) {
        // Find the last assistant message.
        let last_assistant_idx = self
            .conversation
            .iter()
            .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"));
        let Some(idx) = last_assistant_idx else {
            return;
        };

        // Collect tool_use ids from the last assistant message.
        let mut ids: Vec<String> = Vec::new();
        if let Some(content) = self.conversation[idx]
            .get("content")
            .and_then(|c| c.as_array())
        {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && let Some(id) = block.get("id").and_then(|i| i.as_str())
                {
                    ids.push(id.to_string());
                }
            }
        }
        if ids.is_empty() {
            return;
        }

        // Check whether a following user message already carries matching
        // tool_result blocks. If ALL ids are covered, nothing to do.
        let mut resolved: std::collections::HashSet<String> = Default::default();
        for msg in self.conversation.iter().skip(idx + 1) {
            if msg.get("role").and_then(|r| r.as_str()) == Some("user")
                && let Some(arr) = msg.get("content").and_then(|c| c.as_array())
            {
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str())
                    {
                        resolved.insert(id.to_string());
                    }
                }
            }
        }
        let missing: Vec<String> = ids
            .into_iter()
            .filter(|id| !resolved.contains(id))
            .collect();
        if missing.is_empty() {
            return;
        }

        // Synthesize a user message with cancellation stubs for each
        // dangling id.
        let content: Vec<Value> = missing
            .iter()
            .map(|id| {
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": "cancelled: new trigger fired before results were delivered",
                })
            })
            .collect();
        self.conversation.push(serde_json::json!({
            "role": "user",
            "content": content,
        }));
        tracing::info!(
            "ClaudeDJ: closed {} dangling tool_use id(s) from prior chain",
            missing.len()
        );
    }

    /// Retro-truncate older tool_result bodies so cumulative input
    /// tokens stay bounded across a multi-round chain. Keeps the most
    /// recent `TOOL_RESULT_KEEP_RECENT` tool_result user-messages
    /// untouched (the model usually acts on the last result); any
    /// older ones get their `content` replaced with a short signature
    /// that preserves tool_use_id pairing but drops the body text.
    fn compact_old_tool_results(&mut self) {
        // Indexes of user messages that carry tool_result blocks.
        let tr_indexes: Vec<usize> = self
            .conversation
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if m.get("role").and_then(|r| r.as_str()) != Some("user") {
                    return None;
                }
                let arr = m.get("content").and_then(|c| c.as_array())?;
                if arr
                    .iter()
                    .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        if tr_indexes.len() <= TOOL_RESULT_KEEP_RECENT {
            return;
        }
        let stale_end = tr_indexes.len() - TOOL_RESULT_KEEP_RECENT;
        for &idx in &tr_indexes[..stale_end] {
            let Some(content) = self.conversation[idx]
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
            else {
                continue;
            };
            for block in content.iter_mut() {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                    continue;
                }
                let body = block.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let head: String = body.chars().take(TOOL_RESULT_STALE_MAX_CHARS).collect();
                if head.len() == body.len() {
                    continue;
                } // already shorter than cap
                block["content"] = serde_json::Value::String(format!("{head}…[stale-compacted]"));
            }
        }
    }

    /// Tool definitions for the Claude API.
    /// Tool set for the current call mode. Performance mode returns a
    /// slim subset (no browse/queue) to keep the per-call schema
    /// payload small and to prevent the DJ from wandering off into
    /// track-selection mid-mix.
    pub fn tool_definitions_for(mode: CallMode) -> Vec<Value> {
        let all = Self::tool_definitions();
        if mode == CallMode::Prep {
            return all;
        }
        // Performance tools: everything useful for correcting phase,
        // phrase, and rate during an active crossfade.
        const PERFORMANCE_TOOLS: &[&str] = &[
            "read_phase",
            "read_alignment",
            "nudge",
            "jump_beats",
            "adjust_tempo",
            "set_eq",
            "set_filter",
            "set_crossfader",
            "sweep_crossfader",
        ];
        all.into_iter()
            .filter(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| PERFORMANCE_TOOLS.contains(&n))
            })
            .collect()
    }

    pub fn tool_definitions() -> Vec<Value> {
        serde_json::json!([
            {
                "name": "browse_screen",
                "description": "See current browse screen items, selection, and breadcrumb path.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "select_item",
                "description": "Select an item by 0-based index on the current browse screen (like pressing Enter).",
                "input_schema": {
                    "type": "object",
                    "properties": { "index": { "type": "integer", "description": "0-based index" } },
                    "required": ["index"]
                }
            },
            {
                "name": "go_back",
                "description": "Go back one level in the browse tree.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "search_tracks",
                "description": "Search Beatport for tracks. Search for artist names or track titles, NOT BPM numbers.",
                "input_schema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }
            },
            {
                "name": "queue_track",
                "description": "Queue a track from the current screen by 0-based index.",
                "input_schema": {
                    "type": "object",
                    "properties": { "index": { "type": "integer" } },
                    "required": ["index"]
                }
            },
            {
                "name": "queue_all",
                "description": "Queue all tracks on the current screen.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "mix_now",
                "description": "Trigger crossfade to next track immediately.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "skip_track",
                "description": "Skip the current track.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "read_phase",
                "description": "Read current phase alignment, BPM, and time remaining.",
                "input_schema": { "type": "object", "properties": {} }
            },
            {
                "name": "adjust_tempo",
                "description": "Set BPM for a deck.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["playing", "incoming"] },
                        "bpm": { "type": "number" }
                    },
                    "required": ["deck", "bpm"]
                }
            },
            {
                "name": "nudge",
                "description": "Nudge the incoming deck for phase alignment.",
                "input_schema": {
                    "type": "object",
                    "properties": { "direction": { "type": "string", "enum": ["forward", "backward"] } },
                    "required": ["direction"]
                }
            },
            {
                "name": "set_crossfade_bars",
                "description": "Set crossfade duration (4-64 bars).",
                "input_schema": {
                    "type": "object",
                    "properties": { "bars": { "type": "integer" } },
                    "required": ["bars"]
                }
            },
            {
                "name": "extend_playback",
                "description": "Delay upcoming crossfade by N bars.",
                "input_schema": {
                    "type": "object",
                    "properties": { "bars": { "type": "integer" } },
                    "required": ["bars"]
                }
            },
            {
                "name": "set_eq",
                "description": "Set 3-band EQ on a specific deck in dB (range -24 to +12). Omit bands to leave them unchanged. Typical use: kill lows (-24) on playing deck while bringing in incoming.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "low": { "type": "number" },
                        "mid": { "type": "number" },
                        "high": { "type": "number" }
                    },
                    "required": ["deck"]
                }
            },
            {
                "name": "set_filter",
                "description": "Single-knob filter sweep on a specific deck. pos in [-1,+1]: -1 full low-pass (dark), 0 bypass, +1 full high-pass (thin).",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "pos": { "type": "number" }
                    },
                    "required": ["deck", "pos"]
                }
            },
            {
                "name": "set_transition",
                "description": "Pick the transition style used for the next crossfade. BeatMatched: equal-power sin sweep (matched BPMs). EchoOut: fast cut + echo tail, then bring in (mismatched BPMs or dramatic drops). BassSwap: both decks full, swap EQ lows at midpoint for a drop swap. FilterSweep: hi-pass the outgoing, reveal incoming from low-pass.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string", "enum": ["BeatMatched", "EchoOut", "BassSwap", "FilterSweep", "LoopRoll"] }
                    },
                    "required": ["type"]
                }
            },
            {
                "name": "loop_beats",
                "description": "Set a beat-aligned loop of N beats on a deck starting at its current position. Useful for stalling the playing deck while you prep a mix.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "beats": { "type": "number" }
                    },
                    "required": ["deck", "beats"]
                }
            },
            {
                "name": "cue",
                "description": "Hot cue: set, jump, or clear a cue slot (1-4) on a specific deck. Use to mark a downbeat for quick recall or to snap back to a drop.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "slot": { "type": "integer", "minimum": 1, "maximum": 4 },
                        "action": { "type": "string", "enum": ["set", "jump", "clear"] }
                    },
                    "required": ["deck", "slot", "action"]
                }
            },
            {
                "name": "loop_release",
                "description": "Release any active loop on a deck; playback continues past the loop's out-point.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] }
                    },
                    "required": ["deck"]
                }
            },
            // --- Manual-mode tools ---
            // These map to physical decks A/B rather than playing/incoming
            // roles. The engine translates internally. Available in all
            // modes — auto mode just rarely needs them.
            {
                "name": "load_to_deck",
                "description": "Load a track from the current browse screen onto a specific physical deck (A or B), regardless of which deck is currently playing. Use in MANUAL mode to cue up the next track on the idle deck.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "index": { "type": "integer", "description": "0-based index on current browse screen" }
                    },
                    "required": ["deck", "index"]
                }
            },
            {
                "name": "preview_deck",
                "description": "Start playing a deck through the monitor (headphones) bus only, not the main mix. Use to audition a cued track while the other deck is still playing live. Needs monitor_device configured.",
                "input_schema": {
                    "type": "object",
                    "properties": { "deck": { "type": "string", "enum": ["a", "b"] } },
                    "required": ["deck"]
                }
            },
            {
                "name": "stop_preview",
                "description": "Stop the currently-previewing deck (returns it to cued/paused state). Does not unload the track.",
                "input_schema": {
                    "type": "object",
                    "properties": { "deck": { "type": "string", "enum": ["a", "b"] } },
                    "required": ["deck"]
                }
            },
            {
                "name": "play_deck",
                "description": "Start a deck playing on the MAIN output (exits preview). In manual mode this is how you take the mix live — usually paired with a crossfader move.",
                "input_schema": {
                    "type": "object",
                    "properties": { "deck": { "type": "string", "enum": ["a", "b"] } },
                    "required": ["deck"]
                }
            },
            {
                "name": "seek_deck",
                "description": "Move a deck's playhead. `target` accepts a time in seconds (float), or one of: 'first_beat', 'drop', 'middle', 'start'. In manual mode use this to cue a track to the right starting point after BPM/phase matching.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "target": { "description": "seconds (number) or label (first_beat|drop|middle|start)" }
                    },
                    "required": ["deck", "target"]
                }
            },
            {
                "name": "set_crossfader",
                "description": "Snap the crossfader to an absolute position (−1 = full A, +1 = full B). Instant — use sweep_crossfader for a paced move over bars. Only call this when you want a hard cut or a nudge.",
                "input_schema": {
                    "type": "object",
                    "properties": { "pos": { "type": "number", "minimum": -1, "maximum": 1 } },
                    "required": ["pos"]
                }
            },
            {
                "name": "sweep_crossfader",
                "description": "PREFERRED for manual-mode mixes. Move the crossfader smoothly to `target` over `bars` bars of wall time (musical pacing). The engine interpolates the move; you don't call this repeatedly. Typical use: sweep_crossfader(target=+1, bars=8) at the start of a mix, then let the engine carry it. Returns immediately — the mix completes when the sweep finishes.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "number", "minimum": -1, "maximum": 1 },
                        "bars": { "type": "integer", "minimum": 1, "maximum": 32 }
                    },
                    "required": ["target", "bars"]
                }
            },
            {
                "name": "set_channel_fader",
                "description": "Per-deck channel fader (0..1). Use for a manual fade-in of a cued deck independent of the crossfader.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "level": { "type": "number", "minimum": 0, "maximum": 1 }
                    },
                    "required": ["deck", "level"]
                }
            },
            {
                "name": "jump_beats",
                "description": "Shift a deck's playhead by ±N beats (snap-aligned to its beat grid). Use to fix the 'off by 1' problem — when phase is tight but downbeats don't line up. Negative = back, positive = forward.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "deck": { "type": "string", "enum": ["a", "b"] },
                        "beats": { "type": "integer" }
                    },
                    "required": ["deck", "beats"]
                }
            },
            {
                "name": "read_alignment",
                "description": "Richer phase readout: returns beat_phase_ms (within-beat), beat_phase_fraction (0.0-0.5, BPM-independent fraction of a beat), beat_in_bar for each deck (0-3), and bar_in_phrase (0-15). If beat_phase_fraction > 0.02 (2% of a beat), nudge to correct.",
                "input_schema": { "type": "object", "properties": {} }
            }
        ]).as_array().unwrap().clone()
    }

    /// Would the next `continue_with_results` call be short-circuited by
    /// the round cap? Exposed for tests so the off-by-one boundary is
    /// guarded without needing to mock the Anthropic API.
    #[cfg(test)]
    pub(crate) fn would_cap_next_round(&self) -> bool {
        // Mirrors the logic in continue_with_results: the counter is
        // incremented first, so the cap fires when `round + 1 > cap`.
        // Uses the mode-aware cap so manual-mode tests don't trip on
        // the auto-mode ceiling.
        self.round + 1 > self.current_round_cap()
    }

    #[cfg(test)]
    pub(crate) fn set_round_for_test(&mut self, r: u32) {
        self.round = r;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dj() -> ClaudeDJ {
        // `ClaudeDJ::new` calls `DjMemory::load()` which reads
        // ~/.mixr/dj_memory.json. On a dev box where the user has
        // actually been rating mixes, that file has entries — which
        // pollutes tests that assert "empty by default". Wipe to a
        // default so tests are hermetic regardless of local state.
        let mut dj = ClaudeDJ::new("test-key".into());
        dj.inject_memory_for_test(DjMemory::default());
        dj
    }

    #[test]
    fn round_cap_constants_are_sane() {
        // Auto = 10: tuned against the 50k-input-token/min Anthropic
        // Haiku limit. Bumping silently would reintroduce 429s.
        // Manual = 20: beatmatching iterates read_phase → adjust →
        // read_phase loops, which legitimately need more rounds.
        assert_eq!(MAX_ROUNDS, 10);
        assert_eq!(MAX_ROUNDS_MANUAL, 20);
        const {
            assert!(
                MAX_ROUNDS_MANUAL > MAX_ROUNDS,
                "manual cap must exceed auto cap to be useful"
            )
        };
    }

    #[test]
    fn round_cap_boundary_is_inclusive_at_max() {
        // `>` boundary (was `>=` and effectively 1 short). Round
        // MAX_ROUNDS must still run; round MAX_ROUNDS+1 must short.
        let mut dj = fake_dj();
        dj.set_round_for_test(MAX_ROUNDS - 1);
        assert!(
            !dj.would_cap_next_round(),
            "last allowed round should still run"
        );
        dj.set_round_for_test(MAX_ROUNDS);
        assert!(dj.would_cap_next_round(), "round past max must be capped");
    }

    #[test]
    fn tool_result_truncation_constant_is_sane() {
        // Documents the per-tool-result body cap. Guards against
        // accidental zero or absurdly large values.
        const {
            assert!(
                TOOL_RESULT_MAX_CHARS >= 200,
                "too small to carry useful context"
            )
        };
        const {
            assert!(
                TOOL_RESULT_MAX_CHARS <= 5000,
                "too large to keep input tokens bounded"
            )
        };
    }

    #[test]
    fn fresh_dj_initial_state() {
        // A freshly-constructed DJ must be (a) idle so the first trigger
        // can fire, and (b) at round zero so the cap check doesn't
        // short-circuit the first continue_with_results.
        let dj = fake_dj();
        assert!(!dj.is_busy(), "fresh DJ should be idle");
        assert!(!dj.would_cap_next_round(), "fresh DJ should be at round 0");
    }

    #[test]
    fn close_dangling_tool_use_appends_cancellation_stub() {
        let mut dj = fake_dj();
        // Simulate an assistant message with tool_use and no matching
        // tool_result — this was the shape that produced the 400
        // "tool_use ids were referenced but not provided" in prod.
        dj.conversation
            .push(serde_json::json!({"role": "user", "content": "TRIGGER: x"}));
        dj.conversation.push(serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "tool_use", "id": "tool_abc", "name": "search_tracks", "input": {} }]
        }));
        dj.close_dangling_tool_use();
        // A new user message should have been added with a tool_result
        // for tool_abc.
        let last = dj.conversation.last().unwrap();
        assert_eq!(last["role"], "user");
        let content = last["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tool_abc");
    }

    #[test]
    fn close_dangling_tool_use_noop_when_all_resolved() {
        let mut dj = fake_dj();
        dj.conversation
            .push(serde_json::json!({"role": "user", "content": "x"}));
        dj.conversation.push(serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "tool_use", "id": "tool_abc", "name": "search", "input": {} }]
        }));
        dj.conversation.push(serde_json::json!({
            "role": "user",
            "content": [{ "type": "tool_result", "tool_use_id": "tool_abc", "content": "ok" }]
        }));
        let before = dj.conversation.len();
        dj.close_dangling_tool_use();
        assert_eq!(
            dj.conversation.len(),
            before,
            "should not add stub when already resolved"
        );
    }

    #[test]
    fn compact_old_tool_results_squashes_stale_bodies_only() {
        let mut dj = fake_dj();
        let big: String = "x".repeat(500);
        // 4 tool_result-carrying user messages: 0,1 stale; 2,3 recent (keep=2).
        for i in 0..4 {
            dj.conversation.push(serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": format!("t{i}"), "name": "x", "input": {}}]
            }));
            dj.conversation.push(serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": format!("t{i}"), "content": big}]
            }));
        }
        dj.compact_old_tool_results();
        let tr_msgs: Vec<&Value> = dj
            .conversation
            .iter()
            .filter(|m| {
                m.get("role").and_then(|r| r.as_str()) == Some("user")
                    && m.get("content")
                        .and_then(|c| c.as_array())
                        .is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result"))
            })
            .collect();
        assert_eq!(tr_msgs.len(), 4);
        // Stale (0,1) squashed
        for m in &tr_msgs[..2] {
            let body = m["content"][0]["content"].as_str().unwrap();
            assert!(
                body.ends_with("[stale-compacted]"),
                "stale body should be marked: {body}"
            );
            assert!(
                body.chars().count() < 200,
                "stale body should be short: {}",
                body.chars().count()
            );
        }
        // Recent (2,3) intact
        for m in &tr_msgs[2..] {
            let body = m["content"][0]["content"].as_str().unwrap();
            assert_eq!(
                body.chars().count(),
                500,
                "recent body must not be compacted"
            );
        }
    }

    #[test]
    fn compact_old_tool_results_noop_when_few() {
        let mut dj = fake_dj();
        let big: String = "x".repeat(500);
        dj.conversation.push(serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t0", "content": big.clone()}]
        }));
        dj.compact_old_tool_results();
        let body = dj.conversation[0]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert_eq!(
            body.chars().count(),
            500,
            "single tool_result must stay untouched"
        );
    }

    #[test]
    fn compact_old_tool_results_is_idempotent() {
        let mut dj = fake_dj();
        let big: String = "x".repeat(500);
        for i in 0..4 {
            dj.conversation.push(serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": format!("t{i}"), "content": big.clone()}]
            }));
        }
        dj.compact_old_tool_results();
        let snapshot = dj.conversation.clone();
        dj.compact_old_tool_results();
        assert_eq!(dj.conversation, snapshot, "compaction must be idempotent");
    }

    #[test]
    fn rate_limit_clears_state_and_backs_off() {
        let mut dj = fake_dj();
        dj.conversation
            .push(serde_json::json!({"role": "user", "content": "x"}));
        dj.chain_in_progress = true;
        dj.set_round_for_test(5);
        let base = dj.min_interval;
        dj.handle_rate_limit();
        assert!(
            dj.conversation.is_empty(),
            "429 must clear conversation to stop bleed"
        );
        assert!(!dj.is_busy(), "429 must drop chain_in_progress");
        assert_eq!(dj.round, 0, "429 must reset round counter");
        assert!(dj.min_interval > base, "429 must widen min_interval");
    }

    #[test]
    fn rate_limit_caps_backoff_at_60s() {
        let mut dj = fake_dj();
        for _ in 0..20 {
            dj.handle_rate_limit();
        }
        assert_eq!(
            dj.min_interval,
            Duration::from_secs(60),
            "backoff must cap at 60s"
        );
    }

    #[test]
    fn reset_rate_limit_returns_to_default() {
        let mut dj = fake_dj();
        dj.handle_rate_limit();
        dj.handle_rate_limit();
        dj.reset_rate_limit();
        assert_eq!(dj.min_interval, Duration::from_secs(2));
    }

    #[test]
    fn performance_call_mode_has_higher_round_cap_than_prep() {
        // Round caps route off call_mode (Prep/Performance), not the
        // user-visible settings.mode (Auto/Manual). Prep always wants
        // the tight cap (MAX_ROUNDS=10) because track selection rarely
        // needs more than ~8 rounds; Performance gets the higher cap
        // (MAX_ROUNDS_MANUAL=20) because beatmatching iterates.
        let mut dj = fake_dj();
        dj.set_call_mode(CallMode::Prep);
        dj.set_round_for_test(MAX_ROUNDS);
        assert!(
            dj.would_cap_next_round(),
            "prep mode at MAX_ROUNDS must cap the next round"
        );
        dj.set_call_mode(CallMode::Performance);
        assert!(
            !dj.would_cap_next_round(),
            "same round count under performance should still fit (higher cap)"
        );
        dj.set_round_for_test(MAX_ROUNDS_MANUAL);
        assert!(
            dj.would_cap_next_round(),
            "performance at MAX_ROUNDS_MANUAL must cap"
        );
    }

    #[test]
    fn mode_transition_resets_min_interval_floor() {
        let mut dj = fake_dj();
        // Sanity: default (auto) starts at the auto floor.
        assert_eq!(dj.min_interval, Duration::from_secs(MIN_INTERVAL_AUTO_SECS));
        // Simulate a 429 that widened the interval — then flip to manual.
        dj.min_interval = Duration::from_secs(30);
        let s = ClaudeDjSettings {
            mode: ClaudeDjMode::Manual,
            ..Default::default()
        };
        dj.apply_settings(s);
        // Transition must snap to the manual floor (faster loop) so
        // beatmatching can iterate — a stale 30s hold-over from a prior
        // backoff would freeze the whole preview/nudge/read_phase loop.
        assert_eq!(
            dj.min_interval,
            Duration::from_secs(MIN_INTERVAL_MANUAL_SECS)
        );
    }

    #[test]
    fn mode_transition_no_op_when_mode_unchanged() {
        let mut dj = fake_dj();
        dj.min_interval = Duration::from_secs(30);
        // apply_settings with same mode should NOT reset the interval
        // — otherwise an unrelated settings flip would erase an active
        // 429 backoff.
        let s = ClaudeDjSettings::default();
        dj.apply_settings(s);
        assert_eq!(
            dj.min_interval,
            Duration::from_secs(30),
            "same-mode settings patch must preserve the active backoff"
        );
    }

    #[test]
    fn system_prompt_round_budget_reflects_call_mode() {
        // Prep prompt advertises the Prep cap (only the Prep prompt
        // mentions rounds; Performance has its own slim guidance
        // without a budget number).
        let mut dj = fake_dj();
        dj.set_call_mode(CallMode::Prep);
        let prep_p = dj.system_prompt();
        assert!(
            prep_p.contains(&format!("{MAX_ROUNDS} rounds per trigger")),
            "prep prompt must advertise the prep round cap ({MAX_ROUNDS})"
        );
    }

    #[test]
    fn memory_enabled_on_with_entries_adds_block_to_dynamic_state() {
        // Memory now rides in the per-turn user message (dynamic_state_block)
        // rather than the system prompt, so it doesn't invalidate the
        // prompt cache. Test follows the data, not the old location.
        let mut dj = fake_dj();
        let mut m = DjMemory::default();
        m.remember_good(MixEntry {
            pair: "Alice → Bob".into(),
            bpm: None,
            key: None,
            transition: None,
            note: Some("clean".into()),
            rated_at: None,
        });
        dj.inject_memory_for_test(m);
        let block = dj.dynamic_state_block();
        assert!(
            block.contains("MEMORY"),
            "dynamic state must mention memory when non-empty"
        );
        assert!(
            block.contains("Alice → Bob"),
            "entry content must be surfaced"
        );
        // And NOT in the system prompt — that's the cache-stability invariant.
        assert!(
            !dj.system_prompt().contains("Alice → Bob"),
            "system prompt must stay stable — no dynamic memory content"
        );
    }

    #[test]
    fn memory_disabled_hides_block_even_when_entries_exist() {
        let mut dj = fake_dj();
        let s = ClaudeDjSettings {
            memory_enabled: false,
            ..Default::default()
        };
        dj.apply_settings(s);
        let mut m = DjMemory::default();
        m.remember_good(MixEntry {
            pair: "Alice → Bob".into(),
            bpm: None,
            key: None,
            transition: None,
            note: None,
            rated_at: None,
        });
        dj.inject_memory_for_test(m);
        let block = dj.dynamic_state_block();
        assert!(
            !block.contains("MEMORY"),
            "disabling memory must keep entries out of the dynamic state"
        );
        assert!(!block.contains("Alice → Bob"));
    }

    #[test]
    fn memory_empty_does_not_leave_dangling_header() {
        let dj = fake_dj();
        let block = dj.dynamic_state_block();
        assert!(
            !block.contains("MEMORY:"),
            "empty memory must not emit a dangling MEMORY: header in dynamic state"
        );
    }

    #[test]
    fn system_prompt_stays_stable_when_session_state_changes() {
        // Cache-stability guarantee: the system prompt must not depend
        // on recent_screens / recent_queued / memory / user_prompt. If
        // any of those change, the prompt must still match byte-for-byte
        // so Anthropic's prompt cache stays hot.
        let mut dj = fake_dj();
        let baseline = dj.system_prompt();

        dj.remember_browse("Genres/Techno/Top 100".into());
        dj.remember_browse("Discover/Trending".into());
        dj.remember_queued("ARTBAT - Horizon".into());
        dj.set_prompt("peak hour".into());
        let mut m = DjMemory::default();
        m.remember_good(MixEntry {
            pair: "X → Y".into(),
            bpm: None,
            key: None,
            transition: None,
            note: None,
            rated_at: None,
        });
        dj.inject_memory_for_test(m);

        let after = dj.system_prompt();
        assert_eq!(
            baseline, after,
            "session-state changes must NOT change the system prompt \
             — that would invalidate Anthropic prompt caching"
        );
    }

    #[test]
    fn rate_good_no_ops_when_memory_disabled() {
        let mut dj = fake_dj();
        let s = ClaudeDjSettings {
            memory_enabled: false,
            ..Default::default()
        };
        dj.apply_settings(s);
        dj.rate_good(MixEntry {
            pair: "x".into(),
            bpm: None,
            key: None,
            transition: None,
            note: None,
            rated_at: None,
        });
        assert!(
            dj.memory.good.is_empty(),
            "memory disabled must silently drop ratings — no surprise writes to disk"
        );
    }

    #[test]
    fn performance_prompt_is_shorter_than_prep() {
        // The whole point of performance mode is a smaller prompt.
        // Guards against the two prompts accidentally converging.
        let dj = fake_dj();
        let prep = dj.system_prompt_prep().len();
        let perf = dj.system_prompt_performance().len();
        // Performance should be meaningfully shorter — 60% threshold
        // leaves room for the essential mix-correction guidance but
        // guards against the two prompts accidentally converging as
        // future edits pile on.
        assert!(
            (perf as f64) < (prep as f64) * 0.6,
            "performance prompt ({perf} chars) should be <60% of prep prompt ({prep} chars)"
        );
    }

    #[test]
    fn performance_tools_are_subset_of_all() {
        let all = ClaudeDJ::tool_definitions();
        let perf = ClaudeDJ::tool_definitions_for(CallMode::Performance);
        let all_names: std::collections::HashSet<_> = all
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str().map(String::from)))
            .collect();
        let perf_names: std::collections::HashSet<_> = perf
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str().map(String::from)))
            .collect();
        assert!(
            perf_names.is_subset(&all_names),
            "performance tools must exist in the full set"
        );
        assert!(
            perf_names.len() < all_names.len(),
            "performance tools should be a proper subset"
        );
        // Performance must NOT include prep-specific tools.
        for banned in [
            "browse_screen",
            "queue_track",
            "search_tracks",
            "go_back",
            "select_item",
        ] {
            assert!(
                !perf_names.contains(banned),
                "performance set must exclude {banned} — no curation mid-mix"
            );
        }
        // Performance MUST include the critical mix-correction tools.
        for required in ["read_alignment", "nudge", "jump_beats", "sweep_crossfader"] {
            assert!(
                perf_names.contains(required),
                "performance set must include {required} for phase/phrase correction"
            );
        }
    }

    #[test]
    fn call_mode_switches_prompt_route() {
        let mut dj = fake_dj();
        dj.set_call_mode(CallMode::Prep);
        let prep = dj.system_prompt();
        dj.set_call_mode(CallMode::Performance);
        let perf = dj.system_prompt();
        assert_ne!(
            prep, perf,
            "call_mode must actually route to different prompts"
        );
        assert!(
            perf.contains("PERFORMANCE"),
            "performance branch must self-identify"
        );
    }

    #[test]
    fn prep_tools_is_full_set() {
        let all = ClaudeDJ::tool_definitions();
        let prep = ClaudeDJ::tool_definitions_for(CallMode::Prep);
        assert_eq!(
            all.len(),
            prep.len(),
            "prep mode must expose the full tool set — no hidden restrictions"
        );
    }

    #[test]
    fn system_prompt_reflects_manual_mode() {
        let mut dj = fake_dj();
        let s = ClaudeDjSettings {
            mode: ClaudeDjMode::Manual,
            ..Default::default()
        };
        dj.apply_settings(s);
        let prompt = dj.system_prompt();
        assert!(
            prompt.contains("MANUAL"),
            "manual mode must be flagged in the prompt so Claude knows to drive"
        );
        assert!(
            prompt.contains("load_to_deck") || prompt.contains("preview_deck"),
            "manual-mode prompt must reference physical-deck tools"
        );
    }

    #[test]
    fn system_prompt_reflects_assist_mode() {
        let mut dj = fake_dj();
        let s = ClaudeDjSettings {
            mode: ClaudeDjMode::Assist,
            ..Default::default()
        };
        dj.apply_settings(s);
        let prompt = dj.system_prompt();
        assert!(
            prompt.contains("Assist"),
            "assist mode must be flagged distinctly"
        );
    }

    #[test]
    fn system_prompt_default_mode_is_auto() {
        let dj = fake_dj();
        let prompt = dj.system_prompt();
        assert!(prompt.contains("Auto"));
        // Manual-only language must NOT appear in auto mode.
        assert!(
            !prompt.contains("MANUAL"),
            "auto mode should not promise manual tools"
        );
    }

    #[test]
    fn camelot_strictness_changes_prompt() {
        let mut dj = fake_dj();
        let mut s = ClaudeDjSettings {
            camelot_strictness: Strictness::Strict,
            ..Default::default()
        };
        dj.apply_settings(s.clone());
        assert!(
            dj.system_prompt().contains("REQUIRED"),
            "strict camelot must say so"
        );
        s.camelot_strictness = Strictness::Off;
        dj.apply_settings(s);
        let p = dj.system_prompt();
        assert!(
            !p.contains("REQUIRED") && !p.contains("Prefer Camelot"),
            "camelot=off must drop the rule entirely"
        );
    }

    #[test]
    fn style_changes_digging_guidance() {
        let mut dj = fake_dj();
        let mut s = ClaudeDjSettings {
            style: DjStyle::Mainstream,
            ..Default::default()
        };
        dj.apply_settings(s.clone());
        assert!(
            dj.system_prompt().contains("COMMERCIAL"),
            "mainstream style should read differently from default underground"
        );
        s.style = DjStyle::Exploratory;
        dj.apply_settings(s);
        assert!(
            dj.system_prompt().contains("EXPLORE"),
            "exploratory style must appear"
        );
    }

    #[test]
    fn apply_settings_persists_into_later_prompts() {
        // Regression guard: if `apply_settings` accidentally worked on a
        // clone, subsequent system_prompt calls would still use the
        // default settings.
        let mut dj = fake_dj();
        let s = ClaudeDjSettings {
            mode: ClaudeDjMode::Manual,
            ..Default::default()
        };
        dj.apply_settings(s);
        let a = dj.system_prompt();
        let b = dj.system_prompt();
        assert_eq!(a, b, "consecutive prompts must reflect the same settings");
        assert!(a.contains("MANUAL"));
    }

    #[test]
    fn remember_browse_dedupes_and_bounds() {
        let mut dj = fake_dj();
        dj.remember_browse("A".into());
        dj.remember_browse("A".into()); // dup at tail — ignored
        dj.remember_browse("B".into());
        assert_eq!(dj.recent_screens.len(), 2, "tail dedupe should collapse");
        // Push past MEMORY_LEN; oldest must drop.
        for i in 0..MEMORY_LEN + 5 {
            dj.remember_browse(format!("S{i}"));
        }
        assert_eq!(
            dj.recent_screens.len(),
            MEMORY_LEN,
            "recent_screens must be bounded at MEMORY_LEN"
        );
    }

    #[test]
    fn remember_queued_is_bounded_fifo() {
        let mut dj = fake_dj();
        for i in 0..MEMORY_LEN + 3 {
            dj.remember_queued(format!("Track {i}"));
        }
        assert_eq!(dj.recent_queued.len(), MEMORY_LEN);
        // Oldest entries drop first — the front should be Track 3, not Track 0.
        assert_eq!(
            dj.recent_queued.front().map(|s| s.as_str()),
            Some("Track 3"),
            "FIFO must drop oldest"
        );
    }

    #[test]
    fn add_log_caps_long_messages() {
        let mut dj = fake_dj();
        let long = "x".repeat(500);
        dj.add_log(LogEntryType::Info, long);
        let stored = &dj.log.last().unwrap().message;
        assert!(
            stored.chars().count() < 500,
            "long dashboard log entries must be truncated; got {} chars",
            stored.chars().count()
        );
        assert!(
            stored.ends_with('…'),
            "truncation must be marked with an ellipsis; got: {stored}"
        );
    }

    #[test]
    fn trim_conversation_short_is_noop() {
        let mut dj = fake_dj();
        for i in 0..10 {
            dj.conversation
                .push(serde_json::json!({"role": "user", "content": format!("u{i}")}));
        }
        let before = dj.conversation.len();
        dj.trim_conversation();
        assert_eq!(dj.conversation.len(), before);
    }

    #[test]
    fn trim_conversation_respects_tool_pairing() {
        let mut dj = fake_dj();
        // 45 alternating messages, positions 0..20 simple, then a
        // tool_use at 21 followed by tool_result at 22, etc.
        for i in 0..45 {
            let msg = if i % 2 == 0 {
                serde_json::json!({"role": "user", "content": format!("u{i}")})
            } else {
                serde_json::json!({"role": "assistant", "content": format!("a{i}")})
            };
            dj.conversation.push(msg);
        }
        dj.trim_conversation();
        // After trim, first message should be a user with string content
        // — i.e., we cut at a safe boundary.
        assert!(dj.conversation.len() < 45);
        let first = &dj.conversation[0];
        assert_eq!(first["role"], "user");
        assert!(first["content"].is_string());
    }
}
