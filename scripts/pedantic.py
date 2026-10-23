"""Gate new Rust quality findings against the pre-fork Herdr v0.9.5 baseline.

Clippy runs on entire crates; diagnostics from the release and reviewed fork changes
are occurrence-counted by lint, path, message, and source (not line number).
Use --audit-fork to list warnings introduced since the release, including reviewed
exceptions. Never regenerate the release baseline from the current checkout.
"""

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

# Last upstream release before this fork's Rust changes: 2ecf19a9 (v0.9.5).
BASELINE = Path(__file__).with_name("pedantic-baseline.json")
REVIEWED = Path(__file__).with_name("pedantic-fork-reviewed.json")
# Generated FFI declarations are not hand-written Rust policy.
GENERATED = {"crates/ghostty-vt/src/bindings.rs"}
LINTS = ["clippy::pedantic", "clippy::todo", "clippy::unimplemented"]


def diagnostics(
    lines: str, locations: dict[tuple[str, str, str, str], int] | None = None
) -> Counter[tuple[str, str, str, str]]:
    findings: Counter[tuple[str, str, str, str]] = Counter()
    for line in lines.splitlines():
        record = json.loads(line)
        if record.get("reason") != "compiler-message":
            continue
        message = record["message"]
        code = (message.get("code") or {}).get("code", "")
        if not code.startswith("clippy::") or message["level"] not in ("warning", "error"):
            continue
        spans = [span for span in message["spans"] if span["is_primary"]]
        if not spans:
            raise ValueError(f"Clippy diagnostic has no primary span: {code}: {message['message']}")
        span = spans[0]
        # Macro diagnostics can point into the Rust standard library with no
        # source text; identify them by the outermost local invocation instead.
        while not span["text"] and span.get("expansion"):
            span = span["expansion"]["span"]
        path = os.path.normpath(span["file_name"]).replace("\\", "/")
        if os.path.isabs(path):
            path = os.path.relpath(path).replace("\\", "/")
        if path in GENERATED:
            continue
        # Full source lines and highlight columns keep the identity stable when
        # unrelated lines move, but distinguish two diagnostics on one line.
        source = [(part["text"], part["highlight_start"], part["highlight_end"])
                  for part in span["text"]]
        if not source:
            raise ValueError(f"Clippy diagnostic has no source text: {code}: {path}")
        digest = hashlib.sha256(json.dumps(source, ensure_ascii=False).encode()).hexdigest()
        key = (path, code, message["message"], digest)
        findings[key] += 1
        if locations is not None:
            locations[key] = span["line_start"]
    return findings


def save_baseline(findings: Counter[tuple[str, str, str, str]], path: Path) -> None:
    rows = [list(key) + [count] for key, count in sorted(findings.items())]
    path.write_text(json.dumps(rows, ensure_ascii=False, indent=1) + "\n")


def load_baseline(path: Path) -> Counter[tuple[str, str, str, str]]:
    rows = json.loads(path.read_text())
    if any(len(row) != 5 or not isinstance(row[4], int) or row[4] < 1 for row in rows):
        raise ValueError(f"invalid diagnostic baseline: {path}")
    if len({tuple(row[:4]) for row in rows}) != len(rows):
        raise ValueError(f"duplicate diagnostic identity: {path}")
    return Counter({tuple(row[:4]): row[4] for row in rows})


def report(findings: Counter[tuple[str, str, str, str]],
           locations: dict[tuple[str, str, str, str], int]) -> None:
    for key, count in sorted(findings.items()):
        path, code, message, _ = key
        print(f"{path}:{locations[key]}: {code}: {message} ({count})", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audit-fork", action="store_true",
                        help="list every remaining finding introduced since v0.9.5")
    args = parser.parse_args()
    command = ["cargo", "clippy", "--all-targets", "--locked", "--message-format=json",
               "--", *(flag for lint in LINTS for flag in ("-W", lint))]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.stderr:
        print(result.stderr, file=sys.stderr, end="")
    if result.returncode:
        print("Clippy failed; quality gate cannot pass", file=sys.stderr)
        return result.returncode
    try:
        locations: dict[tuple[str, str, str, str], int] = {}
        current = diagnostics(result.stdout, locations)
        legacy = load_baseline(BASELINE)
        reviewed = load_baseline(REVIEWED)
    except (OSError, ValueError, KeyError) as error:
        print(f"Pedantic baseline error: {error}", file=sys.stderr)
        return 1
    fork = current - legacy
    if args.audit_fork:
        report(fork, locations)
        print(f"Fork audit: {sum(fork.values())} findings since v0.9.5")
        return 0
    new = current - (legacy + reviewed)
    report(new, locations)
    print(f"Clippy pedantic/restriction: {sum(current.values())} findings, "
          f"{sum(fork.values())} from fork (reviewed: {sum(reviewed.values())}), "
          f"{sum(new.values())} unreviewed")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main())
