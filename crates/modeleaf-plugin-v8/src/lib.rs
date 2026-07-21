mod api;
mod script;

pub use api::{
    LoadedScriptModes, PLUGIN_API_VERSION, ScriptDiagnostic, ScriptDiagnosticCode,
    TYPESCRIPT_DECLARATIONS, V1_REMOVAL_VERSION,
};
pub use script::{ScriptError, load_default_modes, load_typescript_modes, load_user_modes};

#[cfg(feature = "test-support")]
pub use script::ScriptHost;
