#![cfg(feature = "benchmarking")]

use std::hint::black_box;
use std::time::{Duration, Instant};

use vell_completion::benchmark_support::{CorpusKind, corpus, match_count};

fn p95(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

#[test]
#[ignore = "manual M0 latency distribution"]
fn m0_matcher_latency_distribution() {
    const ITERATIONS: usize = 50;
    for (kind, query) in [
        (CorpusKind::Ascii, "ps42"),
        (CorpusKind::Unicode, "项δс42"),
        (CorpusKind::LongIdentifiers, "psgi42"),
    ] {
        for size in [1_000, 10_000, 100_000] {
            let items = corpus(size, kind);
            let mut samples = Vec::with_capacity(ITERATIONS);
            for _ in 0..ITERATIONS {
                let started = Instant::now();
                black_box(match_count(query, black_box(&items), 100));
                samples.push(started.elapsed());
            }
            let p95 = p95(&mut samples);
            println!(
                "M0_COMPLETION_LATENCY kind={kind:?} size={size} \
                 iterations={ITERATIONS} p95_us={:.3}",
                p95.as_secs_f64() * 1_000_000.0,
            );
        }
    }
}
