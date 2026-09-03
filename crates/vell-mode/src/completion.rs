use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use vell_completion::{
    CompletionBatchKind, CompletionItem, CompletionRequest, IncompleteDirections,
    SourceBatchVersion,
};

const MAX_DEBOUNCE: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_SOURCE_ERROR_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompletionSourceId(Arc<str>);

impl CompletionSourceId {
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, CompletionSourceConfigError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CompletionSourceConfigError::EmptyId);
        }
        if value.len() > MAX_SOURCE_ID_BYTES {
            return Err(CompletionSourceConfigError::IdTooLong);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionSourceConfigError {
    EmptyId,
    IdTooLong,
    DebounceTooLong,
    InvalidTimeout,
}

impl fmt::Display for CompletionSourceConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyId => "completion source id must not be empty",
            Self::IdTooLong => "completion source id exceeds 128 bytes",
            Self::DebounceTooLong => "completion source debounce exceeds ten seconds",
            Self::InvalidTimeout => {
                "completion source timeout must be between zero and sixty seconds"
            }
        })
    }
}

impl std::error::Error for CompletionSourceConfigError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSourceBatch {
    pub version: SourceBatchVersion,
    pub kind: CompletionBatchKind,
    pub items: Vec<CompletionItem>,
    pub is_final: bool,
    pub incomplete: IncompleteDirections,
}

impl CompletionSourceBatch {
    pub fn replace(
        version: SourceBatchVersion,
        items: Vec<CompletionItem>,
        is_final: bool,
        incomplete: IncompleteDirections,
    ) -> Self {
        Self {
            version,
            kind: CompletionBatchKind::Replace,
            items,
            is_final,
            incomplete,
        }
    }
}

#[derive(Clone)]
pub struct CompletionSourceSink {
    publish: Arc<
        dyn Fn(CompletionSourceBatch) -> Result<(), CompletionSourcePublishError> + Send + Sync,
    >,
}

impl CompletionSourceSink {
    pub fn new(
        publish: impl Fn(CompletionSourceBatch) -> Result<(), CompletionSourcePublishError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            publish: Arc::new(publish),
        }
    }

    pub fn publish(
        &self,
        batch: CompletionSourceBatch,
    ) -> Result<(), CompletionSourcePublishError> {
        (self.publish)(batch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionSourcePublishError {
    Cancelled,
    LimitExceeded,
    HostClosed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSourceError(Arc<str>);

impl CompletionSourceError {
    pub fn new(message: impl AsRef<str>) -> Self {
        let message = message.as_ref();
        if message.len() <= MAX_SOURCE_ERROR_BYTES {
            return Self(Arc::from(message));
        }
        let mut end = MAX_SOURCE_ERROR_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self(Arc::from(&message[..end]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CompletionSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::error::Error for CompletionSourceError {}

impl From<String> for CompletionSourceError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for CompletionSourceError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

pub type CompletionSourceResult = Result<(), CompletionSourceError>;
pub type CompletionSourceFuture =
    Pin<Box<dyn Future<Output = CompletionSourceResult> + Send + 'static>>;

type CompletionSourceRunner = dyn Fn(CompletionRequest, CancellationToken, CompletionSourceSink) -> CompletionSourceFuture
    + Send
    + Sync;

#[derive(Clone)]
pub struct CompletionSourceTask {
    debounce: Duration,
    timeout: Duration,
    run: Arc<CompletionSourceRunner>,
}

impl CompletionSourceTask {
    pub fn new(
        debounce: Duration,
        timeout: Duration,
        run: impl Fn(
            CompletionRequest,
            CancellationToken,
            CompletionSourceSink,
        ) -> CompletionSourceFuture
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, CompletionSourceConfigError> {
        validate_timing(debounce, timeout)?;
        Ok(Self {
            debounce,
            timeout,
            run: Arc::new(run),
        })
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Creates the provider future; work starts only when the caller polls it.
    ///
    /// The future must not block its executor thread. CPU work must stay bounded,
    /// periodically yield, and observe cancellation. The host can abort a pending
    /// future, but Rust cannot preempt synchronous code inside one poll.
    pub fn run(
        &self,
        request: CompletionRequest,
        cancellation: CancellationToken,
        sink: CompletionSourceSink,
    ) -> CompletionSourceFuture {
        (self.run)(request, cancellation, sink)
    }
}

impl fmt::Debug for CompletionSourceTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionSourceTask")
            .field("debounce", &self.debounce)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct CompletionSourceDefinition {
    id: CompletionSourceId,
    debounce: Duration,
    timeout: Duration,
    run: Arc<CompletionSourceRunner>,
}

impl CompletionSourceDefinition {
    pub fn new(
        id: CompletionSourceId,
        debounce: Duration,
        timeout: Duration,
        run: impl Fn(
            CompletionRequest,
            CancellationToken,
            CompletionSourceSink,
        ) -> CompletionSourceFuture
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, CompletionSourceConfigError> {
        validate_timing(debounce, timeout)?;
        Ok(Self {
            id,
            debounce,
            timeout,
            run: Arc::new(run),
        })
    }

    pub fn id(&self) -> &CompletionSourceId {
        &self.id
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn task(&self) -> CompletionSourceTask {
        CompletionSourceTask {
            debounce: self.debounce,
            timeout: self.timeout,
            run: self.run.clone(),
        }
    }
}

fn validate_timing(
    debounce: Duration,
    timeout: Duration,
) -> Result<(), CompletionSourceConfigError> {
    if debounce > MAX_DEBOUNCE {
        return Err(CompletionSourceConfigError::DebounceTooLong);
    }
    if timeout.is_zero() || timeout > MAX_TIMEOUT {
        return Err(CompletionSourceConfigError::InvalidTimeout);
    }
    Ok(())
}

impl fmt::Debug for CompletionSourceDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionSourceDefinition")
            .field("id", &self.id)
            .field("debounce", &self.debounce)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_definition_validates_runtime_bounds() {
        assert_eq!(
            CompletionSourceId::new("").unwrap_err(),
            CompletionSourceConfigError::EmptyId
        );
        assert_eq!(
            CompletionSourceId::new("x".repeat(129)).unwrap_err(),
            CompletionSourceConfigError::IdTooLong
        );
        let id = CompletionSourceId::new("words").unwrap();
        assert_eq!(
            CompletionSourceDefinition::new(
                id.clone(),
                Duration::from_secs(11),
                Duration::from_secs(1),
                |_, _, _| Box::pin(async { Ok(()) }),
            )
            .unwrap_err(),
            CompletionSourceConfigError::DebounceTooLong
        );
        assert_eq!(
            CompletionSourceDefinition::new(
                id,
                Duration::ZERO,
                Duration::ZERO,
                |_, _, _| Box::pin(async { Ok(()) }),
            )
            .unwrap_err(),
            CompletionSourceConfigError::InvalidTimeout
        );
    }

    #[test]
    fn source_error_is_bounded_before_crossing_the_message_seam() {
        let error = CompletionSourceError::new("界".repeat(2_000));

        assert!(error.as_str().len() <= MAX_SOURCE_ERROR_BYTES);
        assert!(error.as_str().is_char_boundary(error.as_str().len()));
    }
}
