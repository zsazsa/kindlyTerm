# kindlyterm — Control Deck design brief

## What this is

kindlyterm is a GPU-accelerated Linux terminal written in Rust. Its idea: the
terminal behaves like a tiny operating system. Saved commands are first-class
"apps" you launch from a home screen, and every setting lives behind an icon,
the way iOS Settings works. Design the **Control Deck**: a slide-in sidebar
that holds the shortcut launcher and the settings system.

Audience: developers and homelab people who live in ssh sessions all day,
open dozens of tabs, and want their most-used commands one keypress away.
They love keyboards and tolerate mice.

## The one-sentence brief

Design an iOS-Settings-meets-Launchpad sidebar for a dark, monospace terminal,
where every saved command is an app icon and every preference is an
icon-badged row, and where everything is reachable with the keyboard alone.

## Screens to design (artboards)

1. **Terminal with Deck closed** — the baseline. Top tab bar (tabs with ×,
   a +, right-aligned status text), a small gear glyph at the far right of
   the tab bar that opens the Deck. Show a live shell with colored output so
   the Deck's contrast can be judged against real content.

2. **Deck open: Home** — a 360 px panel slides in from the left over the
   terminal, which dims to ~55% behind it. Top: a search field with `>`
   prompt and a hint "type to filter, Esc closes". Then two zones:
   - **Shortcuts**: a 4-column grid of app-style icons. Each icon is a
     rounded square in a solid accent color with a single glyph (a letter,
     a Unicode symbol, or a Nerd Font icon) and a one-word label under it.
     Examples: `homelab` (a house glyph, blue), `prod-db` (a cylinder, red),
     `logs` (a scroll, amber), `htop` (a pulse, green), `build` (a hammer,
     purple). Folders group icons, iOS-style, with a 2×2 mini preview.
     One tile is focused: a 2 px accent ring, slightly lifted.
   - **Settings**: an iOS-style grouped list. Each row = colored icon badge
     (24 px rounded square), title, current value in muted text on the
     right, chevron. Groups: *Appearance* (Theme, Font, Size, Padding,
     Cursor), *Terminal* (Shell, Scrollback, Tabs), *Input* (Keyboard,
     Clipboard), *Shortcuts* (Manage, Import/Export), *About*.
   - Footer: three theme swatches for instant switching, and a
     "Ctrl+Shift+, toggles Deck" hint.

3. **Settings → Appearance → Theme** — a pushed detail page (title bar with
   `‹ Back`, page title, and the shortcut key shown on the right). A vertical
   list of theme cards; each card is a miniature terminal (5 lines of colored
   sample text, a prompt, a cursor) using that theme's real 16 ANSI colors as
   a swatch strip along the bottom. Selected card has an accent border and a
   check. Include: *Kind Night* (default, below), *Paper* (light), *Ember*
   (warm dark), *Gruvbox*, *Solarized Dark*, *Catppuccin Mocha*, and a
   "Custom…" row that opens a color editor.

4. **Settings → Appearance → Font & Size** — a live preview block at the top
   rendering `dev@kindly:~$ ls -la  ┃ 0O1lI|  →  λ ≠ ≈  🚀` in the chosen
   font. Below: a searchable list of installed monospace families, each row
   rendered in its own font. A size stepper `− 13 +` with a slider, line
   spacing stepper, and a toggle for ligatures. Everything updates the
   preview instantly.

5. **Shortcut editor** — reached from a tile's context menu or Manage. A
   form: Icon picker (grid of glyphs + a color row), Name, Command (multiline,
   monospace, with the `$SHELL -lc` note), Working directory, Host badge
   (auto-detected from `ssh user@host`), toggles *Keep tab open after exit*
   and *Confirm before running*, a Hotkey field showing `Alt+H` with a
   "press keys to bind" state, and Delete in red at the bottom. Show the
   focused field with an accent ring and the Save button as the primary
   action.

6. **Keyboard-only walkthrough strip** — a horizontal sequence of 4 small
   frames showing: `Ctrl+Shift+,` opens Deck → typing `hom` filters tiles →
   `Enter` launches `homelab` in a new tab → Deck closes, new tab active
   with a toast "connected: homelab". This sells the "OS for terminal" story.

## Visual language

- **Dark first.** Base palette (already shipped as *Kind Night*):
  - background `#1b1f27`, tab bar `#12151b`, panel `#232833`,
    highlight `#34405a`, foreground `#d8dee9`, muted `#6b7280`,
    accent `#7aa2f7`, cursor `#e5c07b`, selection `#3e4b5e`.
  - ANSI: `#282c34 #e06c75 #98c379 #e5c07b #61afef #c678dd #56b6c2 #abb2bf`
    and brights `#5c6370 #ef7a82 #a6d189 #f0d197 #74bdf7 #d48ce6 #6cc7d1 #ffffff`.
  - Use the ANSI colors as the icon-badge palette so the Deck looks native
    to the terminal rather than pasted on.
- **Typography is monospace everywhere**, including headings. Hierarchy comes
  from weight, color, and spacing, not from font changes. Default face is
  DejaVu Sans Mono 13 px; the panel may use 12 px for captions.
- **Flat, crisp, grid-aligned.** No gradients, no blur, no drop shadows.
  Depth comes from a 1 px accent border, a 2 px top highlight on the active
  tab, and dimming the terminal behind the Deck. Corner radius 6 px on
  badges and tiles, 4 px on rows. Everything snaps to the 8×16 px cell grid
  where possible.
- **Iconography**: single-glyph icons in rounded squares. Prefer Unicode
  and Nerd Font glyphs (they render through the same glyph atlas as the
  terminal). Provide a 16-glyph starter set: house, server, database,
  scroll, pulse, hammer, cloud, lock, key, terminal prompt, gear, paint
  palette, keyboard, clipboard, folder, star.
- **States to show**: default, hover (row lifts to `#2a3040`), focused
  (2 px `#7aa2f7` ring), active/selected (accent left bar 3 px, like the
  existing palette highlight), disabled (50% muted), destructive (red text).
- **Motion**: Deck slides in 160 ms ease-out; detail pages push right-to-left
  120 ms; tiles scale 1.04 on focus. Nothing bounces.

## Interaction rules the design must respect

- Every element has a keyboard path. Show the key hint inline where it
  matters (right-aligned muted text like `Ctrl+Shift+P`), the way the
  existing right-click menus do.
- `Esc` always goes one level up; `Esc` on Home closes the Deck.
- Type-to-filter is global: on any Deck screen, typing filters the visible
  list. The search field is never a separate mode.
- Mouse works everywhere but never reveals something the keyboard can't reach.
- The Deck never covers more than 40% of the window; the terminal stays
  visible and live behind it.

## Rendering constraints (so the design is buildable as-is)

The whole UI is drawn by one shader as flat rectangles plus glyphs from a
monospace glyph atlas. Available primitives: solid rects with alpha,
1 px borders, monospace text in any color, Unicode/Nerd Font glyphs, and
small bitmap emoji. Rounded corners are fine (up to ~8 px); gradients,
blur, and drop shadows are not. Design within that and it ships pixel-for-pixel.

## Deliverables

Six artboards at 1280×800, dark theme. A tokens page listing colors,
spacing (8 px base), radii, and text styles. The 16-glyph icon set on a
single sheet with the badge colors assigned.
