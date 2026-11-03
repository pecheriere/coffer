"""Tests for scripts/staging.sh's refusals, with a stand-in for docker: none needs Docker."""

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "staging.sh"

# Records its calls; answers `docker ps` with the checkout a running stack came from.
FAKE_DOCKER = """#!/bin/sh
echo "$*" >> "$FAKE_CALLS"
case "$1" in
  ps) [ -n "$FAKE_RUNNING" ] && echo "$FAKE_RUNNING" ;;
esac
exit 0
"""


class Refusals(unittest.TestCase):
    """What the script refuses, before it changes anything."""

    def setUp(self) -> None:
        """Put a recording docker first on the PATH."""
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.calls = Path(work.name) / "calls"
        self.calls.touch()
        fake = Path(work.name) / "docker"
        fake.write_text(FAKE_DOCKER)
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        self.path = f"{work.name}{os.pathsep}{os.environ['PATH']}"

    def run_script(self, *args: str, running: str = "") -> subprocess.CompletedProcess[str]:
        """Run the script with the recording docker."""
        return subprocess.run(
            [str(SCRIPT), *args],
            capture_output=True,
            check=False,
            text=True,
            env={
                **os.environ,
                "PATH": self.path,
                "FAKE_CALLS": str(self.calls),
                "FAKE_RUNNING": running,
            },
        )

    def changes(self) -> list[str]:
        """Whichever docker calls went beyond asking what runs."""
        return [c for c in self.calls.read_text().splitlines() if not c.startswith("ps")]

    def test_a_day_that_is_not_a_date_never_reaches_a_path(self) -> None:
        """A day becomes a directory that is removed, so `..` must not get through."""
        for day in ("../../../crates", "..", "2026-11-02/..", "2026-11-02\n", "", "x"):
            for args in (("template", day), ("start", "18081", day), ("elsewhere", day)):
                result = self.run_script(*args)
                self.assertNotEqual(result.returncode, 0, args)
                self.assertEqual(self.changes(), [], args)

    def test_a_port_outside_the_stack_is_refused(self) -> None:
        """A port names a container, and only these are published."""
        for port in ("80", "18100", "18079", "x; true", "../x"):
            for command in ("start", "stop"):
                result = self.run_script(command, port)
                self.assertNotEqual(result.returncode, 0, (command, port))
                self.assertEqual(self.changes(), [], (command, port))

    def test_a_second_test_run_in_a_checkout_is_refused(self) -> None:
        """Two runs would remove each other's servers."""
        state = SCRIPT.parent.parent / "staging" / ".state"
        if not state.exists():
            state.mkdir()
            self.addCleanup(state.rmdir)
        lock = state / "test.lock"
        lock.mkdir()
        self.addCleanup(lock.rmdir)
        pid = lock / "pid"
        pid.write_text(str(os.getpid()))
        self.addCleanup(pid.unlink)

        result = self.run_script("test")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already running", result.stderr)
        self.assertEqual(self.changes(), [])

    def test_another_checkouts_stack_is_left_alone(self) -> None:
        """Each command that touches the shared stack refuses while another's is running."""
        for args in (
            ("up",),
            ("reset",),
            ("down",),
            ("template",),
            ("start", "18081"),
            ("stop", "18081"),
            ("sweep",),
            ("elsewhere",),
            ("test",),
        ):
            result = self.run_script(*args, running="/somewhere/else/staging")
            self.assertNotEqual(result.returncode, 0, args)
            self.assertIn("/somewhere/else/staging", result.stderr, args)
            self.assertEqual(self.changes(), [], args)


if __name__ == "__main__":
    unittest.main()
