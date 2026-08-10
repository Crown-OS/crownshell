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
    Affinity, Alignment, AlignmentOptions, FontContext, FontStack, FontStyle, FontWeight,
    LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty, layout::cursor::Cursor,
};
use vello::{
    FontEmbolden, Glyph, Scene,
    kurbo::{Affine, Diagonal2, Point, Rect, Size},
    peniko::{Color, Fill},
};

/// Synthetic bold expansion, as a fraction of the font size in pixels. Only
/// applied when the font family has no real bold face.
const SYNTHETIC_BOLD_RATIO: f64 = 0.02;

/// Width of the rectangle [`Text::caret`] reports, in logical pixels. A caret
/// is a zero-width position between two clusters; giving it a hairline width
/// means callers can fill the rect directly instead of inventing one.
const CARET_WIDTH: f64 = 1.0;

/// The character appended to text that a [`Text::set_max_lines`] clamp cut short.
const ELLIPSIS: char = '…';

/// Ceiling on the number of re-layouts [`Text::clamp_lines`] will spend fitting
/// an ellipsis onto the last kept line.
///
/// One pass is normally enough. A pass can fail when the ellipsis is wider than
/// the character it replaced and pushes the line over the wrap width again, and
/// pathological styles (letter spacing wider than the wrap width) never
/// converge at all, so the loop is bounded and falls back to an exact,
/// ellipsis-free truncation. Every pass drops at least one more character, so
/// the bound is only ever reached by such a style.
const MAX_TRUNCATION_PASSES: usize = 8;

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
    /// How each line sits inside the wrap width set by [`Text::set_max_width`].
    ///
    /// Only observable once the text wraps: with no wrap width the container is
    /// the layout's own width, so no line has any free space to be moved in.
    pub alignment: Alignment,
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
            alignment: Alignment::Start,
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

    pub fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
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
/// Text is a single line by default. Give it a
/// [`max_width`](Self::set_max_width) to wrap it into a column and a
/// [`max_lines`](Self::set_max_lines) to clamp that column with an ellipsis.
///
/// [`SurfaceCtx::size`]: crate::SurfaceCtx::size
pub struct Text {
    content: String,
    style: TextStyle,
    layout: TextLayout,
    max_width: Option<f64>,
    max_lines: Option<usize>,
    /// The string the cached layout was actually built from: `content`, unless
    /// a [`max_lines`](Self::set_max_lines) clamp shortened it to a prefix plus
    /// an ellipsis. Held as a field so clamping reuses one allocation instead
    /// of building a new `String` every time the text or the width changes.
    display: String,
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
            max_width: None,
            max_lines: None,
            display: String::new(),
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

    /// The wrap width in logical pixels, or `None` when the text never wraps.
    pub fn max_width(&self) -> Option<f64> {
        self.max_width
    }

    /// Sets the wrap width, in **logical** pixels — the same space
    /// [`size`](Self::size) reports in, so a caller can hand this the column
    /// width it measured without thinking about the display's pixel density.
    ///
    /// `None`, the default, puts the whole content on one line however long it
    /// is, which is what a label or a clock wants. Give it a width for anything
    /// that has to fit a column, such as a notification body.
    ///
    /// Changing the width invalidates the cached layout; setting the width it
    /// already has does not, so a handler can pass the same measurement every
    /// frame without forcing a re-layout.
    pub fn set_max_width(&mut self, max_width: Option<f64>) {
        if self.max_width != max_width {
            self.max_width = max_width;
            self.dirty = true;
        }
    }

    pub fn with_max_width(mut self, max_width: Option<f64>) -> Self {
        self.set_max_width(max_width);
        self
    }

    /// The line limit, or `None` when the text is not clamped.
    pub fn max_lines(&self) -> Option<usize> {
        self.max_lines
    }

    /// Limits the text to `max_lines` lines, ending the last one with an
    /// ellipsis when content had to be dropped. `None` (the default) keeps
    /// every line.
    ///
    /// [`text`](Self::text) still reports the full content — only what is laid
    /// out and drawn is shortened — so the clamp can be raised or removed later
    /// without the original having been lost.
    ///
    /// Usually paired with [`set_max_width`](Self::set_max_width), but it also
    /// clamps text that is multi-line because it contains newlines.
    pub fn set_max_lines(&mut self, max_lines: Option<usize>) {
        if self.max_lines != max_lines {
            self.max_lines = max_lines;
            self.dirty = true;
        }
    }

    pub fn with_max_lines(mut self, max_lines: Option<usize>) -> Self {
        self.set_max_lines(max_lines);
        self
    }

    /// The number of lines the text occupies, after wrapping and clamping.
    pub fn line_count(&mut self, tcx: &mut TextContext) -> usize {
        self.ensure_layout(tcx);
        self.layout.len()
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

    /// The byte index of the character nearest `point`, which is in logical
    /// pixels relative to the text's own top-left corner — the same origin
    /// [`draw`](Self::draw) was given, so subtract that origin from a
    /// surface-local pointer position before calling this.
    ///
    /// The result is always on a cluster boundary, so it is safe to slice
    /// [`text`](Self::text) at. Points above or left of the text clamp to `0`
    /// and points past the end clamp to the content length.
    ///
    /// Indices are into the text that was laid out. That is
    /// [`text`](Self::text) unless a [`max_lines`](Self::set_max_lines) clamp
    /// replaced its tail with an ellipsis, so do not mix caret editing with a
    /// clamp on the same `Text`.
    pub fn index_at(&mut self, tcx: &mut TextContext, point: impl Into<Point>) -> usize {
        let point = point.into();
        let scale = self.ensure_layout(tcx);
        Cursor::from_point(
            &self.layout,
            (point.x * scale) as f32,
            (point.y * scale) as f32,
        )
        .index()
    }

    /// The caret rectangle for a byte index, in logical pixels relative to the
    /// text's top-left corner.
    ///
    /// The rect spans the full height of the line the caret is on and is
    /// [`CARET_WIDTH`] wide, so it can be filled as-is. Indices that land
    /// inside a multi-byte character or past the end snap to the nearest
    /// cluster boundary rather than panicking, which keeps a caret driven by
    /// byte arithmetic from ever splitting a grapheme.
    pub fn caret(&mut self, tcx: &mut TextContext, index: usize) -> Rect {
        let scale = self.ensure_layout(tcx);
        let cursor = Cursor::from_byte_index(&self.layout, index, Affinity::Downstream);
        let bounds = cursor.geometry(&self.layout, (CARET_WIDTH * scale) as f32);
        Rect::new(
            bounds.x0 / scale,
            bounds.y0 / scale,
            bounds.x1 / scale,
            bounds.y1 / scale,
        )
    }

    /// Rebuilds the layout if needed and returns the scale it is built at.
    fn ensure_layout(&mut self, tcx: &mut TextContext) -> f64 {
        if self.dirty || self.built_scale != tcx.scale() {
            self.rebuild(tcx);
        }
        self.built_scale as f64
    }

    fn rebuild(&mut self, tcx: &mut TextContext) {
        let scale = tcx.scale();
        // The wrap width is logical, but Parley breaks lines in the same space
        // it shapes in, which is physical pixels at the context's scale.
        let max_advance = self.max_width.map(|width| (width * scale as f64) as f32);

        self.display.clear();
        self.display.push_str(&self.content);
        self.build_layout(tcx, max_advance);
        if let Some(max_lines) = self.max_lines {
            self.clamp_lines(tcx, max_advance, max_lines);
        }

        self.built_scale = scale;
        self.dirty = false;
    }

    /// Lays `display` out into `layout`, breaking and aligning it to
    /// `max_advance` (already in physical pixels).
    fn build_layout(&mut self, tcx: &mut TextContext, max_advance: Option<f32>) {
        let TextContext {
            font_cx,
            layout_cx,
            scale,
        } = tcx;
        let scale = *scale;
        let style = &self.style;
        let alignment = style.alignment;

        let mut builder = layout_cx.ranged_builder(font_cx, &self.display, scale, true);
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
        builder.build_into(&mut self.layout, &self.display);

        self.layout.break_all_lines(max_advance);
        self.layout
            .align(max_advance, alignment, AlignmentOptions::default());
    }

    /// Shortens `display` until its layout fits in `max_lines` lines, ending it
    /// with an ellipsis.
    ///
    /// Parley 0.6 cannot truncate a layout in place, so the only honest way to
    /// do this is to lay the text out, read where the last line we may keep
    /// ends, and lay out a shortened copy — one that gives up a character to
    /// make room for the ellipsis, because a line that was exactly full has no
    /// room for one and would simply wrap it onto the line we just cut.
    ///
    /// That replacement can itself overflow, so this loops; see
    /// [`MAX_TRUNCATION_PASSES`] for why the loop is bounded and why the
    /// fallback is exact.
    fn clamp_lines(&mut self, tcx: &mut TextContext, max_advance: Option<f32>, max_lines: usize) {
        if max_lines == 0 {
            self.display.clear();
            self.build_layout(tcx, max_advance);
            return;
        }

        for _ in 0..MAX_TRUNCATION_PASSES {
            if self.layout.len() <= max_lines {
                return;
            }
            let Some(keep) = self.last_kept_line_end(max_lines) else {
                return;
            };
            self.display.truncate(keep);
            trim_end(&mut self.display);
            self.display.pop();
            trim_end(&mut self.display);
            self.display.push(ELLIPSIS);
            self.build_layout(tcx, max_advance);
        }

        if self.layout.len() > max_lines {
            // Drop the ellipsis rather than the clamp: the text of the first
            // `max_lines` lines re-breaks into exactly those lines again, so
            // this always terminates the clamp, it just cannot signal that
            // something was cut.
            if let Some(keep) = self.last_kept_line_end(max_lines) {
                self.display.truncate(keep);
                self.build_layout(tcx, max_advance);
            }
        }
    }

    /// Byte offset into `display` one past the end of line `max_lines - 1`.
    fn last_kept_line_end(&self, max_lines: usize) -> Option<usize> {
        self.layout
            .get(max_lines - 1)
            .map(|line| line.text_range().end)
    }
}

/// Drops trailing whitespace in place, so the ellipsis sits against the text
/// rather than after the space the line break consumed.
fn trim_end(text: &mut String) {
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
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
                        id: glyph.id,
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
        assert!(
            (t[0] - 20.0).abs() < 1e-3 && (t[1] - 40.0).abs() < 1e-3,
            "{t:?}"
        );

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

    /// How far alignment shifted the first line inside the wrap width.
    fn first_line_offset(text: &mut Text, tcx: &mut TextContext) -> Option<f32> {
        text.layout(tcx);
        text.layout.get(0).map(|line| line.metrics().offset)
    }

    /// A body long enough to need several lines in any reasonable column.
    const PARAGRAPH: &str =
        "The quick brown fox jumps over the lazy dog while the compositor waits for a frame.";

    #[test]
    fn text_without_a_wrap_width_stays_on_one_line() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH);

        assert_eq!(text.max_width(), None, "wrapping must be opt-in");
        assert_eq!(
            text.line_count(&mut tcx),
            1,
            "unwrapped text laid out as {} lines",
            text.line_count(&mut tcx)
        );
    }

    #[test]
    fn a_wrap_width_breaks_the_text_into_narrower_lines() {
        let mut tcx = TextContext::new();

        let mut unwrapped = Text::new(PARAGRAPH);
        let natural = unwrapped.width(&mut tcx);

        let mut wrapped = Text::new(PARAGRAPH).with_max_width(Some(140.0));
        let lines = wrapped.line_count(&mut tcx);
        let size = wrapped.size(&mut tcx);

        assert!(lines > 1, "expected the text to wrap, got {lines} line(s)");
        assert!(
            size.width <= 140.0,
            "wrapped width {} exceeds the 140.0 wrap width",
            size.width
        );
        assert!(
            size.width < natural,
            "wrapped width {} is not narrower than the unwrapped {natural}",
            size.width
        );
        assert!(
            size.height > unwrapped.height(&mut tcx),
            "wrapping to {lines} lines did not make the text taller: {}",
            size.height
        );
    }

    #[test]
    fn changing_the_wrap_width_invalidates_the_layout() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH);
        text.layout(&mut tcx);

        text.set_max_width(None);
        assert!(!text.dirty, "setting the width it already had invalidated");

        text.set_max_width(Some(140.0));
        assert!(text.dirty, "a new wrap width must invalidate");

        let narrow = text.line_count(&mut tcx);
        text.set_max_width(Some(70.0));
        let narrower = text.line_count(&mut tcx);
        assert!(
            narrower > narrow,
            "halving the wrap width gave {narrower} lines, not more than {narrow}"
        );
    }

    #[test]
    fn max_lines_clamps_the_layout_and_marks_the_cut_with_an_ellipsis() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH)
            .with_max_width(Some(140.0))
            .with_max_lines(Some(2));

        let lines = text.line_count(&mut tcx);
        assert_eq!(lines, 2, "expected the clamp to hold, got {lines} lines");
        assert!(
            text.display.ends_with(ELLIPSIS),
            "clamped text did not end in an ellipsis: {:?}",
            text.display
        );
        assert!(
            text.display.len() < PARAGRAPH.len(),
            "clamped text was not shortened: {:?}",
            text.display
        );
        assert_eq!(
            text.text(),
            PARAGRAPH,
            "the clamp must not destroy the content"
        );
    }

    #[test]
    fn max_lines_leaves_text_that_already_fits_alone() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("Hello").with_max_lines(Some(4));

        assert_eq!(text.line_count(&mut tcx), 1);
        assert_eq!(
            text.display, "Hello",
            "unclamped text was rewritten to {:?}",
            text.display
        );
    }

    #[test]
    fn max_lines_clamps_text_broken_by_newlines() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("one\ntwo\nthree\nfour").with_max_lines(Some(2));

        let lines = text.line_count(&mut tcx);
        assert_eq!(lines, 2, "expected 2 lines, got {lines}");
        assert!(
            text.display.ends_with(ELLIPSIS),
            "expected an ellipsis, got {:?}",
            text.display
        );
    }

    #[test]
    fn a_zero_line_clamp_lays_out_nothing() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH).with_max_lines(Some(0));

        assert!(
            text.display.is_empty(),
            "expected empty display text, got {:?}",
            text.display
        );
        assert_eq!(
            text.width(&mut tcx),
            0.0,
            "expected zero width, got {}",
            text.width(&mut tcx)
        );
    }

    /// The wrap width is quoted in logical pixels, so raising the scale must
    /// change only how finely the same lines are rasterised. Getting this wrong
    /// re-wraps every notification body the moment it moves to a HiDPI output.
    #[test]
    fn the_wrap_width_is_logical_not_physical() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH).with_max_width(Some(140.0));

        let logical = text.size(&mut tcx);
        let lines = text.line_count(&mut tcx);

        tcx.set_scale(2.0);
        let scaled = text.size(&mut tcx);
        assert_eq!(text.built_scale, 2.0, "raising the scale re-lays out");

        assert_eq!(
            text.line_count(&mut tcx),
            lines,
            "line count moved from {lines} at scale 2.0"
        );
        assert!(
            (scaled.width - logical.width).abs() < 2.0,
            "logical width moved: {} -> {}",
            logical.width,
            scaled.width
        );
        assert!(
            (scaled.height - logical.height).abs() < 2.0,
            "logical height moved: {} -> {}",
            logical.height,
            scaled.height
        );
        assert!(
            text.layout.width() as f64 > logical.width * 1.5,
            "the physical layout did not grow with the scale: {}",
            text.layout.width()
        );
        assert!(
            text.layout.width() <= 140.0 * 2.0,
            "the physical layout overflowed the doubled wrap width: {}",
            text.layout.width()
        );
    }

    #[test]
    fn centring_moves_lines_without_changing_the_measured_width() {
        let mut tcx = TextContext::new();
        let style = TextStyle::default();

        let mut start = Text::styled(PARAGRAPH, style.clone()).with_max_width(Some(140.0));
        let mut centred = Text::styled(PARAGRAPH, style.with_alignment(Alignment::Center))
            .with_max_width(Some(140.0));

        assert_eq!(start.line_count(&mut tcx), centred.line_count(&mut tcx));
        assert_eq!(
            start.size(&mut tcx),
            centred.size(&mut tcx),
            "alignment must not change the measured size"
        );

        let start_offset = first_line_offset(&mut start, &mut tcx);
        let centred_offset = first_line_offset(&mut centred, &mut tcx);
        assert_eq!(
            start_offset,
            Some(0.0),
            "start-aligned text was offset by {start_offset:?}"
        );
        assert!(
            centred_offset.is_some_and(|offset| offset > 0.0),
            "centring did not offset the first line: {centred_offset:?}"
        );
    }

    #[test]
    fn the_caret_walks_left_to_right_across_the_text() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("Hello");

        let start = text.caret(&mut tcx, 0);
        let end = text.caret(&mut tcx, 5);
        assert!(
            start.x0 < end.x0,
            "caret did not advance: {} -> {}",
            start.x0,
            end.x0
        );
        assert!(
            end.height() > 0.0 && end.width() > 0.0,
            "expected a fillable caret rect, got {end:?}"
        );
        assert!(
            end.x1 <= text.width(&mut tcx) + 1.0,
            "the end caret {} sits outside the text width {}",
            end.x1,
            text.width(&mut tcx)
        );
    }

    #[test]
    fn index_at_round_trips_with_caret_over_multi_byte_characters() {
        let mut tcx = TextContext::new();
        // "héllo wörld": é and ö are two bytes each, so byte indices and
        // character counts diverge from index 2 onwards.
        let content = "héllo wörld";
        let mut text = Text::new(content);

        for (index, _) in content.char_indices() {
            let rect = text.caret(&mut tcx, index);
            // A quarter of a pixel into the cluster that starts here, which is
            // well inside its leading half for any readable font size.
            let probe = (rect.x0 + 0.25, rect.center().y);
            assert_eq!(
                text.index_at(&mut tcx, probe),
                index,
                "probing just right of the caret for byte {index} of {content:?} \
                 did not come back to it"
            );
        }

        let end = text.caret(&mut tcx, content.len());
        assert_eq!(
            text.index_at(&mut tcx, (end.x0 - 0.25, end.center().y)),
            content.len(),
            "probing just left of the trailing caret did not report the end"
        );
    }

    #[test]
    fn a_caret_inside_a_multi_byte_character_snaps_to_its_start() {
        let mut tcx = TextContext::new();
        let mut text = Text::new("héllo");

        // 'é' occupies bytes 1..3, so 2 is inside it.
        assert_eq!(
            text.caret(&mut tcx, 2),
            text.caret(&mut tcx, 1),
            "a mid-grapheme index did not snap to the grapheme start"
        );
    }

    #[test]
    fn index_at_clamps_to_the_ends_of_the_text() {
        let mut tcx = TextContext::new();
        let content = "Hello";
        let mut text = Text::new(content);
        let height = text.height(&mut tcx);

        assert_eq!(
            text.index_at(&mut tcx, (-1000.0, -1000.0)),
            0,
            "a point far above and left of the text did not clamp to 0"
        );
        assert_eq!(
            text.index_at(&mut tcx, (1000.0, height * 2.0)),
            content.len(),
            "a point past the end did not clamp to the content length"
        );
    }

    #[test]
    fn carets_land_on_the_line_the_wrapped_text_put_them_on() {
        let mut tcx = TextContext::new();
        let mut text = Text::new(PARAGRAPH).with_max_width(Some(140.0));
        assert!(text.line_count(&mut tcx) > 1);

        let first = text.caret(&mut tcx, 0);
        let last = text.caret(&mut tcx, PARAGRAPH.len());
        assert!(
            last.y0 > first.y0,
            "the trailing caret stayed on the first line: {} vs {}",
            last.y0,
            first.y0
        );
        assert!(
            last.y1 <= text.height(&mut tcx) + 1.0,
            "the trailing caret {} fell outside the text height {}",
            last.y1,
            text.height(&mut tcx)
        );
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
