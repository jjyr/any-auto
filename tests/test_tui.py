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


def exercise(args, script, verify, setup=None):
    with tempfile.TemporaryDirectory() as root:
        home = Path(root)
        if setup:
            setup(home)
        transcript = bytearray()
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
                        chunk = os.read(fd, 65536)
                        output += chunk
                        transcript.extend(chunk)
                assert prompt in output, output
                time.sleep(0.1)  # Let the terminal widget enter raw input mode.
                os.write(fd, reply)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    pid = 0
                    assert os.waitstatus_to_exitcode(status) == 0, status
                    assert b"tui-fixture-secret" not in transcript, transcript
                    verify(home)
                    break
                if select.select([fd], [], [], 0.1)[0]:
                    try:
                        transcript.extend(os.read(fd, 65536))
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
# On machines with Desktop detected, deselect it before confirmation.
# Use arrow-down then Space only when the bundle is actually present.
if Path("/Applications/Antigravity.app").is_dir():
    selection[0] = (b"Install integrations", b" \x1b[B \r")
exercise(["install"], selection + [(b"Apply installation?", b"n")], unchanged)
print("TUI root/cancel/decline: 3 checks passed")


def jev_configured(home):
    text = (home / ".config/any-auto/config.toml").read_text()
    assert '[agents.agy-cli.approver]' in text, text
    assert 'provider = "jev"' in text, text
    assert 'base_url = "https://api.typesafe.ai/v1"' in text, text
    assert 'api_key = "tui-fixture-secret"' in text, text
    assert 'probability_threshold = 0.95' in text, text
    assert 'effort = "low"' not in text, text


exercise([], [
    (b"any-auto", b"\x1b[B\x1b[B\r"),
    (b"Customize approver settings?", b"y"),
    (b"Approver backend", b"\x1b[B" * 5 + b"\r"),
    (b"Model (blank", b"\r"),
    (b"API base URL", b"\r"),
    (b"API key", b"tui-fixture-secret\r"),
    (b"Probability threshold", b"0.95\r"),
    (b"Approver backend", b"\r"),
    (b"Approver backend", b"\r"),
    (b"Save configuration?", b"y"),
    (b"any-auto", b"\x1b"),
], jev_configured)
print("TUI Jev configuration: 1 check passed")


def cli_and_pi_installed(home):
    installed(home)
    assert (home / ".pi/agent/extensions/any-auto.ts").is_file()
    assert not (home / ".gemini/config/config.json").exists()


desktop_detected = Path("/Applications/Antigravity.app").is_dir()
# Select CLI and Pi, leaving Desktop unselected even if it was detected.
multi_selection = b" \x1b[B" + (b" " if desktop_detected else b"") + b"\x1b[B \r"
exercise(["install"], [
    (b"Install integrations", multi_selection),
    (b"Apply installation?", b"y"),
], cli_and_pi_installed)
# Empty selection is a no-op, including when Desktop was selected by default.
empty_selection = b"\x1b[B \r" if desktop_detected else b"\r"
exercise(["install"], [(b"Install integrations", empty_selection)], unchanged)


def existing_jev(home):
    extension = home / ".pi/agent/extensions/any-auto.ts"
    extension.parent.mkdir(parents=True)
    extension.write_text("old extension")
    config = home / ".config/any-auto/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text('[agents.pi.approver]\nprovider="jev"\n')


print("TUI install multi-select/empty: 2 checks passed")


def uninstall_preserved(home):
    assert (home / ".pi/agent/extensions/any-auto.ts").read_text() == "old extension"
    assert (home / ".config/any-auto/config.toml").read_text() == '[agents.pi.approver]\nprovider="jev"\n'


def uninstalled(home):
    assert not (home / ".pi/agent/extensions/any-auto.ts").exists()
    assert (home / ".config/any-auto/config.toml").read_text() == '[agents.pi.approver]\nprovider="jev"\n'


for reply in [b"\x1b", b"\r"]:
    exercise(["uninstall"], [(b"Space toggles", reply)], uninstall_preserved, setup=existing_jev)
exercise(["uninstall"], [
    (b"Space toggles", b" \r"), (b"Continue?", b"\r"),
], uninstall_preserved, setup=existing_jev)
exercise([], [
    (b"any-auto", b"\x1b[B" * 5 + b"\r"),
    (b"Space toggles", b" \r"), (b"Continue?", b"y"),
    (b"any-auto", b"\x1b"),
], uninstalled, setup=existing_jev)
print("TUI uninstall cancel/empty/decline/menu: 4 checks passed")
