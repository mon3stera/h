#!/usr/bin/env python3
"""Patch the upstream identity onto one archived session and prune the rest.

One-off migration for archives written before identity tracking existed
(h <= 0.3.0): resume now refuses sessions whose archive does not record the
upstream (protocol + base_url) they were recorded under, so legacy sessions
cannot be resumed at all. This script:

  1. reads the selected profile from the config and derives its identity;
  2. patches that identity into the target session's `.archive` and `.meta`;
  3. deletes every other archived session.

Run it AFTER the session you want to keep has finished (its final archive
write happens on exit; an earlier run would be overwritten by later
auto-saves). The default target is the most recently archived session;
pass `--id` to pin one explicitly.

Usage:
  python3 scripts/patch-resume-identity.py [--id SESSION_ID]
      [--archive-dir DIR] [--config PATH] [--yes] [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib

DEFAULT_ARCHIVE_DIR = pathlib.Path.home() / ".h" / "archive"
DEFAULT_CONFIG = pathlib.Path.home() / ".h" / "config.toml"
ARCHIVE_EXTENSION = "archive"
META_EXTENSION = "meta"


def load_identity(config_path: pathlib.Path) -> dict[str, str]:
    """The selected profile's upstream identity, as h serializes it."""
    with open(config_path, "rb") as f:
        config = tomllib.load(f)

    profile_id = config.get("profile")
    profiles = config.get("profiles")
    if not isinstance(profile_id, str) or not isinstance(profiles, dict):
        raise SystemExit(f"{config_path}: config must define a `profile` and `profiles`")

    profile = profiles.get(profile_id)
    if not isinstance(profile, dict):
        raise SystemExit(f"{config_path}: profile {profile_id!r} is not defined")

    protocol = profile.get("type")
    base_url = profile.get("base_url")
    if protocol not in ("openai", "anthropic") or not base_url:
        raise SystemExit(
            f"{config_path}: profile {profile_id!r} needs type (openai|anthropic) and base_url"
        )

    return {"protocol": protocol, "base_url": base_url}


def session_paths(archive_dir: pathlib.Path, session_id: str) -> tuple[pathlib.Path, pathlib.Path]:
    archive = archive_dir / f"{session_id}.{ARCHIVE_EXTENSION}"
    meta = archive_dir / f"{session_id}.{META_EXTENSION}"
    return archive, meta


def newest_session(archive_dir: pathlib.Path) -> str | None:
    archives = list(archive_dir.glob(f"*.{ARCHIVE_EXTENSION}"))
    if not archives:
        return None
    # Ties (writes within the same second) resolve deterministically.
    return max(archives, key=lambda path: (path.stat().st_mtime, path.name)).stem


def patch_identity(
    path: pathlib.Path, identity: dict[str, str], dry_run: bool
) -> str:
    """Adds `identity` to a session JSON file; returns what was done."""
    data = json.loads(path.read_text())
    current = data.get("identity")
    if current == identity:
        return "already patched"
    if current is not None:
        return f"overwrites {current!r}"

    data["identity"] = identity
    if not dry_run:
        path.write_text(json.dumps(data, ensure_ascii=False, separators=(",", ":")) + "\n")
    return "patched"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--id", help="session to keep (default: most recently archived)")
    parser.add_argument("--archive-dir", type=pathlib.Path, default=DEFAULT_ARCHIVE_DIR)
    parser.add_argument("--config", type=pathlib.Path, default=DEFAULT_CONFIG)
    parser.add_argument("--yes", action="store_true", help="skip the confirmation prompt")
    parser.add_argument("--dry-run", action="store_true", help="print the plan without changing anything")
    args = parser.parse_args()

    archive_dir = args.archive_dir.expanduser()
    if not archive_dir.is_dir():
        raise SystemExit(f"archive directory not found: {archive_dir}")

    identity = load_identity(args.config.expanduser())

    if args.id:
        session_id = args.id
    else:
        session_id = newest_session(archive_dir)
        if session_id is None:
            raise SystemExit(f"no archived session found in {archive_dir}")

    archive_path, meta_path = session_paths(archive_dir, session_id)
    if not archive_path.is_file():
        raise SystemExit(f"no archived session {session_id} at {archive_path}")

    title = ""
    if meta_path.is_file():
        title = json.loads(meta_path.read_text()).get("title", "")
    elif not args.dry_run:
        print(
            f"warning: {meta_path} is missing; the session will not appear in the "
            "resume picker, only `h --resume <id>` can reach it",
            file=sys.stderr,
        )

    other_archives = sorted(
        path
        for path in archive_dir.iterdir()
        if path.is_file()
        and path.name.endswith((f".{ARCHIVE_EXTENSION}", f".{META_EXTENSION}", ".tmp"))
        and path.name.split(".")[0] != session_id
    )

    print(f"keeping   {session_id}  {title}")
    print(f"identity  {identity['protocol']} @ {identity['base_url']}")
    print(f"patching  {archive_path.name}")
    if meta_path.is_file():
        print(f"patching  {meta_path.name}")
    else:
        print(f"patching  {meta_path.name} (missing)")
    for path in other_archives:
        print(f"deleting  {path.name}")

    if args.dry_run:
        print("dry run: nothing changed")
        return 0

    if not args.yes:
        answer = input("delete everything except this session? [y/N] ").strip().lower()
        if answer != "y":
            print("aborted")
            return 1

    print(f"archive   {patch_identity(archive_path, identity, dry_run=False)}")
    if meta_path.is_file():
        print(f"meta      {patch_identity(meta_path, identity, dry_run=False)}")

    for path in other_archives:
        path.unlink()

    print(f"deleted   {len(other_archives)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
