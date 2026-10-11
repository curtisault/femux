//! Spike §16-1: diff extraction cost on a 200×60 grid under `yes`.
//! Each frame feeds one PTY-read-sized chunk of `yes` output (every row scrolls, so every row
//! is dirty: the worst case), then measures `sync()` alone.
use std::time::{Duration, Instant};

use femux_vt::Terminal;

fn main() {
    let mut t = Terminal::new(200, 60, 10_000).unwrap();
    let chunk: Vec<u8> = b"y\n".repeat(32 * 1024); // 64 KiB, a typical PTY read
    let styled: Vec<u8> = (0..60)
        .flat_map(|i| format!("\x1b[3{}m{}\x1b[0m\r\n", i % 8, "y".repeat(199)).into_bytes())
        .collect();

    for (name, input) in [("yes", &chunk), ("full-width styled lines", &styled)] {
        for _ in 0..50 {
            t.write(input);
            t.sync().unwrap();
        }
        let frames = 2000;
        let mut samples = Vec::with_capacity(frames);
        let mut rows = 0;
        for _ in 0..frames {
            t.write(input);
            let s = Instant::now();
            let st = t.sync().unwrap();
            samples.push(s.elapsed());
            rows += st.rows_copied;
        }
        samples.sort();
        let mean: Duration = samples.iter().sum::<Duration>() / frames as u32;
        let p = |q: f64| samples[((frames as f64 * q) as usize).min(frames - 1)];
        println!(
            "{name}: {} rows/frame  mean {:?}  p50 {:?}  p99 {:?}  max {:?}",
            rows / frames,
            mean,
            p(0.5),
            p(0.99),
            samples[frames - 1]
        );
    }
}
