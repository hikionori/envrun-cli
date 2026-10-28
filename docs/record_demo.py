#!/usr/bin/env python3
"""Render real envrun command output and a live TUI capture using sample data.

Dependencies: Pillow, pyte. Run from any directory after `cargo build --locked`.
"""

import argparse
import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import struct
import subprocess
import tempfile
import termios
import time

from PIL import Image, ImageDraw, ImageFont
import pyte


ROOT = Path(__file__).resolve().parents[1]
COLS, ROWS = 116, 24
BG, FG = "#111827", "#d1d5db"
COLORS = {
    "default": FG, "black": "#111827", "red": "#f87171",
    "green": "#4ade80", "brown": "#facc15", "blue": "#60a5fa",
    "magenta": "#c084fc", "cyan": "#22d3ee", "white": "#e5e7eb",
    "brightblack": "#9ca3af", "brightred": "#fca5a5",
    "brightgreen": "#86efac", "brightbrown": "#fde047",
    "brightblue": "#93c5fd", "brightmagenta": "#d8b4fe",
    "brightcyan": "#67e8f9", "brightwhite": "#ffffff",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/envrun")
    parser.add_argument("--font", type=Path,
                        default=Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"))
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error("Build envrun first or pass --binary /path/to/envrun")
    font = ImageFont.truetype(str(args.font), 16)
    cell, line = round(font.getlength("M")), 23
    frames, durations = [], []

    def color(value, background=False):
        if value == "default":
            return BG if background else FG
        return COLORS.get(value, "#" + value if len(value) == 6 else FG)

    def capture(screen, title, duration):
        image = Image.new("RGB", (COLS * cell + 40, ROWS * line + 86), BG)
        draw = ImageDraw.Draw(image)
        draw.text((20, 14), title, font=font, fill="#93c5fd")
        draw.text((20, 39), "envrun | real CLI output + live TUI | sample data", font=font, fill="#9ca3af")
        for y in range(ROWS):
            for x in range(COLS):
                char = screen.buffer[y][x]
                fg, bg = color(char.fg), color(char.bg, True)
                if char.reverse:
                    fg, bg = bg, fg
                left, top = 20 + x * cell, 73 + y * line
                draw.rectangle((left, top, left + cell - 1, top + line - 1), fill=bg)
                draw.text((left, top), char.data, font=font, fill=fg)
        frames.append(image)
        durations.append(duration)

    with tempfile.TemporaryDirectory(prefix="envrun-demo-") as directory:
        base = Path(directory)
        repo = base / "myapp"
        repo.mkdir()
        env = os.environ.copy()
        env.update(XDG_CONFIG_HOME=str(base / "config"), TERM="xterm-256color",
                   GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                   GIT_TERMINAL_PROMPT="0", NO_COLOR="1")
        env["PATH"] = str(binary.parent) + os.pathsep + env["PATH"]

        def run(command, cwd=repo):
            return subprocess.run(command, cwd=cwd, env=env, check=True,
                                  text=True, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT).stdout

        run(["git", "-c", "init.templateDir=", "init", "-b", "main"])
        (repo / ".gitignore").write_text(".env\n")
        (repo / ".env").write_text("APP_ENV=development\nLOG_LEVEL=debug\nDATABASE_URL=postgres://demo:demo@localhost/demo\n")
        run(["git", "add", ".gitignore"])
        run(["git", "-c", "user.name=Demo", "-c", "user.email=demo@example.invalid",
             "-c", "core.hooksPath=/dev/null", "commit", "-m", "Demo fixture"])

        def scene(title, commands, cwd=repo, duration=2800):
            screen = pyte.Screen(COLS, ROWS)
            stream = pyte.Stream(screen)
            for command in commands:
                stream.feed("\x1b[32m$ " + shlex.join(command) + "\x1b[0m\r\n")
                output = run(command, cwd)
                stream.feed(output.replace("\n", "\r\n") + "\r\n")
            capture(screen, title, duration)
            return screen

        scene("1. Register an existing ignored .env", [["envrun", "init", "myapp"], ["envrun", "add", ".env"]])
        scene("2. Create a worktree and link its env files", [["git", "worktree", "add", "-b", "feature-x", "../myapp-feature"]], duration=1800)
        worktree = base / "myapp-feature"
        scene("3. In myapp-feature: connect and inspect", [["envrun", "link"], ["envrun", "status"], ["envrun", "run", "--explain"]], worktree, 3500)
        scene("4. Run a command with the shared environment", [["envrun", "run", "--", "sh", "-c", 'printf "APP_ENV=%s\\n" "$APP_ENV"']], worktree)

        # Capture the actual terminal UI in a pseudo-terminal. No UI is mocked.
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        tui_env = env.copy()
        tui_env.pop("NO_COLOR", None)
        process = subprocess.Popen([str(binary)], cwd=worktree, env=tui_env,
                                   stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        screen = pyte.Screen(COLS, ROWS)
        stream = pyte.ByteStream(screen)

        def drain(seconds=1):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    stream.feed(data)
                    # Crossterm may request cursor position during startup.
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[1;1R")

        try:
            drain(2)
            if process.poll() is not None:
                raise RuntimeError("TUI exited before capture")
            capture(screen, "5. Browse files and keys; values stay masked", 4500)
            assets = ROOT / "docs/assets"
            assets.mkdir(exist_ok=True)
            frames[-1].save(assets / "tui.png")
            os.write(master, b"\t")
            drain()
            capture(screen, "6. Navigate keys in the terminal UI", 3200)
            os.write(master, b"q")
            drain(0.3)
            process.wait(timeout=5)
            if process.returncode:
                raise RuntimeError(f"TUI exited with {process.returncode}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)

    output = ROOT / "docs/assets/demo.gif"
    frames[0].save(output, save_all=True, append_images=frames[1:],
                   duration=durations, loop=0, optimize=True)
    print(f"Saved {output} ({sum(durations) / 1000:.1f}s)")


if __name__ == "__main__":
    main()
