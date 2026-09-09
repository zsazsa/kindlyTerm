# Plan: the Canvas — an infinite, zoomable terminal multiplexer

Status: approved 2026-09-09, in progress on the `canvas` branch. `main` stays
at tag `v0.1-tabs` (the tabbed app) until the Canvas is good enough to merge;
`git checkout v0.1-tabs` is the revert point.

Decisions taken (2026-09-09):
1. Tabs stay, and **every tab is a canvas**: a terminal tab is a canvas
   with one maximized item (today's behaviour); a canvas tab is the same
   thing in free layout. Tabs convert both ways. No sidebar needed.
2. Canvas actions use `Ctrl+Shift` chords; the Deck's quick-run moves off
   `Ctrl+Shift+P` (which becomes Pin) to tap-`Ctrl+Shift` and `Ctrl+Shift+Space`.
3. Font stays fontconfig's monospace (no bundled font).
4. State persists beside the config: `~/.config/kindlyterm/state.json`.
5. MCP tools are shaped for Claude Code as the first client.
The product and feature are simply "kindlyTerm" and "the Canvas".

## Vision

Terminals are windows you place, resize, group and pan around on an infinite
surface, not tiles in a grid. Zoom in to work in one shell; zoom out to watch
builds, tests and agents progress across the whole session. Every shell runs
in its own detached PTY host, so closing or crashing the app never kills a
session: restart, and each terminal comes back with its screen, its scrollback
and a replay of exactly the output it missed. Multiple canvases live in a
sidebar, each with its own working directory and viewport, all restored on
launch. An optional MCP server lets an agent read, type into, and arrange
anything on the canvas.

## What changes for the user

| Today (tabs) | Canvas |
|---|---|
| Tab bar with one terminal visible | Same tab bar; a tab is either a maximized terminal or a free canvas with many terminals at real coordinates |
| New tab | Same (`Ctrl+Shift+T`); inside a canvas, `Ctrl+Shift+Enter` or right-click adds a terminal to it |
| Drag tab to reorder / tear off / merge windows | Unchanged, and works for canvas tabs too (the whole canvas moves) |
| Rename tab | Unchanged; double-click a terminal's title bar renames it inside a canvas |
| Close app = shells die | Close app = shells keep running in their hosts; reopen and continue |
| Deck, effects, themes, clipboard, cheat sheet | Unchanged; the Deck gains a Canvases section and per-terminal theme |

Everything already shipped (Deck, shortcuts, hotkeys, effects, cursor, copy
and paste behaviour, security policies) carries over unchanged.

## Architecture

```
 ┌──────────────── kindlyterm (UI process) ─────────────────┐
 │  Window ── Canvas view (pan/zoom) ── TerminalView × N     │
 │            │                          │ alacritty Term    │
 │            │                          │ (parses replay +  │
 │            │                          │  live stream)     │
 │  Sidebar   │  Groups · Pins · Mirrors · Images · Monitor  │
 │  Deck      │  Persistence (state.json)  Control socket    │
 └────────────┼──────────────────────────────────┬───────────┘
              │ unix sockets, one per session     │ optional
 ┌────────────┴───────────┐  ┌──────────────┐    │
 │ kindlyterm --host <id> │  │ host <id2> … │  kindlyterm --mcp
 │  pty ⇄ shell            │  └──────────────┘  (stdio bridge for
 │  ring log w/ seq nums   │                     Claude Code etc.)
 │  headless Term (snapshot)│
 └─────────────────────────┘
```

### 1. Detached PTY hosts (durability)

- `kindlyterm --host <session-id>` is the same binary in host mode: it
  double-forks into its own session, opens the PTY, spawns the shell, and
  listens on `$XDG_RUNTIME_DIR/kindlyterm/<session-id>.sock`.
- The host keeps two things: a **raw output log** with monotonically
  increasing byte sequence numbers (bounded ring, e.g. 16 MiB), and a
  **headless alacritty `Term`** fed with the same bytes, so it always knows
  the exact screen, scrollback and terminal modes.
- Protocol (length-prefixed frames, bincode or a tiny hand-rolled encoding):
  `Hello{last_seq}`, `Output{seq, bytes}`, `Input{bytes}`, `Resize{cols, rows}`,
  `Snapshot{…}`, `Exited{code}`, `Ping/Pong`.
- **Reconnect rule.** The UI remembers the last `seq` it rendered per
  terminal. On `Hello{last_seq}`: if `last_seq` is still inside the ring, the
  host replays bytes from `last_seq + 1`, so the UI's own `Term` continues
  with no gap and no duplication. If it has fallen out of the ring (or the UI
  has no state, e.g. adopting an orphan), the host sends a **snapshot**:
  the headless `Term`'s scrollback and screen re-materialised as escape
  sequences (SGR + text per line, cursor position, alt-screen and mode flags,
  kitty keyboard flags), followed by the live stream. This is how tmux
  attaches, and it needs no serialisation of alacritty internals.
- **Crash recovery / adoption.** On launch the UI scans the socket
  directory, connects to every live host, matches session ids to the
  persisted canvas state, and places any unmatched host as a new terminal on
  a "Recovered" canvas.
- Hosts exit when their shell exits (and tell the UI), or on an explicit
  `Kill`. A `kindlyterm --sessions` subcommand lists and prunes hosts.
- Security: sockets are `0600` in the user's runtime dir; the protocol is
  local only; no network.

### 2. Canvas model and rendering

- A **Canvas** has a name, a working directory, a viewport (`pan_x, pan_y,
  zoom`), and items: terminals, images, groups, pins.
- A **terminal item** has world-space `x, y, w, h` in pixels at zoom 1; its
  cols/rows derive from its size and the cell metrics. A title bar carries the
  name or shell title, the monitor dot, and close.
- **Rendering.** The existing instanced-quad renderer already draws at
  arbitrary pixel positions, and the glyph atlas already rasterises at any
  size, so zoom is `font_px = base_px × zoom`, quantised to a handful of
  levels so the atlas stays bounded. Two additions to the renderer:
  - **Scissor rects** per item so text is clipped to its frame (also fixes
    the Deck overflow we currently mask by draw order).
  - **Textured quads** for images (one bind group per image, GIF frames as
    a small texture array).
- **Level of detail.** Below ~5 px per cell, a terminal renders as its cell
  background colours plus a per-line "ink density" bar, so activity is
  legible from far out without drawing thousands of unreadable glyphs.
- Only visible items are drawn; the batch is rebuilt per frame as today.

### 3. Interaction

Key mapping note: the source list uses Cmd. On GNOME, `Super` is the
overview key and `Super+drag` moves windows, and `Alt+drag` is a WM gesture
on many desktops, so the canvas chords use `Ctrl+Shift`, consistent with the
rest of the app.

- **Pan**: wheel over empty canvas; middle-drag; `Space`+drag when no text
  field is focused; **double-Ctrl pan** (tap Ctrl, then hold it and move).
- **Zoom**: `Ctrl+wheel` about the pointer; `Ctrl+Shift+=`/`-` about the
  focused terminal; `Ctrl+Shift+0` resets to 100 %.
- **Wheel goes where you look**: over a terminal it scrolls that terminal,
  focused or not; over empty canvas it pans.
- **Focus Mode** `Ctrl+Shift+F`: zoom the selection (terminal or group) to
  fill the view; press again to restore the previous viewport.
- Select by click; move by dragging the title bar; resize from edges and
  corners with live grid reflow; snap to other items' edges with a small
  magnetic threshold.
- **Groups**: `Ctrl+Shift+G` groups the selection into a dashed, tinted frame
  with a name. Dragging the frame moves everything inside; resizing the
  frame changes membership by containment. `Ctrl+Shift+F` on a group zooms
  to it.
- **Pins**: `Ctrl+Shift+P` pins a terminal to the screen (rendered in screen
  space at a chosen corner, unaffected by pan and zoom) so it stays visible
  while you work elsewhere. Note: `Ctrl+Shift+P` currently opens the Deck
  quick-run; that moves to the Deck's own chord (tap Ctrl+Shift) and
  `Ctrl+Shift+Space`.
- **Mirrors**: a second view of the same session anywhere on the canvas
  (or on another canvas); both render the same `Term`, either can type.
- **Inactivity monitor**: toggle per terminal; after N seconds of silence
  its border blinks in the item's highlight colour until output resumes.
- **Images**: drop a file, paste from the clipboard, or pick via the Deck;
  PNG, JPEG, and animated GIF; they move, resize and snap like terminals.
- **Window tabs / titles**: `Ctrl+Shift+R` renames; title mode per terminal
  chooses custom name or shell-reported title.

### 4. Keyboard fidelity and text

- **Kitty keyboard protocol**: alacritty_terminal already tracks the mode
  flags; `keys.rs` grows an encoder for the CSI u form so Shift+Enter,
  Ctrl+Enter, Alt+Backspace and all F-keys reach applications that ask for
  them. Legacy encoding stays for everything else.
- **Scrollback keys**: Home, End, PageUp and PageDown scroll the terminal
  when no application has enabled application-cursor or the kitty protocol.
- **Smart selection**: alacritty's `selection_to_string` already joins
  soft-wrapped lines; expose that consistently for copy and drag.
- **Links**: detect URLs across wrapped lines plus OSC 8 hyperlinks;
  `Ctrl+click` opens with `xdg-open`; hover underlines.
- **Resize reflow**: already provided by alacritty; keep it.
- **Font**: fontconfig's monospace stays the default (any installed family
  via config); add fitted box drawing (draw box glyphs ourselves so they
  join) and keep colour emoji as today.
- **Live configuration**: watch `~/.config/kindlyterm/*.toml` with inotify
  (`notify` crate) and re-apply without restart, using the same code paths
  the Deck already uses to apply settings live.

### 5. Persistence

- State lives in `~/.config/kindlyterm/state.json` (beside the config),
  written debounced after every change and on exit: canvases (name, cwd,
  viewport, sidebar order), items (kind, geometry, name, title mode, theme,
  monitor settings, pin corner, mirror source, image path), groups, focus,
  window size and sidebar width, and per-terminal `session_id` + `last_seq`.
- Terminal modes an application had pushed (alt screen, bracketed paste,
  kitty flags, cursor shape) are not persisted by the UI; they come back from
  the host's snapshot, which is authoritative.

### 6. MCP server

- Off by default; toggled from the Deck's About page. When on, the UI
  listens on a **control socket** (`$XDG_RUNTIME_DIR/kindlyterm/control.sock`,
  `0600`) speaking a small JSON-RPC API.
- `kindlyterm --mcp` is a stdio MCP server (what Claude Code, Cursor and
  others spawn) that bridges to the control socket. Tools, grouped:
  - canvases: `list_canvases`, `create_canvas`, `rename_canvas`,
    `delete_canvas`, `set_viewport`, `focus_canvas`
  - terminals: `list_terminals`, `read_screen`, `read_scrollback`,
    `send_text`, `send_key`, `create_terminal`, `close_terminal`,
    `move_item`, `resize_item`, `rename_terminal`, `set_title_mode`,
    `set_theme`, `zoom_to`
  - groups/pins/mirrors: `create_group`, `set_group_members`,
    `delete_group`, `pin_item`, `unpin_item`, `mirror_terminal`
  - images: `place_image`, `remove_item`
  - monitor: `set_monitor`, `get_activity`
  - session: `list_sessions`, `adopt_session`, `screenshot_canvas`
- Every tool that types or closes something is logged in the status bar, and
  `read_*` tools redact nothing, so the toggle is the security boundary; the
  About page says so plainly.

## Phases (each one ships on its own)

1. **Renderer groundwork**: scissor rects, textured quads, zoom-quantised
   glyph sizes, multiple `Term`s per window. Visible result: nothing new,
   but the existing app runs on the new plumbing.
2. **Canvas core**: every tab is a canvas (Single = classic full-window
   terminal, Free = free layout), terminals at world coordinates,
   pan/zoom/select/move/resize, wheel routing, Focus Mode, snapping, layout
   persistence in `state.json`. Tabs stay; a tab converts to a free canvas
   the moment a second terminal is added, and a one-item canvas can be
   maximized back. Shells still run in-process.
3. **PTY hosts**: host process, protocol, replay and snapshot, adoption,
   `--sessions`. Visible result: quit and relaunch, everything is still
   running.
4. **Spatial features**: groups, pins, mirrors, inactivity monitor, images,
   rename and title mode, LOD rendering.
5. **Keyboard and text**: kitty protocol, scrollback keys, links, live
   config reload (fontconfig monospace stays; no bundled font).
6. **MCP server**: control socket, stdio bridge, About toggle, tool set.

Rough relative sizes: 1 small · 2 large · 3 large · 4 large · 5 medium ·
6 medium. Phases 4 and 5 can run in parallel once 2 and 3 are in.

## Risks and how the plan handles them

- **Wayland**: no global pointer, no window positioning. The canvas is
  entirely inside our window, so none of that matters here; only the
  Super/Alt modifier collision does, hence `Ctrl+Shift` chords.
- **Replay exactness**: guaranteed by byte sequence numbers; the snapshot
  path is the fallback and is reconstructive, not byte-exact, which is
  acceptable because it only happens when the UI has no prior state.
- **Performance at far zoom**: LOD rendering and visibility culling; the
  renderer already handles tens of thousands of quads per frame.
- **Host lifetime**: hosts are plain processes; if a host dies, its shell
  dies with it (same as today). Mitigation is process isolation, not magic.
- **MCP exposure**: default off, local socket, user-visible logging.

## Progress

- [x] Phase 1: renderer groundwork (scissor segments, image textures, zoomed glyphs, reusable terminal drawing, per-terminal view state)
- [x] Phase 2: canvas core (tabs-are-canvases, move/resize/snap, pan/zoom, focus mode, fit, menus, rename, tear-off of canvas tabs, `state.json` restore; Ctrl+Shift+Enter/K/F/A/=/−/0, Deck quick-run moved to Ctrl+Shift+Space)
- [x] Phase 3: PTY hosts (`--host` process per shell, framed Unix-socket protocol, 8 MiB replay ring, headless Term snapshot incl. alt screen + modes + kitty flags, size-aware replay/snapshot choice, kill on close vs detach on quit, orphan adoption onto a Recovered canvas, `--sessions [--prune]`, Deck toggle, in-process fallback)
- [x] Phase 4: spatial features (Shift-select + rubber band, groups with containment membership, pins in screen space, mirrors, inactivity monitor, images incl. animated GIF via drop/paste, minimap level of detail below 6px cells)
- [x] Phase 5: keyboard and text (kitty keyboard protocol with unit tests, prompt-time PageUp/PageDown, OSC 8 + URL links with Ctrl+hover/click and menu, live reload of the three config files via inotify; fitted box-drawing glyphs deferred)
- [ ] Phase 6: MCP server
