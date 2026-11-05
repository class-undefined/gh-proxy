#!/usr/bin/env python3
"""Version validation, binary-only archives and complete release checksums (Python 3.10+)."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)


def valid_version(value: str) -> str:
    number = r"(?:0|[1-9][0-9]*)"
    identifier = r"[0-9A-Za-z-]+"
    pattern = rf"{number}\.{number}\.{number}(?:-{identifier}(?:\.{identifier})*)?(?:\+{identifier}(?:\.{identifier})*)?"
    if not re.fullmatch(pattern, value):
        raise ValueError("version must be a semantic version without a v prefix")
    prerelease = value.split("+", 1)[0].partition("-")[2]
    if any(p.isdigit() and len(p) > 1 and p.startswith("0") for p in prerelease.split(".")):
        raise ValueError("numeric prerelease identifiers cannot have leading zeroes")
    return value


def manifest_version() -> str:
    manifest = subprocess.check_output(
        ["cargo", "read-manifest", "--manifest-path", str(ROOT / "Cargo.toml")], text=True
    )
    return valid_version(json.loads(manifest)["version"])


def check_tag(version: str, tag: str | None) -> None:
    valid_version(version)
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"release tag must match Cargo.toml: v{version}")


def executable(target: str) -> str:
    if target not in TARGETS:
        raise ValueError("unsupported release target")
    return "gh-proxy.exe" if "windows" in target else "gh-proxy"


def archive_name(version: str, target: str) -> str:
    valid_version(version)
    executable(target)
    extension = "zip" if "windows" in target else "tar.gz"
    return f"gh-proxy-v{version}-{target}.{extension}"


def package(binary: Path, target: str, version: str, output: Path) -> Path:
    name = archive_name(version, target)
    if not binary.is_file() or binary.stat().st_size == 0:
        raise ValueError("release binary is missing or empty")
    output.mkdir(parents=True, exist_ok=True)
    archive = output / name
    if "windows" in target:
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            bundle.write(binary, executable(target))
    else:
        with tarfile.open(archive, "w:gz") as bundle:
            info = bundle.gettarinfo(str(binary), arcname=executable(target))
            info.mode, info.uid, info.gid = 0o755, 0, 0
            info.uname = info.gname = ""
            with binary.open("rb") as source:
                bundle.addfile(info, source)
    return archive


def validate_archive(path: Path, target: str) -> None:
    if "windows" in target:
        with zipfile.ZipFile(path) as bundle:
            entries = bundle.infolist()
            valid = len(entries) == 1 and entries[0].filename == executable(target) and entries[0].file_size > 0
            if bundle.testzip() is not None:
                valid = False
    else:
        with tarfile.open(path, "r:gz") as bundle:
            entries = bundle.getmembers()
            valid = (len(entries) == 1 and entries[0].name == executable(target)
                     and entries[0].isfile() and entries[0].size > 0 and entries[0].mode & 0o111 != 0)
    if not valid:
        raise ValueError(f"invalid binary-only archive: {path.name}")


def checksums(version: str, output: Path) -> Path:
    expected = {archive_name(version, target) for target in TARGETS}
    actual = {path.name for path in output.iterdir() if path.name != "SHA256SUMS"}
    if actual != expected:
        raise ValueError(f"release artifacts incomplete or unexpected: missing={sorted(expected - actual)}, extra={sorted(actual - expected)}")
    lines = []
    for target in TARGETS:
        path = output / archive_name(version, target)
        validate_archive(path, target)
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        lines.append(f"{digest.hexdigest()}  {path.name}\n")
    manifest = output / "SHA256SUMS"
    manifest.write_text("".join(sorted(lines)), encoding="ascii")
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    version_parser = commands.add_parser("version")
    version_parser.add_argument("--tag")
    pack_parser = commands.add_parser("package")
    pack_parser.add_argument("--target", required=True, choices=TARGETS)
    pack_parser.add_argument("--version", required=True)
    pack_parser.add_argument("--output", type=Path, default=ROOT / "dist")
    checksum_parser = commands.add_parser("checksums")
    checksum_parser.add_argument("--version", required=True)
    checksum_parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    try:
        if args.command == "version":
            version = manifest_version()
            check_tag(version, args.tag)
            print(f"version={version}")
            print(f"prerelease={str('-' in version.split('+', 1)[0]).lower()}")
        elif args.command == "package":
            binary = ROOT / "target" / args.target / "release" / executable(args.target)
            print(package(binary, args.target, args.version, args.output))
        else:
            print(checksums(args.version, args.output))
    except (ValueError, OSError, tarfile.TarError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        parser.exit(1, f"release: {error}\n")


if __name__ == "__main__":
    main()
