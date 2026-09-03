//! Pure completion session, matching, and acceptance model.
//!
//! The crate owns no I/O and never calls a provider or frontend. Callers drive
//! it with [`CompletionEvent`] values, execute returned [`CompletionEffect`]s,
//! and render an owned [`CompletionSnapshot`].

mod engine;
mod matcher;
mod model;

#[cfg(feature = "benchmarking")]
pub mod benchmark_support;

pub use engine::CompletionEngine;
pub use matcher::CompletionPreviewProbe;
pub use model::{
    CandidateId, CompletionAcceptance, CompletionBatch, CompletionBatchKind, CompletionCandidate,
    CompletionConfigError, CompletionEffect, CompletionEvent, CompletionItem, CompletionLimits,
    CompletionRequest, CompletionRequestContext, CompletionRequestSeed, CompletionSessionId,
    CompletionSnapshot, CompletionSourceKey, CompletionTaskKey, CompletionTextRange,
    CompletionTrigger, IncompleteDirections, RequestEpoch, ResolveData, ResolveRequestKey,
    SelectionMove, SourceBatchVersion, SourceRequestKey,
};
