//! Opt-in frame timing for the desktop UI.
//!
//! UI latency was the one surface that could not be argued about with evidence:
//! the agent and session paths have test harnesses, but "scrolling feels
//! janky" had no number attached to it. This records how long each event pass
//! takes and reports the distribution, so a UI performance claim can be checked
//! instead of debated.
//!
//! Disabled unless `THREADLANE_PERF=1`, so a normal run pays a single relaxed
//! atomic load per event and nothing else.
//!
//! ```text
//! THREADLANE_PERF=1 cargo run -p threadlane
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Frames slower than this drop the app below 60fps and are counted as jank.
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);

/// How often the running summary is emitted.
const REPORT_EVERY: Duration = Duration::from_secs(5);

/// Samples above this are dropped rather than growing without bound; the count
/// keeps rising so the reported total stays honest.
const MAX_SAMPLES: usize = 20_000;

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("THREADLANE_PERF")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

#[derive(Default)]
struct Samples {
    micros: Vec<u64>,
    last_report: Option<Instant>,
}

fn samples() -> &'static Mutex<Samples> {
    static SAMPLES: OnceLock<Mutex<Samples>> = OnceLock::new();
    SAMPLES.get_or_init(|| Mutex::new(Samples::default()))
}

static TOTAL: AtomicU64 = AtomicU64::new(0);
static JANK: AtomicU64 = AtomicU64::new(0);
static REPORTING: AtomicBool = AtomicBool::new(false);

/// Times one event pass.
///
/// Returned by [`frame`]; the measurement is taken when it drops, so a caller
/// cannot forget to stop the clock.
pub struct FrameTimer {
    start: Option<Instant>,
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        record(start.elapsed());
    }
}

/// Starts timing an event pass. A no-op unless `THREADLANE_PERF=1`.
pub fn frame() -> FrameTimer {
    FrameTimer {
        start: enabled().then(Instant::now),
    }
}

fn record(elapsed: Duration) {
    TOTAL.fetch_add(1, Ordering::Relaxed);
    if elapsed > FRAME_BUDGET {
        JANK.fetch_add(1, Ordering::Relaxed);
    }

    // A panic while reporting must not poison timing for the rest of the run.
    let mut samples = match samples().lock() {
        Ok(samples) => samples,
        Err(poisoned) => poisoned.into_inner(),
    };
    if samples.micros.len() < MAX_SAMPLES {
        samples.micros.push(elapsed.as_micros() as u64);
    }

    let now = Instant::now();
    let due = samples
        .last_report
        .is_none_or(|last| now.duration_since(last) >= REPORT_EVERY);
    if !due || samples.micros.is_empty() {
        return;
    }
    samples.last_report = Some(now);
    let mut sorted = std::mem::take(&mut samples.micros);
    drop(samples);

    // Reporting formats and prints; keep it off the lock and out of reentrancy.
    if REPORTING.swap(true, Ordering::Acquire) {
        return;
    }
    sorted.sort_unstable();
    eprintln!("{}", summarize(&sorted));
    REPORTING.store(false, Ordering::Release);
}

/// Formats a sorted sample set. Split out so it is testable without a UI.
fn summarize(sorted_micros: &[u64]) -> String {
    let total = TOTAL.load(Ordering::Relaxed);
    let jank = JANK.load(Ordering::Relaxed);
    format!(
        "[perf] frames={total} jank={jank} ({:.1}%) \
         p50={} p95={} p99={} max={} (over {} samples)",
        if total == 0 {
            0.0
        } else {
            (jank as f64 / total as f64) * 100.0
        },
        format_micros(percentile(sorted_micros, 50.0)),
        format_micros(percentile(sorted_micros, 95.0)),
        format_micros(percentile(sorted_micros, 99.0)),
        format_micros(sorted_micros.last().copied().unwrap_or(0)),
        sorted_micros.len(),
    )
}

/// Nearest-rank percentile over an ascending slice.
fn percentile(sorted_micros: &[u64], percentile: f64) -> u64 {
    if sorted_micros.is_empty() {
        return 0;
    }
    let rank = (percentile / 100.0 * sorted_micros.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted_micros.len() - 1);
    sorted_micros[index]
}

fn format_micros(micros: u64) -> String {
    if micros >= 1000 {
        format!("{:.1}ms", micros as f64 / 1000.0)
    } else {
        format!("{micros}µs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let sorted: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&sorted, 50.0), 50);
        assert_eq!(percentile(&sorted, 95.0), 95);
        assert_eq!(percentile(&sorted, 99.0), 99);
        // The top percentile must not index past the end.
        assert_eq!(percentile(&sorted, 100.0), 100);
    }

    #[test]
    fn percentiles_handle_degenerate_input() {
        assert_eq!(percentile(&[], 95.0), 0);
        assert_eq!(percentile(&[7], 50.0), 7);
        assert_eq!(percentile(&[7], 99.0), 7);
    }

    #[test]
    fn durations_render_at_a_readable_scale() {
        assert_eq!(format_micros(0), "0µs");
        assert_eq!(format_micros(999), "999µs");
        assert_eq!(format_micros(1000), "1.0ms");
        assert_eq!(format_micros(16_667), "16.7ms");
    }

    #[test]
    fn a_disabled_timer_records_nothing() {
        // Without THREADLANE_PERF the timer holds no start instant, which is
        // what keeps a normal run free of measurement overhead.
        let before = TOTAL.load(Ordering::Relaxed);
        {
            let _timer = FrameTimer { start: None };
        }
        assert_eq!(TOTAL.load(Ordering::Relaxed), before);
    }

    #[test]
    fn the_summary_reports_the_shape_of_the_distribution() {
        let sorted = vec![100u64, 200, 300, 20_000];
        let line = summarize(&sorted);
        assert!(line.starts_with("[perf] frames="), "got: {line}");
        assert!(line.contains("max=20.0ms"), "got: {line}");
        assert!(line.contains("over 4 samples"), "got: {line}");
    }
}
