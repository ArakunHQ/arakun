#!/usr/bin/env python3
"""The changelog entry of a release: release-notes/v<version>.md.

The version bump pull request adds it. release.yml puts it at the top of the
release's notes on GitHub, and arakun.dev/changelog shows it from there. See
release-notes/README.md for how to write one.

  tools/release-notes.py new           # draft the file for the version in Cargo.toml
  tools/release-notes.py check         # check it, as CI and release.yml do
  tools/release-notes.py check 1.2.3   # check the file of another version

Runs on the Python 3.9 that macOS ships, so no tomllib.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NOTES = ROOT / "release-notes"

# Conventional Commit types that change nothing a user of the app sees.
UNSEEN = re.compile(r"^(?:build|ci|test|tests|chore|docs|style|refactor)(?:\(.*?\))?!?:", re.I)
HEADING = re.compile(r"^##\s+(.+?)\s*$")
# What arakun.dev reads: `- **Area:** change`.
AREA = re.compile(r"^-\s+\*\*([^*]+?):\*\*\s+(.+)$")
COMMENT = re.compile(r"<!--.*?-->", re.S)

MAX_TITLE = 60
MAX_AREA = 20
MAX_CHANGE = 280
MAX_AREAS = 6


def cargo_version() -> str:
    text = (ROOT / "Cargo.toml").read_text()
    package = text.split("[package]", 1)[1].split("\n[", 1)[0]
    match = re.search(r'^version\s*=\s*"([^"]+)"', package, re.M)
    if not match:
        sys.exit("error: no version in the [package] table of Cargo.toml")
    return match.group(1)


def path_of(version: str) -> Path:
    return NOTES / f"v{version.lstrip('v')}.md"


def git(*args: str) -> str:
    result = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else ""


def changes_since_last_release() -> tuple[str, list[str]]:
    """The last v* tag and the titles of what merged on main after it."""
    last = git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", "HEAD")
    if not last:
        return "", []
    titles = git("log", "--first-parent", "--format=%s", f"{last}..HEAD").splitlines()
    return last, [t for t in titles if t and not UNSEEN.match(t)]


def new(version: str) -> None:
    path = path_of(version)
    if path.exists():
        sys.exit(f"error: {path.relative_to(ROOT)} exists already")
    last, titles = changes_since_last_release()
    if last:
        listed = "\n".join(f"- {t}" for t in titles) or "- (none: only builds, CI, docs and the like)"
        source = f"What merged since {last} that a user may notice:\n\n{listed}"
    else:
        source = "No v* tag found: run `git fetch --tags` to list what merged since the last release."
    NOTES.mkdir(exist_ok=True)
    path.write_text(
        f"""<!--
The changelog entry of v{version.lstrip('v')}, for people who use the app. It goes at the
top of the release on GitHub and on arakun.dev/changelog. How to write it:
release-notes/README.md. Check it with: tools/release-notes.py check

{source}
-->

## TODO the one to three biggest changes

- **TODO:** What the reader can do now, or what works better.
"""
    )
    print(f"wrote {path.relative_to(ROOT)}: replace the TODO lines, then run tools/release-notes.py check")


def problems(text: str) -> list[str]:
    lines = [line.strip() for line in COMMENT.sub("", text).splitlines() if line.strip()]
    if not lines:
        return ["it is empty"]
    found = []
    heading = HEADING.match(lines[0])
    if not heading:
        found.append(f"start with a `## Title` line, not `{lines[0]}`")
    else:
        title = heading.group(1)
        if "TODO" in title:
            found.append("write the title (it still says TODO)")
        if len(title) > MAX_TITLE:
            found.append(f"keep the title under {MAX_TITLE} characters (it has {len(title)})")
        if title.endswith("."):
            found.append("leave out the period at the end of the title")
        if re.search(r"\bv?\d+\.\d+\.\d+\b", title):
            found.append("leave the version out of the title; the changelog shows it beside it")
    areas = []
    for line in lines[1:]:
        match = AREA.match(line)
        if not match:
            found.append(f"write each change as `- **Area:** change`, not `{line}`")
            continue
        area, change = match.group(1).strip(), match.group(2).strip()
        if "TODO" in area or "TODO" in change:
            found.append("replace the TODO change")
        if len(area) > MAX_AREA:
            found.append(f"keep the area `{area}` to one or two words")
        if len(change) > MAX_CHANGE:
            found.append(f"shorten the change under {area} to {MAX_CHANGE} characters (it has {len(change)})")
        if area in areas:
            found.append(f"use the area `{area}` once; put both changes in one line")
        areas.append(area)
    if not areas:
        found.append("add at least one `- **Area:** change` line")
    if len(areas) > MAX_AREAS:
        found.append(f"keep it to {MAX_AREAS} areas (it has {len(areas)})")
    return found


def check(version: str) -> None:
    path = path_of(version)
    name = path.relative_to(ROOT)
    if not path.exists():
        sys.exit(
            f"error: {name} is missing. Every release needs its changelog entry: "
            f"run tools/release-notes.py new in the version bump pull request"
        )
    found = problems(path.read_text())
    if found:
        for problem in found:
            print(f"error: {name}: {problem}", file=sys.stderr)
        sys.exit(1)
    print(f"{name} is fine")


def main() -> None:
    args = sys.argv[1:]
    if not args or args[0] not in ("new", "check") or len(args) > 2:
        sys.exit(__doc__.strip())
    version = args[1] if len(args) == 2 else cargo_version()
    (new if args[0] == "new" else check)(version)


if __name__ == "__main__":
    main()
