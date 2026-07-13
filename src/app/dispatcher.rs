use std::collections::HashMap;

use crate::app::view::View;
use crate::core::command::{AppCommand, Command, ContentCommand};
use crate::core::content_store::ContentStore;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::core::mode::ModeInstance;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::scene::Scene;
use crate::protocol::space::SpaceKind;

pub struct Dispatcher {
    global_keymap: Keymap,
    pending: Option<PendingKeymap>,
}

/// dispatcher 解析出的目标已决命令。命令本身是相对的（Command 不带 ContentId/SpaceId），
/// dispatcher 按捕获来源补全运行期目标：App 无目标、Content 带单 content、
/// ViewContent 带 view+content（文本命令需要 View 的 selections）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ViewContent {
        command: ContentCommand,
        view: ViewId,
        content: ContentId,
    },
    Noop,
}

/// 命令来源：捕获链命中时记录来自哪个 view/content 的 keymap。
/// 前缀状态下挂载在 PendingKeymap，使前缀完成后仍能回溯到原 keymap 的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandSource {
    view: Option<ViewId>,
    cid: Option<ContentId>,
}

#[derive(Clone)]
struct PendingKeymap {
    keymap: Keymap,
    source: CommandSource,
}

struct CaptureEntry<'a> {
    keymap: &'a Keymap,
    source: CommandSource,
}

impl Dispatcher {
    pub fn new(global_keymap: Keymap) -> Self {
        Self {
            global_keymap,
            pending: None,
        }
    }

    #[allow(dead_code)] // 测试辅助：App 不读 is_pending，dispatcher 单测用
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn dispatch(
        &mut self,
        key: KeyEvent,
        focused: SpaceId,
        scene: &Scene,
        contents: &ContentStore,
        views: &HashMap<ViewId, View>,
        mode: &ModeInstance,
    ) -> Option<DispatchCommand> {
        if let Some(pending) = self.pending.take() {
            return match lookup_in(&pending.keymap, &key) {
                LookupResult::Hit(command) => {
                    resolve_command(command, pending.source, focused, scene, contents, views)
                }
                LookupResult::Prefix(sub) => {
                    self.pending = Some(PendingKeymap {
                        keymap: sub.clone(),
                        source: pending.source,
                    });
                    None
                }
                LookupResult::Miss => None,
            };
        }

        for entry in self.capture_chain(focused, scene, contents, views) {
            match lookup_in(entry.keymap, &key) {
                LookupResult::Hit(command) => {
                    return resolve_command(command, entry.source, focused, scene, contents, views);
                }
                LookupResult::Prefix(sub) => {
                    self.pending = Some(PendingKeymap {
                        keymap: sub.clone(),
                        source: entry.source,
                    });
                    return None;
                }
                LookupResult::Miss => continue,
            }
        }

        // 兜底：focused view 使用自己的 mode instance 解析 mode keymap 与 typing。
        let view = focused_view_id(scene, focused)?;
        let cid = views.get(&view)?.content();
        let command = mode.resolve_key(key)?;
        resolve_command(
            command,
            CommandSource {
                view: Some(view),
                cid: Some(cid),
            },
            focused,
            scene,
            contents,
            views,
        )
    }

    fn capture_chain<'a>(
        &'a self,
        focused: SpaceId,
        scene: &'a Scene,
        contents: &'a ContentStore,
        views: &'a HashMap<ViewId, View>,
    ) -> Vec<CaptureEntry<'a>> {
        let mut chain = Vec::new();
        let mut cur = Some(focused);
        while let Some(sid) = cur {
            let node = scene.node(sid);
            if let SpaceKind::Content { view, .. } = &node.space.kind {
                let content = views.get(view).expect("scene view exists").content();
                if let Some(keymap) = contents.keymap(content) {
                    chain.push(CaptureEntry {
                        keymap,
                        source: CommandSource {
                            view: Some(*view),
                            cid: Some(content),
                        },
                    });
                }
            }
            cur = node.parent;
        }
        chain.push(CaptureEntry {
            keymap: &self.global_keymap,
            source: CommandSource {
                view: None,
                cid: None,
            },
        });
        chain
    }
}

enum LookupResult<'a> {
    Hit(Command),
    Prefix(&'a Keymap),
    Miss,
}

fn lookup_in<'a>(keymap: &'a Keymap, key: &KeyEvent) -> LookupResult<'a> {
    match keymap.lookup(*key) {
        Some(KeyBinding::Command(command)) => LookupResult::Hit(command.clone()),
        Some(KeyBinding::Prefix(sub)) => LookupResult::Prefix(sub),
        None => LookupResult::Miss,
    }
}

/// 按 Command 变体补全运行期目标：
/// - App/Noop → 无目标。
/// - Content(Edit|Mode) → ViewContent{view, content}（需要 View session）。
/// - Content(Save) → Content{content}。
fn resolve_command(
    command: Command,
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &ContentStore,
    views: &HashMap<ViewId, View>,
) -> Option<DispatchCommand> {
    match command {
        Command::App(command) => Some(DispatchCommand::App(command)),
        Command::Noop => Some(DispatchCommand::Noop),
        Command::Content(command @ (ContentCommand::Edit(_) | ContentCommand::Mode { .. })) => {
            let (view, content) = view_content_target(source, focused, scene, contents, views)?;
            Some(DispatchCommand::ViewContent {
                command,
                view,
                content,
            })
        }
        Command::Content(command @ ContentCommand::Save) => {
            let content = source.cid.or_else(|| {
                let view = focused_view_id(scene, focused)?;
                Some(views.get(&view)?.content())
            })?;
            contents.keymap(content)?;
            Some(DispatchCommand::Content { command, content })
        }
    }
}

/// 文本命令目标：来源有完整 view+content 用之，否则回退到 focused View 及其 Content。
fn view_content_target(
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &ContentStore,
    views: &HashMap<ViewId, View>,
) -> Option<(ViewId, ContentId)> {
    let (view, content) = match (source.view, source.cid) {
        (Some(view), Some(content)) => (view, content),
        _ => {
            let view = focused_view_id(scene, focused)?;
            let content = views.get(&view)?.content();
            (view, content)
        }
    };
    contents.keymap(content)?;
    Some((view, content))
}

fn focused_view_id(scene: &Scene, focused: SpaceId) -> Option<ViewId> {
    let node = scene.node(focused);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        _ => None,
    }
}

pub fn default_global_keymap() -> Keymap {
    let mut km = Keymap::new();
    km.bind(KeyEvent::ctrl('q'), Command::App(AppCommand::Quit));
    km.bind(KeyEvent::ctrl('s'), Command::Content(ContentCommand::Save));
    km
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::command::{AppCommand, ContentCommand, EditCommand};
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::core::mode::{ModeActionName, ModeInstance, ModeName, ModeRegistry};
    use crate::core::status_bar::StatusBar;
    use crate::protocol::ids::{ContentId, ViewId};
    use crate::protocol::key_event::{ArrowKey, KeyCode};
    use crate::protocol::scene::{SceneBuilder, build_editor_scene};
    fn fixture() -> (
        Dispatcher,
        crate::protocol::scene::Scene,
        SpaceId,
        ContentStore,
        HashMap<ViewId, View>,
        ModeInstance,
    ) {
        fixture_with_buffer(Buffer::new())
    }

    fn fixture_with_buffer(
        buffer: Buffer,
    ) -> (
        Dispatcher,
        crate::protocol::scene::Scene,
        SpaceId,
        ContentStore,
        HashMap<ViewId, View>,
        ModeInstance,
    ) {
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut builder = SceneBuilder::new();
        let (scene, ed_space) =
            build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let mut contents = ContentStore::default();
        contents.insert(editor, Content::Buffer(buffer));
        contents.insert(status, Content::StatusBar(StatusBar::new(editor)));
        let views = HashMap::from([
            (
                ViewId(0),
                View::new(editor, contents.create_view_state(editor).unwrap(), None),
            ),
            (
                ViewId(1),
                View::new(status, contents.create_view_state(status).unwrap(), None),
            ),
        ]);
        let runtime = ModeRegistry::builtin()
            .instantiate(&ModeName::new("vim"))
            .expect("vim mode exists");
        let d = Dispatcher::new(default_global_keymap());
        (d, scene, ed_space, contents, views, runtime)
    }

    fn enter_insert(runtime: &mut ModeInstance) {
        runtime.execute_action_named(&ModeActionName::new("enter-insert"));
    }

    #[test]
    fn global_quit_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::ctrl('q'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(command, DispatchCommand::App(AppCommand::Quit));
    }

    #[test]
    fn global_quit_when_content_no_bind() {
        // 同上：vim Normal 无 Ctrl+Q，落入全局 → App(Quit)。
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();
        let command = dispatcher
            .dispatch(
                KeyEvent::ctrl('q'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();
        assert_eq!(command, DispatchCommand::App(AppCommand::Quit));
    }

    #[test]
    fn global_save_resolves_to_focused_content() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::ctrl('s'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Save,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn global_save_when_content_no_bind() {
        // 同上：vim Normal 无 Ctrl+S，落入全局 Save。
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();
        let command = dispatcher
            .dispatch(
                KeyEvent::ctrl('s'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();
        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Save,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_a_resolves_to_view_content_mode_command_and_z_is_unbound() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        assert_eq!(
            dispatcher
                .dispatch(
                    KeyEvent::char('a'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .unwrap(),
            DispatchCommand::ViewContent {
                command: ContentCommand::Mode {
                    mode: ModeName::new("vim"),
                    action: ModeActionName::new("append"),
                },
                view: ViewId(0),
                content: ContentId(0),
            }
        );
        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('z'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
    }

    #[test]
    fn vim_i_resolves_to_view_content_mode_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::char('i'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Mode {
                    mode: ModeName::new("vim"),
                    action: ModeActionName::new("enter-insert"),
                },
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_h_resolves_to_view_content_edit_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::char('h'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_j_resolves_to_view_content_edit_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::char('j'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::MoveDownBy(1)),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_k_resolves_to_view_content_edit_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::char('k'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::MoveUpBy(1)),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_l_resolves_to_view_content_edit_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();

        let command = dispatcher
            .dispatch(
                KeyEvent::char('l'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::MoveRightBy(1)),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_insert_char_after_enter_insert_resolves_to_view_content() {
        let (mut dispatcher, scene, focused, contents, views, mut runtime) = fixture();
        enter_insert(&mut runtime);

        let command = dispatcher
            .dispatch(
                KeyEvent::char('a'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn buffer_keymap_enter_inserts_newline_when_insert_mode() {
        let (mut dispatcher, scene, focused, contents, views, mut runtime) = fixture();
        enter_insert(&mut runtime);

        let command = dispatcher
            .dispatch(
                KeyEvent::plain(KeyCode::Enter),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::InsertText("\n".to_string())),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn buffer_keymap_arrow_left_when_insert_mode() {
        let (mut dispatcher, scene, focused, contents, views, mut runtime) = fixture();
        enter_insert(&mut runtime);

        let command = dispatcher
            .dispatch(
                KeyEvent::arrow(ArrowKey::Left),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn global_edit_command_resolves_to_focused_view_content() {
        // 全局绑 'g' → InsertText("g")；无来源，回退到 focused View 及其 Content。
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut builder = SceneBuilder::new();
        let (scene, focused) =
            build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let mut contents = ContentStore::default();
        contents.insert(editor, Content::Buffer(Buffer::new()));
        contents.insert(status, Content::StatusBar(StatusBar::new(editor)));
        let views = HashMap::from([
            (
                ViewId(0),
                View::new(editor, contents.create_view_state(editor).unwrap(), None),
            ),
            (
                ViewId(1),
                View::new(status, contents.create_view_state(status).unwrap(), None),
            ),
        ]);
        let runtime = ModeRegistry::builtin()
            .instantiate(&ModeName::new("vim"))
            .expect("vim mode exists");

        let mut global = Keymap::new();
        global.bind(
            KeyEvent::char('g'),
            Command::Content(ContentCommand::Edit(EditCommand::InsertText(
                "g".to_string(),
            ))),
        );
        let mut dispatcher = Dispatcher::new(global);

        let command = dispatcher
            .dispatch(
                KeyEvent::char('g'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::InsertText("g".to_string())),
                view: ViewId(0),
                content: editor,
            }
        );
    }

    #[test]
    fn global_focus_command_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();
        dispatcher
            .global_keymap
            .bind(KeyEvent::char('n'), Command::App(AppCommand::FocusNext));

        let command = dispatcher
            .dispatch(
                KeyEvent::char('n'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(command, DispatchCommand::App(AppCommand::FocusNext));
    }

    #[test]
    fn content_overrides_global_and_resolves_to_content_source() {
        let mut buffer = Buffer::new();
        // Buffer 默认 vim Normal 无 Ctrl+Q；这里改 content 自持 keymap 绑 Ctrl+Q → InsertText。
        // 注意：Buffer::keymap() 返回空 keymap，绑到它不会影响 mode runtime（resolve_key 走 modes），
        // capture_chain reads the static Content keymap, so this binding wins over the global map.
        buffer.keymap_mut().bind(
            KeyEvent::ctrl('q'),
            Command::Content(ContentCommand::Edit(EditCommand::InsertText(
                "q".to_string(),
            ))),
        );
        let (mut dispatcher, scene, focused, contents, views, runtime) =
            fixture_with_buffer(buffer);

        let command = dispatcher
            .dispatch(
                KeyEvent::ctrl('q'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Edit(EditCommand::InsertText("q".to_string())),
                view: ViewId(0),
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn unbound_key_returns_none() {
        let (mut dispatcher, scene, focused, contents, views, runtime) = fixture();
        // Unknown 无绑定；vim Normal typing 返回 None。
        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::unknown(),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
    }

    #[test]
    fn prefix_key_waits_then_completes() {
        let mut buffer = Buffer::new();
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        buffer.keymap_mut().bind_prefix(KeyEvent::char('x'), sub);
        let (mut dispatcher, scene, focused, contents, views, runtime) =
            fixture_with_buffer(buffer);

        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('x'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
        assert!(dispatcher.is_pending());

        let command = dispatcher
            .dispatch(
                KeyEvent::char('s'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Save,
                content: ContentId(0),
            }
        );
        assert!(!dispatcher.is_pending());
    }

    #[test]
    fn prefix_completion_keeps_original_content_source() {
        let mut buffer = Buffer::new();
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        buffer.keymap_mut().bind_prefix(KeyEvent::char('x'), sub);
        let (mut dispatcher, scene, focused, contents, views, runtime) =
            fixture_with_buffer(buffer);

        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('x'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );

        let command = dispatcher
            .dispatch(
                KeyEvent::char('s'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        // 前缀始于 content keymap（content source），完成时 Save 命令的目标 content 仍是该来源 content。
        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Save,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn prefix_interrupt_resets() {
        let mut buffer = Buffer::new();
        let mut sub = Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        buffer.keymap_mut().bind_prefix(KeyEvent::char('x'), sub);
        let (mut dispatcher, scene, focused, contents, views, runtime) =
            fixture_with_buffer(buffer);
        dispatcher.dispatch(
            KeyEvent::char('x'),
            focused,
            &scene,
            &contents,
            &views,
            &runtime,
        );
        assert!(dispatcher.is_pending());
        // 'z' 不在 sub 表：Miss，重置 Idle。但 'z' 会落入 capture_chain + typing 兜底——
        // vim Normal 无 'z' 绑定且 typing 返回 None，故整体 None。
        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('z'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
        assert!(!dispatcher.is_pending());
    }

    #[test]
    fn nested_prefix() {
        let mut buffer = Buffer::new();
        let mut inner = Keymap::new();
        inner.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        let mut outer = Keymap::new();
        outer.bind_prefix(KeyEvent::char('c'), inner);
        buffer.keymap_mut().bind_prefix(KeyEvent::char('x'), outer);
        let (mut dispatcher, scene, focused, contents, views, runtime) =
            fixture_with_buffer(buffer);

        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('x'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
        assert!(
            dispatcher
                .dispatch(
                    KeyEvent::char('c'),
                    focused,
                    &scene,
                    &contents,
                    &views,
                    &runtime
                )
                .is_none()
        );
        let command = dispatcher
            .dispatch(
                KeyEvent::char('s'),
                focused,
                &scene,
                &contents,
                &views,
                &runtime,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Save,
                content: ContentId(0),
            }
        );
    }
}
