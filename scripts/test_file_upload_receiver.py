"""Behavioral tests: real receiver subprocesses and a disposable Herdr socket."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

RECEIVER = Path(__file__).resolve().parents[1] / "src/client/file_upload/receiver.py"


class ReceiverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.destination = self.root / "uploads"
        self.environment = dict(os.environ, HOME=str(self.root))

    def tearDown(self):
        self.temp.cleanup()

    def run_receiver(self, request, data=b""):
        request = {"version": 1, "destination": str(self.destination), **request}
        process = subprocess.run([sys.executable, str(RECEIVER)],
            input=json.dumps(request).encode() + b"\n" + data,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.environment, timeout=5)
        replies = [json.loads(line) for line in process.stdout.splitlines()]
        return process.returncode, replies

    def upload(self, data=b"hello", **overrides):
        request = dict(op="upload", name="report with 'quotes'.pdf", size=len(data),
                       max_file_bytes=100000, chunk_bytes=4096,
                       sha256=hashlib.sha256(data).hexdigest(), retention_seconds=3600)
        request.update(overrides)
        return self.run_receiver(request, data)

    def test_streams_acknowledged_chunks_and_verifies_file(self):
        data = bytes(range(256)) * 40
        code, replies = self.upload(data)
        self.assertEqual(code, 0, replies)
        self.assertEqual([r["received"] for r in replies if "received" in r], [4096, 8192, 10240])
        path = Path(replies[-1]["path"])
        self.assertEqual(path.read_bytes(), data)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
        self.assertTrue((path.parent / ".herdr-file-upload.json").exists())

    def test_truncated_and_corrupt_uploads_leave_no_files(self):
        for override in ({"size": 100}, {"sha256": "0" * 64}):
            code, replies = self.upload(**override)
            self.assertNotEqual(code, 0)
            self.assertIn("error", replies[-1])
            self.assertEqual(list(self.destination.iterdir()), [])

    def test_limits_and_names_rejected_before_ready(self):
        for override in ({"max_file_bytes": 2}, {"name": "../escape"},
                         {"name": "a\nb"}, {"name": ".herdr-file-upload.json"}, {"chunk_bytes": 1}):
            code, replies = self.upload(**override)
            self.assertNotEqual(code, 0)
            self.assertFalse(any("ready" in reply for reply in replies))
        self.assertFalse(self.destination.exists())

    def test_empty_files_and_duplicate_names(self):
        code, first = self.upload(b"")
        code2, second = self.upload(b"")
        self.assertEqual((code, code2), (0, 0))
        self.assertNotEqual(first[-1]["path"], second[-1]["path"])
        self.assertEqual(Path(first[-1]["path"]).read_bytes(), b"")

    def test_receiver_policy_is_independent_of_client_limit(self):
        policy = self.root / ".config/herdr/file-upload-policy.json"
        policy.parent.mkdir(parents=True)
        policy.write_text(json.dumps({"max_file_bytes": 2}))
        code, replies = self.upload(max_file_bytes=999999)
        self.assertNotEqual(code, 0)
        self.assertIn("policy", replies[-1]["error"])

    def test_cleanup_only_removes_expired_marked_directories(self):
        code, replies = self.upload(retention_seconds=-1)
        self.assertEqual(code, 0)
        expired = Path(replies[-1]["path"]).parent
        unmarked = self.destination / "upload-user-files"
        unmarked.mkdir()
        (unmarked / "keep").write_text("mine")
        code, dry = self.run_receiver({"op": "cleanup"})
        self.assertEqual(dry[-1]["directories"], [expired.name])
        self.assertTrue(expired.exists())
        self.run_receiver({"op": "cleanup", "dry_run": False})
        self.assertFalse(expired.exists())
        self.assertTrue((unmarked / "keep").exists())

    def test_symlink_destination_is_rejected(self):
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        self.destination.symlink_to(elsewhere, target_is_directory=True)
        code, replies = self.upload()
        self.assertNotEqual(code, 0)
        self.assertIn("symlink", replies[-1]["error"])
        self.assertEqual(list(elsewhere.iterdir()), [])

    def test_paste_pins_terminal_and_sends_no_enter(self):
        for actual_terminal, status, expected_calls in [("same", "idle", 2), ("replaced", "idle", 1), ("same", "blocked", 1)]:
            sock_path = self.root / "herdr.sock"
            calls = []
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(str(sock_path))
                server.listen()
                server.settimeout(3)
                def serve():
                    for _ in range(expected_calls):
                        connection, _ = server.accept()
                        with connection, connection.makefile("rb") as reader:
                            request = json.loads(reader.readline())
                            calls.append(request)
                            result = {"pane": {"terminal_id": actual_terminal, "agent_status": status}}
                            connection.sendall(json.dumps({"id": request["id"], "result": result}).encode()+b"\n")
                thread = threading.Thread(target=serve)
                thread.start()
                code, _ = self.run_receiver({"op": "paste", "socket": str(sock_path), "pane": "w2:p7", "terminal_id": "same", "text": "/tmp/report.pdf "})
                thread.join(timeout=4)
                self.assertFalse(thread.is_alive())
            sock_path.unlink()
            self.assertEqual(len(calls), expected_calls)
            if expected_calls == 2:
                self.assertEqual(code, 0)
                self.assertEqual(calls[1]["method"], "pane.send_input")
                self.assertEqual(calls[1]["params"], {"pane_id": "w2:p7", "text": "/tmp/report.pdf ", "keys": []})
            else:
                self.assertNotEqual(code, 0)

    def test_unknown_protocol_rejected(self):
        code, replies = self.run_receiver({"version": 9, "op": "doctor"})
        self.assertNotEqual(code, 0)
        self.assertIn("protocol", replies[-1]["error"])


if __name__ == "__main__":
    unittest.main()
