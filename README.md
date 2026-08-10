# crownshell

A small Rust framework for building Wayland **layer shell** surfaces like bar, dock, notifications, etc. Painting is done with [Vello](https://github.com/linebender/vello), so you get GPU-accelerated 2D graphics with paths, gradients, blurs, images and text.

This crate abstracts the Wayland boilerplate. Just configure the surface you want, implement a paint callback and you're good to go.

## What it gives you

- **Layer shell windows** via `wlr-layer-shell`, with the usual controls: layer, anchor, size, exclusive zone and keyboard interactivity.
- **Vello rendering** wired up to the surface — you just push into a `Scene`.
- **Text** shaped and laid out by [Parley](https://github.com/linebender/parley), using your system fonts, with measurement so you can align around it.
- **Input handling** for pointer events (enter, leave, motion, press, release) and keyboard.
- **Drag and drop** with mime-type negotiation and payload delivery.
- **Background blur** through the `ext-background-effect-v1` protocol, when the compositor supports it.
- **HiDPI** handled for you — you draw in logical pixels, crownshell renders into a correctly-sized physical buffer.
- **Ticks and frame callbacks** so animations and periodic redraws are easy.
- **Multiple windows** in a single app, driven by one `calloop` event loop.

## Support

- A Wayland compositor that supports `wlr-layer-shell` (Hyprland, Sway, KWin, River, and most wlroots-based compositors).

## Add it to your project

```toml
[dependencies]
crownshell = "0.1.0"
```

## A minimal example

```rust
use crownshell::predule::*;
use vello::kurbo::{Rect, RoundedRect};
use vello::peniko::{Color, Fill};

struct Bar;

impl SurfaceHandler for Bar {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
        let (w, h) = ctx.size;
        let bg = RoundedRect::new(0.0, 0.0, w as f64, h as f64, 12.0);
        scene.fill(
            Fill::NonZero,
            Default::default(),
            Color::from_rgba8(20, 20, 30, 220),
            None,
            &bg,
        );
    }
}

fn main() -> Result<()> {
    run(|app| {
        app.create_window(
            WindowConfig {
                namespace: "example-bar".into(),
                layer: Layer::Top,
                anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                size: (0, 40),
                exclusive_zone: 40,
                blur: true,
                ..Default::default()
            },
            Bar,
        );
        Ok(())
    })
}
```

This `run` sets up the Wayland connection, dispatches events, calls your `paint` when it needs a frame, and quits when `app.exit` is set.

## The shape of the API

- **`App`** — the top-level context. You get one inside the `run` closure. Use `app.create_window(...)` to attach surfaces.
- **`WindowConfig`** — a struct describing where the surface sits and how it behaves.
- **`SurfaceHandler`** — the trait you implement. `paint` is the only required method. Everything else (pointer, keyboard, drag-and-drop, ticks, frame callbacks) has a default no-op implementation, so you override only what you need.
- **`SurfaceCtx`** — passed into every callback. Gives you the current logical size, the output's `scale`, the shared `TextContext` as `ctx.text`, and the bits you'd need to trigger a commit or set a region.
- **`Text` / `TextStyle`** — retained, measurable text. See [Text](#text).
- **Returning `bool` from event callbacks** — return `true` to ask for a redraw, `false` if nothing visual changed. That keeps you from repainting on every mouse move by default.

- `on_frame` fires when the compositor is ready for the next frame — use it for smooth animation.
- `on_tick` fires on a timer (default 1s, configurable via `WindowConfig::tick_interval`) — use it for things like clocks or battery readings that don't need per-frame updates.

## Text

Text is shaped and laid out with [Parley](https://github.com/linebender/parley) and drawn as glyph outlines by Vello, so it scales cleanly and picks up whatever fonts are installed on the system.

A `Text` is a retained object: build it once, keep it on your handler, and update its content each frame. The layout is cached and only rebuilt when the content, style or scale actually changes, which matters because crownshell only repaints on demand.

```rust
use crownshell::predule::*;

struct Bar {
    clock: Text,
}

impl Bar {
    fn new() -> Self {
        Self {
            clock: Text::new("--:--").with_style(
                TextStyle::new("Inter, sans-serif", 16.0)
                    .with_weight(600.0)
                    .with_color(Color::from_rgba8(240, 240, 250, 255)),
            ),
        }
    }
}

impl SurfaceHandler for Bar {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
        self.clock.set_text("12:45");

        // Measure first, so the text can be centred.
        let size = self.clock.size(ctx.text);
        let x = (ctx.size.0 as f64 - size.width) / 2.0;
        let y = (ctx.size.1 as f64 - size.height) / 2.0;

        self.clock.draw(ctx.text, scene, (x.round(), y.round()));
    }
}
```

`ctx.text` is the shared `TextContext` — the system font database plus Parley's caches. There is one per app, created for you.

- **`TextStyle`** — family stack in CSS syntax (`"Inter, Noto Sans, sans-serif"`, generic families included), size, weight, italic, colour, line height and letter spacing.
- **Measurement** — `size`, `width`, `height` and `baseline` all lay the text out on demand and hand back pixels, so you can size and align boxes around it.
- **Custom fonts** — `ctx.text.register_font(bytes)` adds a font from memory (e.g. `include_bytes!`), so you can ship your own typeface instead of relying on what's installed.
- **Missing families are skipped**, falling through the stack to the generic family at the end. Always end your stack with `sans-serif` or `monospace`.

There is a full example in [`examples/text_bar.rs`](examples/text_bar.rs) — a top bar with a label and a live clock:

```
cargo run --example text_bar
```

Text is currently laid out and drawn as a single line, without wrapping or alignment.

## HiDPI

You draw in **logical pixels** and crownshell handles the rest. `ctx.size` is the logical surface size, `Text` measures and positions in logical pixels, and the whole scene is scaled into the physical buffer before it's submitted. Nothing in your `paint` changes when the display density does.

Under the hood, crownshell tracks the `wl_surface` buffer scale, sets it on the surface, sizes the wgpu surface in physical pixels, and lays text out at the physical pixel density so glyphs are rasterised at their true ppem rather than upscaled. Vello folds the uniform scene scale back into the glyph size, so hinting survives.

`ctx.scale` is there if you want it — for snapping to the physical pixel grid, say — but you shouldn't need it for ordinary drawing.

```rust
fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
    // ctx.size is logical; on a 2x display the buffer behind it is twice this.
    let (w, h) = ctx.size;

    // Text is measured and placed in the same logical space.
    let size = self.clock.size(ctx.text);
    self.clock.draw(ctx.text, scene, (0.0, (h as f64 - size.height) / 2.0));
}
```

Only integer buffer scales are supported today, which is what `wl_surface.set_buffer_scale` accepts. On a fractionally-scaled output the compositor advertises the next integer up (a 1.25x display reports 2), so text is rendered at 2x and scaled down by the compositor — sharp, if not pixel-exact. True fractional scaling needs `wp_fractional_scale_v1`, which crownshell does not bind yet.

## Blur

If you set `blur: true` in the config and the compositor advertises `ext-background-effect-v1`, crownshell will register a blur region covering the whole surface. If the protocol isn't available it's silently skipped — your app still renders, just without the frosted-glass look.

For a surface that is only partly opaque — a menu panel on a surface that covers the screen, say — set `auto_blur_region: false` and drive the region yourself with `ctx.set_blur_region(&[rect])`, so the compositor isn't blurring behind pixels you never drew.

## Popups

A popup is a second layer surface, not a region of the bar. See [`examples/menu_popup.rs`](examples/menu_popup.rs) for a bar with a menu that opens from it under a scale animation.

The pattern it uses:

- Create the popup surface on `Layer::Overlay` in `run`, next to the bar, and keep it for the life of the process. Opening it then costs one repaint rather than a wgpu surface, a set of Vello pipelines and a round of text shaping.
- Anchor it on all four sides with `size: (0, 0)` and leave `exclusive_zone` at 0. The compositor sizes it to the usable area, so its top edge sits just below the bar, and a click anywhere outside the panel arrives as an ordinary pointer event on that surface — which is how it dismisses.
- While it's closed, draw nothing and drop both regions: `ctx.set_input_region(&[])` makes the surface click-through, and `ctx.set_blur_region(&[])` stops the compositor blurring behind an invisible panel.
- Draw the panel into a scratch `Scene` and `append` it under one `Affine`, so a frame of animation re-encodes draw commands without re-laying-out any text.
- Drive frames from `on_frame`, returning `true` while the animation runs, so it's paced by the compositor's frame clock rather than a timer.
- Share state between the bar and the popup with an `Rc<RefCell<_>>`; everything runs on one thread. A click lands on the *bar*, so the popup gets its first frame from `needs_redraw`, which is polled on every surface after each batch of events. It's the one hook that lets a handler repaint a surface other than the one an event arrived on — also what you want for a D-Bus signal or a message from a worker thread.

## Status

This is early crate, built to power layershell on my own distro (CrownOS). The API will move. If you're going to use it, expect breaking changes.

## License

Licensed under the [MIT License](LICENSE).
