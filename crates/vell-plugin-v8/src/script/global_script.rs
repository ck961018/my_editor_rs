use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use deno_ast::ModuleSpecifier;
#[cfg(test)]
use vell_mode::command_registry::CommandCompletion;
use vell_mode::command_registry::{
    CommandAdapter, CommandEntry, CommandError, CommandHost, CommandId, CommandResult, CommandValue,
};
use vell_protocol::ids::ContentId;

use crate::api::GLOBAL_SCRIPT_COMMAND_ID;

use super::commands::{
    ScopedHost, ScriptExecution, ScriptValue, change_count, finish_script_execution,
    poll_script_value, publish_changes,
};
use super::module::{AssetSource, transpile_typescript_program};
use super::{
    MAX_SCRIPT_SOURCE_BYTES, ScriptError, ScriptHost, ScriptInvocationKind, current_exception,
    ensure_file_size, ensure_size, perform_microtask_checkpoint,
};

pub(super) enum EvaluationRequest {
    Interactive(String),
    Buffer {
        content: ContentId,
        resource_path: Option<PathBuf>,
        source: String,
    },
    File(PathBuf),
}

#[derive(Clone)]
struct GlobalScriptAdapter {
    host: Rc<RefCell<ScriptHost>>,
}

impl CommandAdapter for GlobalScriptAdapter {
    fn invoke(&self, host: &mut dyn CommandHost, arguments: Vec<CommandValue>) -> CommandResult {
        let request = parse_request(arguments)?;
        let changes = change_count(&self.host);
        let execution = self
            .host
            .try_borrow_mut()
            .map_err(|_| CommandError::Failed("script runtime is reentrant".to_owned()))?
            .execute_global_script(request, host)
            .map_err(|error| CommandError::Failed(error.to_string()));
        let result = execution.and_then(|execution| execution.into_completion(&self.host));
        publish_changes(&self.host, host, changes);
        result
    }
}

pub(super) fn entry(host: &Rc<RefCell<ScriptHost>>) -> CommandEntry {
    CommandEntry::new(
        CommandId::new(GLOBAL_SCRIPT_COMMAND_ID).expect("global script command id"),
        GlobalScriptAdapter {
            host: Rc::clone(host),
        },
    )
}

impl ScriptHost {
    pub(super) fn execute_global_script(
        &mut self,
        request: EvaluationRequest,
        host: &mut dyn CommandHost,
    ) -> Result<ScriptExecution, ScriptError> {
        let bridge = self.command_host.clone();
        let mut scoped_host = ScopedHost::new(host);
        let active_host = bridge.activate(&mut scoped_host)?;
        let result = match request {
            EvaluationRequest::Interactive(source) => {
                let path = self.next_interactive_path()?;
                self.evaluate_global_source(&path, &source)
            }
            EvaluationRequest::Buffer {
                content,
                resource_path,
                source,
            } => {
                let path = buffer_source_path(content, resource_path.as_deref())?;
                self.evaluate_global_source(&path, &source)
            }
            EvaluationRequest::File(path) => self.evaluate_global_file(&path),
        };
        drop(active_host);
        finish_script_execution(result?, scoped_host.take_pending_tasks())
    }

    fn next_interactive_path(&mut self) -> Result<PathBuf, ScriptError> {
        let id = self.next_interactive_script;
        self.next_interactive_script = id
            .checked_add(1)
            .ok_or_else(|| ScriptError::new("interactive source identity exhausted"))?;
        Ok(script_root()?.join(format!(".vell-interactive-{id}.ts")))
    }

    fn evaluate_global_file(&mut self, path: &Path) -> Result<ScriptValue, ScriptError> {
        let path = path
            .canonicalize()
            .map_err(|error| ScriptError::new(format!("failed to open script: {error}")))?;
        ensure_file_size(&path, "global script", MAX_SCRIPT_SOURCE_BYTES)?;
        let source = fs::read_to_string(&path).map_err(|error| {
            ScriptError::new(format!("failed to read {}: {error}", path.display()))
        })?;
        let specifier = file_specifier(&path)?;
        let program = transpile_typescript_program(&specifier, &source)?;
        reject_top_level_await(&program)?;
        if program.is_module {
            self.execute_module(&path)?;
            Ok(ScriptValue::Ready(CommandValue::Null))
        } else {
            if let Some(source_map) = &program.source_map {
                self.commands
                    .borrow_mut()
                    .record_source_map(path.display().to_string(), source_map)?;
            }
            self.update_type_source(path.display().to_string(), &source);
            let result = self.evaluate_transpiled_global(&path, program.code);
            self.sync_module_type_sources();
            result
        }
    }

    fn evaluate_global_source(
        &mut self,
        path: &Path,
        source: &str,
    ) -> Result<ScriptValue, ScriptError> {
        ensure_size(
            "global TypeScript source",
            source.len(),
            MAX_SCRIPT_SOURCE_BYTES,
        )?;
        let specifier = file_specifier(path)?;
        let program = transpile_typescript_program(&specifier, source)?;
        reject_top_level_await(&program)?;
        if program.is_module {
            return Err(ScriptError::new(
                "global scripts do not support static import or export; use dynamic import()",
            ));
        }
        if let Some(source_map) = &program.source_map {
            self.commands
                .borrow_mut()
                .record_source_map(path.display().to_string(), source_map)?;
        }
        self.update_type_source(path.display().to_string(), source);
        let result = self.evaluate_transpiled_global(path, program.code);
        self.sync_module_type_sources();
        result
    }

    fn evaluate_transpiled_global(
        &mut self,
        path: &Path,
        javascript: String,
    ) -> Result<ScriptValue, ScriptError> {
        ensure_size(
            "transpiled global script",
            javascript.len(),
            MAX_SCRIPT_SOURCE_BYTES,
        )?;
        let context = self.context.clone();
        let modules = self.modules.clone();
        let path = path.to_owned();
        self.invoke(ScriptInvocationKind::Action, |isolate| {
            isolate.set_slot(AssetSource::Filesystem);
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let source = v8::String::new(scope, &javascript)
                .ok_or_else(|| ScriptError::new("script source is too large for V8"))?;
            let resource_name = v8::String::new(scope, &path.display().to_string())
                .ok_or_else(|| ScriptError::new("script source identity is too large for V8"))?;
            let origin = v8::ScriptOrigin::new(
                scope,
                resource_name.into(),
                0,
                0,
                false,
                0,
                None,
                false,
                false,
                false,
                None,
            );
            let root = path.parent().unwrap_or(Path::new("")).to_owned();
            modules
                .borrow_mut()
                .register_script_origin(path.clone(), root);
            let script = v8::Script::compile(scope, source, Some(&origin))
                .ok_or_else(|| current_exception(scope, &path.display().to_string(), "compile"))?;
            let value = script
                .run(scope)
                .ok_or_else(|| current_exception(scope, &path.display().to_string(), "execute"))?;
            perform_microtask_checkpoint(scope);
            poll_script_value(scope, value)
        })
    }
}

fn parse_request(arguments: Vec<CommandValue>) -> Result<EvaluationRequest, CommandError> {
    let [request] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "global script evaluator expects one request object".to_owned(),
        ));
    };
    let request = request.as_object().ok_or_else(|| {
        CommandError::InvalidArguments("global script request must be an object".to_owned())
    })?;
    match request.get("kind").and_then(CommandValue::as_str) {
        Some("interactive") => Ok(EvaluationRequest::Interactive(required_string(
            request, "source",
        )?)),
        Some("buffer") => {
            let content = request
                .get("content")
                .and_then(CommandValue::as_u64)
                .map(ContentId)
                .ok_or_else(|| {
                    CommandError::InvalidArguments(
                        "buffer script requires a non-negative content id".to_owned(),
                    )
                })?;
            let resource_path = request
                .get("resourcePath")
                .and_then(CommandValue::as_str)
                .map(PathBuf::from);
            Ok(EvaluationRequest::Buffer {
                content,
                resource_path,
                source: required_string(request, "source")?,
            })
        }
        Some("file") => Ok(EvaluationRequest::File(PathBuf::from(required_string(
            request, "path",
        )?))),
        _ => Err(CommandError::InvalidArguments(
            "unknown global script request kind".to_owned(),
        )),
    }
}

fn required_string(
    object: &serde_json::Map<String, CommandValue>,
    property: &str,
) -> Result<String, CommandError> {
    object
        .get(property)
        .and_then(CommandValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            CommandError::InvalidArguments(format!(
                "global script request requires string '{property}'"
            ))
        })
}

fn reject_top_level_await(
    program: &super::module::TranspiledTypeScript,
) -> Result<(), ScriptError> {
    if program.has_top_level_await {
        Err(ScriptError::new(
            "global scripts do not support top-level await; use an async function",
        ))
    } else {
        Ok(())
    }
}

fn buffer_source_path(
    content: ContentId,
    resource_path: Option<&Path>,
) -> Result<PathBuf, ScriptError> {
    let root = resource_path
        .and_then(Path::parent)
        .map(Path::to_owned)
        .unwrap_or(script_root()?);
    let root = if root.is_absolute() {
        root
    } else {
        script_root()?.join(root)
    };
    Ok(root.join(format!(".vell-buffer-{}.ts", content.0)))
}

fn script_root() -> Result<PathBuf, ScriptError> {
    std::env::current_dir()
        .map_err(|error| ScriptError::new(format!("failed to resolve script directory: {error}")))
}

fn file_specifier(path: &Path) -> Result<String, ScriptError> {
    ModuleSpecifier::from_file_path(path)
        .map(|specifier| specifier.to_string())
        .map_err(|_| ScriptError::new(format!("invalid script path: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use vell_mode::command_registry::{CommandInvocation, CommandRegistry, CommandRequest};

    use crate::api::GlobalScriptRequest;

    use super::*;

    struct TestHost {
        registry: Rc<RefCell<CommandRegistry>>,
        script: Rc<RefCell<ScriptHost>>,
    }

    impl CommandHost for TestHost {
        fn invoke_command(&mut self, invocation: CommandInvocation) -> CommandResult {
            let entry = self
                .registry
                .borrow()
                .resolve(&invocation.command)
                .ok_or_else(|| CommandError::UnknownCommand(invocation.command.clone()))?;
            entry.invoke(self, invocation.arguments)
        }

        fn request(&mut self, _request: CommandRequest) -> CommandResult {
            Err(CommandError::Failed(
                "test host does not support requests".to_owned(),
            ))
        }

        fn register_command(&mut self, entry: CommandEntry) {
            self.registry.borrow_mut().register(entry);
        }
    }

    fn configured_host() -> (Rc<RefCell<Vec<CommandValue>>>, TestHost) {
        let captured = Rc::new(RefCell::new(Vec::new()));
        let mut script = ScriptHost::new();
        script
            .install_native_commands(&[CommandId::new("capture").unwrap()])
            .unwrap();
        let script = Rc::new(RefCell::new(script));
        let registry = Rc::new(RefCell::new(CommandRegistry::new()));
        for command in ScriptHost::command_entries(&script) {
            registry.borrow_mut().register(command);
        }
        let values = Rc::clone(&captured);
        registry.borrow_mut().register(CommandEntry::new(
            CommandId::new("capture").unwrap(),
            move |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
                values.borrow_mut().extend(arguments);
                Ok(CommandValue::Null.into())
            },
        ));
        (captured, TestHost { registry, script })
    }

    fn evaluate(host: &mut TestHost, source: &str) -> CommandResult {
        host.invoke_command(
            GlobalScriptRequest::Interactive {
                source: source.to_owned(),
            }
            .into_invocation(),
        )
    }

    #[test]
    fn global_functions_closures_and_registered_commands_persist() {
        let (captured, mut host) = configured_host();

        evaluate(
            &mut host,
            r#"
const next = (() => { let value = 40; return () => ++value; })();
function ordinary() { return next(); }
if (Object.hasOwn(editor.commands, "ordinary")) throw new Error("leaked");
"#,
        )
        .unwrap();
        evaluate(
            &mut host,
            r#"
if (ordinary() !== 41) throw new Error("function did not persist");
editor.commands.register("math.double", value => value * 2);
"#,
        )
        .unwrap();
        evaluate(&mut host, "capture(math.double(21), ordinary());").unwrap();

        assert_eq!(
            &*captured.borrow(),
            &[CommandValue::from(42), CommandValue::from(42)]
        );
        assert!(
            host.registry
                .borrow()
                .get(&CommandId::new("math.double").unwrap())
                .is_some()
        );
    }

    #[test]
    fn global_history_is_visible_to_later_type_queries() {
        let (_captured, mut host) = configured_host();
        evaluate(
            &mut host,
            "function typedHelper(value: number): string { return String(value); }",
        )
        .unwrap();

        let diagnostics = host
            .script
            .borrow_mut()
            .type_diagnostics(
                "file:///later-input.ts",
                "typedHelper('bad'); typedHelper(42);",
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    }

    #[test]
    fn synchronously_settled_final_promise_returns_its_value() {
        let (_captured, mut host) = configured_host();

        assert_eq!(
            evaluate(
                &mut host,
                "async function main() { return await Promise.resolve(42); } main();",
            ),
            Ok(CommandCompletion::Ready(CommandValue::from(42)))
        );
    }

    #[test]
    fn registrations_survive_a_later_global_script_exception() {
        let (captured, mut host) = configured_host();

        assert!(
            evaluate(
                &mut host,
                r#"
editor.commands.register("math.triple", value => value * 3);
throw new Error("after registration");
"#,
            )
            .is_err()
        );
        evaluate(&mut host, "capture(math.triple(14));").unwrap();

        assert_eq!(&*captured.borrow(), &[CommandValue::from(42)]);
        assert!(
            host.registry
                .borrow()
                .get(&CommandId::new("math.triple").unwrap())
                .is_some()
        );
    }

    #[test]
    fn top_level_await_and_static_interactive_import_are_explicit_errors() {
        let (_captured, mut host) = configured_host();

        let error = evaluate(&mut host, "await Promise.resolve(42)")
            .unwrap_err()
            .to_string();
        assert!(error.contains("top-level await"), "{error}");
        let error = evaluate(&mut host, "import './dependency.ts'")
            .unwrap_err()
            .to_string();
        assert!(error.contains("static import or export"), "{error}");
        assert!(evaluate(&mut host, "const = broken").is_err());
        assert!(evaluate(&mut host, "throw new Error('runtime')").is_err());
        evaluate(&mut host, "globalThis.recovered = true").unwrap();
    }

    #[test]
    fn buffer_evaluations_reuse_a_stable_source_identity() {
        let (_captured, mut host) = configured_host();
        let invocation = || {
            GlobalScriptRequest::Buffer {
                content: ContentId(7),
                resource_path: Some(PathBuf::from("workspace/example.ts")),
                source: "throw new Error('buffer')".to_owned(),
            }
            .into_invocation()
        };

        let first = host.invoke_command(invocation()).unwrap_err().to_string();
        let second = host.invoke_command(invocation()).unwrap_err().to_string();

        assert!(first.contains(".vell-buffer-7.ts"), "{first}");
        assert!(second.contains(".vell-buffer-7.ts"), "{second}");
    }

    #[test]
    fn buffer_registration_types_are_visible_to_other_sources() {
        let (_captured, mut host) = configured_host();
        host.invoke_command(
            GlobalScriptRequest::Buffer {
                content: ContentId(7),
                resource_path: Some(PathBuf::from("workspace/example.ts")),
                source: r#"
editor.commands.register(
  "math.typed",
  (value: number): number => value + 1,
);
"#
                .to_owned(),
            }
            .into_invocation(),
        )
        .unwrap();

        let diagnostics = host
            .script
            .borrow_mut()
            .type_environment
            .diagnostics(
                "file:///other-buffer.ts",
                r#"
editor.commands.math.typed("bad");
math.typed("bad");
"#,
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    }

    #[test]
    fn interactive_evaluations_receive_distinct_source_identities() {
        let (_captured, mut host) = configured_host();

        let first = evaluate(&mut host, "throw new Error('first')")
            .unwrap_err()
            .to_string();
        let second = evaluate(&mut host, "throw new Error('second')")
            .unwrap_err()
            .to_string();

        assert!(first.contains(".vell-interactive-0.ts"), "{first}");
        assert!(second.contains(".vell-interactive-1.ts"), "{second}");
    }

    #[test]
    fn files_share_globals_but_static_modules_keep_local_bindings_private() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("global.ts");
        let dependency = directory.path().join("dependency.ts");
        let module = directory.path().join("module.ts");
        fs::write(&global, "function fromFile() { return 20; }").unwrap();
        fs::write(&dependency, "export const answer = 22;").unwrap();
        fs::write(
            &module,
            r#"
import { answer } from "./dependency.ts";
const moduleLocal = answer;
globalThis.moduleAnswer = answer;
"#,
        )
        .unwrap();
        let (_captured, mut host) = configured_host();

        host.invoke_command(GlobalScriptRequest::File { path: global }.into_invocation())
            .unwrap();
        host.invoke_command(GlobalScriptRequest::File { path: module }.into_invocation())
            .unwrap();
        evaluate(
            &mut host,
            r#"
if (fromFile() + globalThis.moduleAnswer !== 42) throw new Error("file state");
if (typeof moduleLocal !== "undefined") throw new Error("module binding leaked");
"#,
        )
        .unwrap();
    }

    #[test]
    fn file_global_scripts_resolve_dynamic_imports_from_their_directory() {
        let directory = tempfile::tempdir().unwrap();
        let dependency = directory.path().join("dependency.ts");
        let script = directory.path().join("script.ts");
        fs::write(&dependency, "export const answer = 42;").unwrap();
        fs::write(
            &script,
            r#"
globalThis.dynamicImport = import("./dependency.ts")
  .then(module => { globalThis.dynamicAnswer = module.answer; });
globalThis.dynamicImport;
"#,
        )
        .unwrap();
        let (_captured, mut host) = configured_host();

        assert_eq!(
            host.invoke_command(GlobalScriptRequest::File { path: script }.into_invocation()),
            Ok(CommandCompletion::Ready(CommandValue::Null))
        );
        evaluate(
            &mut host,
            "if (globalThis.dynamicAnswer !== 42) throw new Error('dynamic import');",
        )
        .unwrap();
    }
}
