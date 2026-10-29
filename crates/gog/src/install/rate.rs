//! Smoothing the install's overall transfer rate ourselves, instead of
//! summing what each file reports on its own.
//!
//! `download-manager`'s per-file rate is an EMA, but it force-samples once
//! more the instant a file finishes, bypassing the thresholds that keep
//! every other sample sane. A small file that completes before it ever
//! took a normal sample divides its whole size by whatever sliver of time
//! is left in that final force-sample — sometimes microseconds — and
//! since it's that file's *first* sample, there's no existing average to
//! soften it: the spike becomes the reported rate outright. Summed across
//! however many files are in flight, one such spike per install-wide
//! progress tick was enough to show multiple GB/s on an ordinary
//! connection. Measuring the install's total bytes over wall-clock time
//! here avoids the per-file arithmetic that produces it.

use std::time::{Duration, Instant};

pub(super) struct RateTracker {
    last_sample_at: Instant,
    last_sample_bytes: u64,
    ema_bps: f64,
}

impl RateTracker {
    const EMA_ALPHA: f64 = 0.2;
    const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

    /// `on_disk` is what a resumed download already had, so it doesn't
    /// read as transferred in the first sample.
    pub(super) fn new(on_disk: u64) -> Self {
        let now = Instant::now();
        Self {
            last_sample_at: now,
            last_sample_bytes: on_disk,
            ema_bps: 0.0,
        }
    }

    /// Feed the install's total bytes downloaded so far. Returns the
    /// smoothed rate to report — unchanged from the last sample when too
    /// little time has passed to measure one without the same kind of
    /// tiny-denominator spike this exists to avoid.
    pub(super) fn sample(&mut self, total_bytes: u64) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_sample_at);
        if dt < Self::MIN_SAMPLE_INTERVAL {
            return self.ema_bps;
        }

        let byte_delta = total_bytes.saturating_sub(self.last_sample_bytes);
        self.last_sample_at = now;
        self.last_sample_bytes = total_bytes;

        let instantaneous = byte_delta as f64 / dt.as_secs_f64();
        self.ema_bps = if self.ema_bps <= 0.0 {
            instantaneous
        } else {
            Self::EMA_ALPHA * instantaneous + (1.0 - Self::EMA_ALPHA) * self.ema_bps
        };
        // `trace!`, not `debug!`: this samples on every progress tick, the
        // same per-chunk frequency as the download loop feeding it.
        tracing::trace!(
            byte_delta,
            dt_ms = dt.as_millis(),
            ema_bps = self.ema_bps,
            "sampled transfer rate"
        );
        self.ema_bps
    }
}
