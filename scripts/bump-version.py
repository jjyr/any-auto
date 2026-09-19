#!/usr/bin/env python3
"""Bump Cargo versions and commit them for a release PR (default: patch)."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def package_version(text, header):
    sections = re.finditer(
        r"^" + re.escape(header) + r"\n(?:(?!^\[).*(?:\n|$))*",
        text, re.MULTILINE,
    )
    matches = []
    for section in sections:
        if re.search(r'^name = "any-auto"$', section.group(), re.MULTILINE):
            versions = list(re.finditer(r'^version = "([^"]+)"$', section.group(), re.MULTILINE))
            if len(versions) != 1:
                raise ValueError("Expected one package version")
            version = versions[0]
            matches.append((version.group(1), section.start() + version.start(1),
                            section.start() + version.end(1)))
    if len(matches) != 1:
        raise ValueError("Expected one any-auto package")
    return matches[0]


def stable_version(value):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError(f"Expected a stable X.Y.Z version, got: {value}")
    return tuple(map(int, value.split(".")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?", default="patch",
                        help="patch (default), minor, major, or an explicit version (e.g. 0.4.0)")
    args = parser.parse_args()

    # Resolve relative to the script so it also works outside the repository root.
    os.chdir(Path(__file__).resolve().parent.parent)
    branch = git("symbolic-ref", "--quiet", "--short", "HEAD")
    if git("status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Working tree must be clean; commit or stash changes first")

    updates = []
    for filename, header in [("Cargo.toml", "[package]"), ("Cargo.lock", "[[package]]")]:
        path = Path(filename)
        text = path.read_text()
        old, start, end = package_version(text, header)
        updates.append((path, text, old, start, end))
    current = updates[0][2]
    if updates[1][2] != current:
        raise ValueError("Cargo.toml and Cargo.lock versions must match")
    major, minor, patch = stable_version(current)
    increments = {"major": (major + 1, 0, 0), "minor": (major, minor + 1, 0),
                  "patch": (major, minor, patch + 1)}
    requested = args.version.removeprefix("v")
    new = increments[requested] if requested in increments else stable_version(requested)
    if new <= (major, minor, patch):
        raise ValueError("New version must be greater than the current version")
    version = ".".join(map(str, new))
    tag = f"v{version}"
    if git("tag", "--list", tag):
        raise ValueError(f"Tag {tag} already exists")

    for path, text, old, start, end in updates:
        path.write_text(text[:start] + version + text[end:])
    # Only the root package version changes; dependency versions and checksums stay pinned.
    subprocess.run(["cargo", "metadata", "--offline", "--locked", "--no-deps",
                    "--format-version", "1"], check=True, stdout=subprocess.DEVNULL)
    git("add", "--", "Cargo.toml", "Cargo.lock")
    subprocess.run(["git", "commit", "-m", f"chore: bump version to {version}"], check=True)
    print(f"Bumped {current} -> {version}; committed on {branch}. No tag created.")
    print("Next: push this branch and open a PR into main.")
    print(f"After the PR is merged and CI passes, create annotated tag {tag} on the")
    print(f"merged main commit with Cargo version {version}, then push the tag to release.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"Release preparation failed: {error}", file=sys.stderr)
        print("Inspect git status and git log before retrying; nothing was pushed.", file=sys.stderr)
        sys.exit(1)
