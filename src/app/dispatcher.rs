use crate::core::command::{AppCommand, Command, ContentCommand};
use crate::core::content::ContentLookup;
use crate::core::keymap::{KeyBinding, Keymap};
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::scene::Scene;
use crate::protocol::space::SpaceKind;

pub struct Dispatcher {
    global_keymap: Keymap,
    pending: Option<PendingKeymap>,
}

/// dispatcher 解析出的目标已决命令。命令本身是相对的（Command 不带 ContentId/SpaceId），
/// dispatcher 按捕获来源补全运行期目标：App 无目标、Content 带单 content、
/// ViewContent 带 space+content（Text 命令需要 view 的 selections）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ViewContent {
        command: ContentCommand,
        space: SpaceId,
        content: ContentId,
    },
    Noop,
}

/// 命令来源：捕获链命中时记录来自哪个 space/content 的 keymap。
/// 前缀状态下挂载在 PendingKeymap，使前缀完成后仍能回溯到原 keymap 的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandSource {
    sid: Option<SpaceId>,
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
        contents: &dyn ContentLookup,
    ) -> Option<DispatchCommand> {
        if let Some(pending) = self.pending.take() {
            return match lookup_in(&pending.keymap, &key) {
                LookupResult::Hit(command) => {
                    resolve_command(command, pending.source, focused, scene, contents)
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

        for entry in self.capture_chain(focused, scene, contents) {
            match lookup_in(entry.keymap, &key) {
                LookupResult::Hit(command) => {
                    return resolve_command(command, entry.source, focused, scene, contents);
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

        // 兜底：focused content 自治解析（Buffer 走 mode runtime keymap + typing）。
        // resolve_key 默认查 content keymap；Buffer 覆写为走 mode runtime keymap + typing。
        let cid = focused_content_id(scene, focused)?;
        let command = contents.get(cid)?.resolve_key(key)?;
        resolve_command(
            command,
            CommandSource {
                sid: Some(focused),
                cid: Some(cid),
            },
            focused,
            scene,
            contents,
        )
    }

    fn capture_chain<'a>(
        &'a self,
        focused: SpaceId,
        scene: &'a Scene,
        contents: &'a dyn ContentLookup,
    ) -> Vec<CaptureEntry<'a>> {
        let mut chain = Vec::new();
        let mut cur = Some(focused);
        while let Some(sid) = cur {
            let node = scene.node(sid);
            if let SpaceKind::Content { content } = &node.space.kind {
                if let Some(c) = contents.get(*content) {
                    chain.push(CaptureEntry {
                        keymap: c.keymap(),
                        source: CommandSource {
                            sid: Some(sid),
                            cid: Some(*content),
                        },
                    });
                }
            }
            cur = node.parent;
        }
        chain.push(CaptureEntry {
            keymap: &self.global_keymap,
            source: CommandSource {
                sid: None,
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
/// - Content(Text) → ViewContent{space, content}（Text 需 view 的 selections）。
/// - Content(Save|Mode) → Content{content}（直接作用于 content，不需 view）。
fn resolve_command(
    command: Command,
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<DispatchCommand> {
    match command {
        Command::App(command) => Some(DispatchCommand::App(command)),
        Command::Noop => Some(DispatchCommand::Noop),
        Command::Content(ContentCommand::Text(command)) => {
            let (space, content) = view_content_target(source, focused, scene, contents)?;
            Some(DispatchCommand::ViewContent {
                command: ContentCommand::Text(command),
                space,
                content,
            })
        }
        Command::Content(command @ ContentCommand::Save)
        | Command::Content(command @ ContentCommand::Mode { .. }) => {
            let content = source
                .cid
                .or_else(|| focused_content_id(scene, focused))?;
            contents.get(content)?;
            Some(DispatchCommand::Content { command, content })
        }
    }
}

/// Text 命令目标：来源有完整 space+content 用之，否则回退到 focused space + focused content。
fn view_content_target(
    source: CommandSource,
    focused: SpaceId,
    scene: &Scene,
    contents: &dyn ContentLookup,
) -> Option<(SpaceId, ContentId)> {
    let (space, content) = match (source.sid, source.cid) {
        (Some(space), Some(content)) => (space, content),
        _ => {
            let content = focused_content_id(scene, focused)?;
            (focused, content)
        }
    };
    contents.get(content)?;
    Some((space, content))
}

fn focused_content_id(scene: &Scene, focused: SpaceId) -> Option<ContentId> {
    let node = scene.node(focused);
    match &node.space.kind {
        SpaceKind::Content { content } => Some(*content),
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
    use crate::core::command::{AppCommand, ContentCommand, TextCommand};
    use crate::core::content::ContentHandler;
    use crate::core::mode::{ModeActionId, ModeId};
    use crate::core::status_bar::StatusBar;
    use crate::protocol::ids::ContentId;
    use crate::protocol::key_event::{ArrowKey, KeyCode};
    use crate::protocol::scene::{SceneBuilder, build_editor_scene};
    use std::collections::HashMap;

    fn fixture() -> (
        Dispatcher,
        crate::protocol::scene::Scene,
        SpaceId,
        HashMap<ContentId, Box<dyn ContentHandler>>,
    ) {
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut builder = SceneBuilder::new();
        let (scene, ed_space) = build_editor_scene(&mut builder, 40, 5, editor, status).unwrap();
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor, Box::new(Buffer::new()));
        contents.insert(status, Box::new(StatusBar::new(editor)));
        let d = Dispatcher::new(default_global_keymap());
        (d, scene, ed_space, contents)
    }

    #[test]
    fn global_quit_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(command, DispatchCommand::App(AppCommand::Quit));
    }

    #[test]
    fn global_quit_when_content_no_bind() {
        // 同上：vim Normal 无 Ctrl+Q，落入全局 → App(Quit)。
        let (mut dispatcher, scene, focused, contents) = fixture();
        let command = dispatcher
            .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
            .unwrap();
        assert_eq!(command, DispatchCommand::App(AppCommand::Quit));
    }

    #[test]
    fn global_save_resolves_to_focused_content() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::ctrl('s'), focused, &scene, &contents)
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
        let (mut dispatcher, scene, focused, contents) = fixture();
        let command = dispatcher
            .dispatch(KeyEvent::ctrl('s'), focused, &scene, &contents)
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
    fn vim_normal_char_without_binding_returns_none() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        assert!(
            dispatcher
                .dispatch(KeyEvent::char('a'), focused, &scene, &contents)
                .is_none()
        );
    }

    #[test]
    fn vim_i_resolves_to_content_mode_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::char('i'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::Content {
                command: ContentCommand::Mode {
                    mode: ModeId::new("vim"),
                    action: ModeActionId::new("enter-insert"),
                },
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_h_resolves_to_view_content_text_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::char('h'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::MoveLeftBy(1)),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_j_resolves_to_view_content_text_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::char('j'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::MoveDownBy(1)),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_k_resolves_to_view_content_text_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::char('k'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::MoveUpBy(1)),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_l_resolves_to_view_content_text_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();

        let command = dispatcher
            .dispatch(KeyEvent::char('l'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::MoveRightBy(1)),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn vim_insert_char_after_enter_insert_resolves_to_view_content() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        // 进入 Insert：通过 handle_mode_command 改 Buffer 状态（模拟 dispatcher 先发出 Mode 命令后由 App 执行）。
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));

        let command = dispatcher
            .dispatch(KeyEvent::char('a'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::InsertText("a".to_string())),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn buffer_keymap_enter_inserts_newline_when_insert_mode() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));

        let command = dispatcher
            .dispatch(KeyEvent::plain(KeyCode::Enter), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::InsertText("\n".to_string())),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn buffer_keymap_arrow_left_when_insert_mode() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .handle_mode_command(ModeId::new("vim"), ModeActionId::new("enter-insert"));

        let command = dispatcher
            .dispatch(
                KeyEvent::arrow(ArrowKey::Left),
                focused,
                &scene,
                &contents,
            )
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::MoveLeftBy(1)),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn global_edit_command_resolves_to_focused_view_content() {
        // 全局绑 'g' → InsertText("g")；无来源，回退 focused space + focused content。
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut builder = SceneBuilder::new();
        let (scene, focused) = build_editor_scene(&mut builder, 40, 5, editor, status).unwrap();
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor, Box::new(Buffer::new()));
        contents.insert(status, Box::new(StatusBar::new(editor)));

        let mut global = Keymap::new();
        global.bind(
            KeyEvent::char('g'),
            Command::Content(ContentCommand::Text(TextCommand::InsertText("g".to_string()))),
        );
        let mut dispatcher = Dispatcher::new(global);

        let command = dispatcher
            .dispatch(KeyEvent::char('g'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::InsertText("g".to_string())),
                space: focused,
                content: editor,
            }
        );
    }

    #[test]
    fn global_focus_command_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, contents) = fixture();
        dispatcher
            .global_keymap
            .bind(KeyEvent::char('n'), Command::App(AppCommand::FocusNext));

        let command = dispatcher
            .dispatch(KeyEvent::char('n'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(command, DispatchCommand::App(AppCommand::FocusNext));
    }

    #[test]
    fn content_overrides_global_and_resolves_to_content_source() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        // Buffer 默认 vim Normal 无 Ctrl+Q；这里改 content 自持 keymap 绑 Ctrl+Q → InsertText。
        // 注意：Buffer::keymap() 返回空 keymap，绑到它不会影响 mode runtime（resolve_key 走 modes），
        // 但 capture_chain 用 c.keymap()（空 keymap 的 ContentHandler 实现）——绑 Ctrl+Q 到它即可命中。
        contents.get_mut(&ContentId(0)).unwrap().keymap_mut().bind(
            KeyEvent::ctrl('q'),
            Command::Content(ContentCommand::Text(TextCommand::InsertText("q".to_string()))),
        );

        let command = dispatcher
            .dispatch(KeyEvent::ctrl('q'), focused, &scene, &contents)
            .unwrap();

        assert_eq!(
            command,
            DispatchCommand::ViewContent {
                command: ContentCommand::Text(TextCommand::InsertText("q".to_string())),
                space: focused,
                content: ContentId(0),
            }
        );
    }

    #[test]
    fn unbound_key_returns_none() {
        let (mut dispatcher, scene, focused, contents) = fixture();
        // Unknown 无绑定；vim Normal typing 返回 None。
        assert!(
            dispatcher
                .dispatch(KeyEvent::unknown(), focused, &scene, &contents)
                .is_none()
        );
    }

    #[test]
    fn prefix_key_waits_then_completes() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        let mut sub = Keymap::new();
        sub.bind(
            KeyEvent::char('s'),
            Command::Content(ContentCommand::Save),
        );
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .keymap_mut()
            .bind_prefix(KeyEvent::char('x'), sub);

        assert!(
            dispatcher
                .dispatch(KeyEvent::char('x'), focused, &scene, &contents)
                .is_none()
        );
        assert!(dispatcher.is_pending());

        let command = dispatcher
            .dispatch(KeyEvent::char('s'), focused, &scene, &contents)
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
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        let mut sub = Keymap::new();
        sub.bind(
            KeyEvent::char('s'),
            Command::Content(ContentCommand::Save),
        );
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .keymap_mut()
            .bind_prefix(KeyEvent::char('x'), sub);

        assert!(
            dispatcher
                .dispatch(KeyEvent::char('x'), focused, &scene, &contents)
                .is_none()
        );

        let command = dispatcher
            .dispatch(KeyEvent::char('s'), focused, &scene, &contents)
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
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        let mut sub = Keymap::new();
        sub.bind(
            KeyEvent::char('s'),
            Command::Content(ContentCommand::Save),
        );
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .keymap_mut()
            .bind_prefix(KeyEvent::char('x'), sub);
        dispatcher.dispatch(KeyEvent::char('x'), focused, &scene, &contents);
        assert!(dispatcher.is_pending());
        // 'z' 不在 sub 表：Miss，重置 Idle。但 'z' 会落入 capture_chain + typing 兜底——
        // vim Normal 无 'z' 绑定且 typing 返回 None，故整体 None。
        assert!(
            dispatcher
                .dispatch(KeyEvent::char('z'), focused, &scene, &contents)
                .is_none()
        );
        assert!(!dispatcher.is_pending());
    }

    #[test]
    fn nested_prefix() {
        let (mut dispatcher, scene, focused, mut contents) = fixture();
        let mut inner = Keymap::new();
        inner.bind(
            KeyEvent::char('s'),
            Command::Content(ContentCommand::Save),
        );
        let mut outer = Keymap::new();
        outer.bind_prefix(KeyEvent::char('c'), inner);
        contents
            .get_mut(&ContentId(0))
            .unwrap()
            .keymap_mut()
            .bind_prefix(KeyEvent::char('x'), outer);

        assert!(
            dispatcher
                .dispatch(KeyEvent::char('x'), focused, &scene, &contents)
                .is_none()
        );
        assert!(
            dispatcher
                .dispatch(KeyEvent::char('c'), focused, &scene, &contents)
                .is_none()
        );
        let command = dispatcher
            .dispatch(KeyEvent::char('s'), focused, &scene, &contents)
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
