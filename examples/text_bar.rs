//! A top bar that draws a live clock with Parley-shaped text.
//!
//! Run with `cargo run --example text_bar` under a wlr-layer-shell compositor.

use crownshell::predule::*;
use vello::kurbo::RoundedRect;
use vello::peniko::Fill;

const BAR_HEIGHT: u32 = 40;
const PADDING: f64 = 16.0;

struct Bar {
    clock: Text,
    label: Text,
}

impl Bar {
    fn new() -> Self {
        let clock = Text::new("--:--:--").with_style(
            TextStyle::new("Inter, Noto Sans, sans-serif", 16.0)
                .with_weight(600.0)
                .with_color(Color::from_rgba8(240, 240, 250, 255)),
        );

        let label = Text::new("crownshell").with_style(
            TextStyle::new("Inter, Noto Sans, sans-serif", 14.0)
                .with_color(Color::from_rgba8(150, 150, 170, 255)),
        );

        Self { clock, label }
    }

    fn current_time() -> String {
        chrono::Local::now().format("%H:%M:%S").to_string()
    }
}

impl SurfaceHandler for Bar {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
        let (w, h) = (ctx.size.0 as f64, ctx.size.1 as f64);

        let bg = RoundedRect::new(0.0, 0.0, w, h, 0.0);
        scene.fill(
            Fill::NonZero,
            Default::default(),
            Color::from_rgba8(20, 20, 30, 220),
            None,
            &bg,
        );

        // Left: a static label, vertically centred.
        self.label.set_text("crownshell");
        let label_size = self.label.size(ctx.text);
        self.label.draw(
            ctx.text,
            scene,
            (PADDING, ((h - label_size.height) / 2.0).round()),
        );

        // Right: the clock, measured so it can be right-aligned.
        self.clock.set_text(Self::current_time());
        let clock_size = self.clock.size(ctx.text);
        self.clock.draw(
            ctx.text,
            scene,
            (
                (w - PADDING - clock_size.width).round(),
                ((h - clock_size.height) / 2.0).round(),
            ),
        );
    }

    fn on_tick(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        // Redraw once a second so the clock advances.
        true
    }
}

fn main() -> Result<()> {
    env_logger::init();

    run(|app| {
        app.create_window(
            WindowConfig {
                namespace: "crownshell-text-bar".into(),
                layer: Layer::Top,
                anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                size: (0, BAR_HEIGHT),
                exclusive_zone: BAR_HEIGHT as i32,
                blur: true,
                ..Default::default()
            },
            Bar::new(),
        );
        Ok(())
    })
}
