use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use deno_ast::swc::ast::{Callee, Expr, MemberProp, Stmt};
use deno_ast::{
    MediaType, ModuleItemRef, ModuleSpecifier, ParseParams, SourceRanged, SourceRangedForSpanned,
    parse_program,
};
use vell_mode::command_registry::{
    COMMAND_LINE_COMMAND_ID, CommandAdapter, CommandCompletion, CommandEntry, CommandError,
    CommandHost, CommandId, CommandQuery, CommandRequest, CommandResult, CommandValue,
};
use vell_protocol::ids::ContentId;

use super::commands::{change_count, publish_changes};
use super::global_script::EvaluationRequest;
use super::{ScriptError, ScriptHost};

#[derive(Clone)]
struct CommandLineAdapter {
    host: Rc<RefCell<ScriptHost>>,
}

impl CommandAdapter for CommandLineAdapter {
    fn invoke(&self, host: &mut dyn CommandHost, arguments: Vec<CommandValue>) -> CommandResult {
        let source = one_string(arguments)?;
        let changes = change_count(&self.host);
        let execution = self
            .host
            .try_borrow_mut()
            .map_err(|_| CommandError::Failed("script runtime is reentrant".to_owned()))?
            .execute_command_line(&source, host)
            .map_err(|error| CommandError::Failed(error.to_string()));
        let result = execution.and_then(|execution| execution.into_completion(&self.host));
        publish_changes(&self.host, host, changes);
        result
    }
}

pub(super) fn entry(host: &Rc<RefCell<ScriptHost>>) -> CommandEntry {
    CommandEntry::new(
        CommandId::new(COMMAND_LINE_COMMAND_ID).expect("command line service id"),
        CommandLineAdapter {
            host: Rc::clone(host),
        },
    )
}

impl ScriptHost {
    fn execute_command_line(
        &mut self,
        source: &str,
        host: &mut dyn CommandHost,
    ) -> Result<super::commands::ScriptExecution, ScriptError> {
        let source = source.trim();
        if source.is_empty() {
            return Ok(super::commands::ScriptExecution::Ready(CommandValue::Null));
        }

        let parsed = classify(source);
        if let LineKind::CommandCall { command, source } = parsed {
            let exists = query(host, CommandQuery::CommandExists(command.clone()))?;
            if exists != CommandValue::Bool(true) {
                return Err(ScriptError::new(format!("unknown command '{command}'")));
            }
            return self.execute_global_script(EvaluationRequest::Interactive(source), host);
        }

        let (name, argument) = split_shortcut(source);
        if self.commands.borrow().shortcut(name).is_some() {
            return self.execute_shortcut(name, argument, host);
        }
        if name == "ts" {
            return match argument {
                Some(source) => self
                    .execute_global_script(EvaluationRequest::Interactive(source.to_owned()), host),
                None => self.execute_current_buffer_statement(host),
            };
        }
        match parsed {
            LineKind::TypeScript => Err(ScriptError::new(
                "command line accepts one command call; use ':ts' for TypeScript statements",
            )),
            LineKind::Invalid | LineKind::CommandCall { .. } => {
                Err(ScriptError::new(format!("unknown shortcut '{name}'")))
            }
        }
    }

    fn execute_current_buffer_statement(
        &mut self,
        host: &mut dyn CommandHost,
    ) -> Result<super::commands::ScriptExecution, ScriptError> {
        let document = query(host, CommandQuery::CurrentTextDocument)?;
        let document = TextDocument::parse(document)?;
        let source = document.selected_source()?;
        self.execute_global_script(
            EvaluationRequest::Buffer {
                content: document.content,
                resource_path: document.resource_path,
                source,
            },
            host,
        )
    }
}

enum LineKind {
    CommandCall { command: CommandId, source: String },
    TypeScript,
    Invalid,
}

fn classify(source: &str) -> LineKind {
    let Ok(specifier) = ModuleSpecifier::parse("file:///vell-command-line.ts") else {
        return LineKind::Invalid;
    };
    let Ok(parsed) = parse_program(ParseParams {
        specifier,
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    }) else {
        return LineKind::Invalid;
    };
    let program = parsed.program_ref();
    let mut body = program.body();
    let Some(ModuleItemRef::Stmt(Stmt::Expr(statement))) = body.next() else {
        return LineKind::TypeScript;
    };
    if body.next().is_some() {
        return LineKind::TypeScript;
    }
    let Expr::Call(call) = statement.expr.as_ref() else {
        return LineKind::TypeScript;
    };
    let Callee::Expr(callee) = &call.callee else {
        return LineKind::TypeScript;
    };
    let Some(mut path) = command_path(callee) else {
        return LineKind::TypeScript;
    };
    let explicit = path.starts_with(&["editor", "commands"]);
    if explicit {
        path.drain(..2);
    }
    let Ok(command) = CommandId::new(path.join(".")) else {
        return LineKind::TypeScript;
    };
    let source = if explicit {
        source.to_owned()
    } else {
        let insertion = callee.start() - parsed.start();
        format!(
            "{}editor.commands.{}",
            &source[..insertion],
            &source[insertion..]
        )
    };
    LineKind::CommandCall { command, source }
}

fn command_path(expression: &Expr) -> Option<Vec<&str>> {
    match expression {
        Expr::Ident(identifier) => Some(vec![identifier.sym.as_ref()]),
        Expr::Member(member) => {
            let mut path = command_path(&member.obj)?;
            let MemberProp::Ident(property) = &member.prop else {
                return None;
            };
            path.push(property.sym.as_ref());
            Some(path)
        }
        Expr::TsInstantiation(instantiation) => command_path(&instantiation.expr),
        _ => None,
    }
}

fn split_shortcut(source: &str) -> (&str, Option<&str>) {
    let end = source.find(char::is_whitespace).unwrap_or(source.len());
    let name = &source[..end];
    let argument = source[end..].trim();
    (name, (!argument.is_empty()).then_some(argument))
}

struct TextDocument {
    content: ContentId,
    resource_path: Option<PathBuf>,
    source: String,
    anchor: usize,
    head: usize,
}

impl TextDocument {
    fn parse(value: CommandValue) -> Result<Self, ScriptError> {
        let object = value
            .as_object()
            .ok_or_else(|| ScriptError::new("current text document query returned no object"))?;
        let selection = object
            .get("selection")
            .and_then(CommandValue::as_object)
            .ok_or_else(|| ScriptError::new("current text document has no selection"))?;
        Ok(Self {
            content: ContentId(required_u64(object, "content")?),
            resource_path: object
                .get("resourcePath")
                .and_then(CommandValue::as_str)
                .map(PathBuf::from),
            source: required_string(object, "source")?.to_owned(),
            anchor: usize::try_from(required_u64(selection, "anchor")?)
                .map_err(|_| ScriptError::new("selection anchor is too large"))?,
            head: usize::try_from(required_u64(selection, "head")?)
                .map_err(|_| ScriptError::new("selection head is too large"))?,
        })
    }

    fn selected_source(&self) -> Result<String, ScriptError> {
        if self.anchor != self.head {
            let start = char_to_byte(&self.source, self.anchor.min(self.head))?;
            let end = char_to_byte(&self.source, self.anchor.max(self.head))?;
            return Ok(self.source[start..end].to_owned());
        }
        top_level_source(&self.source, self.head)
    }
}

fn top_level_source(source: &str, cursor: usize) -> Result<String, ScriptError> {
    let specifier = ModuleSpecifier::parse("file:///vell-buffer-selection.ts")
        .expect("static buffer selection specifier");
    let parsed = parse_program(ParseParams {
        specifier,
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|error| ScriptError::new(error.to_string()))?;
    let cursor = char_to_byte(source, cursor)?;
    let start = parsed.start();
    for item in parsed.program_ref().body() {
        let range = (item.start() - start)..(item.end() - start);
        if range.start <= cursor
            && (cursor < range.end || cursor == source.len() && cursor == range.end)
        {
            return Ok(source[range].to_owned());
        }
    }
    Err(ScriptError::new(
        "cursor is not inside a complete top-level TypeScript statement",
    ))
}

fn char_to_byte(source: &str, index: usize) -> Result<usize, ScriptError> {
    if index == source.chars().count() {
        return Ok(source.len());
    }
    source
        .char_indices()
        .nth(index)
        .map(|(offset, _)| offset)
        .ok_or_else(|| ScriptError::new("text selection is outside the current buffer"))
}

fn one_string(arguments: Vec<CommandValue>) -> Result<String, CommandError> {
    let [value] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "command line service expects one string".to_owned(),
        ));
    };
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| CommandError::InvalidArguments("command line must be a string".to_owned()))
}

fn query(host: &mut dyn CommandHost, query: CommandQuery) -> Result<CommandValue, ScriptError> {
    match host
        .request(CommandRequest::Query(query))
        .map_err(|error| ScriptError::new(error.to_string()))?
    {
        CommandCompletion::Ready(value) => Ok(value),
        CommandCompletion::Pending(_) => Err(ScriptError::new(
            "command query unexpectedly returned a pending task",
        )),
    }
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, CommandValue>,
    key: &str,
) -> Result<&'a str, ScriptError> {
    object
        .get(key)
        .and_then(CommandValue::as_str)
        .ok_or_else(|| ScriptError::new(format!("current text document has no '{key}'")))
}

fn required_u64(
    object: &serde_json::Map<String, CommandValue>,
    key: &str,
) -> Result<u64, ScriptError> {
    object
        .get(key)
        .and_then(CommandValue::as_u64)
        .ok_or_else(|| ScriptError::new(format!("current text document has no '{key}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_calls_are_scoped_to_editor_commands() {
        let LineKind::CommandCall { command, source } =
            classify("math.double(projectValue() + nested())")
        else {
            panic!("expected a command call");
        };

        assert_eq!(command.as_str(), "math.double");
        assert_eq!(
            source,
            "editor.commands.math.double(projectValue() + nested())"
        );
    }

    #[test]
    fn declarations_and_multiple_statements_are_not_command_calls() {
        assert!(matches!(classify("const value = 1"), LineKind::TypeScript));
        assert!(matches!(
            classify("content.save(); quit()"),
            LineKind::TypeScript
        ));
    }

    #[test]
    fn shortcut_tail_is_one_trimmed_string() {
        assert_eq!(
            split_shortcut("write   file name.ts  "),
            ("write", Some("file name.ts"))
        );
        assert_eq!(split_shortcut("quit"), ("quit", None));
    }

    #[test]
    fn selects_unicode_top_level_statement_at_cursor() {
        let source = "const 前 = 1;\nfunction 后() { return 前 + 1; }\n";
        let cursor = source[..source.find("后").unwrap()].chars().count();

        assert_eq!(
            top_level_source(source, cursor).unwrap(),
            "function 后() { return 前 + 1; }"
        );
    }

    #[test]
    fn non_empty_selection_wins_without_parsing_the_rest_of_the_buffer() {
        let source = "not valid TypeScript !!!\ncontent.create()";
        let anchor = source[..source.find("content.create").unwrap()]
            .chars()
            .count();
        let document = TextDocument {
            content: ContentId(1),
            resource_path: None,
            source: source.to_owned(),
            anchor,
            head: anchor + "content.create()".chars().count(),
        };

        assert_eq!(document.selected_source().unwrap(), "content.create()");
    }
}
