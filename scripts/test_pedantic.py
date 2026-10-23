"""Behavioral checks for the legacy Clippy diagnostic baseline."""

from collections import Counter
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts import pedantic


def diagnostic(source: str, line: int = 1, *, code: str = "clippy::cast_possible_truncation") -> str:
    return json.dumps({
        "reason": "compiler-message",
        "message": {
            "code": {"code": code},
            "level": "warning",
            "message": "cast may truncate",
            "spans": [{
                "file_name": "src/../src/example.rs",
                "is_primary": True,
                "line_start": line,
                "text": [{"text": source, "highlight_start": 1, "highlight_end": len(source) + 1}],
            }],
        },
    })


class PedanticTests(unittest.TestCase):
    def test_moved_old_finding_does_not_count_as_new(self):
        old = pedantic.diagnostics(diagnostic("value as u8", 10))
        moved = pedantic.diagnostics(diagnostic("value as u8", 100))
        self.assertEqual(moved - old, Counter())

    def test_modified_or_duplicate_finding_counts_as_new(self):
        old_line = diagnostic("value as u8")
        old = pedantic.diagnostics(old_line)
        changed = pedantic.diagnostics(diagnostic("other as u8"))
        duplicate = pedantic.diagnostics(old_line + "\n" + old_line)
        self.assertEqual(sum((changed - old).values()), 1)
        self.assertEqual(sum((duplicate - old).values()), 1)

    def test_lint_and_non_clippy_records_are_distinguished(self):
        original = diagnostic("value as u8")
        different_lint = diagnostic("value as u8", code="clippy::cast_sign_loss")
        non_message = json.dumps({"reason": "build-finished", "success": True})
        self.assertEqual(sum((pedantic.diagnostics(different_lint) -
                              pedantic.diagnostics(original)).values()), 1)
        self.assertFalse(pedantic.diagnostics(non_message))

    def test_generated_bindings_are_not_gated(self):
        record = json.loads(diagnostic("pub const GENERATED: u32 = 42;"))
        record["message"]["spans"][0]["file_name"] = "crates/ghostty-vt/src/bindings.rs"
        self.assertFalse(pedantic.diagnostics(json.dumps(record)))

    def test_release_and_reviewed_counts_preserve_fork_audit(self):
        old_line = diagnostic("value as u8")
        new_line = diagnostic("other as u8")
        release = pedantic.diagnostics(old_line)
        current = pedantic.diagnostics(old_line + "\n" + new_line)
        reviewed = current - release
        self.assertEqual(sum((current - release).values()), 1)
        self.assertFalse(current - (release + reviewed))
        self.assertEqual(sum((current + pedantic.diagnostics(new_line) -
                              (release + reviewed)).values()), 1)

    def test_baseline_round_trip_rejects_duplicate_rows(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "baseline.json"
            findings = pedantic.diagnostics(diagnostic("value as u8"))
            pedantic.save_baseline(findings, path)
            self.assertEqual(pedantic.load_baseline(path), findings)
            rows = json.loads(path.read_text())
            path.write_text(json.dumps(rows + rows))
            with self.assertRaises(ValueError):
                pedantic.load_baseline(path)

    def test_missing_source_fails_closed(self):
        record = json.loads(diagnostic("value as u8"))
        record["message"]["spans"][0]["text"] = []
        with self.assertRaises(ValueError):
            pedantic.diagnostics(json.dumps(record))


if __name__ == "__main__":
    unittest.main()
