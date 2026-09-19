"""PTY smoke checks: python3 tests/test_tui.py /path/to/any-auto"""
import os
import pty
import select
import signal
import sys
import tempfile
import time
from pathlib import Path

BINARY = str(Path(sys.argv[1]).resolve())


def exercise(args, script, verify):
    with tempfile.TemporaryDirectory() as root:
        home = Path(root)
        pid, fd = pty.fork()
        if pid == 0:
            for key in list(os.environ):
                if key.startswith(("XDG_", "ANY_AUTO_", "PI_")):
                    del os.environ[key]
            os.environ.update(HOME=root, TERM="xterm", PATH="/nonexistent")
            os.execv(BINARY, [BINARY, *args])
        try:
            for prompt, reply in script:
                output = b""
                deadline = time.monotonic() + 10
                while prompt not in output and time.monotonic() < deadline:
                    if select.select([fd], [], [], 0.1)[0]:
                        output += os.read(fd, 65536)
                assert prompt in output, output
                time.sleep(0.1)  # Let the terminal widget enter raw input mode.
                os.write(fd, reply)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    pid = 0
                    assert os.waitstatus_to_exitcode(status) == 0, status
                    verify(home)
                    break
                if select.select([fd], [], [], 0.1)[0]:
                    try:
                        os.read(fd, 65536)
                    except OSError:
                        pass
            else:
                raise AssertionError("TUI did not exit")
        finally:
            if pid:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)


def unchanged(home):
    assert not (home / ".config").exists()
    assert not (home / ".gemini").exists()


def installed(home):
    assert (home / ".gemini/config/hooks.json").is_file()
    assert not (home / ".config/any-auto/config.toml").exists()


exercise([], [(b"any-auto", b"\x1b")], unchanged)
exercise(["install"], [(b"Install integrations", b"\x1b")], unchanged)
# First item (agy CLI) is undetected with the isolated PATH; select it.
selection = [(b"Install integrations", b" \r")]
# On machines with Desktop detected, deselect it before running these two cases.
# Use arrow-down then Space only when the bundle is actually present.
if Path("/Applications/Antigravity.app").is_dir():
    selection[0] = (b"Install integrations", b" \x1b[B \r")
exercise(["install"], selection + [(b"Apply installation?", b"n")], unchanged)
exercise(["install"], selection + [(b"Apply installation?", b"y")], installed)
print("TUI root/cancel/install: 4 checks passed")
