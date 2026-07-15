# crownshell

A small Rust framework for building Wayland **layer shell** surfaces like bar, dock, notifications, etc. Painting is done with [Vello](https://github.com/linebender/vello), so you get GPU-accelerated 2D graphics with paths, gradients, blurs, images and text.

This crate abstracts the Wayland boilerplate. Just configure the surface you want, implement a paint callback and you're good to go.

## What it gives you

- **Layer shell windows** via `wlr-layer-shell`, with the usual controls: layer, anchor, size, exclusive zone and keyboard interactivity.
- **Vello rendering** wired up to the surface — you just push into a `Scene`.
- **Input handling** for pointer events (enter, leave, motion, press, release) and keyboard.
- **Drag and drop** with mime-type negotiation and payload delivery.
- **Background blur** through the `ext-background-effect-v1` protocol, when the compositor supports it.
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
- **`SurfaceCtx`** — passed into every callback. Gives you the current size and the bits you'd need to trigger a commit or set a region.
- **Returning `bool` from event callbacks** — return `true` to ask for a redraw, `false` if nothing visual changed. That keeps you from repainting on every mouse move by default.

- `on_frame` fires when the compositor is ready for the next frame — use it for smooth animation.
- `on_tick` fires on a timer (default 1s, configurable via `WindowConfig::tick_interval`) — use it for things like clocks or battery readings that don't need per-frame updates.

## Blur

If you set `blur: true` in the config and the compositor advertises `ext-background-effect-v1`, crownshell will register a blur region covering the whole surface. If the protocol isn't available it's silently skipped — your app still renders, just without the frosted-glass look.

## Status

This is early crate, built to power layershell on my own distro (CrownOS). The API will move. If you're going to use it, expect breaking changes.

## License

Licensed under the [MIT License](LICENSE).
