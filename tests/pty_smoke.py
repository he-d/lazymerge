#!/usr/bin/env python3
"""Exercise the built TUI through a real Unix PTY; uses only Python's standard library.

Run after `cargo build`: python3 tests/pty_smoke.py [path/to/lazymerge]
"""

import fcntl
import codecs
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata


BINARY = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/lazymerge").resolve()


class Tui:
    def __init__(self, arguments, environment=None):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        self.process = subprocess.Popen(
            [str(BINARY), *map(str, arguments)],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env={**os.environ, "TERM": "xterm-256color", **(environment or {})},
        )
        os.close(slave)
        self.output = b""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.screen = [[" "] * 120 for _ in range(36)]
        self.row = self.col = 0

    def feed(self, output):
        self.pending += self.decoder.decode(output)
        while self.pending:
            if self.pending.startswith("\x1b"):
                match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                if not match:
                    if len(self.pending) < 3 or self.pending[1] == "[":
                        return
                    self.pending = self.pending[2:]
                    continue
                self.pending = self.pending[match.end():]
                params, _, code = match.groups()
                if params.startswith("?"):
                    continue
                numbers = [int(n or 0) for n in params.split(";")] if params else [0]
                first = numbers[0] or 1
                if code in "Hf":
                    self.row = first - 1
                    self.col = (numbers[1] or 1) - 1 if len(numbers) > 1 else 0
                elif code == "A": self.row -= first
                elif code == "B": self.row += first
                elif code == "C": self.col += first
                elif code == "D": self.col -= first
                elif code == "G": self.col = first - 1
                elif code == "d": self.row = first - 1
                elif code == "J" and numbers[0] in (2, 3): self.screen = [[" "] * 120 for _ in range(36)]
                elif code == "K":
                    if numbers[0] == 0: self.screen[self.row][self.col:] = [" "] * (120 - self.col)
                    elif numbers[0] == 2: self.screen[self.row] = [" "] * 120
                self.row = max(0, min(35, self.row))
                self.col = max(0, min(119, self.col))
                continue
            character, self.pending = self.pending[0], self.pending[1:]
            if character == "\r": self.col = 0
            elif character == "\n": self.row = min(35, self.row + 1)
            elif character == "\b": self.col = max(0, self.col - 1)
            elif not unicodedata.category(character).startswith("C"):
                self.screen[self.row][self.col] = character
                width = 2 if unicodedata.east_asian_width(character) in "WF" else 1
                self.col = min(119, self.col + width)

    def send(self, text):
        os.write(self.master, text.encode() if isinstance(text, str) else text)

    def wait_for(self, text, timeout=10, present=True):
        start = time.monotonic()
        while time.monotonic() - start < timeout:
            if (text in "\n".join("".join(row) for row in self.screen)) == present:
                return
            if self.process.poll() is not None:
                raise AssertionError(f"TUI exited while waiting for {text!r}: {self.output!r}")
            if select.select([self.master], [], [], 0.1)[0]:
                output = os.read(self.master, 65536)
                self.output += output
                self.feed(output)
        raise AssertionError(f"Timed out waiting for {text!r}:\n" + "\n".join("".join(row) for row in self.screen))

    def quit(self):
        self.send("q")
        self.wait_for("Quit LazyMerge?")
        self.send("\r")  # No is selected by default.
        self.wait_for("Quit LazyMerge?", present=False)
        assert self.process.poll() is None, "Enter must not silently accept quit"
        self.send("q")
        self.wait_for("Quit LazyMerge?")
        self.send("y")
        assert self.wait_for_exit() == 0

    def wait_for_exit(self, timeout=5):
        # Keep reading: macOS PTYs buffer less than Linux, so unread output would block the exit.
        start = time.monotonic()
        while self.process.poll() is None:
            if time.monotonic() - start >= timeout:
                raise AssertionError("TUI did not exit:\n" + "\n".join("".join(row) for row in self.screen))
            if select.select([self.master], [], [], 0.1)[0]:
                try:
                    output = os.read(self.master, 65536)
                except OSError:
                    continue
                self.output += output
                self.feed(output)
        return self.process.returncode

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)


def main():
    assert subprocess.run([str(BINARY), "--help"], capture_output=True, check=True).stdout
    with tempfile.TemporaryDirectory(prefix="lazymerge-pty-") as temporary:
        root = Path(temporary)
        left, right = root / "left q folder", root / "right folder"
        left.mkdir()
        right.mkdir()
        (left / "demo.txt").write_bytes(b"same\nleft\nend\n")
        (right / "demo.txt").write_bytes(b"same\r\nright\r\nend\r\n")
        config_home = root / "config home"
        environment = {"XDG_CONFIG_HOME": str(config_home)}
        process_environment = {**os.environ, **environment}
        default_config = config_home / "lazymerge" / "config.lua"
        defaults = subprocess.run([str(BINARY), "--print-config"], env=process_environment,
                                  capture_output=True, check=True).stdout
        assert b"scroll_left" in defaults and b"jump_after_merge" in defaults
        assert not config_home.exists(), "Printing config must not create a file"
        tui = Tui([left, right], environment)
        try:
            tui.wait_for("Compared 1 files")
            assert default_config.read_bytes() == defaults
            tui.quit()
        finally:
            tui.close()
        customized = b"return { show_logo = false, jump_after_merge = false }\n"
        default_config.write_bytes(customized)
        tui = Tui([left, right], environment)
        try:
            tui.wait_for("Compared 1 files")
            assert default_config.read_bytes() == customized
            tui.quit()
        finally:
            tui.close()
        default_config.write_bytes(b"not valid Lua!!!")
        reset = subprocess.run([str(BINARY), "--reset-config"], env=process_environment,
                               text=True, capture_output=True, check=True)
        assert str(default_config) in reset.stdout
        assert default_config.read_bytes() == defaults
        custom_config = root / "custom settings" / "config.lua"
        subprocess.run([str(BINARY), "--reset-config", "--config", str(custom_config)],
                       env=process_environment, capture_output=True, check=True)
        assert custom_config.read_bytes() == defaults

        config = root / "config.lua"
        config.write_text("return { show_logo = true, poll_seconds = 1 }")

        tui = Tui(["--config", config])
        try:
            tui.wait_for("Left folder or file")
            tui.send(str(root / "left") + "\t\r")
            tui.wait_for("Right")
            tui.send(str(root / "right") + "\t\r")
            tui.wait_for("Compared 1 files")
            tui.send("F")
            tui.wait_for("Exclude patterns")
            tui.send("\x1b")
            tui.wait_for("Exclude patterns", present=False)
            (left / "fresh.py").write_text("print('fresh')\n")
            tui.wait_for("fresh.py")
            (left / "fresh.py").unlink()
            tui.wait_for("fresh.py", present=False)
            tui.send("\r")
            tui.wait_for("sections [active]")
            tui.send("L")
            tui.wait_for("Merge staged")
            assert (right / "demo.txt").read_bytes() == b"same\r\nright\r\nend\r\n"
            tui.send("i")
            tui.wait_for("alt+enter apply")
            tui.send("\x15left edited\x1b\r")  # Ctrl+u, text, Alt+Enter.
            tui.wait_for("Edit staged")
            tui.send("\x1bs")  # Alt+s saves both staged sides.
            tui.wait_for("Saved Left, Right")
            assert (left / "demo.txt").read_bytes() == b"same\nleft edited\nend\n"
            assert (right / "demo.txt").read_bytes() == b"same\r\nleft\r\nend\r\n"
            (right / "demo.txt").write_bytes(b"same\r\nexternal\r\nend\r\n")
            tui.wait_for("reloaded both sides")
            tui.wait_for("external")
            tui.send("i")
            tui.wait_for("alt+enter apply")
            tui.send("\x15mine\x1b\r")
            tui.wait_for("Edit staged")
            (left / "demo.txt").write_bytes(b"same\ndisk edit\nend\n")
            tui.wait_for("staged edits kept")
            tui.send("\x1bs")
            tui.wait_for("changed on disk. Overwrite")
            tui.send("n")
            tui.wait_for("changed on disk. Overwrite", present=False)
            assert (left / "demo.txt").read_bytes() == b"same\ndisk edit\nend\n"
            tui.quit()
        finally:
            tui.close()

        # A long polling interval proves focus events trigger an immediate check.
        config.write_text("return { show_logo = true, poll_seconds = 3600 }")
        for index, arguments in enumerate([[left, right], ["-l", left, "-r", right]]):
            tui = Tui(["--config", config, *arguments])
            try:
                tui.wait_for("Compared 1 files")
                row = next(i for i, line in enumerate(tui.screen) if "demo.txt" in "".join(line))
                tui.send(f"\x1b[<0;6;{row + 1}M\x1b[<0;6;{row + 1}m")
                tui.wait_for("sections [active]")
                focus_text = f"focus gained change {index}"
                (right / "demo.txt").write_text(focus_text + "\n")
                tui.send("\x1b[I")  # Terminal focus gained.
                tui.wait_for(focus_text)
                tui.quit()
            finally:
                tui.close()

        # Two files open directly in the file view, without the folder tree.
        tui = Tui(["--config", config, "-l", left / "demo.txt", "-r", right / "demo.txt"])
        try:
            tui.wait_for("Compared 2 files")
            tui.wait_for("demo.txt · 1 sections [active]")
            assert "Folders ·" not in "\n".join("".join(row) for row in tui.screen)
            tui.send("dd")  # Delete the first line on both sides.
            tui.wait_for("Delete staged")
            tui.send("\x1bs")
            tui.wait_for("Saved Left, Right")
            assert (left / "demo.txt").read_bytes() == b"disk edit\nend\n"
            assert (right / "demo.txt").read_bytes() == b""
            tui.quit()
        finally:
            tui.close()
    print("PTY smoke tests passed: config creation/reset, startup/completion, Shift+f, Alt+Enter, save both, auto-refresh/conflicts, mouse, focus, two files with dd, confirmed quit")


if __name__ == "__main__":
    main()
