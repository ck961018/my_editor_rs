use modeleaf_mode::Mode;

pub const PLUGIN_API_VERSION: u32 = 2;
pub const V1_REMOVAL_VERSION: &str = "0.3.0";
pub const TYPESCRIPT_DECLARATIONS: &str = include_str!("../../../runtime/editor.d.ts");

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
                "TypeScript Mode v1 is deprecated and will be removed in Modeleaf \
                 {V1_REMOVAL_VERSION}; migrate to the on.buffer adapter schema"
            ),
        }
    }
}

pub struct LoadedScriptModes {
    pub modes: Vec<Box<dyn Mode>>,
    pub diagnostics: Vec<ScriptDiagnostic>,
}
