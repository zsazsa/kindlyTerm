#!/usr/bin/env python3
"""Record the Showtime demo: grab frames over MCP while the Showtime tab is
showing, then assemble a GIF and an MP4 with ffmpeg.

    python3 demo/record.py out_dir &      # start the recorder
    python3 demo/showtime.py              # play the demo
    touch out_dir/stop                    # stop; files land in out_dir
"""
import sys, time, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); import showtime as st
out_dir = sys.argv[1] if len(sys.argv) > 1 else "showtime-rec"
m = st.Mcp("kindlyterm")
out = os.path.join(out_dir, "frames"); stop = os.path.join(out_dir, "stop")
os.makedirs(out, exist_ok=True)
period = 1/7
def showtime_active():
    for w in m.call("list_canvases")["windows"]:
        if w["current"]:
            for c in w["canvases"]:
                if c["active"]:
                    return c["name"] == "Showtime"
    return False
i = 0; stamps = []; t0 = None
while not os.path.exists(stop):
    t = time.time()
    if showtime_active():
        if t0 is None: t0 = t
        m.call("screenshot", path=f"{out}/f{i:05d}.png")
        stamps.append(t - t0); i += 1
    d = period - (time.time() - t)
    if d > 0: time.sleep(d)
with open(f"{out}/stamps.txt", "w") as f:
    f.write("\n".join(f"{s:.3f}" for s in stamps))
print("frames:", i)

# Assemble with real frame timings; trim the first second (the camera is
# still gliding to the first card).
keep = [(k, t) for k, t in enumerate(stamps) if t >= 1.3]
with open(f"{out}/list.txt", "w") as f:
    for k, (idx, t) in enumerate(keep):
        d = (keep[k + 1][1] - t) if k + 1 < len(keep) else 1.0
        f.write(f"file 'f{idx:05d}.png'\nduration {d:.3f}\n")
    if keep:
        f.write(f"file 'f{keep[-1][0]:05d}.png'\n")
import subprocess
common = ["ffmpeg", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i", f"{out}/list.txt"]
subprocess.run(common + ["-vf", "scale=1280:-2,format=yuv420p", "-r", "15", "-c:v", "libx264", "-crf", "24", os.path.join(out_dir, "showtime.mp4")], check=True)
subprocess.run(common + ["-vf", "fps=7,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle", os.path.join(out_dir, "showtime.gif")], check=True)
print("wrote", os.path.join(out_dir, "showtime.gif"), "and showtime.mp4")
