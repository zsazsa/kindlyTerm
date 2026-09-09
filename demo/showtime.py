#!/usr/bin/env python3
"""Showtime: a scripted tour of the Canvas, driven over MCP.

Run it against a running kindlyTerm with the control API on (Deck ->
About -> "Let Claude Code drive"):

    python3 demo/showtime.py            # play, leave the board up
    python3 demo/showtime.py --cleanup  # play, then close the board
    python3 demo/showtime.py --fast     # shorter pauses

It opens a "Showtime" canvas, types with the neon trail, lets Pac-Man eat
the line both ways, pastes a banner as rain, then does real work: a cargo
build with a silence monitor, a docs server and a client hitting it, git.
It pans and zooms around the board (each move one call, the camera
glides on its own). Then an agent takes over: it narrates its plan in its
own log card, pinned to the screen so it stays in view, spawns three
workers, reads their screens back over MCP and writes down what it found,
lines the cards up, groups them, grows and renames the group, renames a
card, closes the finished ones and regroups. It then tidies the rest of
the board: clears the build's silence monitor once it has read the
result, groups build with git and the docs server with its client, and
zooms out to the whole board. It returns you to the canvas you started
on. Everything goes through `kindlyterm --mcp`, the same server Claude
Code uses, so what you see is exactly what an agent can do.
"""

import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

TYPED = (
    'echo "Welcome to kindlyTerm: a GPU-accelerated terminal for Linux. This is '
    "the Canvas, an infinite zoomable board where every shell is a card. Shells "
    "outlive the window, and an agent such as Claude Code can open, read, type "
    'into and arrange them. It is typing this now, fast enough to leave a trail."'
)
TYPED_2 = 'echo "Delete eats forward. Backspace eats backward. Either way the line gets shorter."'
CLOSING = 'echo "That\'s the show. Every card here was placed by an agent, and you can watch it work."'


# Every demo card runs bash with a staged prompt (dev@kindlyTerm), so a
# recording shows no real user or host name. `command` runs first.
RC = os.path.join(HERE, "rc.sh")
SHELL = f"exec bash --rcfile {RC}"


def with_shell(command=None):
    return f"{command}; {SHELL}" if command else SHELL


class Mcp:
    """Minimal MCP stdio client: enough to call tools on kindlyterm --mcp."""

    def __init__(self, binary):
        self.p = subprocess.Popen(
            [binary, "--mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        self.n = 0
        self.rpc("initialize", {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "showtime", "version": "1"},
        })

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            sys.exit("kindlyterm --mcp went away. Is the control API enabled in the Deck?")
        return json.loads(line)

    def call(self, tool, **args):
        r = self.rpc("tools/call", {"name": tool, "arguments": args})
        text = r.get("result", {}).get("content", [{}])[0].get("text", "")
        try:
            out = json.loads(text)
        except ValueError:
            out = text
        if isinstance(out, str) and out:
            sys.exit(f"{tool}: {out}")
        return out


def glide(m, canvas, v, dx, dy):
    """Pan the view by (dx, dy) world units. The app glides there itself."""
    v["view"]["x"] += dx
    v["view"]["y"] += dy
    m.call("set_viewport", canvas_id=canvas, x=v["view"]["x"], y=v["view"]["y"])


def zoom_about(m, canvas, wx, wy, factor):
    """Zoom the view by `factor`, keeping world point (wx, wy) fixed on screen."""
    v = m.call("set_viewport", canvas_id=canvas)
    x0, y0, z0 = v["view"]["x"], v["view"]["y"], v["view"]["zoom"]
    sx, sy = (wx - x0) * z0, (wy - y0) * z0  # screen position to hold
    z = z0 * factor
    m.call("set_viewport", canvas_id=canvas, x=wx - sx / z, y=wy - sy / z, zoom=z)


def frame(m, canvas, x, y, w, h, pad=48, left=0):
    """Glide the view to fit the world rectangle (x, y, w, h), using only
    the part of the window right of `left` screen pixels."""
    v = m.call("set_viewport", canvas_id=canvas)
    aw, ah = v["area_w"] - left, v["area_h"]
    z = min(aw / (w + 2 * pad), ah / (h + 2 * pad), 4.0)
    m.call("set_viewport", canvas_id=canvas, x=x + w / 2 - (left + aw / 2) / z, y=y + h / 2 - ah / 2 / z, zoom=z)


def rect_of(m, item_id=None, group_id=None):
    """World rectangle (x, y, w, h) of an item or a group, from list_canvases."""
    for w in m.call("list_canvases")["windows"]:
        for c in w["canvases"]:
            for i in c["items"]:
                if item_id is not None and i["item_id"] == item_id:
                    return i["x"], i["y"], i["w"], i["h"]
            for g in c["groups"]:
                if group_id is not None and g["group_id"] == group_id:
                    return g["x"], g["y"], g["w"], g["h"]
    return 0, 0, 100, 100


def union(*rects):
    x0 = min(r[0] for r in rects)
    y0 = min(r[1] for r in rects)
    x1 = max(r[0] + r[2] for r in rects)
    y1 = max(r[1] + r[3] for r in rects)
    return x0, y0, x1 - x0, y1 - y0


# The agent's log is a bash with a custom prompt. Each line it "says" is a
# shell comment, so it stays on screen exactly as typed and runs nothing.
AGENT_SHELL = r"clear; PS1='\[\e[35m\]agent ›\[\e[0m\] ' exec bash --norc --noprofile"


def say(m, log, text, cps=40):
    """Type a line into the agent's log with the trail, and wait for it."""
    line = "# " + text
    m.call("send_text", terminal_id=log, text=line, typing=cps, enter=True)
    time.sleep(len(line) / cps + 0.6)


def last_line(m, terminal_id):
    """The last line of output on a terminal's screen, prompt excluded."""
    lines = [l.rstrip() for l in m.call("read_screen", terminal_id=terminal_id)["lines"] if l.strip()]
    lines = [l for l in lines if not l.endswith(("$", "#", "%", ">"))]
    return lines[-1].strip() if lines else "no output yet"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=os.environ.get("KINDLYTERM_BIN", "kindlyterm"), help="kindlyterm binary (default: on PATH)")
    ap.add_argument("--cleanup", action="store_true", help="close the Showtime canvas at the end")
    ap.add_argument("--fast", action="store_true", help="shorter pauses between acts")
    a = ap.parse_args()
    pace = 0.5 if a.fast else 1.0

    def wait(s, why, hold=False):
        # Pauses that cover an animation in flight are never shortened;
        # --fast only trims the pauses that just let you look.
        s = s * pace if hold else s
        print(f"   … {why} ({s:.1f}s)")
        time.sleep(s)

    m = Mcp(a.binary)
    home = None
    for w in m.call("list_canvases")["windows"]:
        for c in w["canvases"]:
            if c["active"] and w["current"]:
                home = c["items"][0]["item_id"] if c["items"] else None

    print("▶ Act 1 · the typewriter")
    canvas = m.call("create_canvas", name="Showtime")["canvas_id"]
    tw = m.call("create_terminal", canvas_id=canvas, name="typewriter", command=with_shell(), x=0, y=0, cols=72, rows=10)
    m.call("zoom_to", item_id=tw["item_id"])
    wait(1.5, "shell starting", hold=True)
    m.call("send_text", terminal_id=tw["terminal_id"], text=TYPED, typing=24)
    wait(len(TYPED) / 24 + 2.0, "typing with the neon trail")
    m.call("send_key", terminal_id=tw["terminal_id"], key="backspace", count=len(TYPED) + 5)
    wait((len(TYPED) + 5) * 0.04 + 2.5, "Pac-Man eating it backwards")
    m.call("send_text", terminal_id=tw["terminal_id"], text=TYPED_2, typing=28)
    wait(len(TYPED_2) / 28 + 1.5, "typing again")
    m.call("send_key", terminal_id=tw["terminal_id"], key="home")
    m.call("send_key", terminal_id=tw["terminal_id"], key="delete", count=len(TYPED_2) + 5)
    wait((len(TYPED_2) + 5) * 0.04 + 2.5, "Pac-Man eating it forwards")

    print("▶ Act 2 · the rain")
    # A real shell. The agent pastes a block: a heredoc that prints the
    # banner. The paste falls in as rain, then Enter runs it.
    rain = m.call("create_terminal", canvas_id=canvas, name="rain", command=with_shell(), x=0, y=220, cols=80, rows=18)
    m.call("zoom_to", item_id=rain["item_id"])
    wait(1.5, "a shell, waiting", hold=True)
    with open(os.path.join(HERE, "banner.txt"), encoding="utf-8") as f:
        block = "clear; cat <<'BANNER'\n" + f.read().rstrip("\n") + "\nBANNER"
    m.call("send_text", terminal_id=rain["terminal_id"], text=block)
    wait(4.0, "the block falls in as rain", hold=True)
    m.call("send_key", terminal_id=rain["terminal_id"], key="enter")
    wait(2.0, "Enter: the shell prints the banner", hold=True)

    print("▶ Act 3 · real work")
    # Real cards: a build, a docs server with a client hitting it, and git.
    build = m.call("create_terminal", canvas_id=canvas, name="build", cwd=REPO,
                   command=with_shell('touch src/main.rs && cargo build --release --color=always 2>&1 | sed -u "s|$HOME|~|g"'), x=720, y=0, cols=70, rows=22)
    m.call("set_monitor", item_id=build["item_id"], seconds=6)
    git = m.call("create_terminal", canvas_id=canvas, name="git", cwd=REPO,
                 command=with_shell("git --no-pager log --graph --color=always --oneline -12; echo; git status --short"), x=720, y=420, cols=70, rows=16)
    server = m.call("create_terminal", canvas_id=canvas, name="docs server", cwd=REPO,
                    command=with_shell("python3 -m http.server 8765 --directory docs"), x=1340, y=0, cols=60, rows=12)
    client = m.call("create_terminal", canvas_id=canvas, name="requests", cwd=REPO,
                    command=with_shell("sleep 1; for f in / /PLAN-canvas.md /nope /; do curl -s -o /dev/null -w '%{http_code}  %{url}\\n' http://127.0.0.1:8765$f; sleep 1.5; done"),
                    x=1340, y=250, cols=60, rows=10)
    m.call("zoom_to")
    wait(2.0, "the board, fit to the window", hold=True)

    print("▶ Act 4 · pan and zoom")
    # Every camera move is one call; the app glides there on its own.
    v = m.call("set_viewport", canvas_id=canvas)  # unchanged view, just read it
    glide(m, canvas, v, dx=-320, dy=0)
    wait(1.2, "panned left", hold=True)
    glide(m, canvas, v, dx=+320, dy=0)
    wait(1.2, "and back", hold=True)
    cx, cy = 720 + 280, 180  # middle of the build card
    zoom_about(m, canvas, cx, cy, 2.0)
    wait(2.5, "zoomed in on the build", hold=True)
    zoom_about(m, canvas, cx, cy, 0.55)
    wait(1.0, "zoomed out", hold=True)
    m.call("zoom_to")
    wait(1.5, "back to the whole board", hold=True)

    print("▶ Act 5 · the agents organize")
    # An agent narrates in its own card, spawns workers, reads what they
    # print, then tidies: moves, groups, renames, closes. Every step is one
    # MCP call. The log is pinned to the glass so it stays in view while
    # the camera follows where the agent is looking.
    LX, LY = 0, 800  # the scene sits below the first four acts
    log = m.call("create_terminal", canvas_id=canvas, name="agent · orchestrator", command=AGENT_SHELL, x=LX, y=LY, cols=76, rows=18)
    m.call("zoom_to", item_id=log["item_id"])
    wait(1.5, "the agent opens a log card", hold=True)
    say(m, log["terminal_id"], "plan: audit the repo. three workers, one job each.")
    say(m, log["terminal_id"], "pinning this log to the glass so you can follow along.")
    _, _, log_w, log_h = rect_of(m, item_id=log["item_id"])
    area = m.call("set_viewport", canvas_id=canvas)
    m.call("pin_item", item_id=log["item_id"], pinned=True, x=16, y=area["area_h"] - log_h - 16)
    LEFT = log_w + 48  # the camera keeps out from under the pinned log
    wait(1.0, "log pinned bottom-left, 1:1", hold=True)

    jobs = [
        ("lines", "wc -l src/*.rs src/app/*.rs | sort -n | tail -6", (760, 880)),
        ("todos", "grep -rncE 'TODO|FIXME' src | awk -F: '{n+=$2} END {print n \" markers in \" NR \" files\"}'", (1260, 1000)),
        ("history", "git --no-pager log --format='%h  %ar  %s' -6; echo; git --no-pager log -1 --format='last commit %ar'", (980, 1200)),
    ]
    workers = []
    for name, cmd, (x, y) in jobs:
        say(m, log["terminal_id"], f"spawning worker '{name}'")
        w = m.call("create_terminal", canvas_id=canvas, name=f"worker · {name}", cwd=REPO, command=with_shell(cmd), x=x, y=y, cols=52, rows=10)
        workers.append((name, w))
        frame(m, canvas, *rect_of(m, item_id=w["item_id"]), left=LEFT)
        wait(1.6, f"'{name}' spawned, camera on it", hold=True)
    scene = union(*[rect_of(m, item_id=w["item_id"]) for _, w in workers])
    frame(m, canvas, *scene, left=LEFT)
    wait(1.5, "three scattered workers", hold=True)

    say(m, log["terminal_id"], "reading their screens")
    for name, w in workers:
        found = last_line(m, w["terminal_id"])
        say(m, log["terminal_id"], f"{name}: {found}", cps=48)
    wait(1.5, "each summary came from read_screen", hold=True)

    say(m, log["terminal_id"], "tidying: lining the workers up")
    row_x = [700, 1220, 1740]
    for (name, w), x in zip(workers, row_x):
        m.call("move_item", item_id=w["item_id"], x=x, y=LY)
        wait(0.6, f"moved '{name}' into the row", hold=True)
    row = union(*[rect_of(m, item_id=w["item_id"]) for _, w in workers])
    frame(m, canvas, *row, left=LEFT)
    wait(1.5, "a neat row", hold=True)

    say(m, log["terminal_id"], "grouping 'lines' and 'todos'. 'history' stays out for now.")
    audit = m.call("create_group", item_ids=[workers[0][1]["item_id"], workers[1][1]["item_id"]], name="workers")
    wait(2.5, "a frame around two of the three", hold=True)
    say(m, log["terminal_id"], "now 'history' joins: the frame grows to take it in.")
    m.call("add_to_group", group_id=audit["group_id"], item_id=workers[2][1]["item_id"])
    frame(m, canvas, *rect_of(m, group_id=audit["group_id"]), left=LEFT)
    wait(2.5, "the frame grew", hold=True)
    say(m, log["terminal_id"], "renaming the group: audit · 3 workers")
    m.call("rename_group", group_id=audit["group_id"], name="audit · 3 workers")
    wait(2.0, "renamed", hold=True)

    say(m, log["terminal_id"], "'lines' and 'history' are done. closing them.")
    m.call("close_terminal", terminal_id=workers[0][1]["terminal_id"])
    wait(0.8, "closed 'lines'", hold=True)
    m.call("close_terminal", terminal_id=workers[2][1]["terminal_id"])
    wait(0.8, "closed 'history'", hold=True)
    say(m, log["terminal_id"], "'todos' needs a human. renaming it, making it bigger.")
    todos = workers[1][1]["item_id"]
    m.call("rename_item", item_id=todos, name="todos · for a human")
    m.call("resize_item", item_id=todos, cols=60, rows=14)
    say(m, log["terminal_id"], "regrouping around what is left")
    # A group is a frame and frames never shrink: dissolve the roomy one and
    # build a snug one around the card that stayed.
    m.call("move_item", item_id=todos, x=row_x[0], y=LY)
    m.call("delete_group", group_id=audit["group_id"])
    audit = m.call("create_group", item_ids=[todos], name="audit · 1 open")
    frame(m, canvas, *rect_of(m, group_id=audit["group_id"]), left=LEFT)
    wait(2.5, "one card, renamed and enlarged, in a fresh snug group", hold=True)

    print("▶ Act 6 · the rest of the board")
    # The agent turns to the cards from Act 3: a silence monitor that fired,
    # and loose cards that belong together.
    quiet = any(t["terminal_id"] == build["terminal_id"] and t["quiet_alert"] for t in m.call("get_activity"))
    frame(m, canvas, *rect_of(m, item_id=build["item_id"]), left=LEFT)
    if quiet:
        say(m, log["terminal_id"], "the build card is blinking: its silence monitor fired.")
        say(m, log["terminal_id"], "build says: " + last_line(m, build["terminal_id"])[:52], cps=48)
        say(m, log["terminal_id"], "that is a finished build. clearing the monitor.")
        m.call("set_monitor", item_id=build["item_id"], seconds=None)
        wait(1.5, "the build frame stops blinking", hold=True)
    else:
        say(m, log["terminal_id"], "the build is still running; its monitor will blink when it goes quiet.")
        wait(1.5, "build still busy", hold=True)
    say(m, log["terminal_id"], "build and git belong together: grouping them as 'repo'")
    repo = m.call("create_group", item_ids=[build["item_id"], git["item_id"]], name="repo")
    frame(m, canvas, *rect_of(m, group_id=repo["group_id"]), left=LEFT)
    wait(2.5, "the repo group", hold=True)
    say(m, log["terminal_id"], "the docs server and the requests that hit it: 'docs site'")
    web = m.call("create_group", item_ids=[server["item_id"], client["item_id"]], name="docs site")
    frame(m, canvas, *rect_of(m, group_id=web["group_id"]), left=LEFT)
    wait(2.5, "the docs site group: a server and its client", hold=True)

    say(m, log["terminal_id"], "done. unpinning my log and handing back.")
    m.call("pin_item", item_id=log["item_id"], pinned=False)
    m.call("move_item", item_id=log["item_id"], x=LX, y=LY)
    m.call("zoom_to")
    wait(2.5, "the whole board, tidy", hold=True)
    m.call("send_text", terminal_id=tw["terminal_id"], text=CLOSING, typing=30, enter=True)
    wait(len(CLOSING) / 30 + 3.0, "closing line, the whole board in view")

    if a.cleanup:
        print("▶ cleaning up")
        m.call("delete_canvas", canvas_id=canvas)
    if home is not None:
        m.call("focus", item_id=home)
    print("✓ done" + ("" if a.cleanup else " · the Showtime tab is still up; close it when you like"))


if __name__ == "__main__":
    main()
