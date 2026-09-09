// Drawing helpers legitimately take many positional parameters.
#![allow(clippy::too_many_arguments)]

mod app;
mod canvas;
mod config;
mod control;
mod deck;
mod effects;
mod font;
mod host;
mod keys;
mod mcp;
mod menu;
mod palette;
mod renderer;
mod session;
mod terminal;
mod theme;

use anyhow::Result;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::App;
use crate::config::{CommandStore, Config};
use crate::terminal::UserEvent;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("kindlyterm=info,wgpu_core=warn,wgpu_hal=warn"))
        .init();

    // Subcommands that never open a window.
    let mut argv = std::env::args();
    let _exe = argv.next();
    match argv.next().as_deref() {
        Some("--host") => return host::run(host::HostArgs::parse(argv)?),
        Some("--sessions") => return host::sessions_cli(argv.any(|a| a == "--prune")),
        Some("--mcp") => return mcp::run(),
        Some("--help") | Some("-h") => {
            println!("kindlyterm [--sessions [--prune]] [--mcp]\n\n  --sessions   list detached shell sessions (add --prune to drop dead ones)\n  --mcp        stdio MCP server for Claude Code and friends (needs the control API on in the Deck)");
            return Ok(());
        }
        _ => {}
    }

    let config = Config::load();
    let store = CommandStore::load();
    log::info!(
        "config: {} ({} saved commands)",
        Config::path().display(),
        store.commands.len()
    );

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, store, proxy);
    event_loop.run_app(&mut app)?;
    Ok(())
}
