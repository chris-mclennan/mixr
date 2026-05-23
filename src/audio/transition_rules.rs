//! Rule engine for auto-selecting transitions based on track context.
//!
//! Rules are evaluated top-down; first match wins. Each rule has a `when`
//! (all conditions AND'd) and a `then` (force a type, cycle among a subset,
//! weighted random, or skip to the next rule).
//!
//! Persisted at `~/.mixr/transitions.json`. Missing file = sensible default.

use super::transition::TransitionType;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Snapshot of the state relevant to rule evaluation, built fresh each call.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuleContext {
    /// Percent BPM difference, normalized for double-time (max(p,i)/min − 1).
    pub bpm_gap_pct: f64,
    /// Camelot distance (0..=6) or 99 if either key unknown.
    pub key_dist: usize,
    pub last_transition: Option<TransitionType>,
    /// Number of mixes completed this session.
    pub mix_count: u32,
    /// Incoming track's average energy minus playing's, in RMS units (~0–1).
    /// Positive = incoming is louder/denser. NaN-safe via 0.0 fallback.
    pub energy_delta: f64,
    /// True if the playing deck's *current* phrase (at fade-in time) is a Drop.
    pub phrase_is_drop: bool,
    /// Minutes since the first track of this session started.
    pub time_in_set_min: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Condition {
    pub bpm_gap_pct_gt: Option<f64>,
    pub bpm_gap_pct_lt: Option<f64>,
    pub key_dist_eq: Option<usize>,
    pub key_dist_lte: Option<usize>,
    pub key_dist_gte: Option<usize>,
    pub last_transition_eq: Option<String>,
    pub last_transition_in: Option<Vec<String>>,
    /// [modulus, remainder] — matches when mix_count % modulus == remainder.
    pub mix_count_mod: Option<[u32; 2]>,
    pub energy_delta_gt: Option<f64>,
    pub energy_delta_lt: Option<f64>,
    pub phrase_is_drop: Option<bool>,
    pub time_in_set_min_gt: Option<u32>,
    pub time_in_set_min_lt: Option<u32>,
}

impl Condition {
    // The `!(ctx.X > v)` form here is deliberate, not a clippy candidate for
    // `partial_cmp` rewriting. `f64` is `PartialOrd`, not `Ord` — NaN compares
    // unordered. With `!(NaN > v)` the expression is `true`, so the rule
    // short-circuits to `return false` (the candidate transition is dropped).
    // That's the right default: a rule context with a NaN field must NOT
    // satisfy the condition. Switching to `partial_cmp().is_some_and(...)`
    // changes that semantic — NaN inputs would skip the rule entirely.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub fn matches(&self, ctx: &RuleContext) -> bool {
        if let Some(v) = self.bpm_gap_pct_gt && !(ctx.bpm_gap_pct > v) { return false; }
        if let Some(v) = self.bpm_gap_pct_lt && !(ctx.bpm_gap_pct < v) { return false; }
        if let Some(v) = self.key_dist_eq && ctx.key_dist != v { return false; }
        if let Some(v) = self.key_dist_lte && ctx.key_dist > v { return false; }
        if let Some(v) = self.key_dist_gte && ctx.key_dist < v { return false; }
        if let Some(ref s) = self.last_transition_eq {
            match (parse_type(s), ctx.last_transition) {
                (Some(a), Some(b)) if a == b => (),
                _ => return false,
            }
        }
        if let Some(ref list) = self.last_transition_in {
            let last = match ctx.last_transition { Some(t) => t, None => return false };
            if !list.iter().any(|s| parse_type(s) == Some(last)) { return false; }
        }
        if let Some([m, r]) = self.mix_count_mod
            && (m == 0 || ctx.mix_count % m != r) { return false; }
        if let Some(v) = self.energy_delta_gt && !(ctx.energy_delta > v) { return false; }
        if let Some(v) = self.energy_delta_lt && !(ctx.energy_delta < v) { return false; }
        if let Some(v) = self.phrase_is_drop && ctx.phrase_is_drop != v { return false; }
        if let Some(v) = self.time_in_set_min_gt && (ctx.time_in_set_min <= v) { return false; }
        if let Some(v) = self.time_in_set_min_lt && (ctx.time_in_set_min >= v) { return false; }
        true
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Action {
    /// Force one transition type. Also the single-string form in JSON.
    Force(String),
    /// Round-robin among a subset.
    Cycle { cycle: Vec<String> },
    /// Weighted random pick (seeded by mix_count for determinism).
    Weighted { weighted: Vec<(String, f64)> },
    /// Skip this rule; fall through to the next.
    Skip { skip: bool },
}

impl Action {
    fn resolve(&self, ctx: &RuleContext) -> Option<TransitionType> {
        match self {
            Self::Force(s) => parse_type(s),
            Self::Cycle { cycle } => {
                if cycle.is_empty() { return None; }
                let idx = (ctx.mix_count as usize) % cycle.len();
                parse_type(&cycle[idx])
            }
            Self::Weighted { weighted } => {
                let total: f64 = weighted.iter().map(|(_, w)| w.max(0.0)).sum();
                if total <= 0.0 { return None; }
                // Deterministic hash from mix_count → [0, total).
                let h = hash_u32(ctx.mix_count) as f64 / u32::MAX as f64 * total;
                let mut acc = 0.0;
                for (name, w) in weighted {
                    acc += w.max(0.0);
                    if h <= acc { return parse_type(name); }
                }
                parse_type(&weighted.last()?.0)
            }
            Self::Skip { skip } => if *skip { None } else { parse_type("BeatMatched") },
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    pub when: Condition,
    pub then: Action,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleConfig {
    pub rules: Vec<Rule>,
    pub default: String,
}

impl Default for RuleConfig {
    fn default() -> Self {
        // "Smart defaults" — context-aware with built-in variation so the
        // set doesn't feel like the same transition on every mix.
        // Rules evaluate top-down; first match with an enabled action wins.
        let w = |entries: &[(&str, f64)]| -> Action {
            Action::Weighted { weighted: entries.iter().map(|(s, w)| (s.to_string(), *w)).collect() }
        };
        Self {
            rules: vec![
                // 1. Extreme BPM gap → EchoOut. Engine already forces
                //    this at the 8% cutoff; here for declared intent.
                Rule {
                    when: Condition { bpm_gap_pct_gt: Some(8.0), ..Default::default() },
                    then: Action::Force("EchoOut".into()),
                },
                // 2. Early set (first 3 mixes, <10 min in) — stay safe,
                //    heavy on BeatMatched so the opener feels locked-in.
                Rule {
                    when: Condition { time_in_set_min_lt: Some(10), ..Default::default() },
                    then: w(&[("BeatMatched", 3.0), ("BassSwap", 1.0)]),
                },
                // 3. Incoming lands on a Drop phrase → energy moment,
                //    use LoopRoll to drive the tension, FilterSweep as
                //    a quieter alternative when keys aren't matched.
                Rule {
                    when: Condition { phrase_is_drop: Some(true), key_dist_lte: Some(2), ..Default::default() },
                    then: w(&[("LoopRoll", 2.0), ("FilterSweep", 1.0)]),
                },
                // 4. Big energy step-up → hard cut feel.
                Rule {
                    when: Condition { energy_delta_gt: Some(0.15), ..Default::default() },
                    then: w(&[("LoopRoll", 2.0), ("EchoOut", 1.0), ("FilterSweep", 1.0)]),
                },
                // 5. Streak-breakers: after 3-in-a-row of a type, pick
                //    something else for variety (mix_count_mod cycles).
                Rule {
                    when: Condition { last_transition_eq: Some("BassSwap".into()), mix_count_mod: Some([3, 2]), ..Default::default() },
                    then: w(&[("FilterSweep", 2.0), ("LoopRoll", 1.0)]),
                },
                Rule {
                    when: Condition { last_transition_eq: Some("BeatMatched".into()), mix_count_mod: Some([3, 2]), ..Default::default() },
                    then: w(&[("BassSwap", 2.0), ("FilterSweep", 2.0), ("LoopRoll", 1.0)]),
                },
                Rule {
                    when: Condition { last_transition_eq: Some("FilterSweep".into()), mix_count_mod: Some([3, 2]), ..Default::default() },
                    then: w(&[("BassSwap", 2.0), ("BeatMatched", 1.0), ("LoopRoll", 1.0)]),
                },
                // 6. Compatible keys (dist 0-1): BassSwap dominant, with
                //    occasional BeatMatched / FilterSweep for flavor.
                Rule {
                    when: Condition { key_dist_lte: Some(1), ..Default::default() },
                    then: w(&[("BassSwap", 4.0), ("BeatMatched", 1.0), ("FilterSweep", 1.0)]),
                },
                // 7. Key distance 2: FilterSweep-leaning with variety.
                Rule {
                    when: Condition { key_dist_eq: Some(2), ..Default::default() },
                    then: w(&[("FilterSweep", 3.0), ("BeatMatched", 2.0), ("LoopRoll", 1.0)]),
                },
                // 8. Key distance 3+: BeatMatched-dominant safe picks.
                Rule {
                    when: Condition { key_dist_gte: Some(3), ..Default::default() },
                    then: w(&[("BeatMatched", 3.0), ("LoopRoll", 1.0)]),
                },
            ],
            default: "BeatMatched".into(),
        }
    }
}

/// Holds config + mutable state (mix count, last transition).
pub struct RuleEngine {
    pub config: RuleConfig,
    pub mix_count: u32,
    pub last_transition: Option<TransitionType>,
}

impl RuleEngine {
    pub fn load() -> Self {
        let path = rules_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<RuleConfig>(&text) {
                tracing::info!("Loaded transition rules from {}", path.display());
                return Self { config: cfg, mix_count: 0, last_transition: None };
            }
            tracing::warn!("transitions.json exists but failed to parse — using defaults");
        }
        let engine = Self { config: RuleConfig::default(), mix_count: 0, last_transition: None };
        let _ = engine.save();
        engine
    }

    pub fn save(&self) -> std::io::Result<()> {
        save_rules(&self.config)
    }

    /// Pick a transition given current context. Evaluates rules top-down;
    /// first matching rule whose action resolves to an *enabled* type wins.
    /// Disabled types fall through to the next rule. Empty enabled-set
    /// means "all enabled".
    pub fn choose(&self, ctx: RuleContext, enabled: &[String]) -> TransitionType {
        let is_ok = |t: TransitionType| enabled.is_empty() || enabled.iter().any(|s| parse_type(s) == Some(t));
        for rule in &self.config.rules {
            if !rule.when.matches(&ctx) { continue; }
            if let Some(t) = rule.then.resolve(&ctx)
                && is_ok(t) { return t; }
        }
        let default = parse_type(&self.config.default).unwrap_or(TransitionType::BeatMatched);
        if is_ok(default) { default }
        // Ultimate fallback: first enabled type, else BeatMatched.
        else if let Some(first) = enabled.iter().find_map(|s| parse_type(s)) { first }
        else { TransitionType::BeatMatched }
    }

    /// Called by the engine after a crossfade completes.
    pub fn record(&mut self, t: TransitionType) {
        self.mix_count = self.mix_count.saturating_add(1);
        self.last_transition = Some(t);
    }
}

fn parse_type(s: &str) -> Option<TransitionType> {
    match s.to_ascii_lowercase().as_str() {
        "beatmatched" | "beat_matched" | "beat" => Some(TransitionType::BeatMatched),
        "echoout" | "echo_out" | "echo" => Some(TransitionType::EchoOut),
        "bassswap" | "bass_swap" | "bass" => Some(TransitionType::BassSwap),
        "filtersweep" | "filter_sweep" | "filter" => Some(TransitionType::FilterSweep),
        "looproll" | "loop_roll" | "loop" => Some(TransitionType::LoopRoll),
        _ => None,
    }
}

fn rules_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".mixr/transitions.json")
}

/// Persist a `RuleConfig` to `~/.mixr/transitions.json`. Caller should
/// ensure this is not called while holding the audio state lock — disk I/O
/// would stall the audio callback.
pub fn save_rules(cfg: &RuleConfig) -> std::io::Result<()> {
    let path = rules_path();
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let json = serde_json::to_string_pretty(cfg)
        .map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

fn hash_u32(x: u32) -> u32 {
    // splitmix-style mixer — good distribution, deterministic.
    let mut z = x.wrapping_add(0x9E3779B9);
    z = (z ^ (z >> 16)).wrapping_mul(0x85EBCA6B);
    z = (z ^ (z >> 13)).wrapping_mul(0xC2B2AE35);
    z ^ (z >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(bpm: f64, key: usize, last: Option<TransitionType>, n: u32) -> RuleContext {
        RuleContext { bpm_gap_pct: bpm, key_dist: key, last_transition: last, mix_count: n, ..RuleContext::default() }
    }

    #[test]
    fn default_rules_route_by_bpm_and_key() {
        // Smart defaults use Weighted picks for key-distance tiers,
        // so any single mix_count might land on a "flavor" pick.
        // Distribute across many seeds and assert the mode matches
        // the intended tier's dominant choice.
        let eng = RuleEngine { config: RuleConfig::default(), mix_count: 0, last_transition: None };

        // BPM mismatch → EchoOut (still Force).
        assert_eq!(eng.choose(ctx(12.0, 99, None, 0), &[]), TransitionType::EchoOut);

        // Sample-many across mix_count seeds, take the most common.
        fn mode(eng: &RuleEngine, base: RuleContext) -> TransitionType {
            let types = [
                TransitionType::BeatMatched, TransitionType::EchoOut,
                TransitionType::BassSwap, TransitionType::FilterSweep,
                TransitionType::LoopRoll,
            ];
            let mut counts = [0u32; 5];
            // Use time_in_set high enough to skip the "early set" rule.
            let mut c = base; c.time_in_set_min = 30;
            for i in 0..400u32 {
                c.mix_count = i;
                let picked = eng.choose(c, &[]);
                for (idx, t) in types.iter().enumerate() {
                    if *t == picked { counts[idx] += 1; break; }
                }
            }
            let best = counts.iter().enumerate().max_by_key(|(_, n)| **n).map(|(i, _)| i).unwrap();
            types[best]
        }

        assert_eq!(mode(&eng, ctx(2.0, 1, None, 0)), TransitionType::BassSwap,
            "compatible keys should favor BassSwap");
        assert_eq!(mode(&eng, ctx(2.0, 2, None, 0)), TransitionType::FilterSweep,
            "key distance 2 should favor FilterSweep");
        assert_eq!(mode(&eng, ctx(2.0, 5, None, 0)), TransitionType::BeatMatched,
            "far-key mixes should favor BeatMatched");
    }

    #[test]
    fn variety_rule_breaks_bassswap_streak() {
        // mix_count 2 % 3 == 2 + last=BassSwap matches the streak
        // breaker. Its Weighted action rolls FilterSweep / LoopRoll —
        // either is a valid break. Assert it's NOT BassSwap. Use
        // time_in_set_min=30 to skip the early-set rule.
        let eng = RuleEngine { config: RuleConfig::default(), mix_count: 2, last_transition: Some(TransitionType::BassSwap) };
        let mut c = ctx(2.0, 1, Some(TransitionType::BassSwap), 2);
        c.time_in_set_min = 30;
        let picked = eng.choose(c, &[]);
        assert!(matches!(picked, TransitionType::FilterSweep | TransitionType::LoopRoll),
            "BassSwap streak breaker must pick something other than BassSwap, got {picked:?}");
    }

    #[test]
    fn weighted_action_is_deterministic() {
        let cfg = RuleConfig {
            rules: vec![Rule {
                when: Condition::default(),
                then: Action::Weighted { weighted: vec![("BassSwap".into(), 0.7), ("FilterSweep".into(), 0.3)] },
            }],
            default: "BeatMatched".into(),
        };
        let eng = RuleEngine { config: cfg, mix_count: 0, last_transition: None };
        let a = eng.choose(ctx(0.0, 0, None, 0), &[]);
        let b = eng.choose(ctx(0.0, 0, None, 0), &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn extended_conditions_filter_correctly() {
        let cond = Condition {
            energy_delta_gt: Some(0.1),
            phrase_is_drop: Some(true),
            time_in_set_min_lt: Some(60),
            ..Default::default()
        };
        // Hits all three.
        let c = RuleContext { energy_delta: 0.2, phrase_is_drop: true, time_in_set_min: 30, ..Default::default() };
        assert!(cond.matches(&c));
        // Energy too low.
        let c = RuleContext { energy_delta: 0.05, phrase_is_drop: true, time_in_set_min: 30, ..Default::default() };
        assert!(!cond.matches(&c));
        // Not a drop.
        let c = RuleContext { energy_delta: 0.2, phrase_is_drop: false, time_in_set_min: 30, ..Default::default() };
        assert!(!cond.matches(&c));
        // Past the time limit.
        let c = RuleContext { energy_delta: 0.2, phrase_is_drop: true, time_in_set_min: 90, ..Default::default() };
        assert!(!cond.matches(&c));
    }

    #[test]
    fn disabled_transitions_are_skipped() {
        let eng = RuleEngine { config: RuleConfig::default(), mix_count: 0, last_transition: None };
        // BassSwap would normally win for (matched BPM, key dist 1). With it
        // disabled, the FilterSweep/BeatMatched rules below should catch.
        let enabled = vec!["BeatMatched".into(), "FilterSweep".into(), "EchoOut".into()];
        let t = eng.choose(ctx(2.0, 1, None, 0), &enabled);
        assert_ne!(t, TransitionType::BassSwap);
    }

    #[test]
    fn mix_count_mod_zero_modulus_never_matches() {
        // `m == 0` would be a divide-by-zero; the guard returns false.
        // No test covered this; add one.
        let cond = Condition { mix_count_mod: Some([0, 0]), ..Default::default() };
        assert!(!cond.matches(&ctx(0.0, 0, None, 0)));
        assert!(!cond.matches(&ctx(0.0, 0, None, 5)));
    }

    #[test]
    fn last_transition_in_with_none_returns_false() {
        // If `ctx.last_transition` is None, the list can't match — return
        // false so the rule doesn't fire before any mix has happened.
        let cond = Condition {
            last_transition_in: Some(vec!["BassSwap".into(), "FilterSweep".into()]),
            ..Default::default()
        };
        let mut c = ctx(0.0, 0, None, 0);
        c.last_transition = None;
        assert!(!cond.matches(&c));
        // Sanity: with a matching last_transition it does match.
        c.last_transition = Some(TransitionType::BassSwap);
        assert!(cond.matches(&c));
    }

    #[test]
    fn cycle_action_rotates_with_mix_count() {
        let cfg = RuleConfig {
            rules: vec![Rule {
                when: Condition::default(),
                then: Action::Cycle { cycle: vec!["BassSwap".into(), "FilterSweep".into(), "BeatMatched".into()] },
            }],
            default: "BeatMatched".into(),
        };
        let eng = RuleEngine { config: cfg, mix_count: 0, last_transition: None };
        assert_eq!(eng.choose(ctx(0.0, 0, None, 0), &[]), TransitionType::BassSwap);
        assert_eq!(eng.choose(ctx(0.0, 0, None, 1), &[]), TransitionType::FilterSweep);
        assert_eq!(eng.choose(ctx(0.0, 0, None, 2), &[]), TransitionType::BeatMatched);
        assert_eq!(eng.choose(ctx(0.0, 0, None, 3), &[]), TransitionType::BassSwap);
    }
}
