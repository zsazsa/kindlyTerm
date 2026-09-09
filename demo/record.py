#!/usr/bin/env python3
"""Record the Showtime demo: grab frames over MCP while the Showtime tab is
showing, then assemble a GIF and an MP4 with ffmpeg.

    python3 demo/record.py out --window 1280x800 &   # start the recorder
    python3 demo/showtime.py                         # play the demo
    touch out/stop                                   # stop; files land in out/

`--window WxH` sizes the window for the recording (text stays at 1:1 in
the GIF, so it is readable) and restores it afterwards.
"""

import argparse
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import showtime as st  # noqa: E402


def showtime_active(m):
    for w in m.call("list_canvases")["windows"]:
        if w["current"]:
            for c in w["canvases"]:
                if c["active"]:
                    return c["name"] == "Showtime"
    return False


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out_dir", nargs="?", default="showtime-rec")
    ap.add_argument("--binary", default=os.environ.get("KINDLYTERM_BIN", "kindlyterm"))
    ap.add_argument("--window", help="WxH pixels for the recording, e.g. 1280x800")
    ap.add_argument("--fps", type=float, default=6.0)
    a = ap.parse_args()

    m = st.Mcp(a.binary)
    frames = os.path.join(a.out_dir, "frames")
    stop = os.path.join(a.out_dir, "stop")
    os.makedirs(frames, exist_ok=True)

    before = m.call("set_window")
    if a.window:
        w, h = (int(x) for x in a.window.lower().split("x"))
        m.call("set_window", width=w, height=h)
        time.sleep(1.0)

    period = 1 / a.fps
    stamps, i, t0 = [], 0, None
    while not os.path.exists(stop):
        t = time.time()
        if showtime_active(m):
            if t0 is None:
                t0 = t
            m.call("screenshot", path=f"{frames}/f{i:05d}.png")
            stamps.append(t - t0)
            i += 1
        d = period - (time.time() - t)
        if d > 0:
            time.sleep(d)

    if a.window:
        if before.get("maximized"):
            m.call("set_window", maximized=True)
        else:
            m.call("set_window", width=before["width"], height=before["height"])

    # Assemble with real frame timings; trim the first second, while the
    # camera is still gliding to the first card.
    keep = [(k, t) for k, t in enumerate(stamps) if t >= 1.3]
    if not keep:
        sys.exit("no frames: was the Showtime tab showing?")
    with open(f"{frames}/list.txt", "w") as f:
        for k, (idx, t) in enumerate(keep):
            d = (keep[k + 1][1] - t) if k + 1 < len(keep) else 1.0
            f.write(f"file 'f{idx:05d}.png'\nduration {d:.3f}\n")
        f.write(f"file 'f{keep[-1][0]:05d}.png'\n")
    common = ["ffmpeg", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i", f"{frames}/list.txt"]
    gif_w = "iw" if a.window and int(a.window.lower().split("x")[0]) <= 1280 else "1280"
    subprocess.run(common + ["-vf", "scale=iw:-2,format=yuv420p", "-r", "15", "-c:v", "libx264", "-crf", "24", os.path.join(a.out_dir, "showtime.mp4")], check=True)
    subprocess.run(common + ["-vf", f"fps={a.fps},scale={gif_w}:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle", os.path.join(a.out_dir, "showtime.gif")], check=True)
    print(f"{len(keep)} frames -> {a.out_dir}/showtime.gif and showtime.mp4")


if __name__ == "__main__":
    main()
