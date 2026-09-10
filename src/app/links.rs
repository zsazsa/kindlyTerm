//! Links in terminal output: OSC 8 hyperlinks and plain URLs, found under
//! the pointer across soft-wrapped lines. Ctrl+hover underlines,
//! Ctrl+click opens with xdg-open, the right-click menu offers open/copy.

use super::*;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;

/// A link under the pointer: its target and the grid cells it covers
/// (viewport line, first column, last column inclusive).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LinkHit {
    pub tab: TabId,
    pub uri: String,
    pub cells: Vec<(usize, usize, usize)>,
}

const URL_CHARS: &str = "-._~:/?#[]@!$&'()*+,;=%";

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || URL_CHARS.contains(c) || (!c.is_ascii() && !c.is_whitespace())
}

/// Find a URL in `text` (chars) that covers index `at`. Returns the char
/// range [start, end).
fn url_span(text: &[char], at: usize) -> Option<(usize, usize)> {
    if at >= text.len() || !is_url_char(text[at]) {
        return None;
    }
    let mut start = at;
    while start > 0 && is_url_char(text[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < text.len() && is_url_char(text[end]) {
        end += 1;
    }
    let token: String = text[start..end].iter().collect();
    // Locate a scheme inside the token (the run may have leading junk).
    let lower = token.to_lowercase();
    let schemes = ["https://", "http://", "file://", "ftp://", "ssh://", "git://", "www."];
    let mut best: Option<usize> = None;
    for s in schemes {
        if let Some(i) = lower.find(s) {
            best = Some(best.map(|b| b.min(i)).unwrap_or(i));
        }
    }
    let off = best?;
    // Byte offset -> char offset.
    let off_chars = token[..off].chars().count();
    let mut s = start + off_chars;
    let mut e = end;
    // Trim trailing punctuation that is almost never part of a URL.
    while e > s {
        let c = text[e - 1];
        let unbalanced_paren = c == ')' && text[s..e].iter().filter(|&&x| x == '(').count() < text[s..e].iter().filter(|&&x| x == ')').count();
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '>' | ']') || unbalanced_paren {
            e -= 1;
        } else {
            break;
        }
    }
    if s >= e || at < s || at >= e {
        return None;
    }
    // A bare "www." needs something after it.
    if lower[off..].starts_with("www.") && e - s < 6 {
        return None;
    }
    // Skip a leading '(' or '<' that stuck to the scheme.
    while s < e && matches!(text[s], '(' | '<' | '[') {
        s += 1;
    }
    Some((s, e))
}

impl App {
    /// The link under viewport point `(line, col)` of `tab`, if any.
    pub(super) fn link_at_point(&self, tab: TabId, line: usize, col: usize) -> Option<LinkHit> {
        let t = self.win().term(tab)?;
        let term = t.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        if line >= rows || col >= cols {
            return None;
        }
        let off = grid.display_offset() as i32;
        let gl = |vl: usize| Line(vl as i32 - off);
        let cell = &grid[gl(line)][Column(col)];
        // OSC 8: the cell knows its own link; gather neighbours with the same id.
        if let Some(h) = cell.hyperlink() {
            let uri = h.uri().to_string();
            let mut cells = Vec::new();
            for vl in 0..rows {
                let row = &grid[gl(vl)];
                let mut run: Option<(usize, usize)> = None;
                for c in 0..cols {
                    let same = row[Column(c)].hyperlink().map(|x| x == h).unwrap_or(false);
                    match (same, run) {
                        (true, None) => run = Some((c, c)),
                        (true, Some((s, _))) => run = Some((s, c)),
                        (false, Some((s, e))) => {
                            cells.push((vl, s, e));
                            run = None;
                        }
                        _ => {}
                    }
                }
                if let Some((s, e)) = run {
                    cells.push((vl, s, e));
                }
            }
            return Some(LinkHit { tab, uri, cells });
        }
        // Plain text: join this line with soft-wrapped neighbours.
        let mut first = line;
        while first > 0 && grid[gl(first - 1)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            first -= 1;
        }
        let mut last = line;
        while last + 1 < rows && grid[gl(last)][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            last += 1;
        }
        let mut text: Vec<char> = Vec::with_capacity(cols * (last - first + 1));
        let mut map: Vec<(usize, usize)> = Vec::new(); // char index -> (line, col)
        for vl in first..=last {
            let row = &grid[gl(vl)];
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
                map.push((vl, c));
            }
        }
        drop(term);
        let at = map.iter().position(|&(l, c)| l == line && c == col)?;
        let (s, e) = url_span(&text, at)?;
        let uri: String = text[s..e].iter().collect();
        let uri = if uri.to_lowercase().starts_with("www.") { format!("https://{uri}") } else { uri };
        // Group covered cells into per-line runs.
        let mut cells: Vec<(usize, usize, usize)> = Vec::new();
        for &(l, c) in &map[s..e] {
            match cells.last_mut() {
                Some((ll, _, ce)) if *ll == l && *ce + 1 >= c => *ce = c,
                _ => cells.push((l, c, c)),
            }
        }
        Some(LinkHit { tab, uri, cells })
    }

    /// Link under the pointer on whichever terminal it is over.
    pub(super) fn link_under_pointer(&self) -> Option<LinkHit> {
        let w = self.win();
        let l = w.layout?;
        let (mx, my) = (w.mouse.x as f32, w.mouse.y as f32);
        let (tab, gx, gy, zoom) = if self.on_free_canvas() {
            let (id, part) = self.item_at(mx, my)?;
            if part != ItemPart::Content {
                return None;
            }
            let c = w.canvas()?;
            let item = c.item(id)?;
            let ItemKind::Terminal(tab) = item.kind else { return None };
            let sr = Self::item_screen_rect(w, l, item);
            let z = Self::item_zoom(c, item);
            (tab, sr.x + ITEM_PAD * z, sr.y + (TITLE_H + ITEM_PAD) * z, z)
        } else {
            let tab = w.focused_tab()?;
            if my < l.tab_bar_h {
                return None;
            }
            (tab, l.grid_x, l.grid_y, 1.0)
        };
        let (cw, ch) = (l.cell_w * zoom, l.cell_h * zoom);
        if mx < gx || my < gy {
            return None;
        }
        let col = ((mx - gx) / cw) as usize;
        let line = ((my - gy) / ch) as usize;
        self.link_at_point(tab, line, col)
    }

    /// Recompute the hovered link (Ctrl held) and redraw if it changed.
    pub(super) fn update_link_hover(&mut self) {
        let hit = if self.mods.control_key() && self.win().menu.is_none() { self.link_under_pointer() } else { None };
        if hit != self.win().hover_link {
            // Say where the link really goes: OSC 8 text can differ from
            // its target.
            if let Some(h) = &hit {
                self.set_status(format!("Ctrl+click opens {}", h.uri.chars().take(120).collect::<String>()));
            }
            self.win_mut().hover_link = hit;
            self.request_redraw();
        }
    }

    /// `shown` means the target was on screen when the user chose it (the
    /// right-click menu). `file:` links need that: xdg-open will launch a
    /// `.desktop` file, so they never open on a bare Ctrl+click.
    pub(super) fn open_link(&mut self, uri: &str, shown: bool) {
        // Only web/file style targets go to xdg-open; anything odd is
        // refused rather than handed to the desktop.
        let lower = uri.to_lowercase();
        let web = ["https://", "http://", "ftp://"].iter().any(|s| lower.starts_with(s));
        let file = lower.starts_with("file://");
        if file && !shown {
            self.set_status("file links open from the right-click menu, where the target is shown".into());
            return;
        }
        if !web && !file {
            self.set_status(format!("not opening '{}'", uri.chars().take(40).collect::<String>()));
            return;
        }
        match std::process::Command::new("xdg-open").arg(uri).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.set_status(format!("opened {}", uri.chars().take(60).collect::<String>()));
            }
            Err(e) => self.set_status(format!("xdg-open failed: {e}")),
        }
    }

    /// Underline the hovered link's cells (called after the terminal is
    /// drawn, with the grid origin and zoom used for it).
    pub(super) fn draw_link_underline(w: &mut Win, theme: &Theme, tab: TabId, gx: f32, gy: f32, zoom: f32) {
        let Some(hit) = w.hover_link.as_ref() else { return };
        if hit.tab != tab {
            return;
        }
        let m = w.fonts.metrics;
        let (cw, ch) = (m.width * zoom, m.height * zoom);
        for &(line, c0, c1) in &hit.cells {
            let x = gx + c0 as f32 * cw;
            let y = gy + line as f32 * ch + ch - (2.0 * zoom).max(1.0) - 1.0;
            let wdt = (c1 - c0 + 1) as f32 * cw;
            w.batch.rect(x, y, wdt, (1.5 * zoom).max(1.0), theme.accent);
            w.batch.rrect(x - 1.0, gy + line as f32 * ch, wdt + 2.0, ch, 2.0, with_alpha(theme.accent, 0.10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::url_span;

    fn span(s: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = s.chars().collect();
        url_span(&chars, at).map(|(a, b)| chars[a..b].iter().collect())
    }

    #[test]
    fn finds_urls() {
        assert_eq!(span("see https://example.com/x?y=1. now", 10).as_deref(), Some("https://example.com/x?y=1"));
        assert_eq!(span("(https://en.wikipedia.org/wiki/Foo_(bar))", 5).as_deref(), Some("https://en.wikipedia.org/wiki/Foo_(bar)"));
        assert_eq!(span("<http://a.b/c>", 3).as_deref(), Some("http://a.b/c"));
        assert_eq!(span("visit www.kindly.dev today", 8).as_deref(), Some("www.kindly.dev"));
        assert_eq!(span("plain words here", 3), None);
        assert_eq!(span("x=https://a.b/c;", 1), None); // pointer before the scheme
    }
}
