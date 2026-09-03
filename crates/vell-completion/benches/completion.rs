use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use vell_completion::benchmark_support::{CorpusKind, corpus, match_count};
use vell_completion::{
    CompletionBatch, CompletionEngine, CompletionEvent, CompletionItem, CompletionRequestContext,
    CompletionRequestSeed, CompletionSourceKey, CompletionTextRange, CompletionTrigger,
    IncompleteDirections, SourceBatchVersion,
};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::{Selection, TextOffset};

fn benchmark_source_key() -> vell_completion::SourceRequestKey {
    let seed = CompletionRequestSeed::new(
        ViewId(1),
        ContentId(1),
        Revision(1),
        Revision(1),
        Selection::collapsed(TextOffset::origin()),
        CompletionTextRange::new(TextOffset::origin(), TextOffset::origin()).unwrap(),
        "",
        CompletionTrigger::Manual,
        CompletionRequestContext::new(None::<String>, "bench", None::<String>, "", ""),
    )
    .unwrap();
    let source = CompletionSourceKey::from("benchmark");
    let mut engine = CompletionEngine::default();
    let effects = engine.transition(CompletionEvent::Trigger {
        request: seed,
        sources: vec![source.clone()],
    });
    effects
        .into_iter()
        .find_map(|effect| match effect {
            vell_completion::CompletionEffect::RequestSource { key, .. } => Some(key),
            _ => None,
        })
        .unwrap()
}

fn matcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("completion_match_top_100");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for (kind, query) in [
        (CorpusKind::Ascii, "ps42"),
        (CorpusKind::Unicode, "项δс42"),
        (CorpusKind::LongIdentifiers, "psgi42"),
    ] {
        for size in [1_000, 10_000, 100_000] {
            let items = corpus(size, kind);
            group.bench_with_input(
                BenchmarkId::new(format!("{kind:?}"), size),
                &size,
                |b, _| {
                    b.iter(|| {
                        black_box(match_count(query, black_box(&items), 100));
                    });
                },
            );
        }
    }
    group.finish();
}

fn batch_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("completion_batch_conversion");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    let key = benchmark_source_key();
    for kind in [
        CorpusKind::Ascii,
        CorpusKind::Unicode,
        CorpusKind::LongIdentifiers,
    ] {
        let raw = corpus(10_000, kind)
            .into_iter()
            .map(|item| item.label.to_string())
            .collect::<Vec<_>>();
        group.bench_function(format!("{kind:?}_10000"), |b| {
            b.iter(|| {
                let items = raw
                    .iter()
                    .map(|label| CompletionItem::new(label.clone(), label.clone()))
                    .collect();
                black_box(CompletionBatch::replace(
                    key.clone(),
                    SourceBatchVersion(1),
                    items,
                    true,
                    IncompleteDirections::default(),
                ));
            });
        });
    }
    group.finish();
}

fn memory_accounting(c: &mut Criterion) {
    let items = corpus(100_000, CorpusKind::Ascii);
    let mut group = c.benchmark_group("completion_memory_accounting");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("100000", |b| {
        b.iter(|| {
            black_box(
                items
                    .iter()
                    .map(|item| item.estimated_heap_bytes())
                    .sum::<usize>(),
            )
        });
    });
    group.finish();
}

fn streaming(c: &mut Criterion) {
    let chunks = corpus(100_000, CorpusKind::Ascii)
        .chunks(1_000)
        .map(<[CompletionItem]>::to_vec)
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("completion_streaming");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("100x1000", |b| {
        b.iter_batched(
            || chunks.clone(),
            |mut chunks| {
                let seed = CompletionRequestSeed::new(
                    ViewId(1),
                    ContentId(1),
                    Revision(1),
                    Revision(1),
                    Selection::collapsed(TextOffset::origin()),
                    CompletionTextRange::new(TextOffset::origin(), TextOffset::origin()).unwrap(),
                    "",
                    CompletionTrigger::Manual,
                    CompletionRequestContext::new(None::<String>, "bench", None::<String>, "", ""),
                )
                .unwrap();
                let source = CompletionSourceKey::from("stream");
                let mut engine = CompletionEngine::default();
                let effects = engine.transition(CompletionEvent::Trigger {
                    request: seed,
                    sources: vec![source.clone()],
                });
                let request = effects
                    .into_iter()
                    .find_map(|effect| match effect {
                        vell_completion::CompletionEffect::RequestSource { request, .. } => {
                            Some(request)
                        }
                        _ => None,
                    })
                    .unwrap();
                let first = chunks.remove(0);
                engine.transition(CompletionEvent::InstallBatch(CompletionBatch::replace(
                    request.source_key(source.clone()),
                    SourceBatchVersion(1),
                    first,
                    false,
                    IncompleteDirections::default(),
                )));
                let chunk_count = chunks.len();
                for (index, chunk) in chunks.into_iter().enumerate() {
                    engine.transition(CompletionEvent::InstallBatch(CompletionBatch::append(
                        request.source_key(source.clone()),
                        SourceBatchVersion(1),
                        index as u32 + 1,
                        chunk,
                        index + 1 == chunk_count,
                        IncompleteDirections::default(),
                    )));
                }
                black_box(engine.snapshot(ViewId(1)).unwrap());
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    matcher,
    batch_conversion,
    memory_accounting,
    streaming
);
criterion_main!(benches);
