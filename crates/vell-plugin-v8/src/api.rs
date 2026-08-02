use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use vell_mode::command_registry::{CommandEntry, CommandId, CommandInvocation};
use vell_mode::{Mode, ModeBackground};
use vell_protocol::content_query::{FaceOverride, ThemeName};
use vell_protocol::ids::ContentId;

use crate::script::{ScriptError, ScriptHost};

pub const PLUGIN_API_VERSION: u32 = 2;
pub const V1_REMOVAL_VERSION: &str = "0.3.0";
pub const TYPESCRIPT_DECLARATIONS: &str = include_str!("../../../runtime/editor.d.ts");
pub const GLOBAL_SCRIPT_COMMAND_ID: &str = "$script.evaluate";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalScriptRequest {
    Interactive {
        source: String,
    },
    Buffer {
        content: ContentId,
        resource_path: Option<PathBuf>,
        source: String,
    },
    File {
        path: PathBuf,
    },
}

impl GlobalScriptRequest {
    pub fn into_invocation(self) -> CommandInvocation {
        let value = match self {
            Self::Interactive { source } => serde_json::json!({
                "kind": "interactive",
                "source": source,
            }),
            Self::Buffer {
                content,
                resource_path,
                source,
            } => serde_json::json!({
                "kind": "buffer",
                "content": content.0,
                "resourcePath": resource_path.map(|path| path.to_string_lossy().into_owned()),
                "source": source,
            }),
            Self::File { path } => serde_json::json!({
                "kind": "file",
                "path": path.to_string_lossy(),
            }),
        };
        CommandInvocation::new(
            CommandId::new(GLOBAL_SCRIPT_COMMAND_ID).expect("global script command id"),
            vec![value],
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScriptDiagnosticCode {
    DeprecatedApi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptDiagnostic {
    pub code: ScriptDiagnosticCode,
    pub message: String,
}

impl ScriptDiagnostic {
    pub(crate) fn v1_deprecation() -> Self {
        Self {
            code: ScriptDiagnosticCode::DeprecatedApi,
            message: format!(
                "TypeScript Mode v1 is deprecated and will be removed in Vell \
                 {V1_REMOVAL_VERSION}; migrate to the on.buffer adapter schema"
            ),
        }
    }
}

pub struct LoadedScriptModes {
    pub modes: Vec<Box<dyn Mode>>,
    pub backgrounds: Vec<Box<dyn ModeBackground>>,
    pub commands: Vec<CommandEntry>,
    pub diagnostics: Vec<ScriptDiagnostic>,
    pub(crate) host: Rc<RefCell<ScriptHost>>,
}

impl LoadedScriptModes {
    pub fn install_native_commands(&mut self, native_ids: &[CommandId]) -> Result<(), ScriptError> {
        self.host.borrow_mut().install_native_commands(native_ids)
    }
}

pub struct LoadedEditorConfiguration {
    pub modes: Vec<Box<dyn Mode>>,
    pub backgrounds: Vec<Box<dyn ModeBackground>>,
    pub theme: Option<ThemeName>,
    pub face_overrides: Vec<FaceOverride>,
    pub(crate) host: Rc<RefCell<ScriptHost>>,
}

impl LoadedEditorConfiguration {
    pub fn prepare_commands(
        &mut self,
        native_ids: &[CommandId],
    ) -> Result<Vec<CommandEntry>, ScriptError> {
        self.host.borrow_mut().install_native_commands(native_ids)?;
        Ok(ScriptHost::command_entries(&self.host))
    }
}
