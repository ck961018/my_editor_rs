use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io;

use crate::kernel::Kernel;
use crate::mode::{Mode, ModeId, ModeRegistry};
#[cfg(test)]
use crate::mode_name::ModeName;
use crate::session::{ClientSession, EditorSessionInit, InitialView};
use vell_core::buffer::Buffer;
use vell_core::content::{Content, ContentKind};
use vell_core::content_store::ContentStore;
use vell_core::status_bar::StatusBar;
use vell_protocol::ids::{ContentId, ViewId};

pub(super) struct EditorBootstrap {
    pub kernel: Kernel,
    pub session: ClientSession,
}

#[derive(Default)]
struct BootstrapIds {
    next_content: u64,
    next_view: u64,
}

struct ConfiguredMode {
    name: crate::mode_name::ModeName,
    before: Option<crate::mode_name::ModeName>,
}

#[derive(Debug, PartialEq, Eq)]
enum ModeOrderError {
    Duplicate(crate::mode_name::ModeName),
    UnknownBefore {
        mode: crate::mode_name::ModeName,
        before: crate::mode_name::ModeName,
    },
    Cycle {
        kind: ContentKind,
        blocked: Vec<crate::mode_name::ModeName>,
    },
}

impl BootstrapIds {
    fn content(&mut self) -> ContentId {
        let id = ContentId(self.next_content);
        self.next_content = self
            .next_content
            .checked_add(1)
            .expect("bootstrap content id overflow");
        id
    }

    fn view(&mut self) -> ViewId {
        let id = ViewId(self.next_view);
        self.next_view = self
            .next_view
            .checked_add(1)
            .expect("bootstrap view id overflow");
        id
    }
}

pub(super) fn bootstrap_editor(
    buffer: Buffer,
    width: usize,
    height: usize,
    configured_modes: Vec<Box<dyn Mode>>,
) -> io::Result<EditorBootstrap> {
    let mut ids = BootstrapIds::default();
    let editor_content = ids.content();
    let status_content = ids.content();
    let editor_view = ids.view();
    let status_view = ids.view();
    let configured = configured_modes
        .iter()
        .map(|mode| ConfiguredMode {
            name: mode.name().clone(),
            before: mode.before().cloned(),
        })
        .collect::<Vec<_>>();
    let indexes = validate_mode_order(&configured)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    let mut contents = ContentStore::default();
    contents
        .insert(editor_content, Content::Buffer(buffer))
        .expect("bootstrap allocates unique content ids");
    contents
        .insert(
            status_content,
            Content::StatusBar(StatusBar::new(editor_content)),
        )
        .expect("bootstrap allocates unique content ids");
    let mut modes = ModeRegistry::new();
    let mut registered = Vec::with_capacity(configured_modes.len());
    for mode in configured_modes {
        registered.push(modes.register_boxed(mode).map_err(io::Error::other)?);
    }
    let editor_order = stable_mode_order(
        &configured,
        &indexes,
        &registered,
        &modes,
        ContentKind::Buffer,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let status_order = stable_mode_order(
        &configured,
        &indexes,
        &registered,
        &modes,
        ContentKind::StatusBar,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let editor_modes = editor_order
        .into_iter()
        .map(|index| configured[index].name.clone())
        .collect();
    let status_modes = status_order
        .into_iter()
        .map(|index| configured[index].name.clone())
        .collect();
    let mut kernel = Kernel::new(contents, modes);
    let (contents, modes, mode_contents) = kernel.mode_attachment_parts();
    let session = ClientSession::editor(
        contents,
        modes,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
                modes: editor_modes,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
                modes: status_modes,
            },
            next_view_id: ids.next_view,
        },
    );
    Ok(EditorBootstrap { kernel, session })
}

fn validate_mode_order(
    modes: &[ConfiguredMode],
) -> Result<HashMap<crate::mode_name::ModeName, usize>, ModeOrderError> {
    let mut indexes = HashMap::with_capacity(modes.len());
    for (index, mode) in modes.iter().enumerate() {
        if indexes.insert(mode.name.clone(), index).is_some() {
            return Err(ModeOrderError::Duplicate(mode.name.clone()));
        }
    }
    for mode in modes {
        if let Some(before) = &mode.before
            && !indexes.contains_key(before)
        {
            return Err(ModeOrderError::UnknownBefore {
                mode: mode.name.clone(),
                before: before.clone(),
            });
        }
    }
    Ok(indexes)
}

fn stable_mode_order(
    modes: &[ConfiguredMode],
    indexes: &HashMap<crate::mode_name::ModeName, usize>,
    registered: &[ModeId],
    registry: &ModeRegistry,
    kind: ContentKind,
) -> Result<Vec<usize>, ModeOrderError> {
    let members = modes
        .iter()
        .enumerate()
        .filter_map(|(index, _)| registry.adapter(registered[index], kind).map(|_| index))
        .collect::<Vec<_>>();
    let local_indexes = members
        .iter()
        .enumerate()
        .map(|(local, global)| (*global, local))
        .collect::<HashMap<_, _>>();
    let mut outgoing = vec![Vec::new(); members.len()];
    let mut indegree = vec![0usize; members.len()];
    for (source_local, source_global) in members.iter().copied().enumerate() {
        let Some(before) = &modes[source_global].before else {
            continue;
        };
        let target_global = indexes[before];
        let Some(&target_local) = local_indexes.get(&target_global) else {
            continue;
        };
        outgoing[source_local].push(target_local);
        indegree[target_local] += 1;
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, &degree)| (degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(members.len());
    while let Some(index) = ready.pop_first() {
        ordered.push(members[index]);
        for &target in &outgoing[index] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.insert(target);
            }
        }
    }
    if ordered.len() != members.len() {
        let blocked = indegree
            .iter()
            .enumerate()
            .filter_map(|(local, &degree)| (degree > 0).then(|| modes[members[local]].name.clone()))
            .collect();
        return Err(ModeOrderError::Cycle { kind, blocked });
    }
    Ok(ordered)
}

impl fmt::Display for ModeOrderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(mode) => {
                write!(formatter, "mode '{}' is already registered", mode.as_str())
            }
            Self::UnknownBefore { mode, before } => write!(
                formatter,
                "mode '{}' declares unknown before target '{}'",
                mode.as_str(),
                before.as_str()
            ),
            Self::Cycle { kind, blocked } => {
                let names = blocked
                    .iter()
                    .map(|mode| format!("'{}'", mode.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    formatter,
                    "{kind:?} mode ordering contains a cycle; blocked modes: {names}"
                )
            }
        }
    }
}

impl std::error::Error for ModeOrderError {}

#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "test helper exposes the editor session's independent inputs"
)]
pub(super) fn create_editor_session(
    contents: &ContentStore,
    modes: &ModeRegistry,
    mode_contents: &mut crate::mode::ModeContentStore,
    width: usize,
    height: usize,
    editor_content: ContentId,
    status_content: ContentId,
    editor_modes: Vec<ModeName>,
) -> ClientSession {
    let mut ids = BootstrapIds::default();
    let editor_view = ids.view();
    let status_view = ids.view();
    ClientSession::editor(
        contents,
        modes,
        mode_contents,
        width,
        height,
        EditorSessionInit {
            editor: InitialView {
                view: editor_view,
                content: editor_content,
                modes: editor_modes,
            },
            status: InitialView {
                view: status_view,
                content: status_content,
                modes: Vec::new(),
            },
            next_view_id: ids.next_view,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OrderedTestMode {
        name: ModeName,
        before: Option<ModeName>,
        adapters: crate::mode::ModeAdapters,
    }

    impl Mode for OrderedTestMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[crate::mode_name::ModeActionName] {
            &[]
        }

        fn adapters(&self) -> crate::mode::ModeAdapters {
            self.adapters
        }

        fn before(&self) -> Option<&ModeName> {
            self.before.as_ref()
        }
    }

    fn ordered_mode(
        name: &str,
        before: Option<&str>,
        adapters: crate::mode::ModeAdapters,
    ) -> Box<dyn Mode> {
        Box::new(OrderedTestMode {
            name: ModeName::new(name),
            before: before.map(ModeName::new),
            adapters,
        })
    }

    #[test]
    fn session_bootstrap_uses_explicit_content_roles() {
        let editor = ContentId(7);
        let status = ContentId(11);
        let mut contents = ContentStore::default();
        contents
            .insert(editor, Content::Buffer(Buffer::new()))
            .unwrap();
        contents
            .insert(status, Content::StatusBar(StatusBar::new(editor)))
            .unwrap();
        let modes = ModeRegistry::new();
        let mut mode_contents = crate::mode::ModeContentStore::default();

        let session = create_editor_session(
            &contents,
            &modes,
            &mut mode_contents,
            40,
            5,
            editor,
            status,
            Vec::new(),
        );

        assert_eq!(session.views()[&ViewId(0)].content(), editor);
        assert_eq!(session.views()[&ViewId(1)].content(), status);
        assert_eq!(session.next_view_id_for_test(), 2);
    }

    #[test]
    fn bootstrap_stably_orders_forward_references_per_content_kind() {
        let bootstrap = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("base", None, crate::mode::ModeAdapters::buffer()),
                ordered_mode(
                    "status-first",
                    None,
                    crate::mode::ModeAdapters::status_bar(),
                ),
                ordered_mode("overlay", Some("base"), crate::mode::ModeAdapters::buffer()),
                ordered_mode(
                    "status-late",
                    Some("base"),
                    crate::mode::ModeAdapters::status_bar(),
                ),
                ordered_mode("tail", None, crate::mode::ModeAdapters::buffer()),
            ],
        )
        .unwrap();

        assert_eq!(
            bootstrap.session.view_modes().mode_names(ViewId(0)),
            ["overlay", "base", "tail"].map(ModeName::new)
        );
        assert_eq!(
            bootstrap.session.view_modes().mode_names(ViewId(1)),
            ["status-first", "status-late"].map(ModeName::new)
        );
    }

    #[test]
    fn bootstrap_rejects_invalid_mode_ordering() {
        let duplicate = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("same", None, crate::mode::ModeAdapters::buffer()),
                ordered_mode("same", None, crate::mode::ModeAdapters::buffer()),
            ],
        )
        .err()
        .unwrap();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
        assert!(
            duplicate
                .to_string()
                .contains("'same' is already registered")
        );

        let unknown = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![ordered_mode(
                "orphan",
                Some("missing"),
                crate::mode::ModeAdapters::buffer(),
            )],
        )
        .err()
        .unwrap();
        assert_eq!(unknown.kind(), io::ErrorKind::InvalidInput);
        assert!(
            unknown
                .to_string()
                .contains("unknown before target 'missing'")
        );

        let cycle = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![
                ordered_mode("first", Some("second"), crate::mode::ModeAdapters::buffer()),
                ordered_mode("second", Some("first"), crate::mode::ModeAdapters::buffer()),
            ],
        )
        .err()
        .unwrap();
        assert_eq!(cycle.kind(), io::ErrorKind::InvalidInput);
        assert!(
            cycle
                .to_string()
                .contains("Buffer mode ordering contains a cycle")
        );
        assert!(cycle.to_string().contains("'first', 'second'"));
    }
}
