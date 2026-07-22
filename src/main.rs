use vell_app::App;
use vell_plugin_v8::load_user_configuration;
use vell_protocol::content_query::ThemeName;
use vell_tui::TuiFrontend;
use vell_tui::terminal::lifecycle::TerminalGuard;
use vell_tui::terminal::output::Output;
use vell_tui::terminal::size as term_size;

use std::io;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut theme = None;
    while let Some(argument) = args.next() {
        if argument == "--theme" {
            theme = Some(args.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--theme requires a name")
            })?);
        } else if path.replace(argument).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only one file path may be opened",
            ));
        }
    }

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = TuiFrontend::new(Output::new(io::BufWriter::new(io::stdout())));
    let configuration = load_user_configuration().map_err(io::Error::other)?;
    let theme = theme.map(ThemeName::new).or(configuration.theme);
    let mut app = App::with_modes_and_visuals(
        path.as_deref(),
        width as usize,
        height as usize,
        frontend,
        configuration.modes,
        theme,
        configuration.face_overrides,
    )?;
    let _guard = TerminalGuard::enter()?;
    app.run().await?;
    Ok(())
}
