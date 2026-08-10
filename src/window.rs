use std::time::Duration;

use calloop::{RegistrationToken, timer::Timer};
use smithay_client_toolkit::{
    compositor::CompositorState,
    seat::keyboard::Modifiers,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
    },
};
use vello::{Scene, kurbo::Affine};
use wayland_client::{Connection, QueueHandle, protocol::wl_output::WlOutput};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

use crate::{
    app::App,
    handler::{
        KeyPress, PointerButton, RawSurfaceHandler, ScrollDelta, SurfaceCtx, SurfaceHandler,
    },
    renderer::{RawRenderer, Renderer, SharedGpu},
    text::TextContext,
    wayland::background_effect::BackgroundEffect,
};

pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_millis(1000);

pub struct WindowConfig {
    pub namespace: String,
    pub layer: Layer,
    pub anchor: Anchor,
    pub size: (u32, u32),
    pub exclusive_zone: i32,
    pub keyboard_interactivity: KeyboardInteractivity,
    pub blur: bool,
    /// Whether crownshell keeps the blur region equal to the whole surface.
    ///
    /// Set this to `false` when the surface is only partly opaque — a popup
    /// that occupies a corner of a full-screen surface, say — and drive the
    /// region yourself with [`SurfaceCtx::set_blur_region`]. Ignored unless
    /// [`blur`](Self::blur) is set.
    ///
    /// [`SurfaceCtx::set_blur_region`]: crate::SurfaceCtx::set_blur_region
    pub auto_blur_region: bool,
    pub tick_interval: Option<Duration>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            namespace: "crownshell".to_string(),
            layer: Layer::Top,
            anchor: Anchor::empty(),
            size: (0, 0),
            exclusive_zone: 0,
            keyboard_interactivity: KeyboardInteractivity::None,
            blur: false,
            auto_blur_region: true,
            tick_interval: None,
        }
    }
}

/// The parts specific to a window driven by a [`RawSurfaceHandler`].
pub(crate) struct RawWindow {
    pub handler: Box<dyn RawSurfaceHandler>,
    // Must drop before `Window::layer`: it holds raw pointers into wl_surface.
    pub renderer: Option<RawRenderer>,
}

/// Placeholder installed in [`Window::handler`] for raw windows, so the
/// pointer and drag-and-drop plumbing keeps working on a single handler type.
struct NoopSurfaceHandler;

impl SurfaceHandler for NoopSurfaceHandler {
    fn paint(&mut self, _scene: &mut Scene, _ctx: SurfaceCtx<'_>) {}
}

pub struct Window {
    // Renderer must drop before `layer`: it holds raw pointers into wl_surface.
    pub renderer: Option<Renderer>,
    pub(crate) raw: Option<RawWindow>,
    pub layer: LayerSurface,
    pub bg_effect_surface: Option<ExtBackgroundEffectSurfaceV1>,
    pub handler: Box<dyn SurfaceHandler>,
    pub scene: Scene,
    /// Scratch scene holding `scene` with the buffer scale applied. Unused
    /// while the scale is 1.
    scaled_scene: Scene,
    /// Surface size in logical pixels, as the compositor configured it.
    pub width: u32,
    pub height: u32,
    /// `wl_surface` buffer scale: how many physical pixels make up one logical
    /// pixel. Always at least 1.
    pub scale: i32,
    pub first_configure: bool,
    pub frame_pending: bool,
    pub tick_timer: Option<RegistrationToken>,
    /// The output this window was explicitly created on, if any.
    pub(crate) output: Option<WlOutput>,
    config: WindowConfig,
}

macro_rules! ctx {
    ($self:ident, $compositor_state:expr, $qh:expr, $text_cx:expr) => {{
        // The context is shared between windows, which may sit on outputs of
        // different densities. Point it at this surface before the handler can
        // lay any text out — measuring in a pointer callback has to agree with
        // what painting will produce.
        $text_cx.set_scale($self.scale as f32);
        SurfaceCtx {
            size: ($self.width, $self.height),
            scale: $self.scale as f64,
            compositor_state: $compositor_state,
            layer: &$self.layer,
            bg_effect_surface: $self.bg_effect_surface.as_ref(),
            qh: $qh,
            // Reborrow so the caller can keep using its `&mut TextContext`
            // after the handler returns.
            text: &mut *$text_cx,
        }
    }};
}

impl Window {
    pub fn new(
        config: WindowConfig,
        handler: Box<dyn SurfaceHandler>,
        compositor_state: &CompositorState,
        layer_shell: &LayerShell,
        background_effect: Option<&BackgroundEffect>,
        qh: &QueueHandle<App>,
        output: Option<&WlOutput>,
    ) -> Window {
        Self::build(
            config,
            handler,
            None,
            compositor_state,
            layer_shell,
            background_effect,
            qh,
            output,
        )
    }

    pub(crate) fn new_raw(
        config: WindowConfig,
        handler: Box<dyn RawSurfaceHandler>,
        compositor_state: &CompositorState,
        layer_shell: &LayerShell,
        background_effect: Option<&BackgroundEffect>,
        qh: &QueueHandle<App>,
        output: Option<&WlOutput>,
    ) -> Window {
        Self::build(
            config,
            Box::new(NoopSurfaceHandler),
            Some(RawWindow {
                handler,
                renderer: None,
            }),
            compositor_state,
            layer_shell,
            background_effect,
            qh,
            output,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        config: WindowConfig,
        handler: Box<dyn SurfaceHandler>,
        raw: Option<RawWindow>,
        compositor_state: &CompositorState,
        layer_shell: &LayerShell,
        background_effect: Option<&BackgroundEffect>,
        qh: &QueueHandle<App>,
        output: Option<&WlOutput>,
    ) -> Window {
        let surface = compositor_state.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            config.layer,
            Some(config.namespace.as_str()),
            output,
        );

        let bg_effect_surface = if config.blur {
            background_effect.map(|bg| bg.manager.get_background_effect(layer.wl_surface(), qh, ()))
        } else {
            None
        };

        let (initial_w, initial_h) = config.size;
        let window = Self {
            renderer: None,
            raw,
            layer,
            bg_effect_surface,
            handler,
            scene: Scene::new(),
            scaled_scene: Scene::new(),
            width: initial_w,
            height: initial_h,
            scale: 1,
            first_configure: true,
            frame_pending: false,
            tick_timer: None,
            output: output.cloned(),
            config,
        };

        window.apply_layer();
        window
    }

    fn apply_layer(&self) {
        self.layer.set_anchor(self.config.anchor);
        self.layer.set_size(self.config.size.0, self.config.size.1);
        self.layer.set_exclusive_zone(self.config.exclusive_zone);
        self.layer
            .set_keyboard_interactivity(self.config.keyboard_interactivity);
        self.layer.commit();
    }

    pub fn wants_blur(&self) -> bool {
        self.config.blur
    }

    /// Whether crownshell should keep this surface's blur region in sync with
    /// its size, as opposed to the handler owning the region.
    pub fn wants_auto_blur_region(&self) -> bool {
        self.config.blur && self.config.auto_blur_region
    }

    /// The output this window was explicitly created on, if any.
    ///
    /// `None` for windows created with [`App::create_window`] or
    /// [`App::create_raw_window`], where the compositor picks the output.
    ///
    /// [`App::create_window`]: crate::App::create_window
    /// [`App::create_raw_window`]: crate::App::create_raw_window
    pub fn output(&self) -> Option<&WlOutput> {
        self.output.as_ref()
    }

    /// Whether the handler is asking to repaint for a reason of its own.
    pub fn needs_redraw(&self) -> bool {
        match &self.raw {
            Some(raw) => raw.handler.needs_redraw(),
            None => self.handler.needs_redraw(),
        }
    }

    /// Creates this window's GPU renderer if it does not exist yet. Called on
    /// configure, so windows created at any point in the app's life — setup or
    /// output hotplug — get their renderer as soon as the surface has a size.
    pub(crate) fn ensure_renderer(
        &mut self,
        connection: &Connection,
        raw_gpu: &mut Option<SharedGpu>,
        qh: &QueueHandle<App>,
    ) -> anyhow::Result<()> {
        if let Some(raw) = &self.raw {
            if raw.renderer.is_some() {
                return Ok(());
            }
            let gpu = raw_gpu
                .get_or_insert_with(crate::renderer::new_shared_gpu)
                .clone();
            let renderer = RawRenderer::new(gpu, connection, &self.layer, self.physical_size())?;
            let scale = self.scale as f64;
            let raw = self.raw.as_mut().expect("checked above");
            renderer.with_ctx(scale, &self.layer, qh, |ctx| raw.handler.setup(ctx));
            raw.renderer = Some(renderer);
        } else if self.renderer.is_none() {
            self.renderer = Some(Renderer::new(connection, self)?);
        }
        Ok(())
    }

    /// Size of the underlying buffer in physical pixels.
    pub fn physical_size(&self) -> (u32, u32) {
        let scale = self.scale.max(1) as u32;
        (self.width * scale, self.height * scale)
    }

    /// Adopts a new `wl_surface` buffer scale, resizing the buffer to match.
    ///
    /// Returns `true` if the scale actually changed, in which case the caller
    /// should request a frame — the new buffer size only reaches the
    /// compositor on the next commit.
    pub fn set_scale(&mut self, scale: i32) -> bool {
        let scale = scale.max(1);
        if self.scale == scale {
            return false;
        }
        self.scale = scale;
        self.layer.wl_surface().set_buffer_scale(scale);
        let (physical_w, physical_h) = self.physical_size();
        log::debug!(
            "buffer scale {scale}: {}x{} logical, {physical_w}x{physical_h} physical",
            self.width,
            self.height
        );
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(physical_w, physical_h);
        }
        if let Some(renderer) = self.raw.as_mut().and_then(|raw| raw.renderer.as_mut()) {
            renderer.resize(physical_w, physical_h);
        }
        true
    }

    pub fn paint(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        if let Some(RawWindow { handler, renderer }) = self.raw.as_mut() {
            if let Some(renderer) = renderer.as_mut() {
                renderer.render(handler.as_mut(), self.scale as f64, &self.layer, qh);
            }
            return;
        }

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        self.scene.reset();
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        self.handler.paint(&mut self.scene, ctx);

        // Handlers draw in logical pixels; the buffer is physical.
        let scene = if self.scale <= 1 {
            &self.scene
        } else {
            self.scaled_scene.reset();
            self.scaled_scene
                .append(&self.scene, Some(Affine::scale(self.scale as f64)));
            &self.scaled_scene
        };

        // `blur_sigma` is in logical pixels and the buffer is physical, so the
        // blur widens with the output's density exactly as the drawing does.
        let blur_sigma = self.handler.blur_sigma() * self.scale.max(1) as f32;

        if let Err(e) = renderer.render(scene, blur_sigma) {
            log::error!("render failed: {e}");
        }
    }

    /// Resizes the surface. `width` and `height` are in logical pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let (physical_w, physical_h) = self.physical_size();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(physical_w, physical_h);
        }
        if let Some(renderer) = self.raw.as_mut().and_then(|raw| raw.renderer.as_mut()) {
            renderer.resize(physical_w, physical_h);
        }
    }

    pub fn request_frame(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        // Before the first configure the surface may not be committed with a
        // buffer, and an early frame request would never fire and wedge
        // `frame_pending`. Happens when the app-wide tick hits a window that
        // was just hotplugged.
        if self.first_configure || self.frame_pending {
            return;
        }
        self.frame_pending = true;
        let surface = self.layer.wl_surface().clone();
        surface.frame(qh, surface.clone());
        self.paint(compositor_state, qh, text_cx);
    }

    pub fn on_frame(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        self.frame_pending = false;
        let redraw = if self.raw.is_some() {
            self.raw_callback(qh, |handler, ctx| handler.on_frame(ctx))
        } else {
            let ctx = ctx!(self, compositor_state, qh, text_cx);
            self.handler.on_frame(ctx)
        };
        if redraw {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_tick(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let redraw = if self.raw.is_some() {
            self.raw_callback(qh, |handler, ctx| handler.on_tick(ctx))
        } else {
            let ctx = ctx!(self, compositor_state, qh, text_cx);
            self.handler.on_tick(ctx)
        };
        if redraw {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    /// Invokes a [`RawSurfaceHandler`] callback with a fresh context. Returns
    /// `false` when the GPU surface does not exist yet.
    fn raw_callback(
        &mut self,
        qh: &QueueHandle<App>,
        f: impl FnOnce(&mut dyn RawSurfaceHandler, crate::handler::RawSurfaceCtx<'_>) -> bool,
    ) -> bool {
        let scale = self.scale as f64;
        let layer = &self.layer;
        let Some(RawWindow {
            handler,
            renderer: Some(renderer),
        }) = self.raw.as_mut()
        else {
            return false;
        };
        renderer.with_ctx(scale, layer, qh, |ctx| f(handler.as_mut(), ctx))
    }

    pub fn on_pointer_enter(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_pointer_enter(x, y, ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_pointer_leave(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_pointer_leave(ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_pointer_motion(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_pointer_motion(x, y, ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_pointer_press(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        self.on_pointer_button(
            x,
            y,
            PointerButton::Left,
            true,
            compositor_state,
            qh,
            text_cx,
        );
    }

    pub fn on_pointer_release(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        self.on_pointer_button(
            x,
            y,
            PointerButton::Left,
            false,
            compositor_state,
            qh,
            text_cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn on_pointer_button(
        &mut self,
        x: f64,
        y: f64,
        button: PointerButton,
        pressed: bool,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_pointer_button(x, y, button, pressed, ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_pointer_scroll(
        &mut self,
        x: f64,
        y: f64,
        delta: ScrollDelta,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_pointer_scroll(x, y, delta, ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    /// Delivers a key event, pressed or released, to this window's handler.
    pub(crate) fn on_key(
        &mut self,
        key: KeyPress<'_>,
        pressed: bool,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        let redraw = if pressed {
            self.handler.on_key_press(key, ctx)
        } else {
            self.handler.on_key_release(key, ctx)
        };
        if redraw {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_modifiers(
        &mut self,
        mods: Modifiers,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_modifiers(mods, ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_keyboard_enter(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_keyboard_enter(ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn on_keyboard_leave(
        &mut self,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
        text_cx: &mut TextContext,
    ) {
        let ctx = ctx!(self, compositor_state, qh, text_cx);
        if self.handler.on_keyboard_leave(ctx) {
            self.request_frame(compositor_state, qh, text_cx);
        }
    }

    pub fn tick_interval(&self) -> Duration {
        self.config.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    pub fn build_tick_timer(&self) -> Timer {
        Timer::from_duration(self.tick_interval())
    }

    pub fn apply_blur_region(&self, compositor_state: &CompositorState) {
        use smithay_client_toolkit::compositor::Region;
        let Some(effect_surface) = self.bg_effect_surface.as_ref() else {
            return;
        };
        let Ok(region) = Region::new(compositor_state) else {
            return;
        };
        region.add(0, 0, self.width as i32, self.height as i32);
        effect_surface.set_blur_region(Some(region.wl_region()));
        self.layer.commit();
    }
}
