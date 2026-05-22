//! Audio callback profiler — per-section timings with rolling p99 stats.
//!
//! Goals:
//! - **Zero allocation in the hot path** — the ring buffer is sized at startup.
//! - **Per-section attribution** — separate timers for deck fill, echo read,
//!   mixing loop, and recorder push so regressions land on the guilty section.
//! - **Headroom relative to the budget** — each callback's elapsed is divided
//!   by the buffer-period budget so a `ratio > 0.9` flags a near-miss.
//!
//! Stats are logged every 10 s at INFO and exposed via `engine::diagnose()`.
//! The hot path takes ~6 wall-clock samples per callback (~50 ns each on M1)
//! plus a single ring-buffer write — overhead is < 1 µs.

use std::time::Instant;

#[derive(Debug, Clone, Copy, Default)]
pub struct CallbackSample {
    pub total_us: u32,
    pub decks_us: u32,
    pub echo_us: u32,
    pub mix_us: u32,
    /// budget µs = frames * 1e6 / sample_rate. Cached here to avoid a divide
    /// when computing miss percentages.
    pub budget_us: u32,
}

impl CallbackSample {
    /// total / budget. > 0.9 = near-miss; > 1.0 = dropout.
    pub fn ratio(&self) -> f32 {
        if self.budget_us == 0 { 0.0 } else { self.total_us as f32 / self.budget_us as f32 }
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ProfileStats {
    pub samples: u32,
    pub avg_total_us: u32,
    pub p50_total_us: u32,
    pub p95_total_us: u32,
    pub p99_total_us: u32,
    pub max_total_us: u32,
    pub avg_ratio: f32,
    pub p99_ratio: f32,
    pub miss_count: u32,         // ratio > 1.0 (dropout)
    pub near_miss_count: u32,    // ratio > 0.9
    pub avg_decks_us: u32,
    pub avg_echo_us: u32,
    pub avg_mix_us: u32,
}

pub struct AudioProfiler {
    ring: Vec<CallbackSample>,
    head: usize,
    count: usize,
    capacity: usize,
    last_log: Instant,
}

impl AudioProfiler {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            ring: vec![CallbackSample::default(); cap],
            head: 0,
            count: 0,
            capacity: cap,
            last_log: Instant::now(),
        }
    }

    /// Record one callback. No allocations. O(1).
    pub fn push(&mut self, s: CallbackSample) {
        self.ring[self.head] = s;
        self.head = (self.head + 1) % self.capacity;
        if self.count < self.capacity { self.count += 1; }
    }

    /// Compute current rolling stats. Allocates a temporary buffer for the
    /// percentile sort — call from the main thread, not the audio callback.
    pub fn stats(&self) -> ProfileStats {
        if self.count == 0 { return ProfileStats::default(); }
        let n = self.count;
        let mut totals: Vec<u32> = self.ring[..n].iter().map(|s| s.total_us).collect();
        totals.sort_unstable();
        let p = |q: f32| -> u32 {
            let idx = ((n as f32 - 1.0) * q) as usize;
            totals[idx]
        };
        let mut sum_total = 0u64;
        let mut sum_ratio = 0.0f32;
        let mut max_ratio = 0.0f32;
        let mut sum_decks = 0u64;
        let mut sum_echo = 0u64;
        let mut sum_mix = 0u64;
        let mut miss = 0u32;
        let mut near = 0u32;
        for s in &self.ring[..n] {
            sum_total += s.total_us as u64;
            sum_decks += s.decks_us as u64;
            sum_echo += s.echo_us as u64;
            sum_mix += s.mix_us as u64;
            let r = s.ratio();
            sum_ratio += r;
            if r > max_ratio { max_ratio = r; }
            if r > 1.0 { miss += 1; }
            else if r > 0.9 { near += 1; }
        }
        ProfileStats {
            samples: n as u32,
            avg_total_us: (sum_total / n as u64) as u32,
            p50_total_us: p(0.50),
            p95_total_us: p(0.95),
            p99_total_us: p(0.99),
            max_total_us: *totals.last().unwrap_or(&0),
            avg_ratio: sum_ratio / n as f32,
            p99_ratio: max_ratio, // approximate — full percentile would need a second sort
            miss_count: miss,
            near_miss_count: near,
            avg_decks_us: (sum_decks / n as u64) as u32,
            avg_echo_us: (sum_echo / n as u64) as u32,
            avg_mix_us: (sum_mix / n as u64) as u32,
        }
    }

    /// Log a summary if at least 10 s have passed since the last log.
    /// Returns true if a log was emitted.
    ///
    /// Log-level policy: routine stat lines land at DEBUG (filtered out by
    /// default) to avoid spamming `mixr.log` during normal playback. Only
    /// when the callback is actually unhealthy — `misses > 0` or the p99
    /// ratio is climbing past 0.5 — do we escalate to INFO so the signal
    /// stands out. Turn on DEBUG (`RUST_LOG=mixr=debug` or equivalent) if
    /// you're watching baseline perf.
    pub fn maybe_log(&mut self) -> bool {
        if self.last_log.elapsed().as_secs() < 10 { return false; }
        let s = self.stats();
        if s.samples == 0 { return false; }
        let noisy = s.miss_count > 0 || s.p99_ratio > 0.5;
        let line = format!(
            "audio: avg={}µs p50={}µs p95={}µs p99={}µs max={}µs ratio_avg={:.2} ratio_max={:.2} misses={} near={} | decks={}µs echo={}µs mix={}µs",
            s.avg_total_us, s.p50_total_us, s.p95_total_us, s.p99_total_us, s.max_total_us,
            s.avg_ratio, s.p99_ratio, s.miss_count, s.near_miss_count,
            s.avg_decks_us, s.avg_echo_us, s.avg_mix_us,
        );
        if noisy { tracing::info!("{line}"); } else { tracing::debug!("{line}"); }
        self.last_log = Instant::now();
        true
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_correctly() {
        let mut p = AudioProfiler::new(4);
        for i in 0..10u32 {
            p.push(CallbackSample { total_us: i, budget_us: 100, ..Default::default() });
        }
        assert_eq!(p.count, 4); // saturated
        let s = p.stats();
        // Last 4 pushes had total_us 6, 7, 8, 9. Average = 7.5 → 7.
        assert_eq!(s.avg_total_us, 7);
        assert_eq!(s.max_total_us, 9);
    }

    #[test]
    fn miss_counts_track_ratio_correctly() {
        let mut p = AudioProfiler::new(8);
        // 100 µs budget, sample at 50 → ratio 0.5 (no miss)
        // sample at 95 → 0.95 (near-miss)
        // sample at 110 → 1.10 (miss)
        for us in [50, 95, 110, 50] {
            p.push(CallbackSample { total_us: us, budget_us: 100, ..Default::default() });
        }
        let s = p.stats();
        assert_eq!(s.miss_count, 1);
        assert_eq!(s.near_miss_count, 1);
    }
}
