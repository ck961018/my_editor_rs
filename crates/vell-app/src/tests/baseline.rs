use std::time::{Duration, Instant};

use super::{
    App, AppQuery, BehaviorRecorder, ChainProbeMode, ScriptedFrontend, editor_cid, make_app,
    make_script_app, view_edit, view_id,
};
use crate::bootstrap::bootstrap_editor;
use crate::dispatcher::DispatchCommand;
use crate::mode::{mode_state_clone_metrics, reset_mode_state_clone_metrics};
use crate::mode_name::ModeName;
use vell_core::buffer::Buffer;
use vell_core::command::EditCommand;
use vell_protocol::content_query::{FaceName, NamedTextDecoration, RenderQuery, RowRange};
use vell_protocol::frontend_event::FrontendEvent;
use vell_protocol::key_event::KeyEvent;
use vell_protocol::selection::TextOffset;

const STARTUP_ITERATIONS: usize = 5;
const INPUT_ITERATIONS: usize = 500;
const PRESENTATION_ITERATIONS: usize = 100;

struct LargePresentationMode {
    name: ModeName,
}

impl crate::mode::Mode for LargePresentationMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[crate::mode_name::ModeActionName] {
        &[]
    }

    fn adapters(&self) -> crate::mode::ModeAdapters {
        crate::mode::ModeAdapters::buffer()
    }

    fn content_decorations(
        &self,
        _state: &dyn crate::mode::ModeState,
        _context: &crate::mode::ModeContentContext<'_>,
        rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        (rows.start..rows.end)
            .map(|row| NamedTextDecoration {
                start: TextOffset {
                    char_index: row * 2,
                },
                end: TextOffset {
                    char_index: row * 2 + 1,
                },
                face: FaceName::new("baseline.large"),
            })
            .collect()
    }
}

fn make_native_app() -> App<ScriptedFrontend> {
    let bootstrap = bootstrap_editor(Buffer::new(), 40, 5, Vec::new()).unwrap();
    App {
        kernel: bootstrap.kernel,
        session: bootstrap.session,
        frontend: ScriptedFrontend::new(Vec::new()),
        runtime_diagnostics: Vec::new(),
        behavior: BehaviorRecorder::default(),
    }
}

fn micros_per_iteration(elapsed: Duration, iterations: usize) -> f64 {
    elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
}

fn report(name: &str, elapsed: Duration, iterations: usize) {
    println!(
        "M0_BASELINE {name} iterations={iterations} total_us={} per_iter_us={:.3}",
        elapsed.as_micros(),
        micros_per_iteration(elapsed, iterations),
    );
}

fn report_clones(name: &str) {
    let metrics = mode_state_clone_metrics();
    println!(
        "M0_BASELINE {name}_clones count={} total_ns={} inline_bytes={}",
        metrics.count, metrics.nanos, metrics.inline_bytes,
    );
    assert!(metrics.count > 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual M0 performance baseline"]
async fn m0_performance_baseline() {
    let started = Instant::now();
    drop(make_app(Vec::new(), None));
    report("cold_model_startup", started.elapsed(), 1);

    let started = Instant::now();
    for _ in 0..STARTUP_ITERATIONS {
        drop(make_app(Vec::new(), None));
    }
    report("warm_model_startup", started.elapsed(), STARTUP_ITERATIONS);

    let mut native = make_native_app();
    let native_mode = ModeName::new("baseline-native");
    native
        .kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            native_mode.as_str(),
            vec![view_edit(EditCommand::InsertText("x".to_owned()))],
            false,
        ))
        .unwrap();
    native
        .attach_mode_to_content(editor_cid(), &native_mode)
        .unwrap();
    reset_mode_state_clone_metrics();
    let started = Instant::now();
    for _ in 0..INPUT_ITERATIONS {
        native
            .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
            .await
            .unwrap();
    }
    report("native_input", started.elapsed(), INPUT_ITERATIONS);
    report_clones("native_input");

    let mut script = make_script_app(
        r#"
editor.modes.define({
  name: "baseline-script",
  on: {
    buffer: {
      commands: {
        insert(ctx) { ctx.edit.insert("x"); },
      },
      keys: { q: "insert" },
    },
  },
});
"#,
    );
    reset_mode_state_clone_metrics();
    let started = Instant::now();
    for _ in 0..INPUT_ITERATIONS {
        script
            .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
            .await
            .unwrap();
    }
    report("script_input", started.elapsed(), INPUT_ITERATIONS);
    report_clones("script_input");

    let mut presentation = make_native_app();
    let view = view_id(&presentation, presentation.session.focused());
    presentation
        .execute_command(DispatchCommand::ContentWithView {
            command: crate::command::ContentCommand::Edit(EditCommand::InsertText(
                "x\n".repeat(10_000),
            )),
            view,
            content: editor_cid(),
        })
        .unwrap();
    let presentation_mode = ModeName::new("baseline-presentation");
    presentation
        .kernel
        .modes_mut()
        .register(LargePresentationMode {
            name: presentation_mode.clone(),
        })
        .unwrap();
    presentation
        .attach_mode_to_content(editor_cid(), &presentation_mode)
        .unwrap();
    let query = AppQuery {
        contents: presentation.kernel.contents(),
        views: presentation.session.views(),
        presentation: presentation.session.presentation(),
        faces: presentation.session.faces(),
    };
    let visible_rows = RowRange {
        start: 5_000,
        end: 5_050,
    };
    let started = Instant::now();
    let mut visible_decorations = 0;
    for _ in 0..PRESENTATION_ITERATIONS {
        visible_decorations += query.decorations(view, visible_rows).unwrap().len();
    }
    assert_eq!(visible_decorations, PRESENTATION_ITERATIONS * 50);
    report(
        "large_document_visible_decorations",
        started.elapsed(),
        PRESENTATION_ITERATIONS,
    );
}
