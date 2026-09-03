use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::mode::{
    CompletionSourceBatch, CompletionSourceDefinition, CompletionSourceError, CompletionSourceId,
    CompletionSourceTask, Mode, ModeAdapters, ModeError, ModeState, ModeViewContext,
};
use crate::mode_name::{ModeActionName, ModeName};
use tokio_util::sync::CancellationToken;
use vell_completion::{
    CompletionItem, CompletionRequest, IncompleteDirections, SourceBatchVersion,
};
use vell_core::text_snapshot::TextSnapshot;
use vell_protocol::ids::ContentId;
use vell_protocol::revision::Revision;

const SOURCE_ID: &str = "buffer-words";
const INDEX_CHUNK_CHARS: usize = 4 * 1024;
const MAX_INDEX_WORDS: usize = 100_000;
const MAX_INDEX_WORD_BYTES: usize = 4 * 1024;
const MAX_CACHE_ENTRIES: usize = 16;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_PREVIEW_YIELD_ITEMS: usize = 1024;
const FINAL_BATCH_YIELD_ITEMS: usize = 1024;

pub struct BufferWordCompletionMode {
    name: ModeName,
    sources: Vec<CompletionSourceDefinition>,
    cache: Arc<Mutex<BufferWordIndexCache>>,
}

impl BufferWordCompletionMode {
    fn new() -> Self {
        let source = CompletionSourceDefinition::new(
            CompletionSourceId::new(SOURCE_ID).expect("built-in completion source id is valid"),
            Duration::ZERO,
            Duration::from_secs(2),
            |_, _, _| {
                Box::pin(async {
                    Err(CompletionSourceError::new(
                        "buffer word source was not prepared with a text snapshot",
                    ))
                })
            },
        )
        .expect("built-in completion timing is valid");
        Self {
            name: ModeName::new("builtin.buffer-word-completion"),
            sources: vec![source],
            cache: Arc::new(Mutex::new(BufferWordIndexCache::default())),
        }
    }
}

impl Mode for BufferWordCompletionMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn completion_sources(&self) -> &[CompletionSourceDefinition] {
        &self.sources
    }

    fn prepare_completion_source(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        source: &CompletionSourceId,
        request: &CompletionRequest,
    ) -> Result<CompletionSourceTask, ModeError> {
        if source.as_str() != SOURCE_ID {
            return Err(ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: format!("unknown completion source '{}'", source.as_str()),
            });
        }
        let snapshot = context
            .buffer()
            .and_then(|buffer| buffer.text_snapshot())
            .ok_or_else(|| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: "buffer word completion requires text content".to_owned(),
            })?;
        let content = request.content();
        let revision = request.content_revision();
        let cache = self.cache.clone();
        CompletionSourceTask::new(
            self.sources[0].debounce(),
            self.sources[0].timeout(),
            move |request, cancellation, sink| {
                let snapshot = snapshot.clone();
                let cache = cache.clone();
                Box::pin(async move {
                    let cached = cache
                        .lock()
                        .expect("buffer word cache lock poisoned")
                        .get(content, revision);
                    let mut publisher = WordBatchPublisher::new(request, sink);
                    if let Some(words) = cached {
                        if !publish_cached_preview(&words, &cancellation, &mut publisher).await? {
                            return Ok(());
                        }
                        if !publisher.finish(&words, &cancellation).await? {
                            return Ok(());
                        }
                        return Ok(());
                    }

                    let words =
                        build_word_index_streaming(&snapshot, &cancellation, &mut publisher)
                            .await?;
                    if cancellation.is_cancelled() {
                        return Ok(());
                    }
                    if !publisher.finish(&words, &cancellation).await? {
                        return Ok(());
                    }
                    cache
                        .lock()
                        .expect("buffer word cache lock poisoned")
                        .insert(content, revision, words);
                    Ok(())
                })
            },
        )
        .map_err(|error| ModeError::CallbackFailed {
            mode: self.name.clone(),
            message: error.to_string(),
        })
    }
}

struct WordBatchPublisher {
    request: CompletionRequest,
    preview_probe: vell_completion::CompletionPreviewProbe,
    sink: crate::mode::CompletionSourceSink,
    preview_published: bool,
}

impl WordBatchPublisher {
    fn new(request: CompletionRequest, sink: crate::mode::CompletionSourceSink) -> Self {
        let preview_probe = request.preview_probe();
        Self {
            request,
            preview_probe,
            sink,
            preview_published: false,
        }
    }

    fn push(&mut self, word: Arc<str>) -> Result<bool, CompletionSourceError> {
        if self.preview_published
            || word.as_ref() == self.request.query()
            || !self.preview_probe.matches(&word)
        {
            return Ok(false);
        }
        self.publish(
            SourceBatchVersion(1),
            vec![CompletionItem::new(word.clone(), word)],
            false,
        )?;
        self.preview_published = true;
        Ok(true)
    }

    async fn finish(
        &self,
        words: &[Arc<str>],
        cancellation: &CancellationToken,
    ) -> Result<bool, CompletionSourceError> {
        let query = self.request.query();
        let query_is_indexed = words
            .binary_search_by(|word| word.as_ref().cmp(query))
            .is_ok();
        let mut items = Vec::with_capacity(words.len() - usize::from(query_is_indexed));
        for (index, word) in words.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Ok(false);
            }
            if word.as_ref() != query {
                items.push(CompletionItem::new(word.clone(), word.clone()));
            }
            if (index + 1) % FINAL_BATCH_YIELD_ITEMS == 0 {
                tokio::task::yield_now().await;
                if cancellation.is_cancelled() {
                    return Ok(false);
                }
            }
        }
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        self.publish(
            SourceBatchVersion(if self.preview_published { 2 } else { 1 }),
            items,
            true,
        )?;
        Ok(true)
    }

    fn publish(
        &self,
        version: SourceBatchVersion,
        items: Vec<CompletionItem>,
        is_final: bool,
    ) -> Result<(), CompletionSourceError> {
        self.sink
            .publish(CompletionSourceBatch::replace(
                version,
                items,
                is_final,
                IncompleteDirections::default(),
            ))
            .map_err(|error| {
                CompletionSourceError::new(format!("buffer word batch rejected: {error:?}"))
            })
    }
}

async fn publish_cached_preview(
    words: &[Arc<str>],
    cancellation: &CancellationToken,
    publisher: &mut WordBatchPublisher,
) -> Result<bool, CompletionSourceError> {
    for (index, word) in words.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        if publisher.push(word.clone())? {
            tokio::task::yield_now().await;
            return Ok(!cancellation.is_cancelled());
        }
        if (index + 1) % CACHE_PREVIEW_YIELD_ITEMS == 0 {
            tokio::task::yield_now().await;
        }
    }
    Ok(!cancellation.is_cancelled())
}

pub fn buffer_word_completion_mode() -> Box<dyn Mode> {
    Box::new(BufferWordCompletionMode::new())
}

#[derive(Default)]
struct BufferWordIndexCache {
    entries: VecDeque<BufferWordIndex>,
}

struct BufferWordIndex {
    content: ContentId,
    revision: Revision,
    words: Arc<[Arc<str>]>,
    bytes: usize,
}

impl BufferWordIndexCache {
    fn get(&mut self, content: ContentId, revision: Revision) -> Option<Arc<[Arc<str>]>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.content == content && entry.revision == revision)?;
        let entry = self.entries.remove(index)?;
        let words = entry.words.clone();
        self.entries.push_front(entry);
        Some(words)
    }

    fn insert(&mut self, content: ContentId, revision: Revision, words: Arc<[Arc<str>]>) {
        let bytes = words.iter().map(|word| word.len() + 16).sum::<usize>();
        self.entries.retain(|entry| entry.content != content);
        if bytes > MAX_CACHE_BYTES {
            return;
        }
        self.entries.push_front(BufferWordIndex {
            content,
            revision,
            words,
            bytes,
        });
        while self.entries.len() > MAX_CACHE_ENTRIES
            || self.entries.iter().map(|entry| entry.bytes).sum::<usize>() > MAX_CACHE_BYTES
        {
            self.entries.pop_back();
        }
    }
}

#[cfg(test)]
async fn cached_words(
    cache: Arc<Mutex<BufferWordIndexCache>>,
    content: ContentId,
    revision: Revision,
    snapshot: TextSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<[Arc<str>]>, CompletionSourceError> {
    if let Some(words) = cache
        .lock()
        .expect("buffer word cache lock poisoned")
        .get(content, revision)
    {
        return Ok(words);
    }
    let words = build_word_index(&snapshot, cancellation).await?;
    if cancellation.is_cancelled() {
        return Ok(words);
    }
    cache
        .lock()
        .expect("buffer word cache lock poisoned")
        .insert(content, revision, words.clone());
    Ok(words)
}

#[cfg(test)]
async fn build_word_index(
    snapshot: &TextSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<[Arc<str>]>, CompletionSourceError> {
    scan_word_index_with_limits(
        snapshot,
        cancellation,
        WordIndexLimits {
            chunk_chars: INDEX_CHUNK_CHARS,
            max_words: MAX_INDEX_WORDS,
            max_word_bytes: MAX_INDEX_WORD_BYTES,
            max_bytes: MAX_INDEX_BYTES,
        },
        |_| Ok(()),
    )
    .await
    .map(|(words, _)| words)
}

async fn build_word_index_streaming(
    snapshot: &TextSnapshot,
    cancellation: &CancellationToken,
    publisher: &mut WordBatchPublisher,
) -> Result<Arc<[Arc<str>]>, CompletionSourceError> {
    scan_word_index_with_limits(
        snapshot,
        cancellation,
        WordIndexLimits {
            chunk_chars: INDEX_CHUNK_CHARS,
            max_words: MAX_INDEX_WORDS,
            max_word_bytes: MAX_INDEX_WORD_BYTES,
            max_bytes: MAX_INDEX_BYTES,
        },
        |word| publisher.push(word).map(|_| ()),
    )
    .await
    .map(|(words, _)| words)
}

#[derive(Clone, Copy)]
struct WordIndexLimits {
    chunk_chars: usize,
    max_words: usize,
    max_word_bytes: usize,
    max_bytes: usize,
}

#[cfg(test)]
async fn build_word_index_with_limits(
    snapshot: &TextSnapshot,
    cancellation: &CancellationToken,
    limits: WordIndexLimits,
) -> Result<(Arc<[Arc<str>]>, usize), CompletionSourceError> {
    scan_word_index_with_limits(snapshot, cancellation, limits, |_| Ok(())).await
}

async fn scan_word_index_with_limits(
    snapshot: &TextSnapshot,
    cancellation: &CancellationToken,
    limits: WordIndexLimits,
    mut on_word: impl FnMut(Arc<str>) -> Result<(), CompletionSourceError>,
) -> Result<(Arc<[Arc<str>]>, usize), CompletionSourceError> {
    let mut words: BTreeSet<Arc<str>> = BTreeSet::new();
    let mut current = String::new();
    let mut current_overflowed = false;
    let mut word_bytes = 0_usize;
    let mut offset = 0;
    while offset < snapshot.len_chars()
        && words.len() < limits.max_words
        && word_bytes < limits.max_bytes
    {
        if cancellation.is_cancelled() {
            return Ok((Arc::from([]), offset));
        }
        let end = offset
            .saturating_add(limits.chunk_chars)
            .min(snapshot.len_chars());
        let chunk = snapshot
            .char_range_to_string(offset..end)
            .ok_or_else(|| CompletionSourceError::new("invalid text snapshot range"))?;
        for character in chunk.chars() {
            if crate::completion::is_identifier_char(character) {
                if !current_overflowed
                    && current.len().saturating_add(character.len_utf8()) <= limits.max_word_bytes
                {
                    current.push(character);
                } else {
                    current.clear();
                    current_overflowed = true;
                }
            } else if current_overflowed {
                current_overflowed = false;
            } else if !current.is_empty() {
                if let Some(word) =
                    insert_word(&mut words, &mut current, &mut word_bytes, limits.max_bytes)
                {
                    on_word(word)?;
                }
                if words.len() == limits.max_words || word_bytes >= limits.max_bytes {
                    break;
                }
            }
        }
        offset = end;
        tokio::task::yield_now().await;
    }
    if !current_overflowed
        && !current.is_empty()
        && words.len() < limits.max_words
        && word_bytes < limits.max_bytes
        && let Some(word) = insert_word(&mut words, &mut current, &mut word_bytes, limits.max_bytes)
    {
        on_word(word)?;
    }
    Ok((words.into_iter().collect::<Vec<_>>().into(), offset))
}

fn insert_word(
    words: &mut BTreeSet<Arc<str>>,
    current: &mut String,
    bytes: &mut usize,
    max_bytes: usize,
) -> Option<Arc<str>> {
    let word: Arc<str> = Arc::from(std::mem::take(current));
    let next = bytes.saturating_add(word.len() + 16);
    if next > max_bytes {
        *bytes = max_bytes;
        None
    } else if words.insert(word.clone()) {
        *bytes = next;
        Some(word)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_request(query: &str) -> CompletionRequest {
        use vell_completion::{
            CompletionEffect, CompletionEngine, CompletionEvent, CompletionRequestContext,
            CompletionRequestSeed, CompletionSourceKey, CompletionTextRange, CompletionTrigger,
        };
        use vell_protocol::ids::ViewId;
        use vell_protocol::selection::{Selection, TextOffset};

        let end = TextOffset {
            char_index: query.chars().count(),
        };
        let seed = CompletionRequestSeed::new(
            ViewId(1),
            ContentId(1),
            Revision(1),
            Revision(1),
            Selection::collapsed(end),
            CompletionTextRange::new(TextOffset::origin(), end).unwrap(),
            query,
            CompletionTrigger::Manual,
            CompletionRequestContext::new(None::<Arc<str>>, "", None::<Arc<str>>, query, ""),
        )
        .unwrap();
        CompletionEngine::default()
            .transition(CompletionEvent::Trigger {
                request: seed,
                sources: vec![CompletionSourceKey::from("test")],
            })
            .into_iter()
            .find_map(|effect| match effect {
                CompletionEffect::RequestSource { request, .. } => Some(request),
                _ => None,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn index_is_unicode_aware_unique_and_stable() {
        let snapshot = TextSnapshot::from_text("beta 变量 alpha beta Δelta");
        let words = build_word_index(&snapshot, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            words.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["alpha", "beta", "Δelta", "变量"]
        );
    }

    #[tokio::test]
    async fn cancelled_build_does_not_poison_the_revision_cache() {
        let cache = Arc::new(Mutex::new(BufferWordIndexCache::default()));
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let words = cached_words(
            cache.clone(),
            ContentId(1),
            Revision(1),
            TextSnapshot::from_text("alpha beta"),
            &cancellation,
        )
        .await
        .unwrap();

        assert!(words.is_empty());
        assert!(cache.lock().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn oversized_identifier_is_skipped_instead_of_truncated() {
        let oversized = "x".repeat(MAX_INDEX_WORD_BYTES + 1);
        let text = format!("{oversized} safe");
        let snapshot = TextSnapshot::from_text(&text);

        let words = build_word_index(&snapshot, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            words.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["safe"]
        );
    }

    #[tokio::test]
    async fn byte_limit_stops_before_scanning_a_large_tail() {
        let text = format!("aa bb cc {}", "tail ".repeat(10_000));
        let snapshot = TextSnapshot::from_text(&text);

        let (words, scanned_chars) = build_word_index_with_limits(
            &snapshot,
            &CancellationToken::new(),
            WordIndexLimits {
                chunk_chars: 4,
                max_words: 100,
                max_word_bytes: 16,
                max_bytes: 20,
            },
        )
        .await
        .unwrap();

        assert_eq!(words.iter().map(AsRef::as_ref).collect::<Vec<_>>(), ["aa"]);
        assert!(scanned_chars < snapshot.len_chars());
    }

    #[tokio::test]
    async fn cold_index_publishes_before_scanning_the_large_tail() {
        let text = format!("zzz alpha {}", "tail ".repeat(100_000));
        let snapshot = TextSnapshot::from_text(&text);
        let cancellation = CancellationToken::new();
        let cancel_after_first = cancellation.clone();
        let batches = Arc::new(Mutex::new(Vec::new()));
        let published = batches.clone();
        let sink = crate::mode::CompletionSourceSink::new(move |batch| {
            published.lock().unwrap().push(batch);
            cancel_after_first.cancel();
            Ok(())
        });
        let mut publisher = WordBatchPublisher::new(completion_request("a"), sink);

        let words = build_word_index_streaming(&snapshot, &cancellation, &mut publisher)
            .await
            .unwrap();

        assert!(words.is_empty());
        let batches = batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].items.len(), 1);
        assert_eq!(&*batches[0].items[0].label, "alpha");
        assert!(!batches[0].is_final);
    }

    #[tokio::test]
    async fn cold_index_uses_one_visible_preview_and_one_final_replace() {
        let text = format!(
            "zzz alpha {}",
            (0..10_000)
                .map(|index| format!("word{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let snapshot = TextSnapshot::from_text(&text);
        let batches = Arc::new(Mutex::new(Vec::new()));
        let published = batches.clone();
        let sink = crate::mode::CompletionSourceSink::new(move |batch| {
            published.lock().unwrap().push(batch);
            Ok(())
        });
        let mut publisher = WordBatchPublisher::new(completion_request("a"), sink);

        let words =
            build_word_index_streaming(&snapshot, &CancellationToken::new(), &mut publisher)
                .await
                .unwrap();
        assert!(
            publisher
                .finish(&words, &CancellationToken::new())
                .await
                .unwrap()
        );

        let batches = batches.lock().unwrap();
        assert_eq!(batches.len(), 2);
        assert!(matches!(
            batches[0].kind,
            vell_completion::CompletionBatchKind::Replace
        ));
        assert!(matches!(
            batches[1].kind,
            vell_completion::CompletionBatchKind::Replace
        ));
        assert_eq!(&*batches[0].items[0].label, "alpha");
        assert!(!batches[0].is_final);
        assert!(batches[1].is_final);
    }

    #[tokio::test]
    async fn cached_preview_yields_and_observes_cancel_without_a_match() {
        let words = (0..10_000)
            .map(|index| Arc::from(format!("word{index}").into_boxed_str()))
            .collect::<Vec<Arc<str>>>();
        let batches = Arc::new(Mutex::new(Vec::new()));
        let published = batches.clone();
        let sink = crate::mode::CompletionSourceSink::new(move |batch| {
            published.lock().unwrap().push(batch);
            Ok(())
        });
        let cancellation = CancellationToken::new();
        let mut publisher =
            WordBatchPublisher::new(completion_request(&"q".repeat(MAX_INDEX_WORD_BYTES)), sink);
        let cancel_after_yield = cancellation.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel_after_yield.cancel();
        });

        assert!(
            !publish_cached_preview(&words, &cancellation, &mut publisher)
                .await
                .unwrap()
        );
        cancel_task.await.unwrap();
        assert!(batches.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_batch_conversion_yields_and_observes_cancel() {
        let words = (0..10_000)
            .map(|index| Arc::from(format!("word{index}").into_boxed_str()))
            .collect::<Vec<Arc<str>>>();
        let batches = Arc::new(Mutex::new(Vec::new()));
        let published = batches.clone();
        let sink = crate::mode::CompletionSourceSink::new(move |batch| {
            published.lock().unwrap().push(batch);
            Ok(())
        });
        let cancellation = CancellationToken::new();
        let cancel_after_first_yield = cancellation.clone();
        let cancel_task = tokio::spawn(async move {
            cancel_after_first_yield.cancel();
        });
        let publisher = WordBatchPublisher::new(completion_request("a"), sink);

        assert!(!publisher.finish(&words, &cancellation).await.unwrap());
        cancel_task.await.unwrap();
        assert!(cancellation.is_cancelled());
        assert!(batches.lock().unwrap().is_empty());
    }
}
