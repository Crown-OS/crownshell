//! A wallpaper-shaped example: one `Layer::Background` surface per output,
//! rendered with a raw wgpu pipeline instead of a Vello scene, with windows
//! created and destroyed as outputs come and go.
//!
//! Run inside a Wayland session:
//!
//! ```sh
//! cargo run --example raw_wgpu
//! ```

use crownshell::{
    Anchor, App, Layer, RawSurfaceCtx, RawSurfaceHandler, WindowConfig, WlOutput, wgpu,
};

/// Fills the surface with a slowly-shifting color. A real wallpaper daemon
/// would build its pipelines in `setup` and sample textures in `render`.
struct Fill {
    hue: f32,
}

impl RawSurfaceHandler for Fill {
    fn setup(&mut self, ctx: RawSurfaceCtx<'_>) {
        log::info!(
            "surface ready: {}x{} physical, format {:?}",
            ctx.size.0,
            ctx.size.1,
            ctx.format
        );
    }

    fn render(
        &mut self,
        _surface_texture: &wgpu::SurfaceTexture,
        view: &wgpu::TextureView,
        ctx: RawSurfaceCtx<'_>,
    ) {
        self.hue = (self.hue + 0.002).fract();
        let (r, g, b) = hsl_to_rgb(self.hue);

        let mut encoder = ctx.device.create_command_encoder(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fill"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        ctx.queue.submit([encoder.finish()]);
    }

    // Returning `true` keeps the animation on the compositor's frame clock.
    fn on_frame(&mut self, _ctx: RawSurfaceCtx<'_>) -> bool {
        true
    }
}

fn hsl_to_rgb(h: f32) -> (f64, f64, f64) {
    let f = |n: f32| {
        let k = (n + h * 12.0) % 12.0;
        (0.4 - 0.35 * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)) as f64
    };
    (f(0.0), f(8.0), f(4.0))
}

fn spawn_on(app: &mut App, output: &WlOutput) {
    if app.window_on_output(output).is_some() {
        return; // Already handled: hooks also fire for outputs seen in setup.
    }
    let name = app
        .output_info(output)
        .and_then(|info| info.name)
        .unwrap_or_default();
    log::info!("creating background window on output {name:?}");
    app.create_raw_window_on_output(
        WindowConfig {
            namespace: "raw-wgpu-example".into(),
            layer: Layer::Background,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            exclusive_zone: -1,
            ..Default::default()
        },
        Fill { hue: 0.6 },
        output,
    );
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    crownshell::run(|app| {
        // Fires once per output present at startup and again on hotplug;
        // windows bound to unplugged outputs are cleaned up automatically.
        app.on_output_added(spawn_on);
        Ok(())
    })
}
