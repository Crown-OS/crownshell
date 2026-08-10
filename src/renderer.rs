use std::{cell::RefCell, ffi::c_void, num::NonZeroUsize, ptr::NonNull, rc::Rc};

use anyhow::{Result, anyhow};
use smithay_client_toolkit::shell::{WaylandSurface, wlr_layer::LayerSurface};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions, Scene,
    peniko::Color,
    util::{RenderContext, RenderSurface},
    wgpu::{
        self, CompositeAlphaMode, CurrentSurfaceTexture, PresentMode, SurfaceTargetUnsafe,
        rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle},
    },
};
use wayland_client::{Connection, Proxy, QueueHandle};

use crate::{
    app::App,
    blur::{self, Blur},
    handler::{RawSurfaceCtx, RawSurfaceHandler},
    window::Window,
};

/// Compositor alpha modes that interpret the buffer as straight
/// (un-premultiplied) alpha, in order of preference.
const STRAIGHT_ALPHA_MODES: [CompositeAlphaMode; 3] = [
    CompositeAlphaMode::PostMultiplied,
    CompositeAlphaMode::Inherit,
    CompositeAlphaMode::PreMultiplied,
];

fn wayland_surface_target(
    connection: &Connection,
    layer: &LayerSurface,
) -> Result<SurfaceTargetUnsafe> {
    let display_ptr = connection.backend().display_ptr() as *mut c_void;
    let surface_ptr = layer.wl_surface().id().as_ptr() as *mut c_void;

    let display_handle = NonNull::new(display_ptr)
        .map(WaylandDisplayHandle::new)
        .ok_or_else(|| anyhow!("wl_display pointer is null"))?;
    let window_handle = NonNull::new(surface_ptr)
        .map(WaylandWindowHandle::new)
        .ok_or_else(|| anyhow!("wl_surface pointer is null"))?;

    Ok(SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::Wayland(display_handle)),
        raw_window_handle: RawWindowHandle::Wayland(window_handle),
    })
}

pub struct Renderer {
    context: RenderContext,
    renderer: VelloRenderer,
    surface: RenderSurface<'static>,
    /// Post-process blur, built on the first frame that actually asks for it.
    blur: Option<Blur>,
}

impl Renderer {
    pub fn new(connection: &Connection, window: &Window) -> Result<Self> {
        let mut context = RenderContext::new();

        let target = wayland_surface_target(connection, &window.layer)?;

        // SAFETY: wl_display lives as long as `Connection`, wl_surface lives as long
        // as `Window`. Both must outlive the Renderer, enforced by drop order in Window.
        let wgpu_surface = unsafe { context.instance.create_surface_unsafe(target)? };

        // The buffer is sized in physical pixels; the layer surface is
        // configured in logical ones.
        let (physical_w, physical_h) = window.physical_size();
        let mut surface = pollster::block_on(context.create_render_surface(
            wgpu_surface,
            physical_w.max(1),
            physical_h.max(1),
            PresentMode::AutoVsync,
        ))
        .map_err(|e| anyhow!("create_render_surface: {e}"))?;

        let alpha_caps = surface
            .surface
            .get_capabilities(context.devices[surface.dev_id].adapter())
            .alpha_modes;
        // Vello's fine shader writes straight (un-premultiplied) alpha to the
        // render target (fine.wgsl divides rgb by alpha before textureStore),
        // so the compositor must be told to interpret the surface as straight
        // alpha — otherwise even tiny alpha values clamp to fully-lit RGB.
        let alpha_mode = STRAIGHT_ALPHA_MODES
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
            blur: None,
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
        // Resizing replaces the target texture the blur samples from.
        if let Some(blur) = self.blur.as_mut() {
            blur.invalidate();
        }
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.surface.config.width, self.surface.config.height)
    }

    /// Renders `scene` and presents it, optionally through a Gaussian blur.
    ///
    /// `blur_sigma` is in *physical* pixels; anything at or below `0.05` takes
    /// the plain blit path, so an idle surface never touches the blur pipeline.
    pub fn render(&mut self, scene: &Scene, blur_sigma: f32) -> Result<()> {
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
        if blur::is_enabled(blur_sigma) {
            let format = self.surface.config.format;
            let size = (self.surface.config.width, self.surface.config.height);
            let blur = self
                .blur
                .get_or_insert_with(|| Blur::new(&device_handle.device, format));
            // The vertical pass writes the swapchain view directly, replacing
            // the blit rather than following it.
            blur.record(
                &device_handle.device,
                &device_handle.queue,
                &mut encoder,
                &self.surface.target_view,
                &surface_view,
                size,
                blur_sigma,
            );
        } else {
            self.surface.blitter.copy(
                &device_handle.device,
                &mut encoder,
                &self.surface.target_view,
                &surface_view,
            );
        }
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

/// One wgpu instance and device pool shared by every raw window in the app,
/// so per-output surfaces can share GPU resources.
pub(crate) type SharedGpu = Rc<RefCell<RenderContext>>;

pub(crate) fn new_shared_gpu() -> SharedGpu {
    Rc::new(RefCell::new(RenderContext::new()))
}

/// GPU state behind a raw window: a wgpu surface configured against the
/// app-wide shared device, with no Vello renderer in front of it.
pub(crate) struct RawRenderer {
    gpu: SharedGpu,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    dev_id: usize,
}

impl RawRenderer {
    pub(crate) fn new(
        gpu: SharedGpu,
        connection: &Connection,
        layer: &LayerSurface,
        physical_size: (u32, u32),
    ) -> Result<Self> {
        let target = wayland_surface_target(connection, layer)?;

        // SAFETY: wl_display lives as long as `Connection`, wl_surface lives as
        // long as `Window`. Both must outlive the RawRenderer, enforced by drop
        // order in Window.
        let surface = unsafe { gpu.borrow().instance.create_surface_unsafe(target)? };

        let dev_id = pollster::block_on(gpu.borrow_mut().device(Some(&surface)))
            .ok_or_else(|| anyhow!("no wgpu device compatible with the surface"))?;

        let config = {
            let context = gpu.borrow();
            let device_handle = &context.devices[dev_id];
            let caps = surface.get_capabilities(device_handle.adapter());

            let format = caps
                .formats
                .iter()
                .copied()
                .find(|f| {
                    matches!(
                        f,
                        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
                    )
                })
                .or_else(|| caps.formats.first().copied())
                .ok_or_else(|| anyhow!("surface reports no texture formats"))?;

            let alpha_mode = STRAIGHT_ALPHA_MODES
                .into_iter()
                .find(|m| caps.alpha_modes.contains(m))
                .unwrap_or(CompositeAlphaMode::Auto);

            // Let handlers copy textures straight onto the surface when the
            // compositor supports it.
            let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
            if caps.usages.contains(wgpu::TextureUsages::COPY_DST) {
                usage |= wgpu::TextureUsages::COPY_DST;
            }

            let config = wgpu::SurfaceConfiguration {
                usage,
                format,
                width: physical_size.0.max(1),
                height: physical_size.1.max(1),
                present_mode: PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            };
            surface.configure(&device_handle.device, &config);
            config
        };

        Ok(Self {
            gpu,
            surface,
            config,
            dev_id,
        })
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        let gpu = self.gpu.borrow();
        self.surface
            .configure(&gpu.devices[self.dev_id].device, &self.config);
    }

    pub(crate) fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Runs `f` with a [`RawSurfaceCtx`] borrowing the shared device. Used for
    /// the callbacks that need GPU access but no frame (`setup`, `on_tick`,
    /// `on_frame`).
    pub(crate) fn with_ctx<R>(
        &self,
        scale: f64,
        layer: &LayerSurface,
        qh: &QueueHandle<App>,
        f: impl FnOnce(RawSurfaceCtx<'_>) -> R,
    ) -> R {
        let gpu = self.gpu.borrow();
        let device_handle = &gpu.devices[self.dev_id];
        f(RawSurfaceCtx {
            size: self.surface_size(),
            scale,
            device: &device_handle.device,
            queue: &device_handle.queue,
            format: self.config.format,
            layer,
            qh,
        })
    }

    /// Acquires the next surface texture, hands it to the handler, and
    /// presents it.
    pub(crate) fn render(
        &mut self,
        handler: &mut dyn RawSurfaceHandler,
        scale: f64,
        layer: &LayerSurface,
        qh: &QueueHandle<App>,
    ) {
        let gpu = self.gpu.borrow();
        let device_handle = &gpu.devices[self.dev_id];

        let surface_texture = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(t) | CurrentSurfaceTexture::Suboptimal(t) => t,
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.surface.configure(&device_handle.device, &self.config);
                return;
            }
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Validation => return,
        };

        let view = surface_texture.texture.create_view(&Default::default());
        handler.render(
            &surface_texture,
            &view,
            RawSurfaceCtx {
                size: self.surface_size(),
                scale,
                device: &device_handle.device,
                queue: &device_handle.queue,
                format: self.config.format,
                layer,
                qh,
            },
        );
        surface_texture.present();
    }
}
