use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::application::App;
use crate::kernel::FileBaseline;
use crate::layout::create_view;
use vell_core::content::{
    Content, ContentEffect, ContentInput, ContentKind, ContentResult, SaveSnapshot,
};
use vell_frontend::Frontend;
use vell_protocol::content_query::{
    BufferBackingState, ContentData, ContentQuery, DirtyState, SaveState,
};
use vell_protocol::ids::ContentId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    pub content: ContentId,
    pub resource_name: Option<String>,
    pub resource_path: Option<String>,
    pub backing_state: BufferBackingState,
    pub dirty_state: DirtyState,
    pub save_state: SaveState,
}

#[derive(Debug)]
pub(super) struct PreparedBufferReload {
    pub path: PathBuf,
    pub text: String,
    pub backing_state: BufferBackingState,
    pub baseline: FileBaseline,
}

pub(super) struct PreparedBufferSaveAs {
    pub snapshot: SaveSnapshot,
    pub identity: PathBuf,
    pub force: bool,
}

pub(super) enum PreparedBufferOpen {
    Existing(ContentId),
    Load { path: PathBuf, identity: PathBuf },
}

#[derive(Debug)]
pub enum BufferLifecycleError {
    MissingContent(ContentId),
    UnsupportedContent(ContentId),
    Dirty(ContentId),
    PendingSave(ContentId),
    NoPath(ContentId),
    ExternalConflict { content: ContentId, path: PathBuf },
    PathOccupied { path: PathBuf, content: ContentId },
    Io { path: PathBuf, source: io::Error },
    Layout(String),
}

impl fmt::Display for BufferLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContent(content) => write!(formatter, "missing content {content:?}"),
            Self::UnsupportedContent(content) => {
                write!(formatter, "content {content:?} is not a buffer")
            }
            Self::Dirty(content) => write!(formatter, "buffer {content:?} has unsaved changes"),
            Self::PendingSave(content) => {
                write!(formatter, "buffer {content:?} has a pending save")
            }
            Self::NoPath(content) => write!(formatter, "buffer {content:?} has no path"),
            Self::ExternalConflict { content, path } => write!(
                formatter,
                "buffer {content:?} conflicts with external changes at {}",
                path.display()
            ),
            Self::PathOccupied { path, content } => write!(
                formatter,
                "path {} is already open as buffer {content:?}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            Self::Layout(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for BufferLifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl<F: Frontend> App<F> {
    pub fn new_buffer(&mut self) -> ContentId {
        let content = self.kernel.create_content(ContentKind::Buffer);
        self.session
            .register_content_profile(content, ContentKind::Buffer);
        content
    }

    pub fn open_buffer(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<ContentId, BufferLifecycleError> {
        let prepared = self.prepare_open_buffer(path.as_ref())?;
        self.commit_open_buffer(prepared)
    }

    pub(super) fn prepare_open_buffer(
        &self,
        requested_path: &Path,
    ) -> Result<PreparedBufferOpen, BufferLifecycleError> {
        let (path, identity) =
            normalize_path(requested_path).map_err(|source| BufferLifecycleError::Io {
                path: requested_path.to_owned(),
                source,
            })?;
        if let Some(content) = self.kernel.content_for_path(&identity) {
            return Ok(PreparedBufferOpen::Existing(content));
        }
        if let Some(content) = self.kernel.path_owner(&identity) {
            return Err(BufferLifecycleError::PathOccupied { path, content });
        }
        Ok(PreparedBufferOpen::Load { path, identity })
    }

    pub(super) fn commit_open_buffer(
        &mut self,
        prepared: PreparedBufferOpen,
    ) -> Result<ContentId, BufferLifecycleError> {
        let (path, identity) = match prepared {
            PreparedBufferOpen::Existing(content) => return Ok(content),
            PreparedBufferOpen::Load { path, identity } => (path, identity),
        };
        let (value, baseline) = match fs::read_to_string(&path) {
            Ok(text) => (
                Content::buffer_from_file(path.clone(), text.clone()),
                FileBaseline::Materialized(text),
            ),
            Err(source) if source.kind() == io::ErrorKind::NotFound => (
                Content::buffer_for_new_file(path.clone()),
                FileBaseline::Missing,
            ),
            Err(source) => {
                return Err(BufferLifecycleError::Io { path, source });
            }
        };
        let content = self.kernel.insert_content(value);
        if let Err(existing) =
            self.kernel
                .register_buffer_path(content, identity, path.clone(), baseline)
        {
            self.kernel.remove_content(content);
            return Err(BufferLifecycleError::PathOccupied {
                path,
                content: existing,
            });
        }
        self.session
            .register_content_profile(content, ContentKind::Buffer);
        Ok(content)
    }

    pub fn save_buffer(
        &mut self,
        content: ContentId,
        force: bool,
    ) -> Result<bool, BufferLifecycleError> {
        self.require_buffer(content)?;
        let save_pending = self.kernel.has_pending_save(content);
        let path = self
            .kernel
            .buffer_path_record(content)
            .map(|(path, _)| path.clone())
            .ok_or(BufferLifecycleError::NoPath(content))?;
        if !save_pending {
            self.preflight_registered_save(content, &path, force)?;
        }
        let snapshot = match self.kernel.execute(content, ContentInput::Save) {
            ContentResult::Handled(outcome) => match outcome.effect {
                ContentEffect::Save(snapshot) => snapshot,
                ContentEffect::None => return Err(BufferLifecycleError::NoPath(content)),
            },
            ContentResult::NotHandled => {
                return Err(BufferLifecycleError::UnsupportedContent(content));
            }
        };
        Ok(self.kernel.queue_save(content, snapshot, force))
    }

    pub fn save_buffer_as(
        &mut self,
        content: ContentId,
        path: impl AsRef<Path>,
        force: bool,
    ) -> Result<bool, BufferLifecycleError> {
        let prepared = self.prepare_save_buffer_as(content, path.as_ref(), force)?;
        self.kernel
            .queue_save_as(
                content,
                prepared.snapshot,
                prepared.identity,
                prepared.force,
            )
            .map_err(|existing| BufferLifecycleError::PathOccupied {
                path: path.as_ref().to_owned(),
                content: existing,
            })
    }

    pub(super) fn prepare_save_buffer_as(
        &mut self,
        content: ContentId,
        requested_path: &Path,
        force: bool,
    ) -> Result<PreparedBufferSaveAs, BufferLifecycleError> {
        self.require_buffer(content)?;
        if self.kernel.has_pending_save(content) {
            return Err(BufferLifecycleError::PendingSave(content));
        }
        let (path, identity) =
            normalize_path(requested_path).map_err(|source| BufferLifecycleError::Io {
                path: requested_path.to_owned(),
                source,
            })?;
        if let Some(existing) = self.kernel.path_owner(&identity)
            && existing != content
        {
            return Err(BufferLifecycleError::PathOccupied {
                path,
                content: existing,
            });
        }
        let same_path = self
            .kernel
            .buffer_path_record(content)
            .is_some_and(|(current, _)| current == &path);
        if same_path {
            self.preflight_registered_save(content, &path, force)?;
        } else if !force {
            match fs::read_to_string(&path) {
                Ok(_) => {
                    return Err(BufferLifecycleError::ExternalConflict { content, path });
                }
                Err(source) if matches!(source.kind(), io::ErrorKind::NotFound) => {}
                Err(source) if source.kind() == io::ErrorKind::InvalidData => {
                    return Err(BufferLifecycleError::ExternalConflict { content, path });
                }
                Err(source) => {
                    return Err(BufferLifecycleError::Io { path, source });
                }
            }
        }
        let snapshot = match self
            .kernel
            .execute(content, ContentInput::SaveAs(path.clone()))
        {
            ContentResult::Handled(outcome) => match outcome.effect {
                ContentEffect::Save(snapshot) => snapshot,
                ContentEffect::None => return Err(BufferLifecycleError::NoPath(content)),
            },
            ContentResult::NotHandled => {
                return Err(BufferLifecycleError::UnsupportedContent(content));
            }
        };
        Ok(PreparedBufferSaveAs {
            snapshot,
            identity,
            force,
        })
    }

    pub fn reload_buffer(
        &mut self,
        content: ContentId,
        force: bool,
    ) -> Result<(), BufferLifecycleError> {
        let prepared = self.prepare_reload_buffer(content, force)?;
        self.reload_content_in_frame(
            content,
            prepared.path.clone(),
            prepared.text,
            prepared.backing_state,
        )
        .map_err(|source| BufferLifecycleError::Io {
            path: prepared.path.clone(),
            source,
        })?;
        self.kernel
            .update_buffer_baseline(content, prepared.path, prepared.baseline);
        Ok(())
    }

    pub(super) fn prepare_reload_buffer(
        &self,
        content: ContentId,
        force: bool,
    ) -> Result<PreparedBufferReload, BufferLifecycleError> {
        let info = self.require_buffer(content)?;
        if info.dirty_state == DirtyState::Modified && !force {
            return Err(BufferLifecycleError::Dirty(content));
        }
        if self.kernel.has_pending_save(content) {
            return Err(BufferLifecycleError::PendingSave(content));
        }
        let path = self
            .kernel
            .buffer_path_record(content)
            .map(|(path, _)| path.clone())
            .ok_or(BufferLifecycleError::NoPath(content))?;
        let (text, backing_state, baseline) = match fs::read_to_string(&path) {
            Ok(text) => (
                text.clone(),
                BufferBackingState::Materialized,
                FileBaseline::Materialized(text),
            ),
            Err(source) if source.kind() == io::ErrorKind::NotFound => (
                String::new(),
                BufferBackingState::Unmaterialized,
                FileBaseline::Missing,
            ),
            Err(source) => {
                return Err(BufferLifecycleError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        Ok(PreparedBufferReload {
            path,
            text,
            backing_state,
            baseline,
        })
    }

    pub fn buffers(&self) -> Vec<BufferInfo> {
        let mut buffers = self
            .kernel
            .contents()
            .ids()
            .filter(|content| self.kernel.contents().kind(*content) == Some(ContentKind::Buffer))
            .filter_map(|content| self.buffer_info(content))
            .collect::<Vec<_>>();
        buffers.sort_by_key(|buffer| buffer.content.0);
        buffers
    }

    pub fn close_buffer(
        &mut self,
        content: ContentId,
        force: bool,
    ) -> Result<(), BufferLifecycleError> {
        self.validate_close_buffer(content, force)?;

        let needs_replacement = self
            .session
            .closing_content_needs_replacement(content)
            .map_err(|error| BufferLifecycleError::Layout(error.to_string()))?;
        let replacement = if needs_replacement {
            let replacement_content = self
                .buffers()
                .into_iter()
                .find(|buffer| buffer.content != content)
                .map(|buffer| buffer.content)
                .unwrap_or_else(|| self.new_buffer());
            let mode_names = self.session.mode_chain_for_new_view(replacement_content);
            Some(
                create_view(replacement_content, self.kernel.contents(), &mode_names)
                    .expect("replacement content exists"),
            )
        } else {
            None
        };
        let (contents, modes, content_modes) = self.kernel.mode_attachment_parts();
        let mutation = self
            .session
            .close_content_views(content, replacement, modes, content_modes, contents)
            .map_err(|error| BufferLifecycleError::Layout(error.to_string()))?;
        for (view, removed_content) in mutation.removed {
            if self.kernel.active_transaction_owner(removed_content) == Some(Some(view)) {
                self.kernel.commit_transaction(removed_content);
            }
            self.cancel_pending_commands_for_view(view);
        }
        if mutation.output.is_some() {
            self.kernel.schedule_mode_jobs();
        }
        self.session.forget_content(content);
        if !self.kernel.remove_content(content) {
            return Err(BufferLifecycleError::MissingContent(content));
        }
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        Ok(())
    }

    pub(super) fn validate_close_buffer(
        &self,
        content: ContentId,
        force: bool,
    ) -> Result<(), BufferLifecycleError> {
        let info = self.require_buffer(content)?;
        if info.dirty_state == DirtyState::Modified && !force {
            return Err(BufferLifecycleError::Dirty(content));
        }
        if self.kernel.has_pending_save(content) {
            return Err(BufferLifecycleError::PendingSave(content));
        }
        Ok(())
    }

    pub(super) fn validate_buffer_view_content(
        &self,
        content: ContentId,
    ) -> Result<(), BufferLifecycleError> {
        match self.kernel.contents().kind(content) {
            None => Err(BufferLifecycleError::MissingContent(content)),
            Some(ContentKind::Buffer) => Ok(()),
        }
    }

    pub(super) fn preflight_quit(&self, force: bool) -> Result<(), BufferLifecycleError> {
        if force {
            return Ok(());
        }
        if let Some(buffer) = self
            .buffers()
            .into_iter()
            .find(|buffer| buffer.dirty_state == DirtyState::Modified)
        {
            return Err(BufferLifecycleError::Dirty(buffer.content));
        }
        Ok(())
    }

    pub(super) fn preflight_registered_save(
        &self,
        content: ContentId,
        path: &Path,
        force: bool,
    ) -> Result<(), BufferLifecycleError> {
        if force {
            return Ok(());
        }
        let Some((_, baseline)) = self.kernel.buffer_path_record(content) else {
            return Ok(());
        };
        let current = fs::read_to_string(path);
        let unchanged = match (baseline, current.as_ref()) {
            (FileBaseline::Materialized(expected), Ok(actual)) => expected == actual,
            (FileBaseline::Missing, Err(source)) => source.kind() == io::ErrorKind::NotFound,
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        if let Err(source) = current
            && !matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            )
        {
            return Err(BufferLifecycleError::Io {
                path: path.to_owned(),
                source,
            });
        }
        Err(BufferLifecycleError::ExternalConflict {
            content,
            path: path.to_owned(),
        })
    }

    fn require_buffer(&self, content: ContentId) -> Result<BufferInfo, BufferLifecycleError> {
        match self.kernel.contents().kind(content) {
            None => Err(BufferLifecycleError::MissingContent(content)),
            Some(ContentKind::Buffer) => self
                .buffer_info(content)
                .ok_or(BufferLifecycleError::UnsupportedContent(content)),
        }
    }

    fn buffer_info(&self, content: ContentId) -> Option<BufferInfo> {
        let contents = self.kernel.contents();
        let resource_name = match contents.query(content, ContentQuery::ResourceName) {
            ContentData::ResourceName(value) => value,
            _ => return None,
        };
        let resource_path = match contents.query(content, ContentQuery::ResourcePath) {
            ContentData::ResourcePath(value) => value,
            _ => return None,
        };
        let backing_state = match contents.query(content, ContentQuery::BackingState) {
            ContentData::BackingState(value) => value,
            _ => return None,
        };
        let dirty_state = match contents.query(content, ContentQuery::DirtyState) {
            ContentData::DirtyState(value) => value,
            _ => return None,
        };
        let save_state = match contents.query(content, ContentQuery::SaveState) {
            ContentData::SaveState(value) => value,
            _ => return None,
        };
        Some(BufferInfo {
            content,
            resource_name,
            resource_path,
            backing_state,
            dirty_state,
            save_state,
        })
    }
}

pub(super) fn normalize_path(path: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::<OsString>::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no existing ancestor")
        })?;
        missing.push(name.to_owned());
        existing = existing.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no existing ancestor")
        })?;
    }
    let canonical_ancestor = fs::canonicalize(existing)?;
    let missing = missing.into_iter().rev().collect::<Vec<_>>();
    let mut normalized = canonical_ancestor.clone();
    for part in &missing {
        normalized.push(part);
    }
    let identity = path_identity(&canonical_ancestor, &missing, &normalized);
    Ok((normalized, identity))
}

#[cfg(not(windows))]
fn path_identity(_ancestor: &Path, _missing: &[OsString], normalized: &Path) -> PathBuf {
    normalized.to_owned()
}

#[cfg(windows)]
fn path_identity(ancestor: &Path, missing: &[OsString], normalized: &Path) -> PathBuf {
    if missing.is_empty() || windows_directory_is_case_sensitive(ancestor).unwrap_or(false) {
        return normalized.to_owned();
    }
    let mut identity = ancestor.to_owned();
    for part in missing {
        identity.push(part.to_string_lossy().to_lowercase());
    }
    identity
}

#[cfg(windows)]
fn windows_directory_is_case_sensitive(path: &Path) -> io::Result<bool> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FileCaseSensitiveInfo,
        GetFileInformationByHandleEx,
    };
    use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut info = FILE_CASE_SENSITIVE_INFO::default();
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&raw mut info).cast(),
            u32::try_from(std::mem::size_of_val(&info)).expect("case info size fits u32"),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0)
}
