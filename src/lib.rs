pub mod app;
pub mod handler;
pub mod predule;
pub mod renderer;
pub mod window;

mod wayland;

use anyhow::{anyhow, Result};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;

pub use app::App;
pub use handler::{DragOffer, DropPayload, SurfaceCtx, SurfaceHandler};
pub use renderer::Renderer;
pub use window::{Window, WindowConfig, DEFAULT_TICK_INTERVAL};

pub use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
pub use vello::{self, peniko, Scene};

pub fn run<F>(setup: F) -> Result<()>
where
    F: FnOnce(&mut App) -> Result<()>,
{
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let (connection, mut event_queue, mut app) = App::try_new(loop_handle.clone())?;

    setup(&mut app)?;

    if app.windows.is_empty() {
        return Err(anyhow!("run: setup did not create any windows"));
    }

    while !app.all_configured() {
        event_queue.blocking_dispatch(&mut app)?;
    }

    for window in &mut app.windows {
        let renderer = Renderer::new(&connection, window)?;
        window.renderer = Some(renderer);
    }
    app.apply_blur_regions();
    app.paint_all();
    app.arm_tick_timer();

    WaylandSource::new(connection, event_queue)
        .insert(loop_handle.clone())
        .map_err(|e| anyhow!("register wayland source: {}", e.error))?;

    let signal = event_loop.get_signal();
    event_loop.run(None, &mut app, move |app| {
        if app.exit {
            signal.stop();
        }
    })?;

    Ok(())
}
