use std::time::Duration;

use calloop::{timer::Timer, RegistrationToken};
use smithay_client_toolkit::{
    compositor::CompositorState,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
        WaylandSurface,
    },
};
use vello::Scene;
use wayland_client::{protocol::wl_output::WlOutput, QueueHandle};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

use crate::{
    app::App,
    handler::{SurfaceCtx, SurfaceHandler},
    renderer::Renderer,
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
            tick_interval: None,
        }
    }
}

pub struct Window {
    // Renderer must drop before `layer`: it holds raw pointers into wl_surface.
    pub renderer: Option<Renderer>,
    pub layer: LayerSurface,
    pub bg_effect_surface: Option<ExtBackgroundEffectSurfaceV1>,
    pub handler: Box<dyn SurfaceHandler>,
    pub scene: Scene,
    pub width: u32,
    pub height: u32,
    pub first_configure: bool,
    pub frame_pending: bool,
    pub tick_timer: Option<RegistrationToken>,
    config: WindowConfig,
}

macro_rules! ctx {
    ($self:ident, $compositor_state:expr, $qh:expr) => {
        SurfaceCtx {
            size: ($self.width, $self.height),
            compositor_state: $compositor_state,
            layer: &$self.layer,
            bg_effect_surface: $self.bg_effect_surface.as_ref(),
            qh: $qh,
        }
    };
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
        let surface = compositor_state.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            config.layer,
            Some(config.namespace.as_str()),
            output,
        );

        let bg_effect_surface = if config.blur {
            background_effect
                .map(|bg| bg.manager.get_background_effect(layer.wl_surface(), qh, ()))
        } else {
            None
        };

        let (initial_w, initial_h) = config.size;
        let window = Self {
            renderer: None,
            layer,
            bg_effect_surface,
            handler,
            scene: Scene::new(),
            width: initial_w,
            height: initial_h,
            first_configure: true,
            frame_pending: false,
            tick_timer: None,
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

    pub fn paint(&mut self, compositor_state: &CompositorState, qh: &QueueHandle<App>) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        self.scene.reset();
        let ctx = ctx!(self, compositor_state, qh);
        self.handler.paint(&mut self.scene, ctx);
        if let Err(e) = renderer.render(&self.scene) {
            log::error!("render failed: {e}");
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(width, height);
        }
    }

    pub fn request_frame(&mut self, compositor_state: &CompositorState, qh: &QueueHandle<App>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        let surface = self.layer.wl_surface().clone();
        surface.frame(qh, surface.clone());
        self.paint(compositor_state, qh);
    }

    pub fn on_frame(&mut self, compositor_state: &CompositorState, qh: &QueueHandle<App>) {
        self.frame_pending = false;
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_frame(ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_tick(&mut self, compositor_state: &CompositorState, qh: &QueueHandle<App>) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_tick(ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_pointer_enter(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
    ) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_pointer_enter(x, y, ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_pointer_leave(&mut self, compositor_state: &CompositorState, qh: &QueueHandle<App>) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_pointer_leave(ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_pointer_motion(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
    ) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_pointer_motion(x, y, ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_pointer_press(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
    ) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_pointer_press(x, y, ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn on_pointer_release(
        &mut self,
        x: f64,
        y: f64,
        compositor_state: &CompositorState,
        qh: &QueueHandle<App>,
    ) {
        let ctx = ctx!(self, compositor_state, qh);
        if self.handler.on_pointer_release(x, y, ctx) {
            self.request_frame(compositor_state, qh);
        }
    }

    pub fn tick_interval(&self) -> Duration {
        self.config.tick_interval.unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    pub fn build_tick_timer(&self) -> Timer {
        Timer::from_duration(self.tick_interval())
    }

    pub fn apply_blur_region(
        &self,
        compositor_state: &CompositorState,
        background_effect: Option<&BackgroundEffect>,
    ) {
        use smithay_client_toolkit::compositor::Region;
        let Some(effect_surface) = self.bg_effect_surface.as_ref() else {
            return;
        };
        let Some(bg) = background_effect else {
            return;
        };
        if !bg.supports_blur() {
            return;
        }
        let Ok(region) = Region::new(compositor_state) else {
            return;
        };
        region.add(0, 0, self.width as i32, self.height as i32);
        effect_surface.set_blur_region(Some(region.wl_region()));
        self.layer.commit();
    }
}
