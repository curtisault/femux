//! What debug mode's `metrics` records are made of (REQ-LOG-006): histograms of durations and
//! a few process readings from `/proc` (Linux; elsewhere they are absent).

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Durations in microseconds since the last [`Histogram::take`].
#[derive(Debug, Default, Clone)]
pub struct Histogram {
    samples: Vec<u64>,
}

impl Histogram {
    /// Keeps at most this many samples per interval; later ones replace earlier ones evenly.
    const CAP: usize = 4096;

    pub fn record_us(&mut self, us: u64) {
        if self.samples.len() < Self::CAP {
            self.samples.push(us);
        } else {
            let i = (us as usize).wrapping_mul(2_654_435_761) % Self::CAP;
            self.samples[i] = self.samples[i].max(us);
        }
    }

    pub fn record(&mut self, d: std::time::Duration) {
        self.record_us(d.as_micros() as u64);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// `{n, p50_ms, p99_ms, max_ms}`, and empties the histogram.
    pub fn take(&mut self) -> Value {
        let mut s = std::mem::take(&mut self.samples);
        if s.is_empty() {
            return json!({"n": 0});
        }
        s.sort_unstable();
        let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize] as f64 / 1000.0;
        json!({"n": s.len(), "p50_ms": at(0.5), "p99_ms": at(0.99), "max_ms": at(1.0)})
    }
}

/// Named counters and histograms collected between two `metrics` records.
#[derive(Debug, Default)]
pub struct Metrics {
    pub counters: BTreeMap<String, u64>,
    pub hist: BTreeMap<String, Histogram>,
}

impl Metrics {
    pub fn add(&mut self, name: &str, n: u64) {
        *self.counters.entry(name.to_owned()).or_default() += n;
    }

    pub fn time(&mut self, name: &str, d: std::time::Duration) {
        self.hist.entry(name.to_owned()).or_default().record(d);
    }

    /// Everything collected, as fields, and starts over.
    pub fn take(&mut self) -> Value {
        let counters: serde_json::Map<String, Value> = std::mem::take(&mut self.counters)
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();
        let hist: serde_json::Map<String, Value> = std::mem::take(&mut self.hist)
            .into_iter()
            .map(|(k, mut h)| (k, h.take()))
            .collect();
        json!({"counters": counters, "timings": hist})
    }
}

fn status_field(name: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with(name))?;
    l.split_whitespace().nth(1)?.parse().ok()
}

/// Resident memory of this process in bytes.
pub fn rss_bytes() -> Option<u64> {
    status_field("VmRSS:").map(|kb| kb * 1024)
}

/// Threads of this process.
pub fn threads() -> Option<u64> {
    status_field("Threads:")
}

/// Open file descriptors of this process.
pub fn open_fds() -> Option<u64> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|d| d.count() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_006_histograms_report_percentiles_and_reset() {
        let mut h = Histogram::default();
        for ms in 1..=100 {
            h.record_us(ms * 1000);
        }
        let v = h.take();
        assert_eq!(v["n"], 100);
        assert_eq!(v["p50_ms"], 51.0);
        assert_eq!(v["p99_ms"], 99.0);
        assert_eq!(v["max_ms"], 100.0);
        assert_eq!(h.take()["n"], 0);
        let mut m = Metrics::default();
        m.add("frames", 3);
        m.add("frames", 2);
        m.time("draw", std::time::Duration::from_millis(4));
        let v = m.take();
        assert_eq!(v["counters"]["frames"], 5);
        assert_eq!(v["timings"]["draw"]["n"], 1);
        if cfg!(target_os = "linux") {
            assert!(
                rss_bytes().unwrap() > 0 && threads().unwrap() >= 1 && open_fds().unwrap() >= 3
            );
        }
    }
}
