---
name: showtime
description: Play the scripted Canvas demo (typing trail, laser cutter, paste rain, real work cards, pan and zoom, an agent that spawns workers, reads them, arranges, groups and renames them) on the running kindlyTerm window via MCP.
---

Run the demo script from the project root and relay its progress lines:

```bash
python3 demo/showtime.py
```

Flags: `--fast` for shorter pauses, `--cleanup` to close the Showtime tab at
the end, `--binary PATH` to use a specific kindlyterm build (default: the
one on PATH). The running window must have the control API enabled (Deck →
About). The script returns the user to the canvas they started on.

To record it, start `python3 demo/record.py <dir> &` first, play the demo,
then `touch <dir>/stop`; the script writes showtime.gif and showtime.mp4.
