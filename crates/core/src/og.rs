//! Social card images: 1200×630 PNGs for link previews, rendered in pure Rust.
//!
//! [`render`] draws a [`Card`] (product name, a short label, the title and an
//! optional description) with the embedded Inter font: `harfrust` shapes the
//! text (kerning included), `skrifa` supplies glyph outlines and `tiny-skia`
//! rasterizes. The title wraps to at most three lines and shrinks to fit, like
//! a video thumbnail: few, large words. Render once (at startup) and serve the
//! bytes; drawing a card takes milliseconds, not microseconds.

use anyhow::{Context, Result};
use harfrust::font::{Font, Variation};
use harfrust::{Buffer, ShapeOptions, ShaperFont, Tag};
use skrifa::instance::{Location, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::sync::LazyLock;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 630;
const MARGIN: f32 = 80.0;

// Inter by Rasmus Andersson, SIL Open Font License 1.1 (fonts/OFL.txt).
static INTER: &[u8] = include_bytes!("../fonts/InterVariable.ttf");

/// Colors as `0xRRGGBB`.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
}

impl Theme {
    /// Warm near-black with a rust accent.
    pub const DARK: Theme = Theme { background: 0x0c0a09, text: 0xf5f5f4, muted: 0xa8a29e, accent: 0xe8794a };
}

/// What one card says.
#[derive(Clone, Copy, Debug, Default)]
pub struct Card<'a> {
    /// Product name, top left.
    pub site_name: &'a str,
    /// Short kicker above the title, e.g. "Blog" or "Docs · Features".
    pub label: &'a str,
    /// The headline. Wrapped to at most three lines, shrinking to fit.
    pub title: &'a str,
    /// Optional one-line summary under a short title; omitted when the title needs the room.
    pub description: Option<&'a str>,
}

/// One weight of Inter: a shaping instance and the matching outline location.
struct Face {
    shaper: Font,
    location: Location,
    upem: f32,
}

impl Face {
    fn new(weight: f32) -> Self {
        let settings = [("wght", weight), ("opsz", 32.0)];
        let base = Font::new(INTER, 0).expect("embedded Inter is a valid font");
        let shaper = base
            .instance_builder()
            .variations(settings.map(|(tag, value)| Variation::new(Tag::new_checked(tag.as_bytes()).unwrap(), value)))
            .build();
        let font = FontRef::new(INTER).expect("embedded Inter is a valid font");
        Face { upem: f32::from(shaper.units_per_em()), shaper, location: font.axes().location(settings) }
    }

    /// Glyphs and pen x positions (px) for one line; returns them with the line width.
    fn shape(&self, text: &str, size: f32, tracking: f32) -> (Vec<(GlyphId, f32, f32)>, f32) {
        let shaper = ShaperFont::new(&self.shaper);
        let mut buffer = Buffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        if harfrust::shape(&shaper, &mut buffer, ShapeOptions::new()).is_err() {
            return (Vec::new(), 0.0);
        }
        let scale = size / self.upem;
        let mut x = 0.0;
        let glyphs = buffer
            .glyph_infos()
            .iter()
            .zip(buffer.glyph_positions())
            .map(|(info, pos)| {
                let glyph = (GlyphId::new(info.glyph_id), x + pos.x_offset as f32 * scale, pos.y_offset as f32 * scale);
                x += pos.x_advance as f32 * scale + tracking * size;
                glyph
            })
            .collect::<Vec<_>>();
        let width = if glyphs.is_empty() { 0.0 } else { x - tracking * size };
        (glyphs, width)
    }

    fn width(&self, text: &str, size: f32, tracking: f32) -> f32 {
        self.shape(text, size, tracking).1
    }

    /// Fill `text` with its left edge at `x` and its baseline at `y`.
    fn draw(
        &self,
        pixmap: &mut Pixmap,
        text: &str,
        (size, tracking): (f32, f32),
        (x, baseline): (f32, f32),
        color: u32,
    ) {
        let font = FontRef::new(INTER).expect("embedded Inter is a valid font");
        let outlines = font.outline_glyphs();
        let mut pen = Pen { builder: PathBuilder::new(), dx: 0.0, dy: 0.0 };
        for (glyph, gx, gy) in self.shape(text, size, tracking).0 {
            let Some(outline) = outlines.get(glyph) else { continue };
            pen.dx = x + gx;
            pen.dy = baseline - gy;
            let _ = outline.draw(DrawSettings::unhinted(Size::new(size), &self.location), &mut pen);
        }
        if let Some(path) = pen.builder.finish() {
            pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
        }
    }
}

/// Font outlines are y-up; the pixmap is y-down.
struct Pen {
    builder: PathBuilder,
    dx: f32,
    dy: f32,
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.dx + x, self.dy - y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.dx + x, self.dy - y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.builder.quad_to(self.dx + cx, self.dy - cy, self.dx + x, self.dy - y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(self.dx + cx0, self.dy - cy0, self.dx + cx1, self.dy - cy1, self.dx + x, self.dy - y);
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

struct Faces {
    black: Face,
    bold: Face,
}

static FACES: LazyLock<Faces> = LazyLock::new(|| Faces { black: Face::new(900.0), bold: Face::new(700.0) });

fn paint(rgb: u32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255));
    paint.anti_alias = true;
    paint
}

/// Greedy word wrap with no line limit.
fn lines(face: &Face, text: &str, size: f32, tracking: f32, max_width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if face.width(&format!("{line} {word}"), size, tracking) <= max_width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// Word wrap; `None` if the text needs more than `max_lines` or a word is too wide.
fn wrap(face: &Face, text: &str, size: f32, tracking: f32, max_width: f32, max_lines: usize) -> Option<Vec<String>> {
    let lines = lines(face, text, size, tracking, max_width);
    let fits = lines.len() <= max_lines && lines.iter().all(|line| face.width(line, size, tracking) <= max_width);
    fits.then_some(lines)
}

/// Word wrap cut to `max_lines`, ending in an ellipsis when text was dropped.
fn clamp(face: &Face, text: &str, size: f32, tracking: f32, max_width: f32, max_lines: usize) -> Vec<String> {
    let mut lines = lines(face, text, size, tracking, max_width);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = &mut lines[max_lines - 1];
        while !last.is_empty() && face.width(&format!("{last}…"), size, tracking) > max_width {
            last.pop();
        }
        *last = format!("{}…", last.trim_end_matches([' ', ',', '.', ':', ';']));
    }
    lines
}

const TITLE_SIZES: [f32; 9] = [144.0, 128.0, 112.0, 100.0, 90.0, 80.0, 72.0, 64.0, 56.0];
const TITLE_TRACKING: f32 = -0.035;
const TITLE_LINE: f32 = 1.04;
const LABEL: (f32, f32) = (30.0, 0.08);
const LABEL_GAP: f32 = 34.0;
const DESCRIPTION: (f32, f32) = (36.0, -0.01);
const DESCRIPTION_LINE: f32 = 46.0;
const DESCRIPTION_GAP: f32 = 40.0;
/// Vertical room for label, title and description, between the header and the bottom rule.
const BODY: (f32, f32) = (150.0, 575.0);
const BODY_MAX_HEIGHT: f32 = 375.0;
/// Glyph proportions of Inter: cap height above the baseline, descender depth below it.
const CAP: f32 = 0.73;
const DESCENT: f32 = 0.22;

/// Wrapped title and description at the chosen title size.
struct Layout {
    title: Vec<String>,
    size: f32,
    description: Vec<String>,
}

impl Layout {
    /// From the label's cap height to the last line's descenders.
    fn height(&self) -> f32 {
        let mut height = LABEL.0 * CAP + LABEL_GAP;
        height += self.size * (CAP + TITLE_LINE * (self.title.len() as f32 - 1.0) + DESCENT);
        if !self.description.is_empty() {
            height += DESCRIPTION_GAP + DESCRIPTION.0 * (CAP + DESCENT);
            height += DESCRIPTION_LINE * (self.description.len() as f32 - 1.0);
        }
        height
    }
}

/// The largest title that fits, keeping the description under short titles
/// when there is room; otherwise the smallest title, cut with an ellipsis.
fn layout(faces: &Faces, card: &Card<'_>, max_width: f32) -> Layout {
    let description = |size: f32| {
        let text = card.description.filter(|_| size >= 72.0)?;
        Some(clamp(&faces.bold, text, DESCRIPTION.0, DESCRIPTION.1, max_width, 2))
    };
    for with_description in [true, false] {
        for size in TITLE_SIZES {
            let Some(title) = wrap(&faces.black, card.title, size, TITLE_TRACKING, max_width, 3) else { continue };
            let description = if with_description && title.len() <= 2 { description(size) } else { None };
            if with_description && description.is_none() && card.description.is_some() {
                continue;
            }
            let layout = Layout { title, size, description: description.unwrap_or_default() };
            if layout.height() <= BODY_MAX_HEIGHT {
                return layout;
            }
        }
    }
    let size = TITLE_SIZES[TITLE_SIZES.len() - 1];
    Layout { title: clamp(&faces.black, card.title, size, TITLE_TRACKING, max_width, 3), size, description: Vec::new() }
}

/// Render a card to PNG bytes.
pub fn render(card: &Card<'_>, theme: &Theme) -> Result<Vec<u8>> {
    let faces = &*FACES;
    let mut pixmap = Pixmap::new(WIDTH, HEIGHT).context("allocating the card")?;
    let bg = theme.background;
    pixmap.fill(Color::from_rgba8((bg >> 16) as u8, (bg >> 8) as u8, bg as u8, 255));

    // Header: accent mark and product name.
    let mark = Rect::from_xywh(MARGIN, 66.0, 40.0, 40.0).context("mark")?;
    pixmap.fill_rect(mark, &paint(theme.accent), Transform::identity(), None);
    faces.bold.draw(&mut pixmap, card.site_name, (36.0, -0.02), (MARGIN + 60.0, 99.0), theme.text);

    // Label, title and description, centered vertically in the body.
    let layout = layout(faces, card, WIDTH as f32 - 2.0 * MARGIN);
    let size = layout.size;
    let mut baseline = BODY.0 + ((BODY.1 - BODY.0) - layout.height()) / 2.0 + LABEL.0 * CAP;
    faces.bold.draw(&mut pixmap, &card.label.to_uppercase(), LABEL, (MARGIN, baseline), theme.accent);
    baseline += LABEL_GAP + size * CAP;
    for (i, line) in layout.title.iter().enumerate() {
        if i > 0 {
            baseline += size * TITLE_LINE;
        }
        // Heavy glyphs carry side bearing; nudge left so the title aligns optically.
        faces.black.draw(&mut pixmap, line, (size, TITLE_TRACKING), (MARGIN - size * 0.04, baseline), theme.text);
    }
    baseline += size * DESCENT + DESCRIPTION_GAP + DESCRIPTION.0 * CAP;
    for (i, line) in layout.description.iter().enumerate() {
        if i > 0 {
            baseline += DESCRIPTION_LINE;
        }
        faces.bold.draw(&mut pixmap, line, DESCRIPTION, (MARGIN, baseline), theme.muted);
    }

    // Accent rule along the bottom edge.
    let rule = Rect::from_xywh(0.0, HEIGHT as f32 - 10.0, WIDTH as f32, 10.0).context("rule")?;
    pixmap.fill_rect(rule, &paint(theme.accent), Transform::identity(), None);

    pixmap.encode_png().context("encoding the card")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(title: &str) -> Card<'_> {
        Card { site_name: "Example", label: "Docs", title, description: Some("A short summary.") }
    }

    #[test]
    fn renders_a_png_at_card_size() {
        let png = render(&card("Getting started"), &Theme::DARK).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), WIDTH);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), HEIGHT);
    }

    #[test]
    fn titles_shrink_wrap_and_make_room() {
        let faces = &*FACES;
        let max_width = WIDTH as f32 - 2.0 * MARGIN;
        let short = layout(faces, &card("Passkeys"), max_width);
        let long_title = "Postgres sessions and background jobs ".repeat(8);
        let long = layout(faces, &card(&long_title), max_width);
        assert_eq!(short.title.len(), 1);
        assert_eq!(short.description, ["A short summary."]);
        assert!(long.title.len() <= 3 && long.size < short.size && long.description.is_empty());
        assert!(long.title.iter().all(|line| faces.black.width(line, long.size, TITLE_TRACKING) <= max_width));
        assert!(long.title.last().unwrap().ends_with('…'));
        for title in ["Passkeys", "Background jobs", "Email and password authentication in Axum done right"] {
            assert!(layout(faces, &card(title), max_width).height() <= BODY_MAX_HEIGHT, "{title}");
        }
    }
}
