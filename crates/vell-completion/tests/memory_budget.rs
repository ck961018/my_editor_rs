#![cfg(feature = "benchmarking")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use vell_completion::benchmark_support::{CorpusKind, corpus};
use vell_completion::{
    CompletionBatch, CompletionEffect, CompletionEngine, CompletionEvent, CompletionRequestContext,
    CompletionRequestSeed, CompletionSourceKey, CompletionTextRange, CompletionTrigger,
    IncompleteDirections, SourceBatchVersion,
};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::{Selection, TextOffset};

struct TrackingAllocator;

static CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

fn record_allocation(size: usize) {
    let current = CURRENT_BYTES.fetch_add(size, Ordering::Relaxed) + size;
    PEAK_BYTES.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Delegates the exact layout supplied by the caller to System.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        CURRENT_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: The pointer was allocated by System with this layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: Delegates the allocation and original layout to System.
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                record_allocation(size - layout.size());
            } else {
                CURRENT_BYTES.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[test]
fn hundred_thousand_item_session_stays_below_memory_budget() {
    let baseline = CURRENT_BYTES.load(Ordering::SeqCst);
    PEAK_BYTES.store(baseline, Ordering::SeqCst);
    let items = corpus(100_000, CorpusKind::LongIdentifiers);
    let end = TextOffset { char_index: 4 };
    let request = CompletionRequestSeed::new(
        ViewId(1),
        ContentId(1),
        Revision(1),
        Revision(1),
        Selection::collapsed(end),
        CompletionTextRange::new(TextOffset::origin(), end).unwrap(),
        "ps42",
        CompletionTrigger::Manual,
        CompletionRequestContext::new(Some("rust"), "long.rs", Some("src/long.rs"), "ps42", ""),
    )
    .unwrap();
    let source = CompletionSourceKey::from("memory");
    let mut engine = CompletionEngine::default();
    let effects = engine.transition(CompletionEvent::Trigger {
        request,
        sources: vec![source.clone()],
    });
    let request = effects
        .iter()
        .find_map(|effect| match effect {
            CompletionEffect::RequestSource { request, .. } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    engine.transition(CompletionEvent::InstallBatch(CompletionBatch::replace(
        request.source_key(source),
        SourceBatchVersion(1),
        items,
        true,
        IncompleteDirections::default(),
    )));
    assert_eq!(engine.snapshot(ViewId(1)).unwrap().item_count, 100_000);

    let incremental_peak = PEAK_BYTES.load(Ordering::SeqCst).saturating_sub(baseline);
    println!("M0_COMPLETION_MEMORY incremental_peak_bytes={incremental_peak}");
    assert!(incremental_peak < 64 * 1024 * 1024);
}
