use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    seat::{
        keyboard::{Keysym, Modifiers},
        pointer::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{KeyboardInteractivity, LayerSurface},
    },
};
use vello::{Scene, kurbo::Rect, wgpu};
use wayland_client::QueueHandle;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

use crate::{app::App, text::TextContext};

pub struct SurfaceCtx<'a> {
    /// Surface size in logical pixels — the space `paint` draws in.
    pub size: (u32, u32),
    /// Physical pixels per logical pixel on the output this surface is on.
    ///
    /// You rarely need this: crownshell scales the whole scene for you, and
    /// [`Text`] already rasterises at this density. It is here for the cases
    /// that genuinely care, such as snapping to the physical pixel grid.
    ///
    /// [`Text`]: crate::Text
    pub scale: f64,
    pub compositor_state: &'a CompositorState,
    pub layer: &'a LayerSurface,
    pub bg_effect_surface: Option<&'a ExtBackgroundEffectSurfaceV1>,
    pub qh: &'a QueueHandle<App>,
    /// Shared font database and layout caches, for use with [`Text`].
    ///
    /// [`Text`]: crate::Text
    pub text: &'a mut TextContext,
}

impl SurfaceCtx<'_> {
    /// Restricts which parts of the surface accept pointer input.
    ///
    /// Rectangles are in logical, surface-local pixels. An empty slice makes
    /// the whole surface click-through, which is how a popup surface that is
    /// kept alive while hidden stays out of the way of what is underneath it.
    ///
    /// The region is double-buffered state: it takes effect at the next commit,
    /// which is the commit the following paint performs. Call it from `paint`
    /// alongside the drawing it belongs to and the two stay in step.
    pub fn set_input_region(&self, rects: &[Rect]) {
        let Some(region) = self.region(rects) else {
            return;
        };
        self.layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
    }

    /// Restricts which parts of the surface have their backdrop blurred.
    ///
    /// Rectangles are in logical, surface-local pixels; an empty slice removes
    /// the effect. Does nothing unless the window was created with
    /// [`WindowConfig::blur`]. Set [`WindowConfig::auto_blur_region`] to
    /// `false` if you drive the region yourself from here.
    ///
    /// Like the input region, this applies at the next commit.
    ///
    /// [`WindowConfig::blur`]: crate::WindowConfig::blur
    /// [`WindowConfig::auto_blur_region`]: crate::WindowConfig::auto_blur_region
    pub fn set_blur_region(&self, rects: &[Rect]) {
        let Some(effect) = self.bg_effect_surface else {
            return;
        };
        if rects.is_empty() {
            effect.set_blur_region(None);
            return;
        }
        let Some(region) = self.region(rects) else {
            return;
        };
        effect.set_blur_region(Some(region.wl_region()));
    }

    /// Changes how the compositor routes keyboard focus to this surface.
    ///
    /// Like the region setters this is double-buffered state, so it lands with
    /// the commit the next paint performs — call it from `paint`, or follow it
    /// with a repaint, or the compositor will not see it.
    pub fn set_keyboard_interactivity(&self, mode: KeyboardInteractivity) {
        self.layer.set_keyboard_interactivity(mode);
    }

    fn region(&self, rects: &[Rect]) -> Option<Region> {
        let region = Region::new(self.compositor_state).ok()?;
        for rect in rects {
            let x = rect.x0.round() as i32;
            let y = rect.y0.round() as i32;
            let width = rect.x1.round() as i32 - x;
            let height = rect.y1.round() as i32 - y;
            if width > 0 && height > 0 {
                region.add(x, y, width, height);
            }
        }
        Some(region)
    }
}

/// A key going down or coming back up, as delivered to the surface that holds
/// keyboard focus.
pub struct KeyPress<'a> {
    /// The symbol the current keymap produces for this key, with modifiers
    /// applied — match on [`Keysym`] constants such as `Keysym::Return`.
    pub keysym: Keysym,
    /// The evdev scancode, for the rare binding that has to be layout-independent.
    pub raw_code: u32,
    /// The text this key would insert, if any. Always `None` on release.
    pub text: Option<&'a str>,
    /// Modifier state at the time of the event.
    pub modifiers: Modifiers,
    /// Whether this event came from the key being held rather than newly pressed.
    pub repeat: bool,
}

/// A pointer button, named for the three every mouse has and numbered for the
/// rest (side, extra, task, and whatever else the device reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Other(u32),
}

impl PointerButton {
    /// Interprets a raw evdev button code, as carried by `wl_pointer.button`.
    pub fn from_code(code: u32) -> Self {
        match code {
            BTN_LEFT => Self::Left,
            BTN_RIGHT => Self::Right,
            BTN_MIDDLE => Self::Middle,
            other => Self::Other(other),
        }
    }

    /// The raw evdev button code this button came from.
    pub fn code(self) -> u32 {
        match self {
            Self::Left => BTN_LEFT,
            Self::Right => BTN_RIGHT,
            Self::Middle => BTN_MIDDLE,
            Self::Other(code) => code,
        }
    }
}

/// One frame's worth of scrolling.
///
/// `x` / `y` are the continuous delta in logical pixels, which is what a
/// touchpad or a high-resolution wheel reports. `discrete_x` / `discrete_y`
/// count notches and stay zero on continuous devices, so a handler that wants
/// to page or step per notch can tell the two kinds of device apart instead of
/// guessing from the magnitude.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScrollDelta {
    pub x: f64,
    pub y: f64,
    pub discrete_x: i32,
    pub discrete_y: i32,
}

pub struct DragOffer<'a> {
    pub mime_types: &'a [String],
    pub x: f64,
    pub y: f64,
}

pub struct DropPayload {
    pub mime_type: String,
    pub data: Vec<u8>,
    pub x: f64,
    pub y: f64,
}

pub trait SurfaceHandler {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>);

    fn on_pointer_enter(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_motion(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_press(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_pointer_release(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    /// Any pointer button, pressed or released.
    ///
    /// The default forwards the left button to
    /// [`on_pointer_press`](Self::on_pointer_press) /
    /// [`on_pointer_release`](Self::on_pointer_release) and ignores the rest,
    /// so a handler only overrides this when it wants the other buttons.
    /// Overriding it replaces that forwarding: call the two yourself, or move
    /// their bodies here.
    fn on_pointer_button(
        &mut self,
        x: f64,
        y: f64,
        button: PointerButton,
        pressed: bool,
        ctx: SurfaceCtx<'_>,
    ) -> bool {
        match (button, pressed) {
            (PointerButton::Left, true) => self.on_pointer_press(x, y, ctx),
            (PointerButton::Left, false) => self.on_pointer_release(x, y, ctx),
            _ => false,
        }
    }

    /// A scroll frame, at the pointer's current position.
    fn on_pointer_scroll(
        &mut self,
        _x: f64,
        _y: f64,
        _delta: ScrollDelta,
        _ctx: SurfaceCtx<'_>,
    ) -> bool {
        false
    }

    /// A key went down on the surface holding keyboard focus. Fires again while
    /// the key is held, with [`KeyPress::repeat`] set.
    fn on_key_press(&mut self, _key: KeyPress<'_>, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_key_release(&mut self, _key: KeyPress<'_>, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    /// Modifier state changed. The new state also reaches every subsequent
    /// [`KeyPress`], so this is only worth overriding for handlers that show
    /// the modifiers themselves.
    fn on_modifiers(&mut self, _mods: Modifiers, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_keyboard_enter(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_keyboard_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    /// Gaussian blur applied to the finished frame, in logical pixels.
    /// `0.0` (the default) bypasses the pass entirely.
    ///
    /// Read once per paint, right after [`paint`](Self::paint) returns, so a
    /// handler that animates the blur can set it from the same state it just
    /// drew with. Values at or below `0.05` are treated as zero.
    fn blur_sigma(&self) -> f32 {
        0.0
    }

    /// Return the mime type to accept, or None to reject the drag.
    fn on_drag_enter(&mut self, _offer: DragOffer<'_>, _ctx: SurfaceCtx<'_>) -> Option<String> {
        None
    }
    fn on_drag_motion(&mut self, _x: f64, _y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_drag_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    /// Called after the drop payload has been fully read from the source pipe.
    fn on_drop(&mut self, _drop: DropPayload, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    fn on_tick(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }
    fn on_frame(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        false
    }

    /// Whether this surface has to repaint because of something that did not
    /// arrive as one of its own events.
    ///
    /// Every other callback repaints by returning `true`, which only ever
    /// repaints the surface the event landed on. This one is polled on every
    /// surface after each batch of events, so a handler can repaint in response
    /// to state it shares with someone else — a bar button opening a popup on
    /// another surface, a D-Bus signal, a channel from a worker thread.
    ///
    /// Keep it cheap: it runs often. Returning `true` forever pins the surface
    /// to the compositor's frame clock, so clear the flag once you have drawn
    /// it, normally in [`paint`](Self::paint).
    fn needs_redraw(&self) -> bool {
        false
    }
}

/// What a [`RawSurfaceHandler`] gets to work with in every callback.
///
/// The raw counterpart of [`SurfaceCtx`]: instead of a Vello [`Scene`], the
/// handler drives wgpu directly through `device` and `queue`.
pub struct RawSurfaceCtx<'a> {
    /// Surface size in *physical* pixels — the size of the texture handed to
    /// [`RawSurfaceHandler::render`]. Divide by [`scale`](Self::scale) for
    /// logical pixels.
    pub size: (u32, u32),
    /// Physical pixels per logical pixel on the output this surface is on.
    pub scale: f64,
    /// The device the surface is configured against. Shared by every raw
    /// window in the app, so resources created here can be reused across
    /// outputs.
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    /// The format of the surface texture; render pipelines must target it.
    pub format: wgpu::TextureFormat,
    pub layer: &'a LayerSurface,
    pub qh: &'a QueueHandle<App>,
}

/// A surface handler that renders with its own wgpu pipeline instead of a
/// Vello [`Scene`].
///
/// Raw windows go through the same layer-surface lifecycle as Vello windows:
/// the compositor configures the surface, [`setup`](Self::setup) runs once the
/// GPU surface exists, and redraws are frame-callback driven — a callback
/// returning `true` (or [`needs_redraw`](Self::needs_redraw) reporting `true`)
/// schedules the next [`render`](Self::render).
///
/// Create one with [`App::create_raw_window`] or
/// [`App::create_raw_window_on_output`].
///
/// [`App::create_raw_window`]: crate::App::create_raw_window
/// [`App::create_raw_window_on_output`]: crate::App::create_raw_window_on_output
pub trait RawSurfaceHandler {
    /// Called exactly once, after the surface's first configure, when the wgpu
    /// surface has been created and sized. Build pipelines and static
    /// resources here — `ctx.size` is already the real surface size.
    fn setup(&mut self, _ctx: RawSurfaceCtx<'_>) {}

    /// Renders a frame. Record passes targeting `view` (or copy into
    /// `surface_texture.texture` if the surface supports `COPY_DST`) and
    /// submit them on `ctx.queue`; the texture is presented right after this
    /// returns.
    ///
    /// There is no separate resize callback: compare `ctx.size` against the
    /// last frame's to notice size or scale changes.
    fn render(
        &mut self,
        surface_texture: &wgpu::SurfaceTexture,
        view: &wgpu::TextureView,
        ctx: RawSurfaceCtx<'_>,
    );

    /// Called when a frame callback fires. Return `true` to render another
    /// frame — this is how animations stay on the compositor's frame clock.
    fn on_frame(&mut self, _ctx: RawSurfaceCtx<'_>) -> bool {
        false
    }

    /// Called on the app-wide tick timer. Return `true` to render a frame.
    fn on_tick(&mut self, _ctx: RawSurfaceCtx<'_>) -> bool {
        false
    }

    /// Polled after every batch of events, like
    /// [`SurfaceHandler::needs_redraw`]: return `true` to render in response
    /// to state changed outside this surface's own callbacks (an IPC command,
    /// a worker thread finishing a decode). Clear the flag once drawn.
    fn needs_redraw(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BTN_SIDE: u32 = 0x113;

    #[test]
    fn named_buttons_map_from_their_evdev_codes() {
        assert_eq!(PointerButton::from_code(BTN_LEFT), PointerButton::Left);
        assert_eq!(PointerButton::from_code(BTN_RIGHT), PointerButton::Right);
        assert_eq!(PointerButton::from_code(BTN_MIDDLE), PointerButton::Middle);
    }

    #[test]
    fn unnamed_buttons_keep_their_code() {
        assert_eq!(
            PointerButton::from_code(BTN_SIDE),
            PointerButton::Other(BTN_SIDE)
        );
        assert_eq!(PointerButton::from_code(0), PointerButton::Other(0));
    }

    #[test]
    fn button_code_round_trips() {
        for code in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, 0xffff] {
            assert_eq!(PointerButton::from_code(code).code(), code);
        }
    }
}
