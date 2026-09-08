// Drawing helpers legitimately take many positional parameters.
#![allow(clippy::too_many_arguments)]

mod app;
mod config;
mod deck;
mod effects;
mod font;
mod keys;
mod menu;
mod palette;
mod renderer;
mod terminal;
mod theme;

use anyhow::Result;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::App;
use crate::config::{CommandStore, Config};
use crate::terminal::UserEvent;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("kindterm=info,wgpu_core=warn,wgpu_hal=warn"))
        .init();

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
