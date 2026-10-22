"""Versioned, one-shot SSH receiver. Uses only the Python standard library.

The local uploader sends this source to python3 -c; it installs no daemon.
All variable input arrives as bounded JSON and binary data on stdin.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import tempfile
import time

VERSION = 1
MAX_JSON = 1024 * 1024
MAX_CHUNK = 8 * 1024 * 1024
POLICY = Path.home() / ".config/herdr/file-upload-policy.json"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_json(stream):
    line = stream.readline(MAX_JSON + 1)
    require(line.endswith(b"\n") and len(line) <= MAX_JSON, "invalid or oversized request")
    return json.loads(line)


def emit(value):
    sys.stdout.write(json.dumps(value) + "\n")
    sys.stdout.flush()


def api(path, method, params):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(15)
        client.connect(os.path.expanduser(path))
        request = {"id": "herdr-file-upload", "method": method, "params": params}
        client.sendall(json.dumps(request).encode() + b"\n")
        with client.makefile("rb") as reader:
            response = read_json(reader)
    require("error" not in response, "Herdr rejected request: " + str(response.get("error", {}).get("code", "unknown")))
    return response["result"]


def pane_info(req):
    return api(req["socket"], "pane.get", {"pane_id": req["pane"]})["pane"]


def root_path(req, create=True):
    raw = req["destination"]
    require(raw.startswith(("/", "~/")), "destination must be absolute or ~/ relative")
    root = Path(raw).expanduser()
    require(not any(ord(ch) < 32 or ord(ch) == 127 for ch in str(root)), "invalid destination")
    require(root != Path(root.anchor) and root != Path.home(), "use a dedicated upload directory")
    # Reject symlink components, including an existing destination itself.
    require(not any(p.is_symlink() for p in [root, *root.parents]), "destination contains a symlink")
    if POLICY.exists():
        policy = json.loads(POLICY.read_text())
        allowed = policy.get("allowed_directories", [])
        require(not allowed or any(root.resolve().is_relative_to(Path(p).expanduser().resolve()) for p in allowed), "destination blocked by receiver policy")
    if not create and not root.exists():
        return root
    if create:
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
    require(root.stat().st_uid == os.getuid(), "destination must belong to the SSH user")
    require(root.stat().st_mode & 0o077 == 0, "destination must have private permissions (0700)")
    return root


def upload(req, stream):
    name = req["name"]
    require(isinstance(name, str) and name not in ("", ".", "..") and Path(name).name == name and "\\" not in name, "invalid filename")
    require(not any(ord(ch) < 32 or ord(ch) == 127 for ch in name), "control character in filename")
    require(name != ".herdr-file-upload.json", "reserved filename")
    size, limit, chunk = req["size"], req["max_file_bytes"], req["chunk_bytes"]
    require(isinstance(size, int) and 0 <= size <= limit, "file exceeds upload limit")
    require(4096 <= chunk <= MAX_CHUNK, "invalid chunk size")
    if POLICY.exists():
        policy = json.loads(POLICY.read_text())
        require(size <= policy.get("max_file_bytes", limit), "file exceeds receiver policy limit")
        ext = Path(name).suffix.lstrip(".").lower()
        require(ext not in [v.lower() for v in policy.get("deny_extensions", [])], "extension denied by receiver policy")
        allow = [v.lower() for v in policy.get("allow_extensions", [])]
        require(not allow or ext in allow, "extension not allowed by receiver policy")
    root = root_path(req)
    folder = Path(tempfile.mkdtemp(prefix="upload-", dir=root))
    path = folder / name
    committed = False
    try:
        emit({"ready": True})
        digest = hashlib.sha256()
        received = 0
        with path.open("xb") as output:
            os.chmod(path, 0o600)
            while received < size:
                wanted = min(chunk, size - received)
                data = stream.read(wanted)
                require(len(data) == wanted, "upload interrupted")
                output.write(data)
                digest.update(data)
                received += len(data)
                emit({"received": received})
            output.flush()
            os.fsync(output.fileno())
        require(digest.hexdigest() == req["sha256"], "checksum mismatch")
        marker = {"version": VERSION, "created": time.time(), "expires": time.time() + req["retention_seconds"], "name": name}
        (folder / ".herdr-file-upload.json").write_text(json.dumps(marker))
        committed = True
        emit({"path": str(path), "size": received, "sha256": digest.hexdigest()})
    finally:
        if not committed:
            shutil.rmtree(folder)


def paste(req):
    before = pane_info(req)
    require(before["terminal_id"] == req["terminal_id"], "target terminal changed; file was not inserted")
    expected = req.get("agent_session")
    require(not expected or before.get("agent_session") == expected, "target agent session changed; file was not inserted")
    require(before.get("agent_status") != "blocked", "agent is waiting for approval; file was not inserted")
    # Public API applies bracketed paste when supported and sends no Enter key.
    return api(req["socket"], "pane.send_input", {"pane_id": req["pane"], "text": req["text"], "keys": []})


def cleanup(req):
    root = root_path(req, create=False)
    removed = []
    if not root.exists():
        return {"directories": [], "dry_run": req.get("dry_run", True)}
    for folder in root.iterdir():
        if not folder.name.startswith("upload-") or folder.is_symlink() or not folder.is_dir():
            continue
        marker = folder / ".herdr-file-upload.json"
        if marker.is_symlink() or not marker.is_file():
            continue
        try:
            data = json.loads(marker.read_text())
            expired = data.get("version") == VERSION and data["expires"] < time.time()
        except (ValueError, KeyError, TypeError):
            continue
        if expired:
            removed.append(folder.name)
            if not req.get("dry_run", True):
                shutil.rmtree(folder)
    return {"directories": removed, "dry_run": req.get("dry_run", True)}


def main():
    os.umask(0o077)
    stream = sys.stdin.buffer
    req = read_json(stream)
    require(req.get("version") == VERSION, "unsupported receiver protocol")
    op = req["op"]
    if op == "upload":
        upload(req, stream)
    elif op == "panes":
        emit(api(req["socket"], "pane.list", {}))
    elif op == "pane":
        emit(pane_info(req))
    elif op == "paste":
        emit(paste(req))
    elif op == "cleanup":
        emit(cleanup(req))
    elif op == "doctor":
        emit({"python": list(sys.version_info[:3]), "protocol": VERSION, "policy": POLICY.exists()})
    else:
        raise ValueError("unknown operation")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit({"error": str(error)})
        sys.exit(1)
