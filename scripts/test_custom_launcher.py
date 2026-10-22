import os
import subprocess
import tempfile
import unittest
from pathlib import Path

LAUNCHER = Path(__file__).with_name("herdr-custom-launcher")


class CustomLauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.store = Path(self.temp.name)
        self.active = self.store / "herdr"
        self.active.write_text('#!/bin/sh\nprintf "arg:%s\\n" "$@"\n', encoding="utf-8")
        self.active.chmod(0o755)
        self.env = {**os.environ, "HERDR_CUSTOM_STORE": str(self.store)}

    def run_launcher(self, *args):
        return subprocess.run(
            ["bash", str(LAUNCHER), *args],
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_normal_launch_passes_arguments_unchanged(self):
        result = self.run_launcher("client", "two words")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "arg:client\narg:two words\n")

    def test_update_options_cannot_fall_through_to_official_updater(self):
        result = self.run_launcher("update", "--handoff")
        self.assertEqual(result.returncode, 2)
        self.assertIn("does not support", result.stderr)
        self.assertEqual(self.active.read_text(encoding="utf-8"), '#!/bin/sh\nprintf "arg:%s\\n" "$@"\n')

    def test_missing_binary_fails_instead_of_falling_back(self):
        self.active.unlink()
        result = self.run_launcher("client")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("official binary is unchanged", result.stderr)

    def test_failed_build_preserves_active_binary_and_releases_lock(self):
        checkout = self.store / "checkout"
        (checkout / ".git").mkdir(parents=True)
        mockbin = self.store / "mockbin"
        mockbin.mkdir()
        commands = {
            "git": '''#!/bin/sh
shift; shift
case "$1 $2" in
    'remote get-url') echo https://github.com/jekozyra/herdr.git ;;
    'branch --show-current') echo master ;;
    'rev-parse HEAD') echo new-revision ;;
esac
exit 0
''',
            "zig": "#!/bin/sh\necho 0.16.0\n",
            "python3": "#!/bin/sh\nexit 0\n",
            "cargo": "#!/bin/sh\nexit 17\n",
        }
        for name, contents in commands.items():
            script = mockbin / name
            script.write_text(contents, encoding="utf-8")
            script.chmod(0o755)
        self.env.update(
            {
                "HERDR_CUSTOM_CHECKOUT": str(checkout),
                "PATH": f"{mockbin}:{os.environ['PATH']}",
                "ZIG": str(mockbin / "zig"),
            }
        )
        previous = self.active.read_bytes()
        result = self.run_launcher("update")
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(self.active.read_bytes(), previous)
        self.assertFalse((self.store / ".update-lock").exists())


if __name__ == "__main__":
    unittest.main()
