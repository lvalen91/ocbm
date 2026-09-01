#!/usr/bin/env python3
"""One-shot command execution on the box over the OCBM CONSOLE channel.

The UART replacement for `scratchpad/sercmd.py`. Drives `ocbm-host console`, which bridges
stdin/stdout to a persistent root PTY on the box (`/bin/sh -l`, spawned on first attach and
reused across host reconnects).

Completion is detected with a sentinel (`echo __OCBM_DONE_<tag>__$?`) rather than a fixed
sleep, so long commands are not truncated and short ones return immediately. Exit status is
the box-side command's status.

    tools/ocbmcmd.py 'ls -l /usr/sbin'
    tools/ocbmcmd.py 'cat /tmp/wl.log'
    tools/ocbmcmd.py --timeout 30 '/script/wlan_on.sh >/tmp/wlan.log 2>&1 &'

Note: the PTY is a *shared, persistent* shell — cwd and env persist between invocations.
"""
import argparse
import os
import re
import subprocess
import sys
import time

HOST = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "ocbm-host")
VID, PID = "1314", "2d00"
ANSI = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]|\x1b\[m")
# The box's PS1, e.g. `[(19:05)root@~]#`. It is emitted without a trailing newline, so it glues
# itself onto the front of the next line of real output.
PROMPT = re.compile(r"\[\(\d\d:\d\d\)[^\]]*\]#\s?")


def run(cmd: str, timeout: float = 15.0, raw: bool = False):
    tag = f"{os.getpid()}{int(time.time() * 1000) % 100000}"
    begin, done = f"__OCBM_B{tag}__", f"__OCBM_D{tag}__"

    p = subprocess.Popen(
        [HOST, "console", VID, PID],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    os.set_blocking(p.stdout.fileno(), False)

    def feed(s: str):
        p.stdin.write(s.encode())
        p.stdin.flush()

    # Phase 1: get a prompt, then kill PTY echo. Without this the box echoes the command back,
    # hard-wrapped at the PTY's 80 columns, which no amount of string-matching unwraps reliably.
    # Phase 2: run the command fenced by exact-line markers.
    buf, deadline, phase = b"", time.time() + timeout, 0
    try:
        feed("\n")
        while time.time() < deadline:
            chunk = p.stdout.read(65536)
            if chunk:
                buf += chunk
            else:
                time.sleep(0.02)
            if phase == 0 and b"#" in buf:
                feed("stty -echo\n")
                phase, buf = 1, b""
                time.sleep(0.25)
            elif phase == 1:
                feed(f"printf '{begin}\\n'\n{cmd}\nprintf '{done}%s\\n' \"$?\"\n")
                phase, buf = 2, b""
            elif phase == 2 and done.encode() in buf:
                break
    finally:
        try:
            feed("stty echo\n")  # leave the shared PTY as we found it
            p.stdin.close()
        except Exception:
            pass
        p.wait(timeout=5)

    text = buf.decode("utf-8", "replace")
    if not raw:
        text = PROMPT.sub("", ANSI.sub("", text).replace("\r\n", "\n"))

    status = 0
    m = re.findall(re.escape(done) + r"(\d+)", text)
    if m:
        status = int(m[-1])

    # The shell prompt is written without a trailing newline, so it prefixes the marker's line
    # (`[(19:05)root@~]#__OCBM_B123__`) — match the marker at the line's end, not as the whole line.
    out, started = [], False
    for ln in text.split("\n"):
        s = ln.strip()
        if not started:
            started = s.endswith(begin)
            continue
        if done in s:
            break
        out.append(ln)
    return ("\n".join(out).strip("\n") if started else text.strip()), status


def main():
    ap = argparse.ArgumentParser(description="Run a command on the ccpa box over OCBM CONSOLE.")
    ap.add_argument("command", help="shell command to run on the box")
    ap.add_argument("--timeout", type=float, default=15.0)
    ap.add_argument("--raw", action="store_true", help="don't strip ANSI/echo")
    a = ap.parse_args()

    if not os.path.exists(HOST):
        sys.exit(f"ocbm-host not built: {HOST}\n  cargo build --release -p ocbm-host")

    body, status = run(a.command, a.timeout, a.raw)
    if body:
        print(body)
    sys.exit(status)


if __name__ == "__main__":
    main()
