mod app;

use modeleaf_core as core;
use modeleaf_frontend as frontend;
use modeleaf_plugin_v8::load_user_modes;
use modeleaf_protocol as protocol;
use modeleaf_tui::TuiFrontend;
use modeleaf_tui::terminal::lifecycle::TerminalGuard;
use modeleaf_tui::terminal::output::Output;
use modeleaf_tui::terminal::size as term_size;

use std::io;

use app::App;
#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = TuiFrontend::new(Output::new(io::BufWriter::new(io::stdout())));
    let modes = load_user_modes().map_err(io::Error::other)?;
    let mut app = App::with_modes(path, width as usize, height as usize, frontend, modes)?;
    let _guard = TerminalGuard::enter()?;
    app.run().await?;
    Ok(())
}
