//! Bounded, monotonic runtime request history, independent of presentation.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Number of measured intervals retained; each spans at least ten seconds.
const HISTORY_CAPACITY: usize = 120;
/// Minimum spacing between observations, including manual refreshes.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// One measured request interval.
#[derive(Clone, Debug)]
pub struct TrafficSample {
    /// Requests observed in this interval.
    pub requests: u64,
    /// Actual interval duration; refresh delays must not distort rates.
    pub seconds: f64,
}

impl TrafficSample {
    /// Measured request count for one sparkline interval.
    #[must_use]
    pub const fn spark_value(&self) -> u64 {
        // Integer counts retain exact observations; sparkline heights use
        // requests in the interval rather than fabricated interpolation.
        self.requests
    }
}

/// Fixed-capacity history collected only while the interactive console runs.
#[derive(Clone, Debug, Default)]
pub struct TrafficHistory {
    /// Oldest-to-newest measured intervals.
    pub samples: VecDeque<TrafficSample>,
    /// Last counter/time pair used as an interval baseline.
    baseline: Option<(Instant, u64)>,
}

impl TrafficHistory {
    /// Observe a cumulative counter, ignoring rapid manual refreshes.
    pub fn observe(&mut self, now: Instant, requests: u64) {
        let Some((previous_time, previous_count)) = self.baseline else {
            self.baseline = Some((now, requests));
            return;
        };
        let elapsed = now.saturating_duration_since(previous_time);
        if elapsed < SAMPLE_INTERVAL {
            return;
        }
        if self.samples.len() == HISTORY_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(TrafficSample {
            requests: requests.saturating_sub(previous_count),
            seconds: elapsed.as_secs_f64(),
        });
        self.baseline = Some((now, requests));
    }

    /// Return measured average and peak requests per second.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        clippy::as_conversions,
        reason = "display rates intentionally convert bounded counter sums to floating point"
    )]
    pub fn rates(&self) -> (f64, f64) {
        let requests = self
            .samples
            .iter()
            .map(|sample| sample.requests)
            .fold(0_u64, u64::saturating_add);
        let seconds: f64 = self.samples.iter().map(|sample| sample.seconds).sum();
        let peak = self
            .samples
            .iter()
            .map(|sample| sample.requests as f64 / sample.seconds)
            .fold(0.0_f64, f64::max);
        (requests as f64 / seconds.max(1.0), peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_do_not_add_samples_and_history_is_bounded() {
        let mut history = TrafficHistory::default();
        let start = Instant::now();
        history.observe(start, 100);
        history.observe(start + Duration::from_secs(1), 105);
        assert!(
            history.samples.is_empty(),
            "manual refresh cannot fabricate history"
        );
        for index in 1..=200_u64 {
            history.observe(start + Duration::from_secs(index * 10), 100 + index * 20);
        }
        assert_eq!(
            history.samples.len(),
            HISTORY_CAPACITY,
            "old observations must be evicted"
        );
        assert_eq!(
            history.rates(),
            (2.0, 2.0),
            "rates must use measured elapsed time"
        );
        history.observe(start + Duration::from_secs(2010), 0);
        assert_eq!(
            history.samples.back().map(|sample| sample.requests),
            Some(0),
            "counter reset must not underflow"
        );
    }
}
