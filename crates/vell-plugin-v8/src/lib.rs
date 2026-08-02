mod api;
mod script;

pub use api::{
    GLOBAL_SCRIPT_COMMAND_ID, GlobalScriptRequest, LoadedEditorConfiguration, LoadedScriptModes,
    PLUGIN_API_VERSION, ScriptDiagnostic, ScriptDiagnosticCode, TYPESCRIPT_DECLARATIONS,
    V1_REMOVAL_VERSION,
};
pub use script::{
    ScriptError, load_default_configuration, load_typescript_modes, load_user_configuration,
};

#[cfg(feature = "test-support")]
pub use script::ScriptHost;
