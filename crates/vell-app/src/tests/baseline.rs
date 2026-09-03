use std::time::{Duration, Instant};

use super::{
    App, AppQuery, BehaviorRecorder, ChainProbeMode, ScriptedFrontend, editor_cid, make_app,
    make_completion_app, make_script_app, view_edit, view_id,
};
use crate::bootstrap::bootstrap_editor;
use crate::dispatcher::DispatchCommand;
use crate::message::AppMessage;
use crate::mode::{CompletionSourceDefinition, CompletionSourceId};
use crate::mode::{mode_state_clone_metrics, reset_mode_state_clone_metrics};
use crate::mode_name::ModeName;
use vell_completion::{
    CompletionBatch, CompletionItem, CompletionTrigger, IncompleteDirections, SourceBatchVersion,
};
use vell_core::buffer::Buffer;
use vell_core::command::EditCommand;
use vell_protocol::content_query::{FaceName, NamedTextDecoration, RenderQuery, RowRange};
use vell_protocol::frontend_event::FrontendEvent;
use vell_protocol::key_event::KeyEvent;
use vell_protocol::selection::{Selection, Selections, TextOffset};
use vell_protocol::space::SplitDirection;

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
        next_command_task: 0,
        command_tasks: Default::default(),
        pending_commands: Vec::new(),
        completion_diagnostics: Default::default(),
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

fn measure_async_sample(started: Instant, samples: &mut Vec<Duration>) {
    samples.push(started.elapsed());
}

fn report_samples(name: &str, samples: &mut [Duration]) {
    samples.sort_unstable();
    let percentile = |percent: usize| {
        let index = (samples.len() * percent).div_ceil(100).saturating_sub(1);
        samples[index.min(samples.len() - 1)].as_secs_f64() * 1_000_000.0
    };
    println!(
        "M0_BASELINE {name} iterations={} p50_us={:.3} p95_us={:.3} p99_us={:.3}",
        samples.len(),
        percentile(50),
        percentile(95),
        percentile(99),
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

    let mut idle = make_native_app();
    let mut idle_samples = Vec::with_capacity(INPUT_ITERATIONS);
    for _ in 0..INPUT_ITERATIONS {
        let started = Instant::now();
        idle.handle_event(FrontendEvent::Key(KeyEvent::unknown()))
            .await
            .unwrap();
        measure_async_sample(started, &mut idle_samples);
    }
    report_samples("idle_input", &mut idle_samples);

    let mut insert = make_native_app();
    let mut insert_samples = Vec::with_capacity(INPUT_ITERATIONS);
    for _ in 0..INPUT_ITERATIONS {
        let started = Instant::now();
        insert
            .handle_event(FrontendEvent::Paste("x".to_owned()))
            .await
            .unwrap();
        measure_async_sample(started, &mut insert_samples);
    }
    report_samples("ordinary_insert", &mut insert_samples);

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
    let mut native_samples = Vec::with_capacity(INPUT_ITERATIONS);
    for _ in 0..INPUT_ITERATIONS {
        let started = Instant::now();
        native
            .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
            .await
            .unwrap();
        measure_async_sample(started, &mut native_samples);
    }
    report_samples("native_mode_chain", &mut native_samples);
    report_clones("native_mode_chain");

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

    let mut render = make_native_app();
    let mut render_samples = Vec::with_capacity(PRESENTATION_ITERATIONS);
    for _ in 0..PRESENTATION_ITERATIONS {
        let started = Instant::now();
        render.render().unwrap();
        measure_async_sample(started, &mut render_samples);
    }
    report_samples("app_render_noop_frontend", &mut render_samples);

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
    let body_space = presentation.session.body_space_for_view(view).unwrap();
    let visible_rows = RowRange {
        start: 5_000,
        end: 5_050,
    };
    let started = Instant::now();
    let mut visible_decorations = 0;
    for _ in 0..PRESENTATION_ITERATIONS {
        visible_decorations += query
            .decorations(view, body_space, visible_rows)
            .unwrap()
            .len();
    }
    assert_eq!(visible_decorations, PRESENTATION_ITERATIONS * 50);
    report(
        "large_document_visible_decorations",
        started.elapsed(),
        PRESENTATION_ITERATIONS,
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "release-only M2 completion performance gate"]
async fn m2_completion_performance_gate() {
    const SAMPLES: usize = 100;
    let mut input_samples = Vec::with_capacity(SAMPLES);
    let mut visible_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let bootstrap = bootstrap_editor(
            Buffer::new(),
            40,
            5,
            vec![crate::buffer_word_completion_mode()],
        )
        .unwrap();
        let mut app = App {
            kernel: bootstrap.kernel,
            session: bootstrap.session,
            frontend: ScriptedFrontend::new(Vec::new()),
            runtime_diagnostics: Vec::new(),
            next_command_task: 0,
            command_tasks: Default::default(),
            pending_commands: Vec::new(),
            completion_diagnostics: Default::default(),
            behavior: BehaviorRecorder::default(),
        };
        let view = view_id(&app, app.session.focused());
        app.execute_command(DispatchCommand::ContentWithView {
            command: crate::command::ContentCommand::Edit(EditCommand::InsertText(
                "alpha beta a".to_owned(),
            )),
            view,
            content: editor_cid(),
        })
        .unwrap();

        let started = Instant::now();
        assert!(
            app.trigger_completion_for_view(view, vell_completion::CompletionTrigger::Identifier,)
        );
        input_samples.push(started.elapsed());
        while app
            .session
            .completion()
            .snapshot(view)
            .is_none_or(|snapshot| snapshot.candidates.is_empty())
        {
            let message =
                tokio::time::timeout(Duration::from_millis(100), app.kernel.receive_message())
                    .await
                    .unwrap()
                    .unwrap();
            app.handle_app_message(message).unwrap();
        }
        visible_samples.push(started.elapsed());
        app.kernel.cancel();
        tokio::task::yield_now().await;
    }
    input_samples.sort_unstable();
    visible_samples.sort_unstable();
    let p99_input = input_samples[(SAMPLES * 99).div_ceil(100) - 1];
    let p95_visible = visible_samples[(SAMPLES * 95).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_GATE p99_input_us={:.3} p95_visible_us={:.3}",
        p99_input.as_secs_f64() * 1_000_000.0,
        p95_visible.as_secs_f64() * 1_000_000.0,
    );
    assert!(p99_input < Duration::from_micros(500));
    assert!(p95_visible < Duration::from_millis(8));

    const COLD_SAMPLES: usize = 20;
    let mut large_text = format!("zzz alpha {}", "tail ".repeat(100_000));
    large_text.push('a');
    let bootstrap = bootstrap_editor(
        Buffer::new(),
        40,
        5,
        vec![crate::buffer_word_completion_mode()],
    )
    .unwrap();
    let mut cold_app = App {
        kernel: bootstrap.kernel,
        session: bootstrap.session,
        frontend: ScriptedFrontend::new(Vec::new()),
        runtime_diagnostics: Vec::new(),
        next_command_task: 0,
        command_tasks: Default::default(),
        pending_commands: Vec::new(),
        completion_diagnostics: Default::default(),
        behavior: BehaviorRecorder::default(),
    };
    let cold_view = view_id(&cold_app, cold_app.session.focused());
    cold_app
        .execute_command(DispatchCommand::ContentWithView {
            command: crate::command::ContentCommand::Edit(EditCommand::InsertText(large_text)),
            view: cold_view,
            content: editor_cid(),
        })
        .unwrap();
    let mut cold_visible_samples = Vec::with_capacity(COLD_SAMPLES);
    for _ in 0..COLD_SAMPLES {
        let started = Instant::now();
        assert!(cold_app.trigger_completion_for_view(
            cold_view,
            vell_completion::CompletionTrigger::Identifier,
        ));
        while cold_app
            .session
            .completion()
            .snapshot(cold_view)
            .is_none_or(|snapshot| snapshot.candidates.is_empty())
        {
            let message = tokio::time::timeout(
                Duration::from_millis(100),
                cold_app.kernel.receive_message(),
            )
            .await
            .unwrap()
            .unwrap();
            cold_app.handle_app_message(message).unwrap();
        }
        cold_visible_samples.push(started.elapsed());
        cold_app.cancel_completion_view(cold_view);
        cold_app
            .execute_command(DispatchCommand::ContentWithView {
                command: crate::command::ContentCommand::Edit(EditCommand::Delete(-1)),
                view: cold_view,
                content: editor_cid(),
            })
            .unwrap();
        cold_app
            .execute_command(DispatchCommand::ContentWithView {
                command: crate::command::ContentCommand::Edit(EditCommand::InsertText(
                    "a".to_owned(),
                )),
                view: cold_view,
                content: editor_cid(),
            })
            .unwrap();
        tokio::task::yield_now().await;
    }
    cold_visible_samples.sort_unstable();
    let cold_p95 = cold_visible_samples[(COLD_SAMPLES * 95).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_COLD_LARGE p95_visible_us={:.3}",
        cold_p95.as_secs_f64() * 1_000_000.0,
    );
    assert!(cold_p95 < Duration::from_millis(8));
    cold_app.kernel.cancel();

    let long_no_match_query = "q".repeat(4 * 1024);
    let long_query_start = "zzz alpha zzmatchsuffix ".chars().count();
    let mut full_text = String::with_capacity(1_200_000);
    full_text.push_str("zzz alpha zzmatchsuffix ");
    full_text.push_str(&long_no_match_query);
    full_text.push(' ');
    for index in 0..99_996 {
        use std::fmt::Write as _;
        let _ = write!(full_text, "word{index} ");
    }
    full_text.push('a');
    let bootstrap = bootstrap_editor(
        Buffer::new(),
        40,
        5,
        vec![crate::buffer_word_completion_mode()],
    )
    .unwrap();
    let mut full_app = App {
        kernel: bootstrap.kernel,
        session: bootstrap.session,
        frontend: ScriptedFrontend::new(Vec::new()),
        runtime_diagnostics: Vec::new(),
        next_command_task: 0,
        command_tasks: Default::default(),
        pending_commands: Vec::new(),
        completion_diagnostics: Default::default(),
        behavior: BehaviorRecorder::default(),
    };
    let full_view = view_id(&full_app, full_app.session.focused());
    full_app
        .execute_command(DispatchCommand::ContentWithView {
            command: crate::command::ContentCommand::Edit(EditCommand::InsertText(full_text)),
            view: full_view,
            content: editor_cid(),
        })
        .unwrap();
    let full_started = Instant::now();
    assert!(full_app.trigger_completion_for_view(
        full_view,
        vell_completion::CompletionTrigger::Identifier,
    ));
    let mut full_messages = 0;
    let mut max_input = Duration::ZERO;
    let mut max_source_poll = Duration::ZERO;
    while full_app.kernel.completion_task_count_for_test() > 0
        || full_app
            .session
            .completion()
            .snapshot(full_view)
            .is_some_and(|snapshot| snapshot.collecting)
    {
        let poll_started = Instant::now();
        tokio::task::yield_now().await;
        max_source_poll = max_source_poll.max(poll_started.elapsed());
        let message =
            tokio::time::timeout(Duration::from_secs(2), full_app.kernel.receive_message())
                .await
                .unwrap()
                .unwrap();
        let input_started = Instant::now();
        assert!(
            !full_app
                .handle_completion_interaction(KeyEvent::char('z'))
                .unwrap()
        );
        max_input = max_input.max(input_started.elapsed());
        full_app.handle_app_message(message).unwrap();
        full_messages += 1;
        assert!(
            full_messages <= 3,
            "source emitted an unbounded message stream"
        );
    }
    let full_elapsed = full_started.elapsed();
    println!(
        "M2_COMPLETION_FULL_LIFECYCLE total_ms={:.3} messages={} \
         max_input_us={:.3} max_source_poll_us={:.3}",
        full_elapsed.as_secs_f64() * 1_000.0,
        full_messages,
        max_input.as_secs_f64() * 1_000_000.0,
        max_source_poll.as_secs_f64() * 1_000_000.0,
    );
    assert!(full_elapsed < Duration::from_millis(100));
    assert!(max_input < Duration::from_micros(500));
    assert!(max_source_poll < Duration::from_micros(500));

    const CACHE_SAMPLES: usize = 20;
    let mut warm_visible_samples = Vec::with_capacity(CACHE_SAMPLES);
    for _ in 0..CACHE_SAMPLES {
        let started = Instant::now();
        assert!(full_app.trigger_completion_for_view(
            full_view,
            vell_completion::CompletionTrigger::Identifier,
        ));
        while full_app
            .session
            .completion()
            .snapshot(full_view)
            .is_none_or(|snapshot| snapshot.candidates.is_empty())
        {
            let message = tokio::time::timeout(
                Duration::from_millis(100),
                full_app.kernel.receive_message(),
            )
            .await
            .unwrap()
            .unwrap();
            full_app.handle_app_message(message).unwrap();
        }
        warm_visible_samples.push(started.elapsed());
        full_app.cancel_completion_view(full_view);
        tokio::task::yield_now().await;
    }
    warm_visible_samples.sort_unstable();
    let warm_p95 = warm_visible_samples[(CACHE_SAMPLES * 95).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_WARM_RETRIGGER p95_visible_us={:.3}",
        warm_p95.as_secs_f64() * 1_000_000.0,
    );
    assert!(warm_p95 < Duration::from_millis(8));

    let second_space = full_app
        .split_space(
            full_app.session.body_space_for_view(full_view).unwrap(),
            editor_cid(),
            true,
            SplitDirection::Right,
            true,
        )
        .unwrap()
        .new_space;
    let second_view = view_id(&full_app, second_space);
    let mut shared_content_samples = Vec::with_capacity(CACHE_SAMPLES);
    for _ in 0..CACHE_SAMPLES {
        let started = Instant::now();
        assert!(full_app.trigger_completion_for_view(
            second_view,
            vell_completion::CompletionTrigger::Identifier,
        ));
        while full_app
            .session
            .completion()
            .snapshot(second_view)
            .is_none_or(|snapshot| snapshot.candidates.is_empty())
        {
            let message = tokio::time::timeout(
                Duration::from_millis(100),
                full_app.kernel.receive_message(),
            )
            .await
            .unwrap()
            .unwrap();
            full_app.handle_app_message(message).unwrap();
        }
        shared_content_samples.push(started.elapsed());
        full_app.cancel_completion_view(second_view);
        tokio::task::yield_now().await;
    }
    shared_content_samples.sort_unstable();
    let shared_p95 = shared_content_samples[(CACHE_SAMPLES * 95).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_SHARED_CONTENT p95_visible_us={:.3}",
        shared_p95.as_secs_f64() * 1_000_000.0,
    );
    assert!(shared_p95 < Duration::from_millis(8));

    full_app
        .session
        .view_mut(second_view)
        .unwrap()
        .require_document_state_mut()
        .replace_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: "zzz alpha zzmatch".chars().count(),
        })))
        .unwrap();
    let mut late_match_samples = Vec::with_capacity(CACHE_SAMPLES);
    for _ in 0..CACHE_SAMPLES {
        let started = Instant::now();
        assert!(full_app.trigger_completion_for_view(
            second_view,
            vell_completion::CompletionTrigger::Identifier,
        ));
        while full_app
            .session
            .completion()
            .snapshot(second_view)
            .is_none_or(|snapshot| snapshot.candidates.is_empty())
        {
            let message = tokio::time::timeout(
                Duration::from_millis(100),
                full_app.kernel.receive_message(),
            )
            .await
            .unwrap()
            .unwrap();
            full_app.handle_app_message(message).unwrap();
        }
        late_match_samples.push(started.elapsed());
        full_app.cancel_completion_view(second_view);
        tokio::task::yield_now().await;
    }
    late_match_samples.sort_unstable();
    let late_match_p95 = late_match_samples[(CACHE_SAMPLES * 95).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_WARM_LATE_MATCH p95_visible_us={:.3}",
        late_match_p95.as_secs_f64() * 1_000_000.0,
    );
    assert!(late_match_p95 < Duration::from_millis(8));

    full_app
        .session
        .view_mut(second_view)
        .unwrap()
        .require_document_state_mut()
        .replace_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: long_query_start + long_no_match_query.chars().count(),
        })))
        .unwrap();
    assert!(
        full_app.trigger_completion_for_view(
            second_view,
            vell_completion::CompletionTrigger::Identifier,
        )
    );
    let mut max_no_match_poll = Duration::ZERO;
    for _ in 0..8 {
        let started = Instant::now();
        tokio::task::yield_now().await;
        assert!(
            !full_app
                .handle_completion_interaction(KeyEvent::char('z'))
                .unwrap()
        );
        max_no_match_poll = max_no_match_poll.max(started.elapsed());
    }
    assert!(
        full_app
            .session
            .completion()
            .snapshot(second_view)
            .is_some_and(|snapshot| snapshot.candidates.is_empty())
    );
    full_app.cancel_completion_view(second_view);
    for _ in 0..8 {
        if full_app.kernel.completion_task_count_for_test() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    println!(
        "M2_COMPLETION_WARM_NO_MATCH max_poll_us={:.3}",
        max_no_match_poll.as_secs_f64() * 1_000_000.0,
    );
    assert!(max_no_match_poll < Duration::from_micros(500));
    assert_eq!(full_app.kernel.completion_task_count_for_test(), 0);
    full_app.kernel.cancel();

    let source = CompletionSourceDefinition::new(
        CompletionSourceId::new("hot-path").unwrap(),
        Duration::ZERO,
        Duration::from_secs(10),
        |_, _, _| Box::pin(std::future::pending()),
    )
    .unwrap();
    let mut app = make_completion_app(vec![source]);
    let view = view_id(&app, app.session.focused());
    let query = "a".repeat(4 * 1024);
    app.execute_command(DispatchCommand::ContentWithView {
        command: crate::command::ContentCommand::Edit(EditCommand::InsertText(query.clone())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert!(app.trigger_completion_for_view(view, CompletionTrigger::Manual));
    let snapshot = app.session.completion().snapshot(view).unwrap();
    let key = snapshot.request.source_key(snapshot.sources[0].clone());
    let items = (0..100)
        .map(|_| CompletionItem::new(query.clone(), query.clone()))
        .collect();
    app.handle_app_message(AppMessage::CompletionBatchForTest(
        CompletionBatch::replace(
            key,
            SourceBatchVersion(1),
            items,
            true,
            IncompleteDirections::default(),
        ),
    ))
    .unwrap();
    assert_eq!(
        app.session
            .completion()
            .snapshot(view)
            .unwrap()
            .candidates
            .len(),
        100
    );
    let mut fallback_samples = Vec::with_capacity(SAMPLES);
    let mut navigation_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        assert!(
            !app.handle_completion_interaction(KeyEvent::char('z'))
                .unwrap()
        );
        fallback_samples.push(started.elapsed());
        let started = Instant::now();
        assert!(
            app.handle_completion_interaction(KeyEvent::ctrl('n'))
                .unwrap()
        );
        navigation_samples.push(started.elapsed());
    }
    fallback_samples.sort_unstable();
    navigation_samples.sort_unstable();
    let p99_fallback = fallback_samples[(SAMPLES * 99).div_ceil(100) - 1];
    let p99_navigation = navigation_samples[(SAMPLES * 99).div_ceil(100) - 1];
    println!(
        "M2_COMPLETION_INTERACTION p99_fallback_us={:.3} p99_navigation_us={:.3}",
        p99_fallback.as_secs_f64() * 1_000_000.0,
        p99_navigation.as_secs_f64() * 1_000_000.0,
    );
    assert!(p99_fallback < Duration::from_micros(500));
    assert!(p99_navigation < Duration::from_micros(500));
    app.kernel.cancel();
}
