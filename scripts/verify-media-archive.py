#!/usr/bin/env python3
"""Verify a pinned digest and extract only regular ffmpeg/ffprobe executables."""
import hashlib
import re
import sys
import tarfile
from pathlib import Path


def extract(archive: Path, expected: str, output: Path) -> None:
    if not re.fullmatch(r"[0-9a-fA-F]{64}", expected):
        raise ValueError("Set the platform DESKTOP_MEDIA_TOOLS_*_SHA256 variable to a pinned SHA-256 digest")
    with archive.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    if actual != expected.lower():
        raise ValueError("Media archive SHA-256 mismatch")
    with tarfile.open(archive, "r:gz") as bundle:
        selected = {}
        for member in bundle:
            name = member.name.removeprefix("./")
            if name not in {"ffmpeg", "ffprobe"}:
                raise ValueError(f"Unexpected archive entry: {member.name}")
            if not member.isfile() or name in selected or member.size > 512 * 1024 * 1024:
                raise ValueError("Archive must contain one regular executable per tool, each at most 512 MiB")
            selected[name] = member
        if set(selected) != {"ffmpeg", "ffprobe"}:
            raise ValueError("Archive must contain both ffmpeg and ffprobe")
        output.mkdir(parents=True, exist_ok=True)
        for name, member in selected.items():
            source = bundle.extractfile(member)
            if source is None:
                raise ValueError("Missing archive body")
            # Exclusive creation also rejects pre-existing symlinks at the destination.
            with (output / name).open("xb") as target:
                while chunk := source.read(1024 * 1024):
                    target.write(chunk)
            (output / name).chmod(0o755)


if __name__ == "__main__":
    extract(Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]))
