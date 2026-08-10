//! A bar with a macOS-style menu that opens from it with a scale animation.
//!
//! Run with `cargo run --example menu_popup` under a wlr-layer-shell compositor.
//!
//! # How the popup is put together
//!
//! The popup is its own layer surface on [`Layer::Overlay`], anchored to all
//! four edges. With `exclusive_zone: 0` the compositor sizes it to the *usable*
//! area, so its top edge lands exactly below the bar whether or not anything
//! else on the output has reserved space. Covering that whole area buys three
//! things: the panel can be drawn anywhere in it, with a shadow that spills
//! past the panel; a click outside the panel is an ordinary pointer event on the
//! same surface, which is how it dismisses; and the panel can be animated
//! without ever resizing the surface, so no frame of the animation waits on a
//! compositor round trip.
//!
//! # How it stays hot
//!
//! Both surfaces are created up front, in `main`, so the popup's wgpu surface,
//! Vello pipelines and shaped text all exist before the user ever clicks. The
//! cost of opening is one repaint. While the popup is closed it draws nothing —
//! a fully transparent buffer with no input region (clicks fall through to the
//! bar) and no blur region (the compositor is not blurring a rectangle nobody
//! can see).
//!
//! The alternative is to unmap the surface between uses, by attaching a null
//! buffer. That releases the compositor's copy of it, but remapping needs a
//! configure round trip before the first frame can be drawn, which is exactly
//! the latency the animation cannot afford.
//!
//! # How the animation runs
//!
//! Content is drawn into a scratch [`Scene`] and appended to the frame under
//! one [`Affine`]. Per frame that means re-encoding some draw commands — no
//! text shaping, no layout — and the whole panel scales as one unit from its
//! top-left corner, where it meets the bar.
//!
//! Frames are driven by [`SurfaceHandler::on_frame`], which returns `true` for
//! as long as the animation is running, so it is paced by the compositor's
//! frame clock rather than a timer. The first frame comes from
//! [`SurfaceHandler::needs_redraw`]: the click lands on the *bar*, and that is
//! how the bar's handler gets the popup's surface to repaint.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use crownshell::predule::*;
use vello::{
    kurbo::{Affine, RoundedRect, Stroke},
    peniko::{Fill, Mix},
};

const BAR_HEIGHT: u32 = 36;
const BAR_PAD: f64 = 10.0;

/// Left edge of the panel, and of the bar button it hangs from.
const PANEL_X: f64 = 8.0;
/// Gap below the bar. The popup surface starts where the bar's exclusive zone
/// ends, so this is measured from the top of the surface, not the output.
const PANEL_Y: f64 = 2.0;
const PANEL_W: f64 = 258.0;
const PANEL_RADIUS: f64 = 10.0;
const PANEL_PAD_V: f64 = 6.0;
const ROW_H: f64 = 26.0;
const ROW_INSET: f64 = 5.0;
const ROW_TEXT_PAD: f64 = 13.0;
const SEPARATOR_H: f64 = 11.0;
const SHADOW_DY: f64 = 5.0;
const SHADOW_BLUR: f64 = 14.0;

const OPEN_MS: f64 = 150.0;
const CLOSE_MS: f64 = 100.0;
/// How much of its final size the panel starts at.
const START_SCALE: f64 = 0.93;
/// How far above its resting place the panel starts.
const START_RISE: f64 = 6.0;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Closed,
    Opening,
    Open,
    Closing,
}

/// State the bar and the popup both read.
///
/// Everything in crownshell runs on one thread, so `Rc<RefCell<_>>` is all the
/// sharing this needs. `version` is what the two handlers watch: each remembers
/// the version it last painted, and [`SurfaceHandler::needs_redraw`] compares
/// the two. That is what lets a click on the bar repaint the popup.
struct Menu {
    phase: Phase,
    since: Instant,
    version: u64,
}

impl Menu {
    fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            phase: Phase::Closed,
            since: Instant::now(),
            version: 0,
        }))
    }

    fn is_open(&self) -> bool {
        matches!(self.phase, Phase::Open | Phase::Opening)
    }

    /// How open the panel is, from 0 (closed) to 1 (fully open), before easing.
    fn progress(&self) -> f64 {
        let elapsed = self.since.elapsed().as_secs_f64() * 1000.0;
        match self.phase {
            Phase::Closed => 0.0,
            Phase::Open => 1.0,
            Phase::Opening => (elapsed / OPEN_MS).clamp(0.0, 1.0),
            Phase::Closing => 1.0 - (elapsed / CLOSE_MS).clamp(0.0, 1.0),
        }
    }

    fn set_open(&mut self, open: bool) {
        if self.is_open() == open {
            return;
        }
        // Start from wherever the panel currently is, so clicking twice in
        // quick succession reverses the animation instead of snapping.
        let progress = self.progress();
        let (phase, duration) = if open {
            (Phase::Opening, OPEN_MS)
        } else {
            (Phase::Closing, CLOSE_MS)
        };
        let done = if open { progress } else { 1.0 - progress };
        let consumed = Duration::from_secs_f64(done * duration / 1000.0);
        let now = Instant::now();
        self.since = now.checked_sub(consumed).unwrap_or(now);
        self.phase = phase;
        self.version += 1;
    }

    /// Retires a finished animation. Returns whether another frame is wanted:
    /// one more while running, and one final one to paint the settled state.
    fn advance(&mut self) -> bool {
        let elapsed = self.since.elapsed().as_secs_f64() * 1000.0;
        match self.phase {
            Phase::Opening if elapsed >= OPEN_MS => {
                self.phase = Phase::Open;
                true
            }
            Phase::Closing if elapsed >= CLOSE_MS => {
                self.phase = Phase::Closed;
                true
            }
            Phase::Opening | Phase::Closing => true,
            Phase::Open | Phase::Closed => false,
        }
    }
}

fn ease_out_cubic(t: f64) -> f64 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

// ---------------------------------------------------------------------------
// Bar
// ---------------------------------------------------------------------------

struct Bar {
    menu: Rc<RefCell<Menu>>,
    seen: u64,
    title: Text,
    clock: Text,
    hovered: bool,
}

impl Bar {
    fn new(menu: Rc<RefCell<Menu>>) -> Self {
        Self {
            menu,
            seen: 0,
            title: Text::new("CrownOS").with_style(
                TextStyle::new("Inter, Noto Sans, sans-serif", 13.0)
                    .with_weight(600.0)
                    .with_color(Color::from_rgba8(240, 240, 250, 255)),
            ),
            clock: Text::new("--:--").with_style(
                TextStyle::new("Inter, Noto Sans, sans-serif", 13.0)
                    .with_color(Color::from_rgba8(200, 200, 215, 255)),
            ),
            hovered: false,
        }
    }

    /// The clickable button, in surface-local pixels. Kept aligned with the
    /// panel's left edge so the popup appears to grow out of it.
    fn button_rect(&mut self, tcx: &mut TextContext) -> Rect {
        let width = self.title.width(tcx) + 2.0 * BAR_PAD;
        Rect::new(PANEL_X, 3.0, PANEL_X + width, BAR_HEIGHT as f64 - 3.0)
    }
}

impl SurfaceHandler for Bar {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
        let (w, h) = (ctx.size.0 as f64, ctx.size.1 as f64);
        let open = {
            let menu = self.menu.borrow();
            self.seen = menu.version;
            menu.is_open()
        };

        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(18, 18, 26, 210),
            None,
            &Rect::new(0.0, 0.0, w, h),
        );

        let button = self.button_rect(ctx.text);
        if open || self.hovered {
            let tint = if open { 40 } else { 22 };
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(255, 255, 255, tint),
                None,
                &RoundedRect::from_rect(button, 6.0),
            );
        }

        let title_size = self.title.size(ctx.text);
        self.title.draw(
            ctx.text,
            scene,
            (
                (button.x0 + BAR_PAD).round(),
                (button.center().y - title_size.height / 2.0).round(),
            ),
        );

        self.clock
            .set_text(chrono::Local::now().format("%a %d %b  %H:%M").to_string());
        let clock_size = self.clock.size(ctx.text);
        self.clock.draw(
            ctx.text,
            scene,
            (
                (w - 14.0 - clock_size.width).round(),
                ((h - clock_size.height) / 2.0).round(),
            ),
        );
    }

    fn on_pointer_press(&mut self, x: f64, y: f64, ctx: SurfaceCtx<'_>) -> bool {
        if !self.button_rect(ctx.text).contains(Point::new(x, y)) {
            return false;
        }
        // Only the popup's own surface can repaint itself. Flipping the shared
        // state is enough: `Popup::needs_redraw` picks it up at the end of this
        // event-loop iteration and the popup takes it from there.
        let mut menu = self.menu.borrow_mut();
        let open = menu.is_open();
        menu.set_open(!open);
        true
    }

    fn on_pointer_motion(&mut self, x: f64, y: f64, ctx: SurfaceCtx<'_>) -> bool {
        let hovered = self.button_rect(ctx.text).contains(Point::new(x, y));
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    fn on_pointer_enter(&mut self, x: f64, y: f64, ctx: SurfaceCtx<'_>) -> bool {
        self.on_pointer_motion(x, y, ctx)
    }

    fn on_pointer_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        let changed = self.hovered;
        self.hovered = false;
        changed
    }

    fn on_tick(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        // The popup can close itself, from a click outside it. That is the case
        // this catches: the button highlight has to come back off.
        self.menu.borrow().version != self.seen
    }
}

// ---------------------------------------------------------------------------
// Popup
// ---------------------------------------------------------------------------

struct Item {
    label: Text,
    shortcut: Option<Text>,
}

enum Entry {
    /// Boxed because an item owns two retained `Text`s and a separator owns
    /// nothing, so inlining it would make every separator as large as a row.
    Item(Box<Item>),
    Separator,
}

fn item(label: &str, shortcut: Option<&str>) -> Entry {
    let label = Text::new(label).with_style(
        TextStyle::new("Inter, Noto Sans, sans-serif", 13.0)
            .with_color(Color::from_rgba8(238, 238, 245, 255)),
    );
    let shortcut = shortcut.map(|s| {
        Text::new(s).with_style(
            TextStyle::new("Inter, Noto Sans, sans-serif", 13.0)
                .with_color(Color::from_rgba8(238, 238, 245, 130)),
        )
    });
    Entry::Item(Box::new(Item { label, shortcut }))
}

struct Popup {
    menu: Rc<RefCell<Menu>>,
    seen: u64,
    entries: Vec<Entry>,
    /// Top of each entry, relative to the panel's top edge. The menu is static,
    /// so these are measured once.
    tops: Vec<f64>,
    panel_h: f64,
    hovered: Option<usize>,
    /// Scratch scene for the panel, appended to the frame under the animation's
    /// transform. Reused so it keeps its allocation.
    content: Scene,
}

impl Popup {
    fn new(menu: Rc<RefCell<Menu>>) -> Self {
        let entries = vec![
            item("About This Mac", None),
            Entry::Separator,
            item("System Settings...", None),
            item("App Store...", None),
            Entry::Separator,
            item("Recent Items", Some("›")),
            Entry::Separator,
            item("Force Quit...", Some("⌥⌘⎋")),
            Entry::Separator,
            item("Sleep", None),
            item("Restart...", None),
            item("Shut Down...", None),
            Entry::Separator,
            item("Lock Screen", Some("^⌘Q")),
            item("Log Out...", Some("⇧⌘Q")),
        ];

        let mut tops = Vec::with_capacity(entries.len());
        let mut y = PANEL_PAD_V;
        for entry in &entries {
            tops.push(y);
            y += match entry {
                Entry::Item(_) => ROW_H,
                Entry::Separator => SEPARATOR_H,
            };
        }

        Self {
            menu,
            seen: 0,
            entries,
            tops,
            panel_h: y + PANEL_PAD_V,
            hovered: None,
            content: Scene::new(),
        }
    }

    /// The panel at rest, in surface-local pixels.
    fn panel_rect(&self) -> Rect {
        Rect::new(PANEL_X, PANEL_Y, PANEL_X + PANEL_W, PANEL_Y + self.panel_h)
    }

    fn row_rect(&self, index: usize) -> Rect {
        let panel = self.panel_rect();
        let top = panel.y0 + self.tops[index];
        Rect::new(panel.x0, top, panel.x1, top + ROW_H)
    }

    /// Which row is under a point, ignoring separators.
    fn row_at(&self, point: Point) -> Option<usize> {
        if !self.panel_rect().contains(point) {
            return None;
        }
        (0..self.entries.len()).find(|&i| {
            matches!(self.entries[i], Entry::Item(_)) && self.row_rect(i).contains(point)
        })
    }

    /// Encodes the panel into `self.content`, at rest. The animation is applied
    /// when the scene is appended, not here, so nothing below has to know that
    /// an animation is running.
    fn build_content(&mut self, tcx: &mut TextContext) {
        let panel = self.panel_rect();
        self.content.reset();

        let shape = RoundedRect::from_rect(panel, PANEL_RADIUS);
        self.content.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            // Translucent, so the compositor's blur shows through — but opaque
            // enough to stay readable on a compositor that has no
            // ext-background-effect, where the blur silently does nothing.
            Color::from_rgba8(36, 36, 44, 238),
            None,
            &shape,
        );
        // A hairline lifts the panel off whatever it is blurring.
        self.content.stroke(
            &Stroke::new(1.0),
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 30),
            None,
            &RoundedRect::from_rect(panel.inset(-0.5), PANEL_RADIUS - 0.5),
        );

        for index in 0..self.entries.len() {
            let top = panel.y0 + self.tops[index];
            match &self.entries[index] {
                Entry::Separator => {
                    let y = (top + SEPARATOR_H / 2.0).round();
                    self.content.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::from_rgba8(255, 255, 255, 24),
                        None,
                        &Rect::new(panel.x0 + 1.0, y, panel.x1 - 1.0, y + 1.0),
                    );
                }
                Entry::Item(_) => {
                    if self.hovered == Some(index) {
                        // The highlight is a fill only: tinting the text
                        // instead would change its style and throw away the
                        // cached layout on every pointer move.
                        self.content.fill(
                            Fill::NonZero,
                            Affine::IDENTITY,
                            Color::from_rgba8(255, 255, 255, 26),
                            None,
                            &RoundedRect::new(
                                panel.x0 + ROW_INSET,
                                top,
                                panel.x1 - ROW_INSET,
                                top + ROW_H,
                                5.0,
                            ),
                        );
                    }

                    let Entry::Item(item) = &mut self.entries[index] else {
                        unreachable!()
                    };
                    let label_size = item.label.size(tcx);
                    let baseline = (top + (ROW_H - label_size.height) / 2.0).round();
                    item.label
                        .draw(tcx, &mut self.content, (panel.x0 + ROW_TEXT_PAD, baseline));

                    if let Some(shortcut) = item.shortcut.as_mut() {
                        let width = shortcut.width(tcx);
                        shortcut.draw(
                            tcx,
                            &mut self.content,
                            ((panel.x1 - ROW_TEXT_PAD - width).round(), baseline),
                        );
                    }
                }
            }
        }
    }
}

/// A wl_region is a set of rectangles, so a rounded corner cannot be described
/// exactly. Three bands inset at the ends keep the blur from squaring off the
/// corners, which is the only place the difference shows.
fn rounded_bands(rect: Rect, radius: f64) -> [Rect; 3] {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    [
        Rect::new(
            rect.x0 + radius,
            rect.y0,
            rect.x1 - radius,
            rect.y0 + radius,
        ),
        Rect::new(rect.x0, rect.y0 + radius, rect.x1, rect.y1 - radius),
        Rect::new(
            rect.x0 + radius,
            rect.y1 - radius,
            rect.x1 - radius,
            rect.y1,
        ),
    ]
}

impl SurfaceHandler for Popup {
    fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
        let (phase, progress) = {
            let menu = self.menu.borrow();
            self.seen = menu.version;
            (menu.phase, menu.progress())
        };

        if phase == Phase::Closed {
            // Draw nothing: the buffer is fully transparent. Dropping both
            // regions is what makes a surface that is still mapped, and still
            // holding its GPU state, cost nothing while it waits.
            self.hovered = None;
            ctx.set_input_region(&[]);
            ctx.set_blur_region(&[]);
            return;
        }

        // Open, in any degree: the whole surface takes pointer input, so that a
        // click outside the panel dismisses it.
        ctx.set_input_region(&[Rect::new(0.0, 0.0, ctx.size.0 as f64, ctx.size.1 as f64)]);

        let eased = ease_out_cubic(progress);
        let panel = self.panel_rect();
        // Grow from the top-left corner, where the panel meets the bar button,
        // and settle downward over the last few pixels.
        let transform = Affine::translate((0.0, -START_RISE * (1.0 - eased)))
            * Affine::scale_about(
                START_SCALE + (1.0 - START_SCALE) * eased,
                Point::new(panel.x0, panel.y0),
            );
        let visible = transform.transform_rect_bbox(panel);
        // Alpha leads the scale: by the time the panel is halfway out it is
        // already solid, which reads as quicker than it is.
        let alpha = (progress * 1.8).min(1.0) as f32;

        ctx.set_blur_region(&rounded_bands(visible, PANEL_RADIUS));

        scene.draw_blurred_rounded_rect(
            Affine::translate((0.0, SHADOW_DY)) * transform,
            panel,
            Color::from_rgba8(0, 0, 0, (120.0 * alpha) as u8),
            PANEL_RADIUS,
            SHADOW_BLUR,
        );

        self.build_content(ctx.text);
        if alpha < 1.0 {
            // Clipped to the panel rather than the surface: a full-screen layer
            // would make the compositor's cheapest frame the most expensive one.
            let clip = visible.inflate(1.0, 1.0);
            scene.push_layer(Fill::NonZero, Mix::Normal, alpha, Affine::IDENTITY, &clip);
            scene.append(&self.content, Some(transform));
            scene.pop_layer();
        } else {
            scene.append(&self.content, Some(transform));
        }
    }

    fn on_frame(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        // Returning true asks for another frame, so the animation is paced by
        // the compositor's frame clock.
        self.menu.borrow_mut().advance()
    }

    fn on_pointer_press(&mut self, x: f64, y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        let point = Point::new(x, y);
        if self.panel_rect().contains(point) {
            if let Some(index) = self.row_at(point) {
                if let Entry::Item(item) = &self.entries[index] {
                    log::info!("menu item activated: {}", item.label.text());
                }
            } else {
                // A separator: not something to act on, and not a dismissal.
                return false;
            }
        }
        self.menu.borrow_mut().set_open(false);
        true
    }

    fn on_pointer_motion(&mut self, x: f64, y: f64, _ctx: SurfaceCtx<'_>) -> bool {
        // Only hit-test once the panel has stopped moving; hovering a target
        // that is still sliding under the pointer is worse than not hovering.
        let hovered = if self.menu.borrow().phase == Phase::Open {
            self.row_at(Point::new(x, y))
        } else {
            None
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    fn on_pointer_enter(&mut self, x: f64, y: f64, ctx: SurfaceCtx<'_>) -> bool {
        self.on_pointer_motion(x, y, ctx)
    }

    fn on_pointer_leave(&mut self, _ctx: SurfaceCtx<'_>) -> bool {
        let changed = self.hovered.is_some();
        self.hovered = None;
        changed
    }

    fn needs_redraw(&self) -> bool {
        self.menu.borrow().version != self.seen
    }
}

// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    env_logger::init();

    let menu = Menu::new();

    run(|app| {
        app.create_window(
            WindowConfig {
                namespace: "crownshell-menu-bar".into(),
                layer: Layer::Top,
                anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                size: (0, BAR_HEIGHT),
                exclusive_zone: BAR_HEIGHT as i32,
                blur: true,
                ..Default::default()
            },
            Bar::new(menu.clone()),
        );

        // The popup, created now and kept for the life of the process. Anchored
        // on all four sides with a size of (0, 0), it is configured to fill the
        // output; leaving `exclusive_zone` at 0 means the compositor shrinks it
        // out of the way of the bar, so the surface starts immediately below the
        // bar however much space other clients have reserved as well.
        app.create_window(
            WindowConfig {
                namespace: "crownshell-menu-popup".into(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                size: (0, 0),
                blur: true,
                // The panel is a small part of a full-screen surface, so the
                // handler sets the blur region per frame instead.
                auto_blur_region: false,
                ..Default::default()
            },
            Popup::new(menu),
        );

        Ok(())
    })
}
