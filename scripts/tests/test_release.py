"""Release artifacts must contain only binaries and cover every supported target."""
import hashlib
import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("release", Path(__file__).parents[1] / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def test_tag_must_exactly_match_manifest_version(self):
        for version in ["0.1.0", "1.2.3-rc.1", "1.2.3+build.1"]:
            release.check_tag(version, f"v{version}")
        for version in ["../escape", "01.2.3", "v1.2.3", "1.2.3-01", "1.2.3\n"]:
            with self.assertRaises(ValueError):
                release.valid_version(version)
        with self.assertRaises(ValueError):
            release.check_tag("0.1.0", "v0.2.0")

    def test_archives_preserve_bytes_permissions_and_complete_checksums(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binary = root / "fixture"
            binary.write_bytes(b"binary\x00fixture\xff")
            for target in release.TARGETS:
                archive = release.package(binary, target, "0.1.0", root / "dist")
                if "windows" in target:
                    with zipfile.ZipFile(archive) as bundle:
                        self.assertEqual(bundle.namelist(), ["gh-proxy.exe"])
                        self.assertEqual(bundle.read("gh-proxy.exe"), binary.read_bytes())
                else:
                    with tarfile.open(archive) as bundle:
                        self.assertEqual(bundle.getnames(), ["gh-proxy"])
                        self.assertEqual(bundle.extractfile("gh-proxy").read(), binary.read_bytes())
                        self.assertEqual(bundle.getmember("gh-proxy").mode, 0o755)
            manifest = release.checksums("0.1.0", root / "dist")
            self.assertEqual(len(manifest.read_text().splitlines()), 6)
            for line in manifest.read_text().splitlines():
                digest, name = line.split("  ")
                self.assertEqual(digest, hashlib.sha256((root / "dist" / name).read_bytes()).hexdigest())
            manifest.unlink()
            next((root / "dist").glob("*.zip")).unlink()
            with self.assertRaises(ValueError):
                release.checksums("0.1.0", root / "dist")

    def test_extra_files_and_non_binary_archive_entries_are_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binary = root / "fixture"
            binary.write_bytes(b"fixture")
            for target in release.TARGETS:
                release.package(binary, target, "0.1.0", root / "dist")
            secret = root / "dist" / "config.toml"
            secret.write_text("must not ship")
            with self.assertRaises(ValueError):
                release.checksums("0.1.0", root / "dist")
            secret.unlink()
            target = "x86_64-pc-windows-msvc"
            with zipfile.ZipFile(root / "dist" / release.archive_name("0.1.0", target), "a") as bundle:
                bundle.writestr("config.toml", "must not ship")
            with self.assertRaises(ValueError):
                release.checksums("0.1.0", root / "dist")


if __name__ == "__main__":
    unittest.main()
