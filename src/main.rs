mod app;
mod core;
mod frontend;
mod protocol;
mod terminal;
mod tui;

use std::io;

use app::App;
use crossterm::terminal::size as term_size;
use terminal::lifecycle::TerminalGuard;
use terminal::output::Output;
use tui::tui_frontend::TuiFrontend;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let _guard = TerminalGuard::enter()?;

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = TuiFrontend::new(Output::new(io::stdout()));
    let mut app = App::new(path, width as usize, height as usize, frontend)?;
    app.run().await?;
    Ok(())
}
