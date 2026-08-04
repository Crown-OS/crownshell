//! Text layout and rendering, powered by [Parley] and drawn through [Vello].
//!
//! [`TextContext`] holds the system font database and Parley's layout scratch
//! space. It is created once per process and handed to [`SurfaceHandler`] calls
//! through [`SurfaceCtx::text`].
//!
//! [`Text`] is a retained, reusable piece of text: set its content and style,
//! measure it, and draw it. The underlying Parley layout is cached and only
//! rebuilt when the content, style or scale actually changes, which matters
//! because crownshell only repaints on demand.
//!
//! ```no_run
//! use crownshell::predule::*;
//!
//! struct Bar {
//!     clock: Text,
//! }
//!
//! impl SurfaceHandler for Bar {
//!     fn paint(&mut self, scene: &mut Scene, ctx: SurfaceCtx<'_>) {
//!         self.clock.set_text("12:45");
//!         let size = self.clock.size(ctx.text);
//!         let x = (ctx.size.0 as f64 - size.width) / 2.0;
//!         self.clock.draw(ctx.text, scene, (x, 8.0));
//!     }
//! }
//! ```
//!
//! [Parley]: https://docs.rs/parley
//! [Vello]: https://docs.rs/vello
//! [`SurfaceHandler`]: crate::SurfaceHandler
//! [`SurfaceCtx::text`]: crate::SurfaceCtx::text

use parley::{
    FontContext, FontStack, FontStyle, FontWeight, LayoutContext, LineHeight, PositionedLayoutItem,
    StyleProperty,
};
use vello::{
    kurbo::{Affine, Diagonal2, Point, Size},
    peniko::{Color, Fill},
    FontEmbolden, Glyph, Scene,
};

/// Synthetic bold expansion, as a fraction of the font size in pixels. Only
/// applied when the font family has no real bold face.
const SYNTHETIC_BOLD_RATIO: f64 = 0.02;

/// The brush carried through a text layout.
///
/// Parley requires brushes to be `Default`, which [`Color`] is not, so this
/// newtype supplies one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ColorBrush(pub Color);

impl Default for ColorBrush {
    fn default() -> Self {
        Self(Color::WHITE)
    }
}

impl From<Color> for ColorBrush {
    fn from(color: Color) -> Self {
        Self(color)
    }
}

/// A Parley layout specialised to crownshell's brush type.
pub type TextLayout = parley::Layout<ColorBrush>;

/// The system font database plus Parley's shaping and layout caches.
///
/// Building one discovers the system fonts (via fontconfig on Linux), so it is
/// relatively expensive and should exist exactly once per process. [`App`]
/// owns one and lends it to handlers as [`SurfaceCtx::text`].
///
/// [`App`]: crate::App
/// [`SurfaceCtx::text`]: crate::SurfaceCtx::text
pub struct TextContext {
    pub font_cx: FontContext,
    pub layout_cx: LayoutContext<ColorBrush>,
    scale: f32,
}

impl TextContext {
    /// Creates a context backed by the fonts installed on the system.
    pub fn new() -> Self {
        Self {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            scale: 1.0,
        }
    }

    /// The device pixel ratio text is rasterised at.
    ///
    /// This does not change the size text appears — [`Text`] always works in
    /// logical pixels — only how finely it is rendered. Each window sets it
    /// from its own buffer scale before painting, so handlers rarely touch it.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Sets the device pixel ratio. Values below `0.01` are clamped.
    ///
    /// Any [`Text`] built at a different scale re-lays out on next use.
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.01);
    }

    /// Registers an additional font from in-memory data (e.g. `include_bytes!`),
    /// returning the family names it was registered under.
    ///
    /// Registered families take priority over system fonts of the same name.
    pub fn register_font(&mut self, data: Vec<u8>) -> Vec<String> {
        let data: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync> = std::sync::Arc::new(data);
        let registered = self
            .font_cx
            .collection
            .register_fonts(parley::fontique::Blob::new(data), None);
        registered
            .iter()
            .filter_map(|(family_id, _)| {
                self.font_cx
                    .collection
                    .family_name(*family_id)
                    .map(str::to_owned)
            })
            .collect()
    }
}

impl Default for TextContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Visual properties applied to a whole [`Text`].
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font family stack in CSS syntax, e.g. `"Inter, Noto Sans, sans-serif"`.
    ///
    /// The generic families `sans-serif`, `serif`, `monospace`, `cursive`,
    /// `fantasy`, `system-ui`, `ui-monospace` and friends are understood, and
    /// families that are not installed are skipped.
    pub family: String,
    /// Font size in pixels per em, before [`Text::scale`] is applied.
    pub size: f32,
    /// CSS font weight: 100 (thin) to 900 (black), 400 is regular.
    pub weight: f32,
    /// Selects the italic face, synthesising an oblique if none exists.
    pub italic: bool,
    /// Glyph colour.
    pub color: Color,
    /// Line height as a multiple of [`TextStyle::size`].
    pub line_height: f32,
    /// Extra spacing inserted after every glyph, in pixels.
    pub letter_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: "sans-serif".to_string(),
            size: 14.0,
            weight: 400.0,
            italic: false,
            color: Color::WHITE,
            line_height: 1.2,
            letter_spacing: 0.0,
        }
    }
}

impl TextStyle {
    /// A style with the given family stack and size, other fields defaulted.
    pub fn new(family: impl Into<String>, size: f32) -> Self {
        Self {
            family: family.into(),
            size,
            ..Self::default()
        }
    }

    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = family.into();
        self
    }

    pub fn with_size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn with_line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }

    pub fn with_letter_spacing(mut self, letter_spacing: f32) -> Self {
        self.letter_spacing = letter_spacing;
        self
    }
}

/// A reusable piece of styled text with a cached layout.
///
/// Keep one of these alive across frames rather than rebuilding it in `paint`:
/// [`set_text`](Self::set_text) and [`set_style`](Self::set_style) only
/// invalidate the layout when the value actually changes, so an unchanged
/// `Text` costs nothing to draw again.
///
/// All positions and measurements are in **logical pixels**, the same space
/// [`SurfaceCtx::size`] is given in. The layout is built at the context's
/// [`scale`](TextContext::scale) so glyphs are rasterised at the display's true
/// pixel density, but that never changes the size text reports or appears.
///
/// [`SurfaceCtx::size`]: crate::SurfaceCtx::size
pub struct Text {
    content: String,
    style: TextStyle,
    layout: TextLayout,
    /// The [`TextContext::scale`] the cached layout was built at.
    built_scale: f32,
    dirty: bool,
}

impl Text {
    /// Creates text with the default style.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: TextStyle::default(),
            layout: TextLayout::new(),
            built_scale: 1.0,
            dirty: true,
        }
    }

    /// Creates text with an explicit style.
    pub fn styled(content: impl Into<String>, style: TextStyle) -> Self {
        Self {
            style,
            ..Self::new(content)
        }
    }

    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.set_style(style);
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the content. The layout is only invalidated if it differs.
    pub fn set_text(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref();
        if self.content != text {
            self.content.clear();
            self.content.push_str(text);
            self.dirty = true;
        }
    }

    pub fn style(&self) -> &TextStyle {
        &self.style
    }

    /// Replaces the style. The layout is only invalidated if it differs.
    pub fn set_style(&mut self, style: TextStyle) {
        if self.style != style {
            self.style = style;
            self.dirty = true;
        }
    }

    /// Mutable access to the style. Always invalidates the layout.
    pub fn style_mut(&mut self) -> &mut TextStyle {
        self.dirty = true;
        &mut self.style
    }

    /// Returns the laid-out text, rebuilding it first if anything changed.
    ///
    /// The layout is in physical pixels — it is built at
    /// [`TextContext::scale`]. Every other method on `Text` converts back to
    /// logical pixels for you.
    pub fn layout(&mut self, tcx: &mut TextContext) -> &TextLayout {
        if self.dirty || self.built_scale != tcx.scale() {
            self.rebuild(tcx);
        }
        &self.layout
    }

    /// The size of the laid-out text, in logical pixels.
    pub fn size(&mut self, tcx: &mut TextContext) -> Size {
        let scale = self.ensure_layout(tcx);
        Size::new(
            self.layout.width() as f64 / scale,
            self.layout.height() as f64 / scale,
        )
    }

    pub fn width(&mut self, tcx: &mut TextContext) -> f64 {
        let scale = self.ensure_layout(tcx);
        self.layout.width() as f64 / scale
    }

    pub fn height(&mut self, tcx: &mut TextContext) -> f64 {
        let scale = self.ensure_layout(tcx);
        self.layout.height() as f64 / scale
    }

    /// Distance from the top of the text to the first baseline, in logical
    /// pixels.
    ///
    /// Use this to sit text on a baseline shared with other content:
    /// `draw(.., (x, baseline_y - text.baseline(tcx)))`.
    pub fn baseline(&mut self, tcx: &mut TextContext) -> f64 {
        let scale = self.ensure_layout(tcx);
        self.layout
            .lines()
            .next()
            .map(|line| line.metrics().baseline as f64 / scale)
            .unwrap_or(0.0)
    }

    /// Draws the text into `scene` with its top-left corner at `origin`, in
    /// logical pixels.
    ///
    /// Pixel-aligned origins give the crispest result, since glyph hinting is
    /// enabled and Parley quantises the layout to whole pixels.
    pub fn draw(&mut self, tcx: &mut TextContext, scene: &mut Scene, origin: impl Into<Point>) {
        let origin = origin.into();
        let scale = self.ensure_layout(tcx);
        // The layout is in physical pixels, so undo the scale here. Windows
        // re-apply it to the whole scene, leaving a unit matrix on the glyph
        // run — which is what lets Vello keep hinting at the physical ppem.
        let transform = Affine::translate((origin.x, origin.y)) * Affine::scale(1.0 / scale);
        draw_layout(scene, &self.layout, transform);
    }

    /// Rebuilds the layout if needed and returns the scale it is built at.
    fn ensure_layout(&mut self, tcx: &mut TextContext) -> f64 {
        if self.dirty || self.built_scale != tcx.scale() {
            self.rebuild(tcx);
        }
        self.built_scale as f64
    }

    fn rebuild(&mut self, tcx: &mut TextContext) {
        let TextContext {
            font_cx,
            layout_cx,
            scale,
        } = tcx;
        let scale = *scale;
        let style = &self.style;

        let mut builder = layout_cx.ranged_builder(font_cx, &self.content, scale, true);
        builder.push_default(StyleProperty::FontStack(FontStack::from(
            style.family.as_str(),
        )));
        builder.push_default(StyleProperty::FontSize(style.size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        builder.push_default(StyleProperty::FontStyle(if style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            style.line_height,
        )));
        builder.push_default(StyleProperty::LetterSpacing(style.letter_spacing));
        builder.push_default(StyleProperty::Brush(ColorBrush(style.color)));
        builder.build_into(&mut self.layout, &self.content);

        // No wrapping: this pass targets single-line text.
        self.layout.break_all_lines(None);
        self.built_scale = scale;
        self.dirty = false;
    }
}

/// Encodes an already-built Parley layout into a Vello scene under `transform`,
/// which maps the layout's own coordinate space into the scene's.
///
/// [`Text::draw`] is the usual entry point; this is exposed for callers that
/// drive Parley themselves. Pass [`Affine::IDENTITY`] to draw the layout 1:1
/// with its top-left corner at the scene origin.
///
/// Keep `transform` a uniform scale plus translation if you care about text
/// quality: Vello can only hint glyphs when the run transform has no rotation
/// or shear.
pub fn draw_layout(scene: &mut Scene, layout: &TextLayout, transform: Affine) {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let synthesis = run.synthesis();

            // Fontique asks for a synthetic oblique when the family has no
            // italic face; the angle is in degrees from the vertical.
            let glyph_transform = synthesis
                .skew()
                .map(|angle| Affine::skew(-(angle as f64).to_radians().tan(), 0.0));

            // Likewise for synthetic bold. Vello expands the outline after it
            // has been scaled to the font size, so the amount is in pixels.
            let embolden = if synthesis.embolden() {
                let amount = run.font_size() as f64 * SYNTHETIC_BOLD_RATIO;
                FontEmbolden::new(Diagonal2::new(amount, amount))
            } else {
                FontEmbolden::default()
            };

            scene
                .draw_glyphs(run.font())
                .brush(glyph_run.style().brush.0)
                .transform(transform)
                .glyph_transform(glyph_transform)
                .font_size(run.font_size())
                .font_embolden(embolden)
                .normalized_coords(run.normalized_coords())
                .hint(true)
                .draw(
                    Fill::NonZero,
                    glyph_run.positioned_glyphs().map(|glyph| Glyph {
                        id: glyph.id as u32,
                        x: glyph.x,
                        y: glyph.y,
                    }),
                );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_text() {
        let mut tcx = TextContext::new();

        let mut text = Text::new("Hello");
        let size = text.size(&mut tcx);
        assert!(size.width > 0.0, "expected non-zero width, got {size:?}");
        assert!(size.height > 0.0, "expected non-zero height, got {size:?}");

        let mut longer = Text::new("Hello, world");
        assert!(longer.width(&mut tcx) > size.width);
    }

    #[test]
    fn larger_font_size_is_taller() {
        let mut tcx = TextContext::new();

        let mut small = Text::styled("Hg", TextStyle::default().with_size(12.0));
        let mut large = Text::styled("Hg", TextStyle::default().with_size(32.0));

        assert!(large.height(&mut tcx) > small.height(&mut tcx));
        assert!(large.width(&mut tcx) > small.width(&mut tcx));
    }

    #[test]
    fn layout_is_reused_until_something_changes() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("abc");
        text.layout(&mut tcx);
        assert!(!text.dirty);

        text.set_text("abc");
        assert!(!text.dirty, "identical content should not invalidate");
        text.set_style(TextStyle::default());
        assert!(!text.dirty, "identical style should not invalidate");

        text.set_text("abcd");
        assert!(text.dirty);
    }

    #[test]
    fn scale_changes_rasterisation_not_logical_size() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("Hello");

        let logical = text.size(&mut tcx);
        assert_eq!(text.built_scale, 1.0);

        tcx.set_scale(2.0);
        let scaled = text.size(&mut tcx);
        assert_eq!(text.built_scale, 2.0, "raising the scale re-lays out");

        // The layout doubled in physical pixels, so the logical size is
        // unchanged (up to Parley's per-scale pixel quantisation).
        assert!(
            (scaled.width - logical.width).abs() < 1.0,
            "logical width moved: {} -> {}",
            logical.width,
            scaled.width
        );
        assert!(text.layout.width() as f64 > logical.width * 1.5);
    }

    /// The core of the HiDPI scheme: `Text` draws with a `1/scale` transform
    /// and the window re-applies `scale` to the whole scene. The two must
    /// cancel into a unit matrix on the glyph run, because Vello only hints
    /// glyphs — and only folds the scale into the ppem — when the run
    /// transform is a uniform scale.
    #[test]
    fn window_scale_cancels_into_a_hintable_glyph_run() {
        let mut tcx = TextContext::new();
        tcx.set_scale(2.0);

        let mut text = Text::new("Hello");
        let mut inner = Scene::new();
        text.draw(&mut tcx, &mut inner, (10.0, 20.0));

        // What Window::paint does for a scale-2 surface.
        let mut outer = Scene::new();
        outer.append(&inner, Some(Affine::scale(2.0)));

        let run = &outer.encoding().resources.glyph_runs[0];
        let m = run.transform.matrix;
        assert!(
            (m[0] - 1.0).abs() < 1e-5 && (m[3] - 1.0).abs() < 1e-5,
            "expected a unit matrix, got {m:?}"
        );
        assert!(m[1].abs() < 1e-5 && m[2].abs() < 1e-5, "unexpected shear");

        // Logical origin lands on the right physical pixel.
        let t = run.transform.translation;
        assert!((t[0] - 20.0).abs() < 1e-3 && (t[1] - 40.0).abs() < 1e-3, "{t:?}");

        // And the run carries the physical ppem, not the logical one.
        assert!(
            (run.font_size - TextStyle::default().size * 2.0).abs() < 1e-3,
            "expected the doubled ppem, got {}",
            run.font_size
        );
    }

    #[test]
    fn scale_is_clamped_away_from_zero() {
        let mut tcx = TextContext::new();
        tcx.set_scale(0.0);
        assert!(tcx.scale() > 0.0);

        let mut text = Text::new("Hello");
        assert!(text.width(&mut tcx).is_finite());
    }

    #[test]
    fn baseline_sits_inside_the_layout() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("Hello");
        let baseline = text.baseline(&mut tcx);
        assert!(baseline > 0.0 && baseline < text.height(&mut tcx));
    }

    #[test]
    fn encodes_glyphs_into_the_scene() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("Hello");
        let mut scene = Scene::new();

        text.draw(&mut tcx, &mut scene, (0.0, 0.0));

        assert_eq!(
            scene.encoding().resources.glyphs.len(),
            5,
            "expected one glyph per character"
        );
    }
}
