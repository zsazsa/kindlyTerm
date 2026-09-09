//! Font loading (fontdb), glyph rasterization (swash), and a CPU-side glyph
//! atlas that the renderer uploads to the GPU.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use fontdb::{Database, Family, Query, Source, Style, Weight};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source as RenderSource, StrikeWith};
use swash::zeno::Format;
use swash::{CacheKey, FontRef, GlyphId};

pub const ATLAS_SIZE: u32 = 2048;

/// A font face kept in memory. `FontRef` borrows `data`, so we rebuild it on
/// demand from the stored offset and cache key (cheap).
#[derive(Clone)]
pub struct LoadedFont {
    data: Arc<Vec<u8>>,
    offset: u32,
    key: CacheKey,
}

impl LoadedFont {
    fn from_source(src: &Source, index: u32) -> Result<Self> {
        let data: Arc<Vec<u8>> = match src {
            Source::File(path) => Arc::new(std::fs::read(path)?),
            Source::Binary(b) | Source::SharedFile(_, b) => Arc::new(b.as_ref().as_ref().to_vec()),
        };
        let font = FontRef::from_index(&data, index as usize).ok_or_else(|| anyhow!("bad font"))?;
        let (offset, key) = (font.offset, font.key);
        Ok(Self { data, offset, key })
    }

    pub fn as_ref(&self) -> FontRef<'_> {
        FontRef { data: &self.data, offset: self.offset, key: self.key }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub c: char,
    pub bold: bool,
    pub italic: bool,
    /// Pixel size in tenths of a pixel; 0 means the terminal's base size.
    pub size10: u16,
}

impl GlyphKey {
    pub fn cell(c: char, bold: bool, italic: bool) -> Self {
        Self { c, bold, italic, size10: 0 }
    }
    pub fn sized(c: char, size_px: f32, bold: bool) -> Self {
        Self { c, bold, italic: false, size10: (size_px * 10.0).round().max(10.0) as u16 }
    }
    /// Cell glyph at an explicit pixel size (canvas zoom). `zoom == 1`
    /// uses the atlas' base-size entries.
    pub fn cell_zoomed(c: char, bold: bool, italic: bool, base_px: f32, zoom: f32) -> Self {
        if (zoom - 1.0).abs() < 1e-3 {
            Self::cell(c, bold, italic)
        } else {
            Self { c, bold, italic, size10: (base_px * zoom * 10.0).round().max(10.0) as u16 }
        }
    }
    fn px(&self, base: f32) -> f32 {
        if self.size10 == 0 { base } else { self.size10 as f32 / 10.0 }
    }
}

/// Where a rasterized glyph lives in the atlas and how to place it in a cell.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Atlas rect in pixels.
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Offset from the cell's baseline-left origin.
    pub left: i32,
    pub top: i32,
    /// True for color bitmaps (emoji) which are not tinted.
    pub colored: bool,
}

/// Pixel metrics of one terminal cell, derived from the primary font.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell to the baseline.
    pub ascent: f32,
    pub underline_pos: f32,
    pub underline_thickness: f32,
    pub strikeout_pos: f32,
}

/// Ask fontconfig which family a pattern resolves to. None if fc-match is
/// missing or fails.
fn fc_match(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["--format=%{family[0]}", pattern])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// A trivial shelf packer: rows of glyphs, left to right, top to bottom.
struct Shelf {
    x: u32,
    y: u32,
    row_h: u32,
}

pub struct FontSystem {
    db: Database,
    /// [regular, bold, italic, bold-italic]
    faces: [LoadedFont; 4],
    /// Fallback faces discovered per character.
    fallbacks: Vec<LoadedFont>,
    fallback_ids: Vec<fontdb::ID>,
    /// Font family the user asked for (after alias resolution).
    pub family: String,
    /// Unscaled metrics of the primary face, for sizing UI text.
    base_metrics: swash::Metrics,
    scale: ScaleContext,
    pub size_px: f32,
    pub metrics: CellMetrics,

    glyphs: HashMap<GlyphKey, Option<Glyph>>,
    shelf: Shelf,
    /// RGBA8, ATLAS_SIZE x ATLAS_SIZE.
    pub atlas: Vec<u8>,
    /// Dirty rects (x, y, w, h) awaiting upload.
    pub dirty: Vec<(u32, u32, u32, u32)>,
}

impl FontSystem {
    pub fn new(family: &str, size_px: f32, line_padding: f32) -> Result<Self> {
        let mut db = Database::new();
        db.load_system_fonts();
        // Resolve the generic "monospace" alias the same way every other app
        // on the system does: ask fontconfig. fontdb's own alias parsing can
        // disagree with `fc-match` (it picked FreeMono over DejaVu here).
        let family = if family.eq_ignore_ascii_case("monospace") {
            fc_match("monospace").unwrap_or_else(|| "monospace".to_string())
        } else {
            family.to_string()
        };
        let family = family.as_str();
        db.set_monospace_family(family);
        log::info!("monospace resolves to {:?}", db.family_name(&Family::Monospace));

        let load = |weight: Weight, style: Style| -> Result<LoadedFont> {
            let families = [
                Family::Name(family),
                Family::Monospace,
                Family::Name("DejaVu Sans Mono"),
                Family::Name("Liberation Mono"),
                Family::Name("Noto Sans Mono"),
                Family::Name("JetBrains Mono"),
                Family::Name("Fira Mono"),
                Family::Name("Hack"),
                Family::Name("Ubuntu Mono"),
                Family::Name("Source Code Pro"),
                Family::Name("FreeMono"),
            ];
            let query = Query { families: &families, weight, style, ..Query::default() };
            let id = db.query(&query).ok_or_else(|| anyhow!("no monospace font found"))?;
            let face = db.face(id).context("face vanished")?;
            LoadedFont::from_source(&face.source, face.index)
        };
        let regular = load(Weight::NORMAL, Style::Normal)?;
        let bold = load(Weight::BOLD, Style::Normal).unwrap_or_else(|_| regular.clone());
        let italic = load(Weight::NORMAL, Style::Italic).unwrap_or_else(|_| regular.clone());
        let bold_italic = load(Weight::BOLD, Style::Italic).unwrap_or_else(|_| bold.clone());

        let metrics = Self::compute_metrics(&regular, size_px, line_padding);
        let base_metrics = regular.as_ref().metrics(&[]);
        log::info!(
            "font {:?} @ {size_px}px -> cell {:.1}x{:.1}",
            family,
            metrics.width,
            metrics.height
        );

        Ok(Self {
            db,
            faces: [regular, bold, italic, bold_italic],
            fallbacks: Vec::new(),
            fallback_ids: Vec::new(),
            family: family.to_string(),
            base_metrics,
            scale: ScaleContext::new(),
            size_px,
            metrics,
            glyphs: HashMap::new(),
            shelf: Shelf { x: 1, y: 1, row_h: 0 },
            atlas: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
            dirty: Vec::new(),
        })
    }

    /// Installed monospace families, sorted, with the current one first.
    pub fn monospace_families(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .db
            .faces()
            .filter(|f| f.monospaced)
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        names.sort();
        names.dedup();
        names.retain(|n| !n.contains("CJK") && !n.contains("Emoji") && !n.contains("Symbols") && !n.starts_with("Noto Sans Mono,"));
        if let Some(pos) = names.iter().position(|n| n.eq_ignore_ascii_case(&self.family)) {
            let cur = names.remove(pos);
            names.insert(0, cur);
        }
        names
    }

    /// Baseline offset from the top of a line box of `size_px` text.
    pub fn ascent_for(&self, size_px: f32) -> f32 {
        let m = self.base_metrics.scale(size_px);
        (m.ascent).round()
    }

    /// Natural line height for `size_px` text.
    pub fn line_height_for(&self, size_px: f32) -> f32 {
        let m = self.base_metrics.scale(size_px);
        (m.ascent + m.descent + m.leading).ceil()
    }

    /// Horizontal advance for `size_px` text (monospace, so one value).
    pub fn advance_for(&self, size_px: f32) -> f32 {
        (self.metrics.width * size_px / self.size_px).max(1.0)
    }

    /// Change the pixel size. Clears the glyph cache.
    pub fn set_size(&mut self, size_px: f32, line_padding: f32) {
        self.size_px = size_px;
        self.metrics = Self::compute_metrics(&self.faces[0], size_px, line_padding);
        self.glyphs.clear();
        self.shelf = Shelf { x: 1, y: 1, row_h: 0 };
        self.atlas.iter_mut().for_each(|b| *b = 0);
        self.dirty.clear();
        self.dirty.push((0, 0, ATLAS_SIZE, ATLAS_SIZE));
    }

    fn compute_metrics(font: &LoadedFont, size_px: f32, line_padding: f32) -> CellMetrics {
        let f = font.as_ref();
        let m = f.metrics(&[]).scale(size_px);
        let gm = f.glyph_metrics(&[]).scale(size_px);
        // Advance of a representative glyph; fall back to average width.
        let gid = f.charmap().map('M');
        let mut width = gm.advance_width(gid);
        if width <= 0.0 {
            width = m.average_width.max(size_px * 0.6);
        }
        let height = (m.ascent + m.descent + m.leading + line_padding).ceil();
        let ascent = (m.ascent + line_padding / 2.0).round();
        let ul_thick = (m.stroke_size.max(1.0)).round();
        CellMetrics {
            width: width.round().max(1.0),
            height: height.max(1.0),
            ascent,
            underline_pos: (ascent - m.underline_offset).round(),
            underline_thickness: ul_thick,
            strikeout_pos: (ascent - m.strikeout_offset).round(),
        }
    }

    /// Fetch (rasterizing on first use) the glyph for a character.
    pub fn glyph(&mut self, key: GlyphKey) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let g = self.rasterize(key);
        self.glyphs.insert(key, g);
        g
    }

    fn style_index(key: GlyphKey) -> usize {
        match (key.bold, key.italic) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }
    }

    /// Find a font (primary style first, then fallbacks) that has the char.
    fn find_font(&mut self, key: GlyphKey) -> Option<(LoadedFont, GlyphId)> {
        let primary = &self.faces[Self::style_index(key)];
        let gid = primary.as_ref().charmap().map(key.c);
        if gid != 0 {
            return Some((primary.clone(), gid));
        }
        for fb in &self.fallbacks {
            let gid = fb.as_ref().charmap().map(key.c);
            if gid != 0 {
                return Some((fb.clone(), gid));
            }
        }
        // Preferred symbol/text fallbacks first, so UI glyphs like ⚙ and ☁
        // come out as monochrome text rather than color emoji.
        let is_emoji_presentation = matches!(key.c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x26FF if key.c as u32 >= 0x1F300);
        if !is_emoji_presentation {
            for fam in ["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols", "Symbola", "Noto Sans Math", "FreeSerif"] {
                let families = [Family::Name(fam)];
                let q = Query { families: &families, ..Query::default() };
                if let Some(id) = self.db.query(&q) {
                    if self.fallback_ids.contains(&id) {
                        continue;
                    }
                    if let Some(face) = self.db.face(id)
                        && let Ok(lf) = LoadedFont::from_source(&face.source, face.index) {
                            let gid = lf.as_ref().charmap().map(key.c);
                            if gid != 0 {
                                self.fallback_ids.push(id);
                                self.fallbacks.push(lf.clone());
                                return Some((lf, gid));
                            }
                        }
                }
            }
        }
        // Slow path: scan the system font database once for this char.
        let mut found: Option<(fontdb::ID, LoadedFont, GlyphId)> = None;
        for face in self.db.faces() {
            if self.fallback_ids.contains(&face.id) {
                continue;
            }
            // Only consider fonts that plausibly cover the char cheaply:
            // load, check charmap, drop if miss.
            if let Ok(lf) = LoadedFont::from_source(&face.source, face.index) {
                let gid = lf.as_ref().charmap().map(key.c);
                if gid != 0 {
                    // Prefer monospace / emoji fonts, but accept the first hit.
                    found = Some((face.id, lf, gid));
                    break;
                }
            }
        }
        if let Some((id, lf, gid)) = found {
            log::debug!("fallback font for {:?}: {:?}", key.c, self.db.face(id).map(|f| f.post_script_name.clone()));
            self.fallback_ids.push(id);
            self.fallbacks.push(lf.clone());
            return Some((lf, gid));
        }
        None
    }

    fn rasterize(&mut self, key: GlyphKey) -> Option<Glyph> {
        let (font, gid) = self.find_font(key)?;
        let fref = font.as_ref();
        let px = key.px(self.size_px);
        let mut scaler = self.scale.builder(fref).size(px).hint(true).build();
        let image = Render::new(&[
            RenderSource::ColorOutline(0),
            RenderSource::ColorBitmap(StrikeWith::BestFit),
            RenderSource::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, gid)?;

        let (w, h) = (image.placement.width, image.placement.height);
        if w == 0 || h == 0 {
            // Blank glyph (space etc.): cache as "nothing to draw".
            return None;
        }

        // Bitmap emoji may come back at a different size than requested;
        // scale down into the cell if needed by nearest-neighbour sampling.
        let colored = matches!(image.content, Content::Color);
        let (mut rgba, mut w, mut h, mut left, mut top) = (Vec::new(), w, h, image.placement.left, image.placement.top);
        match image.content {
            Content::Mask => {
                rgba.reserve((w * h * 4) as usize);
                for &a in &image.data {
                    rgba.extend_from_slice(&[255, 255, 255, a]);
                }
            }
            Content::SubpixelMask => {
                for px in image.data.chunks(4) {
                    let a = ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8;
                    rgba.extend_from_slice(&[255, 255, 255, a]);
                }
            }
            Content::Color => {
                rgba = image.data.clone();
                let max_h = if key.size10 == 0 { self.metrics.height.floor() as u32 } else { px.ceil() as u32 };
                if h > max_h {
                    let s = max_h as f32 / h as f32;
                    let nw = ((w as f32 * s).round() as u32).max(1);
                    let nh = max_h.max(1);
                    let mut out = vec![0u8; (nw * nh * 4) as usize];
                    for y in 0..nh {
                        for x in 0..nw {
                            let sx = ((x as f32 / s) as u32).min(w - 1);
                            let sy = ((y as f32 / s) as u32).min(h - 1);
                            let si = ((sy * w + sx) * 4) as usize;
                            let di = ((y * nw + x) * 4) as usize;
                            out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                        }
                    }
                    rgba = out;
                    left = (left as f32 * s) as i32;
                    top = (top as f32 * s) as i32;
                    w = nw;
                    h = nh;
                }
            }
        }

        let (x, y) = self.alloc(w, h)?;
        for row in 0..h {
            let src = ((row * w) * 4) as usize;
            let dst = (((y + row) * ATLAS_SIZE + x) * 4) as usize;
            self.atlas[dst..dst + (w * 4) as usize].copy_from_slice(&rgba[src..src + (w * 4) as usize]);
        }
        self.dirty.push((x, y, w, h));
        Some(Glyph { x, y, w, h, left, top, colored })
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        // 1px gutter between glyphs to avoid bleeding with linear filtering.
        let (pw, ph) = (w + 1, h + 1);
        if pw > ATLAS_SIZE || ph > ATLAS_SIZE {
            return None;
        }
        if self.shelf.x + pw > ATLAS_SIZE {
            self.shelf.x = 1;
            self.shelf.y += self.shelf.row_h;
            self.shelf.row_h = 0;
        }
        if self.shelf.y + ph > ATLAS_SIZE {
            log::warn!("glyph atlas full; clearing cache");
            self.glyphs.clear();
            self.shelf = Shelf { x: 1, y: 1, row_h: 0 };
            self.atlas.iter_mut().for_each(|b| *b = 0);
            self.dirty.clear();
            self.dirty.push((0, 0, ATLAS_SIZE, ATLAS_SIZE));
        }
        let pos = (self.shelf.x, self.shelf.y);
        self.shelf.x += pw;
        self.shelf.row_h = self.shelf.row_h.max(ph);
        Some(pos)
    }

}
