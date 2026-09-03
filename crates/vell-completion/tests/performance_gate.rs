#![cfg(feature = "benchmarking")]

use std::hint::black_box;
use std::time::{Duration, Instant};

use vell_completion::benchmark_support::{CorpusKind, corpus, match_count};

#[derive(Clone, Copy)]
struct Measurement {
    kind: CorpusKind,
    size: usize,
    p95: Duration,
}

fn p95_match(size: usize, kind: CorpusKind, query: &str) -> Duration {
    let items = corpus(size, kind);
    for _ in 0..3 {
        black_box(match_count(query, black_box(&items), 100));
    }
    let mut samples = (0..25)
        .map(|_| {
            let started = Instant::now();
            black_box(match_count(query, black_box(&items), 100));
            started.elapsed()
        })
        .collect::<Vec<_>>();
    samples.sort_unstable();
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

#[test]
#[ignore = "release-only CI performance gate"]
fn completion_matcher_absolute_and_scaling_gate() {
    let mut measurements = Vec::new();
    for (kind, query, gate_100k) in [
        (CorpusKind::Ascii, "ps42", true),
        (CorpusKind::Unicode, "项δс42", true),
        (CorpusKind::LongIdentifiers, "psgi42", true),
    ] {
        let ten_thousand = p95_match(10_000, kind, query);
        let hundred_thousand = p95_match(100_000, kind, query);
        measurements.extend([
            Measurement {
                kind,
                size: 10_000,
                p95: ten_thousand,
            },
            Measurement {
                kind,
                size: 100_000,
                p95: hundred_thousand,
            },
        ]);
        println!(
            "M0_COMPLETION_GATE kind={kind:?} p95_10k_us={:.3} \
             p95_100k_us={:.3}",
            ten_thousand.as_secs_f64() * 1_000_000.0,
            hundred_thousand.as_secs_f64() * 1_000_000.0,
        );
        assert!(ten_thousand < Duration::from_millis(4));
        if gate_100k {
            assert!(hundred_thousand < Duration::from_millis(16));
        }
        // This hardware-normalized scaling gate catches complexity regressions.
        assert!(hundred_thousand < ten_thousand.saturating_mul(15));
    }
    assert_relative_regression(&measurements);
}

fn assert_relative_regression(measurements: &[Measurement]) {
    let reference_log = std::env::var("COMPLETION_REFERENCE_LOG").ok();
    let reference = reference_log
        .as_deref()
        .map(std::fs::read_to_string)
        .transpose()
        .unwrap()
        .unwrap_or_else(|| include_str!("../benchmarks/ci-baseline.csv").to_owned());
    let allowed_ratio = if reference_log.is_some() { 1.25 } else { 2.0 };
    for measurement in measurements {
        let kind = format!("{:?}", measurement.kind);
        let reference_us = parse_reference(&reference, &kind, measurement.size)
            .unwrap_or_else(|| panic!("missing reference for {kind}/{}", measurement.size));
        let current_us = measurement.p95.as_secs_f64() * 1_000_000.0;
        assert!(
            current_us <= reference_us * allowed_ratio,
            "{kind}/{} regressed: current={current_us:.3}us \
             reference={reference_us:.3}us allowed={:.0}%",
            measurement.size,
            (allowed_ratio - 1.0) * 100.0,
        );
    }
}

fn parse_reference(input: &str, kind: &str, size: usize) -> Option<f64> {
    input.lines().find_map(|line| {
        if let Some(rest) = line.strip_prefix("M0_COMPLETION_GATE kind=") {
            let mut fields = rest.split_whitespace();
            let actual_kind = fields.next()?;
            let ten = fields.next()?.strip_prefix("p95_10k_us=")?.parse().ok()?;
            let hundred = fields.next()?.strip_prefix("p95_100k_us=")?.parse().ok()?;
            return (actual_kind == kind).then_some(if size == 10_000 { ten } else { hundred });
        }
        let mut fields = line.split(',');
        let actual_kind = fields.next()?;
        let actual_size = fields.next()?.parse::<usize>().ok()?;
        let p95 = fields.next()?.parse().ok()?;
        (actual_kind == kind && actual_size == size).then_some(p95)
    })
}
