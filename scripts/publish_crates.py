#!/usr/bin/env python3
"""Push a vMAJOR.MINOR.PATCH tag so CI can publish to crates.io.

Does not run cargo publish and does not read a registry token.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO = ROOT / "Cargo.toml"
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")


def git(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=check,
        text=True,
        capture_output=True,
    )


def current_version() -> str:
    in_pkg = False
    for line in CARGO.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_pkg = stripped == "[workspace.package]"
            continue
        if not in_pkg:
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if match:
            return match.group(1)
    raise SystemExit("workspace.package version not found in Cargo.toml")


def set_version(new: str) -> None:
    lines = CARGO.read_text().splitlines(keepends=True)
    section = None
    changed = 0
    out: list[str] = []
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped[1:-1]
        if section == "workspace.package" and re.match(r"version\s*=", stripped):
            line = f'version = "{new}"\n'
            changed += 1
        elif section == "workspace.dependencies" and (
            stripped.startswith("gpui-agent =")
            or stripped.startswith("gpui-agent-recipe =")
        ):
            line, count = re.subn(
                r'version\s*=\s*"[^"]+"', f'version = "{new}"', line, count=1
            )
            if count != 1:
                raise SystemExit(f"missing version on dependency line: {stripped}")
            changed += 1
        out.append(line)
    if changed != 3:
        raise SystemExit(f"updated {changed} version fields, expected 3")
    CARGO.write_text("".join(out))


def bump_patch(version: str) -> str:
    major, minor, patch = version.split(".")
    return f"{major}.{minor}.{int(patch) + 1}"


def ensure_main_clean() -> None:
    branch = git("rev-parse", "--abbrev-ref", "HEAD").stdout.strip()
    if branch != "main":
        raise SystemExit(f"just publish must be run on main (currently {branch})")
    dirty = git("status", "--porcelain").stdout.strip()
    if dirty:
        raise SystemExit("working tree is dirty; commit or stash before publish\n" + dirty)


def tag_exists(tag: str) -> bool:
    local = git("rev-parse", "-q", "--verify", f"refs/tags/{tag}", check=False)
    if local.returncode == 0:
        return True
    remote = git("ls-remote", "--tags", "origin", f"refs/tags/{tag}", check=False)
    if remote.returncode != 0:
        raise SystemExit(remote.stderr.strip() or "git ls-remote failed")
    return bool(remote.stdout.strip())


def run_visible(args: list[str]) -> None:
    result = subprocess.run(args, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(f"{args[0]} {' '.join(args[1:])} failed ({result.returncode})")


def main() -> None:
    if len(sys.argv) > 2:
        raise SystemExit("usage: publish_crates.py [MAJOR.MINOR.PATCH]")
    requested = sys.argv[1] if len(sys.argv) == 2 else ""
    ensure_main_clean()
    current = current_version()
    if not SEMVER.match(current):
        raise SystemExit(f"current version is not MAJOR.MINOR.PATCH: {current}")
    new = requested or bump_patch(current)
    if not SEMVER.match(new):
        raise SystemExit(f"version must be MAJOR.MINOR.PATCH, got {new}")
    tag = f"v{new}"
    if tag_exists(tag):
        raise SystemExit(f"tag {tag} already exists; not moving it")
    if new != current:
        set_version(new)
        git("add", "--", "Cargo.toml")
        run_visible(["git", "commit", "-m", f"release: {tag}"])
    run_visible(["git", "push", "origin", "main"])
    run_visible(["git", "tag", "-a", tag, "-m", f"Release {tag}"])
    run_visible(["git", "push", "origin", tag])
    print(f"tagged {tag}; CI publishes gpui-agent, gpui-agent-recipe, and gpui-agent-cli")


if __name__ == "__main__":
    main()
