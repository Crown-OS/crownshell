pub mod animations;
pub mod app;
mod blur;
pub mod handler;
pub mod predule;
pub mod renderer;
pub mod text;
pub mod window;

mod wayland;

use anyhow::{Result, anyhow};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;

pub use animations::{Clock, Spring, SpringProfile};
pub use app::App;
pub use handler::{
    DragOffer, DropPayload, KeyPress, PointerButton, RawSurfaceCtx, RawSurfaceHandler, ScrollDelta,
    SurfaceCtx, SurfaceHandler,
};
pub use parley::Alignment;
pub use renderer::Renderer;
pub use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
pub use text::{ColorBrush, Text, TextContext, TextLayout, TextStyle, draw_layout};
pub use window::{DEFAULT_TICK_INTERVAL, Window, WindowConfig};

pub use calloop;
pub use parley;
pub use smithay_client_toolkit::output::OutputInfo;
pub use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
pub use vello::{self, Scene, kurbo, peniko, wgpu};
pub use wayland_client::{self, protocol::wl_output::WlOutput};

pub fn run<F>(setup: F) -> Result<()>
where
    F: FnOnce(&mut App) -> Result<()>,
{
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let (connection, mut event_queue, mut app) = App::try_new(loop_handle.clone())?;

    setup(&mut app)?;

    // An app may start with no windows if it creates them from output hooks —
    // the hooks fire for the outputs present at startup too.
    if app.windows.is_empty() && !app.has_output_hooks() {
        return Err(anyhow!(
            "run: setup did not create any windows or register output hooks"
        ));
    }

    // Renderers are created per window on its first configure, so windows
    // created later — by an output hook, on hotplug — work the same way as
    // the ones from setup.
    while !app.windows.is_empty() && !app.all_configured() {
        event_queue.blocking_dispatch(&mut app)?;
    }
    if let Some(e) = app.init_error.take() {
        return Err(e);
    }

    app.apply_blur_regions();
    app.paint_all();
    app.arm_tick_timer();

    WaylandSource::new(connection, event_queue)
        .insert(loop_handle.clone())
        .map_err(|e| anyhow!("register wayland source: {}", e.error))?;

    let signal = event_loop.get_signal();
    event_loop.run(None, &mut app, move |app| {
        app.flush_redraws();
        if app.exit || app.init_error.is_some() {
            signal.stop();
        }
    })?;

    if let Some(e) = app.init_error.take() {
        return Err(e);
    }
    Ok(())
}
