//! Page builders: turn app state into the Deck's widget list.

use super::*;

impl Deck {
    // -----------------------------------------------------------------------
    // Page builders
    // -----------------------------------------------------------------------

    pub(super) fn build(&self, env: &DeckEnv) -> Built {
        let mut b = Built { widgets: Vec::new(), focusables: Vec::new() };
        let page = self.page();
        match page.id {
            PageId::Home => self.build_home(env, &mut b),
            PageId::Theme => self.build_theme(env, &mut b),
            PageId::Font => self.build_font(env, &mut b),
            PageId::Padding => self.build_padding(env, &mut b),
            PageId::Opacity => self.build_opacity(env, &mut b),
            PageId::Effects => self.build_effects(env, &mut b),
            PageId::Cursor => self.build_cursor(env, &mut b),
            PageId::Shell => self.build_shell(env, &mut b),
            PageId::Scrollback => self.build_scrollback(env, &mut b),
            PageId::Tabs => self.build_tabs(env, &mut b),
            PageId::Keyboard => self.build_keyboard(env, &mut b),
            PageId::Clipboard => self.build_clipboard(env, &mut b),
            PageId::Manage => self.build_manage(env, &mut b),
            PageId::ImportExport => self.build_import_export(env, &mut b),
            PageId::About => self.build_about(env, &mut b),
            PageId::Editor => self.build_editor(env, &mut b),
        }
        b
    }

    pub(super) fn settings_groups(env: &DeckEnv, b: &mut Built, filter: &str) {
        let c = env.config;
        let font_size = format!("{} pt", c.font.size);
        let cursor = {
            let mut s = c.terminal.cursor.clone();
            if let Some(f) = s.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            s
        };
        let chomp = match c.terminal.chomp.to_lowercase().as_str() {
            "laser" => "Laser cutter",
            "none" => "Plain",
            _ => "Pac-Man",
        }
        .to_string();
        let scrollback = format!("{} lines", group_thousands(c.terminal.scrollback));
        let clip = if c.clipboard.copy_on_select { "Copy on select" } else { "Manual copy" };
        let manage = format!("{} shortcuts", env.shortcuts.len());
        type SettingRow = (char, u8, &'static str, String, PageId);
        let groups: [(&str, Vec<SettingRow>); 5] = [
            (
                "APPEARANCE",
                vec![
                    ('◐', 5, "Theme", env.theme.name.clone(), PageId::Theme),
                    ('A', 4, "Font", c.font.family.clone(), PageId::Font),
                    ('↕', 6, "Size", font_size, PageId::Font),
                    ('▣', 7, "Padding", format!("{} px", c.terminal.padding), PageId::Padding),
                    ('◱', 6, "Opacity", format!("{}%", (c.colors.opacity * 100.0).round()), PageId::Opacity),
                    ('✦', 13, "Effects", env.effects.preset.clone(), PageId::Effects),
                    ('▊', 3, "Cursor", cursor, PageId::Cursor),
                    ('⚡', 5, "Backspace", chomp, PageId::Cursor),
                ],
            ),
            (
                "TERMINAL",
                vec![
                    ('❯', 2, "Shell", c.shell(), PageId::Shell),
                    ('⇕', 8, "Scrollback", scrollback, PageId::Scrollback),
                    ('▤', 12, "Tabs", format!("{} open", env.tab_count), PageId::Tabs),
                ],
            ),
            (
                "INPUT",
                vec![
                    ('⌨', 14, "Keyboard", "bindings".into(), PageId::Keyboard),
                    ('⎘', 11, "Clipboard", clip.into(), PageId::Clipboard),
                ],
            ),
            (
                "SHORTCUTS",
                vec![
                    ('★', 13, "Manage", manage, PageId::Manage),
                    ('⇅', 10, "Import / Export", "commands.toml".into(), PageId::ImportExport),
                ],
            ),
            ("ABOUT", vec![('i', 4, "kindlyTerm", format!("{} · vulkan", env!("CARGO_PKG_VERSION")), PageId::About)]),
        ];
        let f = filter.trim().to_lowercase();
        for (label, rows) in groups {
            let rows: Vec<Row> = rows
                .into_iter()
                .filter(|(_, _, title, value, _)| {
                    f.is_empty() || title.to_lowercase().contains(&f) || value.to_lowercase().contains(&f)
                })
                .map(|(g, col, title, value, page)| Row {
                    badge: Some(Badge { glyph: g, color: col }),
                    title: title.to_string(),
                    hi: vec![],
                    subtitle: String::new(),
                    value,
                    kind: RowKind::Chevron,
                    enabled: true,
                    danger: false,
                    focus: Some(b.simple(Act::Push(page))),
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            b.widgets.push(W::Label { text: label.into(), right: String::new() });
            b.widgets.push(W::Group(rows));
        }
    }

    pub(super) fn shortcut_row(b: &mut Built, env: &DeckEnv, cmd: &SavedCommand, hi: Vec<usize>, selected_hint: bool) -> Row {
        let (g, col) = badge_for(cmd);
        let mut sub = Vec::new();
        if let Some(h) = cmd.host() {
            sub.push(format!("ssh {h}"));
        } else {
            sub.push("local".into());
        }
        sub.push(if cmd.runs_here() { "this tab".into() } else { "new tab".into() });
        if cmd.confirm {
            sub.push("confirm first".into());
        }
        let stat = env.state.stat(&cmd.name);
        let value = cmd.hotkey.clone().unwrap_or_else(|| if stat.runs > 0 { ago(stat.last_run) } else { String::new() });
        let idx = b.add(Focusable {
            enter: Act::App(DeckAction::Launch { name: cmd.name.clone(), target: LaunchTarget::Saved }),
            left: Act::None,
            right: Act::Edit(Some(cmd.name.clone())),
            field: None,
            grid: None,
            shortcut: Some(cmd.name.clone()),
        });
        Row {
            badge: Some(Badge { glyph: g, color: col }),
            title: cmd.name.clone(),
            hi,
            subtitle: sub.join(" · "),
            value,
            kind: if selected_hint { RowKind::Hint("⏎".into()) } else { RowKind::Info },
            enabled: true,
            danger: false,
            focus: Some(idx),
        }
    }

    pub(super) fn build_home(&self, env: &DeckEnv, b: &mut Built) {
        let page = self.page();
        let n = env.shortcuts.len();
        let filter = page.filter.trim();
        let placeholder = if n == 0 { "no shortcuts yet — Ctrl+Shift+S saves one".to_string() } else { format!("type to filter {n} shortcut{}", if n == 1 { "" } else { "s" }) };

        if filter.is_empty() {
            b.widgets.push(W::Search { text: String::new(), placeholder, right: String::new() });
            if n > 0 && n <= 12 {
                b.widgets.push(W::Label { text: "SHORTCUTS".into(), right: "⏎ launch · ⌥⏎ new tab · → edit".into() });
                let start = b.focusables.len();
                let tiles: Vec<Tile> = env
                    .shortcuts
                    .iter()
                    .map(|c| {
                        let (g, col) = badge_for(c);
                        let idx = b.add(Focusable {
                            enter: Act::App(DeckAction::Launch { name: c.name.clone(), target: LaunchTarget::Saved }),
                            left: Act::None,
                            right: Act::None,
                            field: None,
                            grid: Some((start, n, TILE_COLS)),
                            shortcut: Some(c.name.clone()),
                        });
                        Tile { badge: Badge { glyph: g, color: col }, label: c.name.clone(), focus: idx }
                    })
                    .collect();
                b.widgets.push(W::Tiles(tiles));
            } else if n > 12 {
                // List-first layout at scale: pinned row, recents, then A–Z.
                let mut by_runs: Vec<&SavedCommand> = env.shortcuts.iter().collect();
                by_runs.sort_by_key(|c| std::cmp::Reverse(env.state.stat(&c.name).runs));
                let pinned: Vec<&SavedCommand> = by_runs.iter().take(6).copied().filter(|c| env.state.stat(&c.name).runs > 0).collect();
                if !pinned.is_empty() {
                    b.widgets.push(W::Label { text: "PINNED".into(), right: "most used".into() });
                    let start = b.focusables.len();
                    let len = pinned.len();
                    let tiles: Vec<Tile> = pinned
                        .iter()
                        .map(|c| {
                            let (g, col) = badge_for(c);
                            let idx = b.add(Focusable {
                                enter: Act::App(DeckAction::Launch { name: c.name.clone(), target: LaunchTarget::Saved }),
                                left: Act::None,
                                right: Act::None,
                                field: None,
                                grid: Some((start, len, TILE_COLS)),
                                shortcut: Some(c.name.clone()),
                            });
                            Tile { badge: Badge { glyph: g, color: col }, label: c.name.clone(), focus: idx }
                        })
                        .collect();
                    b.widgets.push(W::Tiles(tiles));
                }
                let mut recent: Vec<&SavedCommand> = env.shortcuts.iter().filter(|c| env.state.stat(&c.name).runs > 0).collect();
                recent.sort_by_key(|c| std::cmp::Reverse(env.state.stat(&c.name).last_run));
                if !recent.is_empty() {
                    b.widgets.push(W::Label { text: "RECENT".into(), right: String::new() });
                    let rows: Vec<Row> = recent.iter().take(3).map(|c| Self::shortcut_row(b, env, c, vec![], false)).collect();
                    b.widgets.push(W::Group(rows));
                }
                b.widgets.push(W::Label { text: "ALL · A–Z".into(), right: n.to_string() });
                let mut all: Vec<&SavedCommand> = env.shortcuts.iter().collect();
                all.sort_by_key(|c| c.name.to_lowercase());
                let mut letter = None;
                let mut rows = Vec::new();
                for c in all {
                    let l = c.name.chars().next().map(|ch| ch.to_ascii_uppercase()).filter(|ch| ch.is_ascii_alphabetic()).unwrap_or('#');
                    if letter != Some(l) {
                        if !rows.is_empty() {
                            b.widgets.push(W::Group(std::mem::take(&mut rows)));
                        }
                        b.widgets.push(W::LetterHeader(l.to_string()));
                        letter = Some(l);
                    }
                    rows.push(Self::shortcut_row(b, env, c, vec![], false));
                }
                if !rows.is_empty() {
                    b.widgets.push(W::Group(rows));
                }
            }
            Self::settings_groups(env, b, "");
        } else {
            // Quick-run: ranked results with the selected command previewed.
            let mut hits: Vec<(Match, &SavedCommand)> = env.shortcuts.iter().filter_map(|c| rank(filter, c).map(|m| (m, c))).collect();
            hits.sort_by(|a, b| {
                let sa = env.state.stat(&a.1.name);
                let sb = env.state.stat(&b.1.name);
                a.0.tier.cmp(&b.0.tier).then(sb.last_run.cmp(&sa.last_run)).then(sb.runs.cmp(&sa.runs)).then(a.1.name.cmp(&b.1.name))
            });
            let total = hits.len();
            b.widgets.push(W::Search { text: page.filter.clone(), placeholder: String::new(), right: format!("{total} of {n}") });
            if total > 0 {
                b.widgets.push(W::Label { text: "RANKED".into(), right: "exact, prefix, initials, then subsequence".into() });
                let mut rows = Vec::new();
                let mut selected_cmd: Option<String> = None;
                for (i, (m, c)) in hits.iter().take(9).enumerate() {
                    let is_sel = i == page.focus;
                    if is_sel {
                        selected_cmd = Some(c.command.clone());
                    }
                    rows.push(Self::shortcut_row(b, env, c, m.positions.clone(), is_sel));
                }
                b.widgets.push(W::Group(rows));
                if let Some(cmd) = selected_cmd {
                    b.widgets.push(W::CommandStrip(cmd));
                }
            } else {
                let idx = b.simple(Act::App(DeckAction::RunRaw(filter.to_string())));
                b.widgets.push(W::Group(vec![Row {
                    badge: Some(Badge { glyph: '❯', color: 10 }),
                    title: format!("run \"{filter}\" as a command"),
                    hi: vec![],
                    subtitle: "new tab · not saved".into(),
                    value: String::new(),
                    kind: RowKind::Hint("⏎".into()),
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }]));
            }
            Self::settings_groups(env, b, filter);
        }
    }

    pub(super) fn build_theme(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Back".into(), title: "Theme".into(), hint: "Esc".into() });
        let f = self.page().filter.to_lowercase();
        for (i, t) in BUILTIN_THEMES.iter().enumerate() {
            if !f.is_empty() && !t.name.to_lowercase().contains(&f) {
                continue;
            }
            let idx = b.simple(Act::App(DeckAction::ApplyTheme(t.name.to_string())));
            b.widgets.push(W::ThemeCard { idx: i, selected: env.theme.name.eq_ignore_ascii_case(t.name), focus: idx });
        }
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '◐', color: 4 }),
            title: "Custom…".into(),
            hi: vec![],
            subtitle: String::new(),
            value: "edit [colors] in config.toml".into(),
            kind: RowKind::Info,
            enabled: false,
            danger: false,
            focus: None,
        }]));
        b.widgets.push(W::Note("↑↓ previews live behind the panel · ⏎ applies · Esc reverts".into()));
    }

    pub(super) fn build_font(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Appearance".into(), title: "Font & Size".into(), hint: "Esc".into() });
        b.widgets.push(W::FontPreview);
        let f = self.page().filter.to_lowercase();
        let shown: Vec<&String> = env.font_families.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)).collect();
        b.widgets.push(W::Search {
            text: self.page().filter.clone(),
            placeholder: "type to filter fonts".into(),
            right: format!("{} of {}", shown.len(), env.font_families.len()),
        });
        let rows: Vec<Row> = shown
            .iter()
            .map(|name| {
                let current = name.eq_ignore_ascii_case(env.font_family);
                let idx = b.simple(Act::App(DeckAction::ApplyFont((*name).clone())));
                Row {
                    badge: None,
                    title: (*name).clone(),
                    hi: vec![],
                    subtitle: String::new(),
                    value: if current { "0O1lI| ✓".into() } else { "0O1lI|".into() },
                    kind: RowKind::Info,
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }
            })
            .collect();
        if !rows.is_empty() {
            b.widgets.push(W::Group(rows));
        }
        b.widgets.push(W::Label { text: "METRICS".into(), right: String::new() });
        let size = env.config.font.size;
        let lp = env.config.font.line_padding;
        let size_idx = b.adjustable(
            Act::None,
            Act::App(DeckAction::SetFontSize(size - 1.0)),
            Act::App(DeckAction::SetFontSize(size + 1.0)),
        );
        let lp_idx = b.adjustable(
            Act::None,
            Act::App(DeckAction::SetLinePadding((lp - 1.0).max(0.0))),
            Act::App(DeckAction::SetLinePadding(lp + 1.0)),
        );
        b.widgets.push(W::Group(vec![
            Row {
                badge: None,
                title: "Size".into(),
                hi: vec![],
                subtitle: String::new(),
                value: format!("{size} pt"),
                kind: RowKind::Stepper(format!("{size}")),
                enabled: true,
                danger: false,
                focus: Some(size_idx),
            },
            Row {
                badge: None,
                title: "Line spacing".into(),
                hi: vec![],
                subtitle: String::new(),
                value: format!("+{lp} px"),
                kind: RowKind::Stepper(format!("{lp}")),
                enabled: true,
                danger: false,
                focus: Some(lp_idx),
            },
            Row {
                badge: None,
                title: "Ligatures".into(),
                hi: vec![],
                subtitle: String::new(),
                value: "not yet supported".into(),
                kind: RowKind::Toggle(false),
                enabled: false,
                danger: false,
                focus: None,
            },
        ]));
        b.widgets.push(W::Note("Ctrl+= / Ctrl+− size · ←→ on a row steps · applies live".into()));
    }

    pub(super) fn build_padding(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Appearance".into(), title: "Padding".into(), hint: "Esc".into() });
        let p = env.config.terminal.padding;
        let idx = b.adjustable(Act::None, Act::App(DeckAction::SetPadding((p - 2.0).max(0.0))), Act::App(DeckAction::SetPadding((p + 2.0).min(64.0))));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '▣', color: 7 }),
            title: "Grid padding".into(),
            hi: vec![],
            subtitle: "space between window edge and text".into(),
            value: format!("{p} px"),
            kind: RowKind::Stepper(format!("{p}")),
            enabled: true,
            danger: false,
            focus: Some(idx),
        }]));
        b.widgets.push(W::Note("←→ steps by 2 px · applies live".into()));
    }

    pub(super) fn build_opacity(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Appearance".into(), title: "Opacity".into(), hint: "Esc".into() });
        let o = env.config.colors.opacity.clamp(0.3, 1.0);
        let idx = b.adjustable(Act::None, Act::App(DeckAction::SetOpacity((o - 0.05).max(0.3))), Act::App(DeckAction::SetOpacity((o + 0.05).min(1.0))));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '◱', color: 6 }),
            title: "Window opacity".into(),
            hi: vec![],
            subtitle: "terminal and tab bar; the Deck stays solid".into(),
            value: String::new(),
            kind: RowKind::Stepper(format!("{}%", (o * 100.0).round())),
            enabled: true,
            danger: false,
            focus: Some(idx),
        }]));
        b.widgets.push(W::Note("←→ steps 5% · applies live · needs a compositor (GNOME, KDE, sway…)".into()));
    }

    pub(super) fn build_effects(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Appearance".into(), title: "Effects".into(), hint: "Esc".into() });
        let e = env.effects;
        let step = |b: &mut Built, dec: EffectChange, inc: EffectChange| b.adjustable(Act::None, Act::App(DeckAction::Effect(dec)), Act::App(DeckAction::Effect(inc)));
        let tog = |b: &mut Built, c: EffectChange| b.simple(Act::App(DeckAction::Effect(c)));
        let row = |title: &str, sub: &str, kind: RowKind, focus: usize| Row {
            badge: None,
            title: title.into(),
            hi: vec![],
            subtitle: sub.into(),
            value: String::new(),
            kind,
            enabled: true,
            danger: false,
            focus: Some(focus),
        };

        let preset = step(b, EffectChange::PresetStep(-1), EffectChange::PresetStep(1));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '✦', color: 13 }),
            title: "Preset".into(),
            hi: vec![],
            subtitle: "off · subtle · cyberpunk · matrix".into(),
            value: String::new(),
            kind: RowKind::Stepper(e.preset.clone()),
            enabled: true,
            danger: false,
            focus: Some(preset),
        }]));

        b.widgets.push(W::Label { text: "TYPING TRAIL".into(), right: String::new() });
        let t = &e.typing_trail;
        let t_on = tog(b, EffectChange::TrailToggle);
        let t_len = step(b, EffectChange::TrailLength(-1), EffectChange::TrailLength(1));
        let t_fade = step(b, EffectChange::TrailFade(-1), EffectChange::TrailFade(1));
        let t_col = step(b, EffectChange::TrailColorNext, EffectChange::TrailColorNext);
        let t_glow = tog(b, EffectChange::TrailGlowToggle);
        b.widgets.push(W::Group(vec![
            row("Typing trail", "glyphs glow behind the cursor as you type", RowKind::Toggle(t.enabled), t_on),
            row("Length", "cells that keep glowing", RowKind::Stepper(format!("{}", t.length)), t_len),
            row("Fade", "milliseconds per cell", RowKind::Stepper(format!("{}", t.fade_ms)), t_fade),
            row("Color", "neon · accent · cursor · green", RowKind::Stepper(t.color.clone()), t_col),
            row("Glow", "soft halo behind each glyph", RowKind::Toggle(t.glow), t_glow),
        ]));

        b.widgets.push(W::Label { text: "PASTE RAIN".into(), right: String::new() });
        let r = &e.paste_rain;
        let r_on = tog(b, EffectChange::RainToggle);
        let r_min = step(b, EffectChange::RainThreshold(-1), EffectChange::RainThreshold(1));
        let r_dur = step(b, EffectChange::RainDuration(-1), EffectChange::RainDuration(1));
        let r_gl = step(b, EffectChange::RainGlyphsNext, EffectChange::RainGlyphsNext);
        let r_col = step(b, EffectChange::RainColorNext, EffectChange::RainColorNext);
        let r_den = step(b, EffectChange::RainDensity(-1), EffectChange::RainDensity(1));
        b.widgets.push(W::Group(vec![
            row("Paste rain", "pasted text falls in from the top", RowKind::Toggle(r.enabled), r_on),
            row("Threshold", "minimum characters pasted", RowKind::Stepper(format!("{}", r.min_chars)), r_min),
            row("Duration", "milliseconds", RowKind::Stepper(format!("{}", r.duration_ms)), r_dur),
            row("Glyphs", "text · katakana · ascii · binary", RowKind::Stepper(r.glyphs.clone()), r_gl),
            row("Color", "accent · cursor · green", RowKind::Stepper(r.color.clone()), r_col),
            row("Density", "share of characters that fall", RowKind::Stepper(format!("{:.0}%", r.density * 100.0)), r_den),
        ]));

        b.widgets.push(W::Label { text: "SHARE".into(), right: String::new() });
        let reload = b.simple(Act::App(DeckAction::Effect(EffectChange::Reload)));
        b.widgets.push(W::Group(vec![
            info_row("Effects file", &EffectsConfig::path().display().to_string()),
            Row {
                badge: Some(Badge { glyph: '⇅', color: 10 }),
                title: "Reload effects.toml".into(),
                hi: vec![],
                subtitle: "after editing or dropping in someone's file".into(),
                value: String::new(),
                kind: RowKind::Chevron,
                enabled: true,
                danger: false,
                focus: Some(reload),
            },
        ]));
        b.widgets.push(W::Note("←→ on a row steps · every change writes effects.toml · copy it to share".into()));
    }

    pub(super) fn build_cursor(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Appearance".into(), title: "Cursor".into(), hint: "Esc".into() });
        let cur = env.config.terminal.cursor.to_lowercase();
        let rows: Vec<Row> = [("block", '▊', "Block"), ("beam", '▏', "Beam"), ("underline", '▁', "Underline")]
            .iter()
            .map(|(id, g, title)| {
                let idx = b.simple(Act::App(DeckAction::SetCursor(id.to_string())));
                Row {
                    badge: Some(Badge { glyph: *g, color: 3 }),
                    title: title.to_string(),
                    hi: vec![],
                    subtitle: String::new(),
                    value: String::new(),
                    kind: RowKind::Check(cur == *id),
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }
            })
            .collect();
        b.widgets.push(W::Group(rows));
        b.widgets.push(W::Note("programs may override the shape (vim, tmux) · blinking: not yet".into()));
        // What the cursor turns into while Backspace or Delete is held.
        let chomp = env.config.terminal.chomp.to_lowercase();
        b.widgets.push(W::Label { text: "HOLD BACKSPACE".into(), right: "or Delete".into() });
        let rows: Vec<Row> = [("pacman", '●', "Pac-Man", "chomps the text it eats"), ("laser", '⚡', "Laser cutter", "a beam, sparks and embers"), ("none", '▊', "Plain", "just the cursor")]
            .iter()
            .map(|(id, g, title, sub)| {
                let idx = b.simple(Act::App(DeckAction::SetChomp(id.to_string())));
                Row {
                    badge: Some(Badge { glyph: *g, color: 5 }),
                    title: title.to_string(),
                    hi: vec![],
                    subtitle: sub.to_string(),
                    value: String::new(),
                    kind: RowKind::Check(chomp == *id),
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }
            })
            .collect();
        b.widgets.push(W::Group(rows));
    }

    pub(super) fn build_shell(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Terminal".into(), title: "Shell".into(), hint: "⏎ save".into() });
        let value = self.shell_draft.clone().unwrap_or_else(|| env.config.terminal.shell.clone().unwrap_or_default());
        let idx = b.field(FieldId::Shell, Act::ShellCommit);
        b.widgets.push(W::Field {
            label: "PROGRAM".into(),
            value,
            placeholder: format!("$SHELL ({})", std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())),
            focus: idx,
            multiline: false,
            active: self.page().focus == idx,
            right: String::new(),
        });
        let args = if env.config.terminal.shell_args.is_empty() { "none".to_string() } else { env.config.terminal.shell_args.join(" ") };
        b.widgets.push(W::Group(vec![Row {
            badge: None,
            title: "Arguments".into(),
            hi: vec![],
            subtitle: "shell_args in config.toml".into(),
            value: args,
            kind: RowKind::Info,
            enabled: false,
            danger: false,
            focus: None,
        }]));
        b.widgets.push(W::Note("empty = use $SHELL · new tabs only · Ctrl+U clears".into()));
    }

    pub(super) fn build_scrollback(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Terminal".into(), title: "Scrollback".into(), hint: "Esc".into() });
        let n = env.config.terminal.scrollback;
        let step = if n >= 20_000 { 10_000 } else { 1_000 };
        let idx = b.adjustable(Act::None, Act::App(DeckAction::SetScrollback(n.saturating_sub(step))), Act::App(DeckAction::SetScrollback((n + step).min(200_000))));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '⇕', color: 8 }),
            title: "History".into(),
            hi: vec![],
            subtitle: "lines kept per tab".into(),
            value: group_thousands(n),
            kind: RowKind::Stepper(group_thousands(n)),
            enabled: true,
            danger: false,
            focus: Some(idx),
        }]));
        b.widgets.push(W::Note("←→ steps · applies to open tabs too".into()));

        // Detached sessions live here too: they decide what "history" means
        // across a restart.
        let on = env.config.terminal.persistent_sessions;
        let idx = b.simple(Act::App(DeckAction::SetPersistentSessions(!on)));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '∞', color: 2 }),
            title: "Keep shells running".into(),
            hi: vec![],
            subtitle: "shells outlive the window and come back on the next start".into(),
            value: String::new(),
            kind: RowKind::Toggle(on),
            enabled: true,
            danger: false,
            focus: Some(idx),
        }]));
        b.widgets.push(W::Note("new shells only · closing a tab still ends its shell · kindlyterm --sessions lists them".into()));
    }

    pub(super) fn build_tabs(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Terminal".into(), title: "Tabs".into(), hint: "Esc".into() });
        let info = [
            ("Open now", format!("{}", env.tab_count)),
            ("Position", "Top".into()),
            ("Titles", "program title, else shortcut name".into()),
            ("New tab", "Ctrl+Shift+T · + button".into()),
            ("Switch", "Ctrl+Tab · Alt+1…9 · click".into()),
            ("Close", "Ctrl+Shift+W · × · middle click".into()),
        ];
        b.widgets.push(W::Group(info.iter().map(|(t, v)| info_row(t, v)).collect()));
    }

    pub(super) fn build_keyboard(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Input".into(), title: "Keyboard".into(), hint: "Esc".into() });
        let f = self.page().filter.to_lowercase();
        let bindings: &[(&str, &str)] = &[
            ("Ctrl+/", "Keyboard cheat sheet"),
            ("Ctrl+Shift (tap)", "Toggle Control Deck"),
            ("Ctrl+Shift+,", "Toggle Control Deck"),
            ("Ctrl+Shift+Space", "Deck: quick-run"),
            ("Ctrl+Shift+S", "Deck: save a shortcut"),
            ("Ctrl+Shift+O", "Tab switcher"),
            ("Ctrl+Shift+T", "New tab"),
            ("Ctrl+Shift+W", "Close tab / focused canvas terminal"),
            ("Ctrl+Shift+Enter", "Canvas: add a terminal"),
            ("Ctrl+Shift+K", "Canvas: new canvas tab"),
            ("Ctrl+Shift+F", "Canvas: focus mode"),
            ("Ctrl+Shift+A", "Canvas: fit all"),
            ("Ctrl+Shift+G", "Canvas: group selected terminals"),
            ("Ctrl+Shift+P", "Canvas: pin / unpin focused terminal"),
            ("Ctrl+Shift+= / − / 0", "Canvas: zoom in / out / reset"),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
            ("Alt+1 … Alt+9", "Jump to tab"),
            ("Shift+PageUp / PageDown", "Scroll a page"),
            ("Ctrl+Shift+Up / Down", "Scroll a line"),
            ("Shift+Home / End", "Top / bottom of history"),
            ("Ctrl+Shift+C / V", "Copy / paste"),
            ("Ctrl+Insert / Shift+Insert", "Copy / paste"),
            ("Ctrl+C (selection)", "Copy selection"),
            ("Ctrl+V (prompt)", "Paste"),
            ("Ctrl+= / Ctrl+−", "Font size"),
            ("Ctrl+0", "Reset font size"),
            ("Ctrl+Shift+Q", "Quit"),
        ];
        let mut rows: Vec<Row> = bindings
            .iter()
            .filter(|(k, v)| f.is_empty() || k.to_lowercase().contains(&f) || v.to_lowercase().contains(&f))
            .map(|(k, v)| info_row(v, k))
            .collect();
        for c in env.shortcuts.iter().filter(|c| c.hotkey.is_some()) {
            if f.is_empty() || c.name.to_lowercase().contains(&f) {
                let (g, col) = badge_for(c);
                let mut r = info_row(&c.name, c.hotkey.as_deref().unwrap_or(""));
                r.badge = Some(Badge { glyph: g, color: col });
                rows.push(r);
            }
        }
        b.widgets.push(W::Search { text: self.page().filter.clone(), placeholder: "type to filter bindings".into(), right: rows.len().to_string() });
        b.widgets.push(W::Group(rows));
        b.widgets.push(W::Note("rebinding: edit src/app.rs for now · shortcut hotkeys in the editor".into()));
    }

    pub(super) fn build_clipboard(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Input".into(), title: "Clipboard".into(), hint: "Esc".into() });
        let c = &env.config.clipboard;
        let items = [
            (ClipField::CopyOnSelect, c.copy_on_select, "Copy on select", "mouse selection goes to the clipboard"),
            (ClipField::CtrlC, c.ctrl_c_copies_selection, "Ctrl+C copies selection", "otherwise sends the interrupt"),
            (ClipField::CtrlV, c.ctrl_v_pastes_in_shell, "Ctrl+V pastes at a prompt", "full-screen apps still get Ctrl+V"),
            (ClipField::TrimNewline, c.trim_trailing_newline, "Trim trailing newline", "a single pasted line does not run itself"),
        ];
        let rows: Vec<Row> = items
            .iter()
            .map(|(field, on, title, sub)| {
                let idx = b.simple(Act::App(DeckAction::SetClipboard(*field, !on)));
                Row {
                    badge: None,
                    title: title.to_string(),
                    hi: vec![],
                    subtitle: sub.to_string(),
                    value: String::new(),
                    kind: RowKind::Toggle(*on),
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }
            })
            .collect();
        b.widgets.push(W::Group(rows));
        b.widgets.push(W::Note("⏎ or Space toggles · middle click always pastes the primary selection".into()));
    }

    pub(super) fn build_manage(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Deck".into(), title: "Shortcuts".into(), hint: "⏎ edit".into() });
        let f = self.page().filter.to_lowercase();
        let new_idx = b.simple(Act::Edit(None));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '+', color: 10 }),
            title: "New shortcut".into(),
            hi: vec![],
            subtitle: "name, command, icon, hotkey".into(),
            value: "Ctrl+Shift+S".into(),
            kind: RowKind::Chevron,
            enabled: true,
            danger: false,
            focus: Some(new_idx),
        }]));
        let mut all: Vec<&SavedCommand> = env.shortcuts.iter().filter(|c| f.is_empty() || c.name.to_lowercase().contains(&f) || c.command.to_lowercase().contains(&f)).collect();
        all.sort_by_key(|c| c.name.to_lowercase());
        b.widgets.push(W::Search { text: self.page().filter.clone(), placeholder: "type to filter".into(), right: format!("{} of {}", all.len(), env.shortcuts.len()) });
        let rows: Vec<Row> = all
            .iter()
            .map(|c| {
                let (g, col) = badge_for(c);
                let idx = b.add(Focusable {
                    enter: Act::Edit(Some(c.name.clone())),
                    left: Act::None,
                    right: Act::Edit(Some(c.name.clone())),
                    field: None,
                    grid: None,
                    shortcut: Some(c.name.clone()),
                });
                Row {
                    badge: Some(Badge { glyph: g, color: col }),
                    title: c.name.clone(),
                    hi: vec![],
                    subtitle: c.command.clone(),
                    value: c.hotkey.clone().unwrap_or_default(),
                    kind: RowKind::Chevron,
                    enabled: true,
                    danger: false,
                    focus: Some(idx),
                }
            })
            .collect();
        if !rows.is_empty() {
            b.widgets.push(W::Group(rows));
        }
        b.widgets.push(W::Note("⏎ edit · ⌥⏎ launch in new tab".into()));
    }

    pub(super) fn build_import_export(&self, _env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Shortcuts".into(), title: "Import / Export".into(), hint: "Esc".into() });
        let cmds = crate::config::CommandStore::path();
        let cfg = crate::config::Config::path();
        let reload = b.simple(Act::App(DeckAction::ReloadCommands));
        b.widgets.push(W::Group(vec![
            info_row("Shortcuts file", &cmds.display().to_string()),
            info_row("Config file", &cfg.display().to_string()),
            Row {
                badge: Some(Badge { glyph: '⇅', color: 10 }),
                title: "Reload shortcuts from disk".into(),
                hi: vec![],
                subtitle: "after editing commands.toml by hand".into(),
                value: String::new(),
                kind: RowKind::Chevron,
                enabled: true,
                danger: false,
                focus: Some(reload),
            },
        ]));
        b.widgets.push(W::Note("copy commands.toml between machines to move your deck".into()));
    }

    pub(super) fn build_about(&self, env: &DeckEnv, b: &mut Built) {
        b.widgets.push(W::Header { back: "Deck".into(), title: "About".into(), hint: "Esc".into() });
        b.widgets.push(W::Group(vec![
            info_row("kindlyTerm", env!("CARGO_PKG_VERSION")),
            info_row("GPU", env.gpu),
            info_row("Font", &format!("{} {} pt", env.config.font.family, env.config.font.size)),
            info_row("Theme", &env.theme.name),
            info_row("Config", &crate::config::config_dir().display().to_string()),
        ]));
        b.widgets.push(W::Note("alacritty_terminal · wgpu · winit · swash — from scratch in Rust".into()));

        // Tool access is a security boundary, so it lives here, plainly.
        let on = env.config.mcp.enabled;
        let idx = b.simple(Act::App(DeckAction::SetMcp(!on)));
        b.widgets.push(W::Group(vec![Row {
            badge: Some(Badge { glyph: '⌬', color: if on { 3 } else { 8 } }),
            title: "Let tools drive kindlyTerm (MCP)".into(),
            hi: vec![],
            subtitle: if on { "on: Claude Code can read screens and type via `kindlyterm --mcp`".into() } else { "off: no tool can drive this window (shells stay reachable to your own processes, as with tmux)".into() },
            value: String::new(),
            kind: RowKind::Toggle(on),
            enabled: true,
            danger: false,
            focus: Some(idx),
        }]));
        b.widgets.push(W::Note("register once: claude mcp add kindlyterm -- kindlyterm --mcp · every tool action shows in the tab bar".into()));
    }

    pub(super) fn build_editor(&self, _env: &DeckEnv, b: &mut Built) {
        let Some(ed) = self.editor.as_ref() else { return };
        let title = if ed.previous.is_some() { "Edit shortcut" } else { "New shortcut" };
        b.widgets.push(W::Header { back: "Manage".into(), title: title.into(), hint: "Ctrl+S".into() });
        let focus = self.page().focus;

        let g_idx = b.adjustable(Act::EditorNext, Act::EditorGlyph(-1), Act::EditorGlyph(1));
        let c_idx = b.adjustable(Act::EditorNext, Act::EditorColor(-1), Act::EditorColor(1));
        b.widgets.push(W::GlyphPicker { focus_glyph: g_idx, focus_color: c_idx });

        let name_idx = b.field(FieldId::Name, Act::EditorNext);
        b.widgets.push(W::Field { label: "NAME".into(), value: ed.draft.name.clone(), placeholder: "homelab".into(), focus: name_idx, multiline: false, active: focus == name_idx, right: String::new() });

        let cmd_idx = b.field(FieldId::Command, Act::EditorNext);
        let host = ed.draft.host().map(|h| format!("HOST {h}")).unwrap_or_default();
        b.widgets.push(W::Field { label: "COMMAND".into(), value: ed.draft.command.clone(), placeholder: "ssh dev@192.168.1.10".into(), focus: cmd_idx, multiline: true, active: focus == cmd_idx, right: host });

        let cwd_idx = b.field(FieldId::Cwd, Act::EditorNext);
        b.widgets.push(W::Field { label: "WORKING DIRECTORY".into(), value: ed.draft.cwd.clone().unwrap_or_default(), placeholder: "~ (optional)".into(), focus: cwd_idx, multiline: false, active: focus == cwd_idx, right: String::new() });

        let run_idx = b.adjustable(Act::EditorRunIn, Act::EditorRunIn, Act::EditorRunIn);
        let keep_idx = b.simple(Act::EditorToggle(FieldId::Cwd));
        let conf_idx = b.simple(Act::EditorToggle(FieldId::Name));
        b.widgets.push(W::Group(vec![
            Row {
                badge: None,
                title: "Run in".into(),
                hi: vec![],
                subtitle: "what ⏎ does from the Deck".into(),
                value: String::new(),
                kind: RowKind::Stepper(if ed.draft.runs_here() { "This tab".into() } else { "New tab".into() }),
                enabled: true,
                danger: false,
                focus: Some(run_idx),
            },
            Row {
                badge: None,
                title: "Keep tab open after exit".into(),
                hi: vec![],
                subtitle: String::new(),
                value: String::new(),
                kind: RowKind::Toggle(ed.draft.keep_open),
                enabled: true,
                danger: false,
                focus: Some(keep_idx),
            },
            Row {
                badge: None,
                title: "Confirm before running".into(),
                hi: vec![],
                subtitle: String::new(),
                value: String::new(),
                kind: RowKind::Toggle(ed.draft.confirm),
                enabled: true,
                danger: false,
                focus: Some(conf_idx),
            },
        ]));

        let hot_idx = b.field(FieldId::Hotkey, Act::EditorBind);
        let (hot_val, hot_right) = if ed.binding {
            ("press keys to bind…".to_string(), "Esc".to_string())
        } else {
            (ed.draft.hotkey.clone().unwrap_or_default(), if ed.draft.hotkey.is_some() { "⏎ rebind · ⌫ clear".into() } else { "⏎ bind".into() })
        };
        b.widgets.push(W::Field { label: "HOTKEY".into(), value: hot_val, placeholder: "none".into(), focus: hot_idx, multiline: false, active: focus == hot_idx || ed.binding, right: hot_right });

        if ed.previous.is_some() {
            let del = b.simple(Act::EditorDelete);
            b.widgets.push(W::Group(vec![Row {
                badge: None,
                title: "Delete shortcut".into(),
                hi: vec![],
                subtitle: String::new(),
                value: String::new(),
                kind: RowKind::Info,
                enabled: true,
                danger: true,
                focus: Some(del),
            }]));
        }
        let cancel = b.simple(Act::Pop);
        let _save = b.simple(Act::EditorSave);
        b.widgets.push(W::Buttons { cancel: "Cancel".into(), save: "Save".into(), focus: cancel });
    }

}
