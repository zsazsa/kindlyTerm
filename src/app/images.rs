//! Images on the canvas: dropped files, pasted clipboard images, PNG /
//! JPEG / WebP / animated GIF. Pixels live in the window's renderer;
//! `state.json` only keeps the path.

use super::*;
use crate::renderer::ImageId;
use std::path::{Path, PathBuf};

/// Longest side an image is decoded to; bigger inputs are downscaled.
const MAX_SIDE: u32 = 2048;
/// Animated GIFs keep at most this many frames on the GPU.
const MAX_FRAMES: usize = 120;

/// Decoded, uploaded image for one item.
pub(super) struct LoadedImage {
    /// Texture and how long it shows (ms); one entry for a still image.
    pub frames: Vec<(ImageId, u32)>,
    pub w: u32,
    pub h: u32,
    pub total_ms: u32,
    /// Set when decoding failed, so the item shows a note instead of
    /// retrying every frame.
    pub error: Option<String>,
}

impl LoadedImage {
    /// Frame to show at time `t` since the app started.
    pub fn frame_at(&self, ms: u128) -> Option<ImageId> {
        if self.frames.is_empty() {
            return None;
        }
        if self.frames.len() == 1 || self.total_ms == 0 {
            return Some(self.frames[0].0);
        }
        let mut t = (ms % self.total_ms as u128) as u32;
        for (id, d) in &self.frames {
            if t < *d {
                return Some(*id);
            }
            t -= d;
        }
        Some(self.frames[0].0)
    }
    pub fn animated(&self) -> bool {
        self.frames.len() > 1
    }
}

pub(super) fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    )
}

/// RGBA frames with their delays (ms), plus width and height.
type Decoded = (Vec<(Vec<u8>, u32)>, u32, u32);

/// Decode a file into RGBA frames, downscaled to `MAX_SIDE`.
fn decode(path: &Path) -> anyhow::Result<Decoded> {
    use image::AnimationDecoder;
    let bytes = std::fs::read(path)?;
    let fmt = image::guess_format(&bytes)?;
    let scale = |img: image::RgbaImage| -> image::RgbaImage {
        let (w, h) = img.dimensions();
        let side = w.max(h);
        if side > MAX_SIDE {
            let f = MAX_SIDE as f32 / side as f32;
            image::imageops::resize(&img, ((w as f32 * f) as u32).max(1), ((h as f32 * f) as u32).max(1), image::imageops::FilterType::Triangle)
        } else {
            img
        }
    };
    if fmt == image::ImageFormat::Gif {
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))?;
        let mut frames = Vec::new();
        let (mut w, mut h) = (0, 0);
        for f in dec.into_frames().take(MAX_FRAMES) {
            let f = f?;
            let (num, den) = f.delay().numer_denom_ms();
            let delay = num.checked_div(den).map(|d| d.max(20)).unwrap_or(100);
            let img = scale(f.into_buffer());
            (w, h) = img.dimensions();
            frames.push((img.into_raw(), delay));
        }
        if frames.is_empty() {
            anyhow::bail!("gif has no frames");
        }
        return Ok((frames, w, h));
    }
    let img = image::load_from_memory_with_format(&bytes, fmt)?.to_rgba8();
    let img = scale(img);
    let (w, h) = img.dimensions();
    Ok((vec![(img.into_raw(), 0)], w, h))
}

impl App {
    /// Directory where pasted images are kept.
    fn images_dir() -> PathBuf {
        Config::path().parent().map(|p| p.join("images")).unwrap_or_else(|| PathBuf::from("images"))
    }

    /// Put an image file on the active canvas (converting a plain tab).
    /// `at` is a world position for the top-left; None picks a spot.
    pub(super) fn place_image_file(&mut self, path: PathBuf, at: Option<(f32, f32)>) {
        if !path.is_file() {
            self.set_status(format!("not a file: {}", path.display()));
            return;
        }
        // Size at 1:1 from the header, capped to fit the view comfortably.
        let dims = image::image_dimensions(&path).ok();
        let Some(l) = self.win().layout else { return };
        if self.win().canvas().map(|c| c.is_single()).unwrap_or(false) {
            self.convert_to_canvas();
        }
        let (iw, ih) = dims.map(|(w, h)| (w as f32, h as f32)).unwrap_or((400.0, 300.0));
        let vis = self.win().canvas().map(|c| c.view.visible(l.area)).unwrap_or(l.area);
        let max_w = (vis.w * 0.6).max(120.0);
        let max_h = (vis.h * 0.6).max(90.0);
        let f = (max_w / iw).min(max_h / ih).min(1.0);
        let (w, h) = ((iw * f).round().max(64.0), (ih * f).round().max(48.0) + TITLE_H);
        let item_id = self.next_item_id;
        self.next_item_id += 1;
        let win = self.win_mut();
        let Some(c) = win.canvas_mut() else { return };
        let rect = match at {
            Some((x, y)) => WRect::new(x.round(), y.round(), w, h),
            None => c.spawn_rect(l.area, w, h),
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        c.items.push(Item { id: item_id, kind: ItemKind::Image { path: path.to_string_lossy().into_owned() }, rect, name, launch: None, pin: None, mirror: false, monitor: None });
        c.focus = Some(item_id);
        c.selected.clear();
        win.dirty = true;
        self.reveal_rect(rect);
        self.request_redraw();
    }

    /// Clipboard holds an image: save it as PNG under the config dir and
    /// place it. Returns false if there was no image.
    pub(super) fn paste_image(&mut self) -> bool {
        let Some(img) = self.clipboard.as_mut().and_then(|c| c.get_image().ok()) else { return false };
        let dir = Self::images_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        // Name by content so the same paste is not stored twice.
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        img.bytes.hash(&mut h);
        let path = dir.join(format!("paste-{:016x}.png", h.finish()));
        if !path.exists() {
            let Some(buf) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else { return false };
            if let Err(e) = buf.save(&path) {
                self.set_status(format!("could not save pasted image: {e}"));
                return false;
            }
        }
        self.place_image_file(path, None);
        self.set_status("image pasted onto the canvas".into());
        true
    }

    /// Decode and upload an item's image on first use.
    pub(super) fn ensure_image_loaded(w: &mut Win, id: ItemId, path: &str) {
        if w.images.contains_key(&id) {
            return;
        }
        let loaded = match decode(Path::new(path)) {
            Ok((frames, iw, ih)) => {
                let mut out = Vec::with_capacity(frames.len());
                let mut total = 0;
                for (rgba, delay) in frames {
                    out.push((w.renderer.upload_image(iw, ih, &rgba), delay));
                    total += delay;
                }
                LoadedImage { frames: out, w: iw, h: ih, total_ms: total, error: None }
            }
            Err(e) => LoadedImage { frames: Vec::new(), w: 0, h: 0, total_ms: 0, error: Some(e.to_string()) },
        };
        w.images.insert(id, loaded);
    }

    /// Free an item's textures (item removed).
    pub(super) fn drop_image(w: &mut Win, id: ItemId) {
        if let Some(img) = w.images.remove(&id) {
            for (tex, _) in img.frames {
                w.renderer.remove_image(tex);
            }
        }
    }

    /// Any visible animated image on this window's active canvas?
    pub(super) fn any_animated_image(w: &Win) -> bool {
        w.canvas()
            .map(|c| c.items.iter().any(|i| matches!(i.kind, ItemKind::Image { .. }) && w.images.get(&i.id).map(|l| l.animated()).unwrap_or(false)))
            .unwrap_or(false)
    }

    /// A file was dropped on the window: images land on the canvas, other
    /// files paste their (quoted) path into the focused terminal.
    pub(super) fn on_file_drop(&mut self, path: PathBuf) {
        if is_image_path(&path) {
            let at = {
                let w = self.win();
                let (mx, my) = (w.mouse.x as f32, w.mouse.y as f32);
                match (w.layout, w.canvas()) {
                    (Some(l), Some(c)) if !c.is_single() && l.area.contains(mx, my) => Some(c.view.screen_to_world(l.area, mx, my)),
                    _ => None,
                }
            };
            self.place_image_file(path, at);
            return;
        }
        let quoted = shell_quote(&path.to_string_lossy());
        if let Some(t) = self.win().active_term() {
            t.write(quoted.into_bytes());
        }
    }
}

/// Single-quote a path for the shell.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./+:@%=".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}
