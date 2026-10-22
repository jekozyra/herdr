#!/usr/bin/env python3
"""Select a newer stable Herdr release for the fork's upgrade workflow."""

import argparse
import json
import re
from pathlib import Path

TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")


def version(tag: str) -> tuple[int, int, int]:
    match = TAG.fullmatch(tag)
    if not match:
        raise ValueError(f"Expected a stable vMAJOR.MINOR.PATCH tag, got {tag!r}")
    return tuple(map(int, match.groups()))


def select_release(current: str, release: dict) -> str | None:
    tag = release["tag_name"]
    if release.get("draft") or release.get("prerelease"):
        raise ValueError(f"{tag} is not a published stable release")
    return tag if version(tag) > version(current) else None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current", type=Path, required=True)
    parser.add_argument("--release-json", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    current = args.current.read_text(encoding="utf-8").strip()
    release = json.loads(args.release_json.read_text(encoding="utf-8"))
    tag = select_release(current, release)
    with args.output.open("a", encoding="utf-8") as output:
        output.write(f"tag={tag or ''}\n")
    print(f"Upstream release: {release['tag_name']}; fork base: {current}; upgrade: {tag or 'none'}")


if __name__ == "__main__":
    main()
