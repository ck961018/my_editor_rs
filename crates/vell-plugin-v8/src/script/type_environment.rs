use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::TYPESCRIPT_DECLARATIONS;

use super::commands::ScriptCommandRegistration;
use super::{
    HeapLimitState, InvocationWatchdog, ScriptError, ScriptInvocationKind, current_exception,
    initialize_v8, install_heap_limit, json_to_v8, recover_heap_limit, v8_to_json,
};

pub const TYPESCRIPT_COMPILER_VERSION: &str = "5.9.3";

const COMPILER_BUNDLE: &str = include_str!("../../vendor/typescript/typescript.js");
const COMPILER_HOST: &str = include_str!("../../vendor/type_environment.js");
const NATIVE_COMMAND_DECLARATIONS: &str =
    include_str!("../../../../runtime/commands.generated.d.ts");
const LIB_ES5: &str = include_str!("../../vendor/typescript/lib/lib.es5.d.ts");
const LIB_PROMISE: &str = include_str!("../../vendor/typescript/lib/lib.es2015.promise.d.ts");
const LIB_DECORATORS: &str = include_str!("../../vendor/typescript/lib/lib.decorators.d.ts");
const LIB_DECORATORS_LEGACY: &str =
    include_str!("../../vendor/typescript/lib/lib.decorators.legacy.d.ts");
const COMPILER_HEAP_LIMIT_BYTES: usize = 256 * 1024 * 1024;
const COMPILER_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const COMPILER_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct TypeEnvironment {
    sources: BTreeMap<String, String>,
    compiler: Option<CompilerClient>,
    fault: Option<String>,
    generated_declarations: String,
}

impl Default for TypeEnvironment {
    fn default() -> Self {
        let mut sources = BTreeMap::new();
        sources.insert(
            "/editor.d.ts".to_owned(),
            TYPESCRIPT_DECLARATIONS.to_owned(),
        );
        sources.insert(
            "/commands.generated.d.ts".to_owned(),
            NATIVE_COMMAND_DECLARATIONS.to_owned(),
        );
        sources.insert("/lib.es5.d.ts".to_owned(), LIB_ES5.to_owned());
        sources.insert(
            "/lib.es2015.promise.d.ts".to_owned(),
            LIB_PROMISE.to_owned(),
        );
        sources.insert("/lib.decorators.d.ts".to_owned(), LIB_DECORATORS.to_owned());
        sources.insert(
            "/lib.decorators.legacy.d.ts".to_owned(),
            LIB_DECORATORS_LEGACY.to_owned(),
        );
        Self {
            sources,
            compiler: None,
            fault: None,
            generated_declarations: NATIVE_COMMAND_DECLARATIONS.to_owned(),
        }
    }
}

impl TypeEnvironment {
    pub(super) fn update_source(&mut self, source: impl Into<String>, text: impl Into<String>) {
        let source = source.into().replace('\\', "/");
        let text = text.into();
        if self.sources.get(&source) == Some(&text) {
            return;
        }
        self.sources.insert(source.clone(), text.clone());
        let Some(compiler) = self.compiler.as_mut() else {
            return;
        };
        if let Err(error) = compiler.update_source(&source, &text) {
            self.disable(error);
        }
    }

    pub(super) fn publish(&mut self, registrations: &[ScriptCommandRegistration]) {
        if registrations.is_empty() || self.fault.is_some() {
            return;
        }
        let result = (|| {
            let compiler = self.compiler()?;
            compiler.publish(registrations)
        })();
        match result {
            Ok(declarations) => {
                self.generated_declarations = declarations.clone();
                self.sources
                    .insert("/commands.generated.d.ts".to_owned(), declarations);
            }
            Err(error) => self.disable(error),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn diagnostics(
        &mut self,
        source: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Vec<String>, ScriptError> {
        let source = source.into();
        self.update_source(source.clone(), text);
        let value = self.compiler()?.call("diagnostics", &[json!(source)])?;
        serde_json::from_value(value)
            .map_err(|error| ScriptError::new(format!("invalid compiler diagnostics: {error}")))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn compiler_version(&mut self) -> Result<String, ScriptError> {
        let value = self.compiler()?.call("version", &[])?;
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ScriptError::new("compiler returned an invalid version"))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn generated_declarations(&self) -> &str {
        &self.generated_declarations
    }

    #[cfg(test)]
    pub(super) fn fault_for_test(&mut self) {
        self.fault = Some("injected compiler fault".to_owned());
        self.compiler = None;
    }

    #[cfg(test)]
    pub(super) fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    fn compiler(&mut self) -> Result<&mut CompilerClient, ScriptError> {
        if let Some(fault) = &self.fault {
            return Err(ScriptError::new(format!(
                "TypeScript compiler is unavailable: {fault}"
            )));
        }
        if self.compiler.is_none() {
            let mut compiler = CompilerClient::new()?;
            for (source, text) in &self.sources {
                compiler.update_source(source, text)?;
            }
            compiler.initialize_commands()?;
            self.compiler = Some(compiler);
        }
        Ok(self.compiler.as_mut().expect("compiler initialized"))
    }

    fn disable(&mut self, error: ScriptError) {
        self.fault = Some(error.to_string());
        self.compiler = None;
    }
}

struct CompilerIsolate {
    isolate: v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
    heap_limit: Box<HeapLimitState>,
}

struct CompilerClient {
    sender: mpsc::Sender<CompilerRequest>,
    thread: Option<thread::JoinHandle<()>>,
}

enum CompilerRequest {
    Call {
        method: String,
        arguments: Vec<Value>,
        response: mpsc::SyncSender<Result<Value, String>>,
    },
    Shutdown,
}

impl CompilerClient {
    fn new() -> Result<Self, ScriptError> {
        let (sender, receiver) = mpsc::channel();
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("vell-typescript-compiler".to_owned())
            .spawn(move || {
                let mut compiler = match CompilerIsolate::new() {
                    Ok(compiler) => {
                        let _ = startup_sender.send(Ok(()));
                        compiler
                    }
                    Err(error) => {
                        let _ = startup_sender.send(Err(error.to_string()));
                        return;
                    }
                };
                while let Ok(request) = receiver.recv() {
                    match request {
                        CompilerRequest::Call {
                            method,
                            arguments,
                            response,
                        } => {
                            let result = compiler
                                .call(&method, &arguments)
                                .map_err(|error| error.to_string());
                            let _ = response.send(result);
                        }
                        CompilerRequest::Shutdown => break,
                    }
                }
            })
            .map_err(|error| {
                ScriptError::new(format!("failed to start TypeScript compiler: {error}"))
            })?;
        match startup_receiver.recv_timeout(COMPILER_STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                sender,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(ScriptError::new(error))
            }
            Err(error) => Err(ScriptError::new(format!(
                "TypeScript compiler did not start: {error}"
            ))),
        }
    }

    fn update_source(&mut self, source: &str, text: &str) -> Result<(), ScriptError> {
        self.call("updateSource", &[json!(source), json!(text)])?;
        Ok(())
    }

    fn initialize_commands(&mut self) -> Result<(), ScriptError> {
        self.call("initializeCommands", &[])?;
        Ok(())
    }

    fn publish(
        &mut self,
        registrations: &[ScriptCommandRegistration],
    ) -> Result<String, ScriptError> {
        let registrations = registrations
            .iter()
            .map(|registration| {
                json!({
                    "id": registration.id.as_str(),
                    "source": registration.source.as_ref().map(|source| &source.identity),
                    "line": registration.source.as_ref().map(|source| source.line),
                    "column": registration.source.as_ref().map(|source| source.column),
                })
            })
            .collect::<Vec<_>>();
        let value = self.call("publishRegistrations", &[Value::Array(registrations)])?;
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ScriptError::new("compiler returned invalid command declarations"))
    }

    fn call(&mut self, method: &str, arguments: &[Value]) -> Result<Value, ScriptError> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(CompilerRequest::Call {
                method: method.to_owned(),
                arguments: arguments.to_vec(),
                response,
            })
            .map_err(|_| ScriptError::new("TypeScript compiler thread stopped"))?;
        receiver
            .recv_timeout(COMPILER_STARTUP_TIMEOUT + COMPILER_QUERY_TIMEOUT)
            .map_err(|error| {
                ScriptError::new(format!("TypeScript compiler did not reply: {error}"))
            })?
            .map_err(ScriptError::new)
    }
}

impl Drop for CompilerClient {
    fn drop(&mut self) {
        let _ = self.sender.send(CompilerRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl CompilerIsolate {
    fn new() -> Result<Self, ScriptError> {
        initialize_v8();
        let params = v8::CreateParams::default().heap_limits(0, COMPILER_HEAP_LIMIT_BYTES);
        let mut isolate = v8::Isolate::new(params);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        let context = {
            v8::scope!(scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            v8::Global::new(scope, context)
        };
        let heap_limit = install_heap_limit(&mut isolate);
        let mut compiler = Self {
            isolate,
            context,
            heap_limit,
        };
        compiler.run_source("typescript.js", COMPILER_BUNDLE)?;
        compiler.run_source("type_environment.js", COMPILER_HOST)?;
        Ok(compiler)
    }

    fn run_source(&mut self, name: &str, source: &str) -> Result<(), ScriptError> {
        let context = self.context.clone();
        self.invoke(
            ScriptInvocationKind::ModuleEvaluation,
            COMPILER_STARTUP_TIMEOUT,
            |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                v8::tc_scope!(let scope, scope);
                let source = v8::String::new(scope, source)
                    .ok_or_else(|| ScriptError::new("compiler bundle is too large for V8"))?;
                let resource_name = v8::String::new(scope, name)
                    .ok_or_else(|| ScriptError::new("invalid compiler source name"))?;
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
                let script = v8::Script::compile(scope, source, Some(&origin))
                    .ok_or_else(|| current_exception(scope, name, "compile"))?;
                script
                    .run(scope)
                    .ok_or_else(|| current_exception(scope, name, "execute"))?;
                Ok(())
            },
        )
    }

    fn call(&mut self, method: &str, arguments: &[Value]) -> Result<Value, ScriptError> {
        let context = self.context.clone();
        self.invoke(
            ScriptInvocationKind::Action,
            COMPILER_QUERY_TIMEOUT,
            |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                v8::tc_scope!(let scope, scope);
                let global = scope.get_current_context().global(scope);
                let environment_key = v8::String::new(scope, "__vellTypeEnvironment")
                    .ok_or_else(|| ScriptError::new("invalid compiler environment key"))?;
                let environment = global
                    .get(scope, environment_key.into())
                    .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
                    .ok_or_else(|| ScriptError::new("compiler environment is unavailable"))?;
                let method_key = v8::String::new(scope, method)
                    .ok_or_else(|| ScriptError::new("invalid compiler method name"))?;
                let function = environment
                    .get(scope, method_key.into())
                    .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
                    .ok_or_else(|| ScriptError::new("compiler method is unavailable"))?;
                let arguments = arguments
                    .iter()
                    .map(|value| json_to_v8(scope, value))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = function
                    .call(scope, environment.into(), &arguments)
                    .ok_or_else(|| current_exception(scope, method, "execute compiler method"))?;
                v8_to_json(scope, value, method)
            },
        )
    }

    fn invoke<T>(
        &mut self,
        kind: ScriptInvocationKind,
        timeout: Duration,
        callback: impl FnOnce(&mut v8::OwnedIsolate) -> Result<T, ScriptError>,
    ) -> Result<T, ScriptError> {
        let watchdog = InvocationWatchdog::start(self.isolate.thread_safe_handle(), kind, timeout)?;
        let result = callback(&mut self.isolate);
        let result = watchdog.finish(result);
        if recover_heap_limit(
            &mut self.isolate,
            &mut self.heap_limit,
            COMPILER_HEAP_LIMIT_BYTES,
        ) {
            return Err(ScriptError::new("TypeScript compiler heap limit exceeded"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_bundle_matches_the_locked_package_version() {
        let package: Value =
            serde_json::from_str(include_str!("../../../../package.json")).unwrap();
        assert_eq!(
            package["devDependencies"]["typescript"],
            TYPESCRIPT_COMPILER_VERSION
        );
        let mut environment = TypeEnvironment::default();
        assert_eq!(
            environment.compiler_version().unwrap(),
            TYPESCRIPT_COMPILER_VERSION
        );
    }

    #[test]
    fn vendored_compiler_includes_its_license() {
        let license = include_str!("../../vendor/typescript/LICENSE.txt");
        assert!(license.contains("Apache License"));
    }
}
