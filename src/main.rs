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
use tui::TuiFrontend;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str());

    let (width, height) = term_size().unwrap_or((80, 24));
    let frontend = TuiFrontend::new(Output::new(io::BufWriter::new(io::stdout())));
    let mut app = App::new_configured(path, width as usize, height as usize, frontend)?;
    let _guard = TerminalGuard::enter()?;
    app.run().await?;
    Ok(())
}
