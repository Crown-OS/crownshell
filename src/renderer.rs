use std::{ffi::c_void, num::NonZeroUsize, ptr::NonNull};

use anyhow::{anyhow, Result};
use smithay_client_toolkit::shell::WaylandSurface;
use vello::{
    peniko::Color,
    util::{RenderContext, RenderSurface},
    wgpu::{
        self,
        rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle},
        CompositeAlphaMode, CurrentSurfaceTexture, PresentMode, SurfaceTargetUnsafe,
    },
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions, Scene,
};
use wayland_client::{Connection, Proxy};

use crate::window::Window;

pub struct Renderer {
    context: RenderContext,
    renderer: VelloRenderer,
    surface: RenderSurface<'static>,
}

impl Renderer {
    pub fn new(connection: &Connection, window: &Window) -> Result<Self> {
        let mut context = RenderContext::new();

        let display_ptr = connection.backend().display_ptr() as *mut c_void;
        let surface_ptr = window.layer.wl_surface().id().as_ptr() as *mut c_void;

        let display_handle = NonNull::new(display_ptr)
            .map(WaylandDisplayHandle::new)
            .ok_or_else(|| anyhow!("wl_display pointer is null"))?;
        let window_handle = NonNull::new(surface_ptr)
            .map(WaylandWindowHandle::new)
            .ok_or_else(|| anyhow!("wl_surface pointer is null"))?;

        let target = SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(display_handle)),
            raw_window_handle: RawWindowHandle::Wayland(window_handle),
        };

        // SAFETY: wl_display lives as long as `Connection`, wl_surface lives as long
        // as `Window`. Both must outlive the Renderer, enforced by drop order in Window.
        let wgpu_surface = unsafe { context.instance.create_surface_unsafe(target)? };

        let mut surface = pollster::block_on(context.create_render_surface(
            wgpu_surface,
            window.width.max(1),
            window.height.max(1),
            PresentMode::AutoVsync,
        ))
        .map_err(|e| anyhow!("create_render_surface: {e}"))?;

        let alpha_caps = surface
            .surface
            .get_capabilities(context.devices[surface.dev_id].adapter())
            .alpha_modes;
        let alpha_mode = [
            CompositeAlphaMode::PreMultiplied,
            CompositeAlphaMode::PostMultiplied,
            CompositeAlphaMode::Inherit,
        ]
        .into_iter()
        .find(|m| alpha_caps.contains(m))
        .unwrap_or(CompositeAlphaMode::Auto);
        surface.config.alpha_mode = alpha_mode;
        context.configure_surface(&surface);

        let device = &context.devices[surface.dev_id].device;
        let renderer = VelloRenderer::new(
            device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| anyhow!("Renderer::new: {e}"))?;

        Ok(Self {
            context,
            renderer,
            surface,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.surface.config.width == width && self.surface.config.height == height {
            return;
        }
        self.context
            .resize_surface(&mut self.surface, width, height);
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.surface.config.width, self.surface.config.height)
    }

    pub fn render(&mut self, scene: &Scene) -> Result<()> {
        let Some(surface_texture) = self.acquire() else {
            return Ok(());
        };

        let device_handle = &self.context.devices[self.surface.dev_id];
        self.renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                scene,
                &self.surface.target_view,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: self.surface.config.width,
                    height: self.surface.config.height,
                    antialiasing_method: AaConfig::Msaa16,
                },
            )
            .map_err(|e| anyhow!("render_to_texture: {e}"))?;

        let surface_view = surface_texture.texture.create_view(&Default::default());
        let mut encoder = device_handle
            .device
            .create_command_encoder(&Default::default());
        self.surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &self.surface.target_view,
            &surface_view,
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
        Ok(())
    }

    fn acquire(&mut self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) | CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.context.configure_surface(&self.surface);
                None
            }
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Validation => None,
        }
    }
}
