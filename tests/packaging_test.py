#!/usr/bin/env python3
"""Package provenance, ARM migration and exact-release checks without root."""
import io
import os
import pwd
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
COMMIT = "a" * 40
PACKAGES = ("icloud-session", "icloud-photos", "icloud-findmy", "icloud-notes", "icloud-reminders")


class Packaging(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}")
        self.stub("bsdtar", 'exec tar "$@"')

    def stub(self, name, script):
        path = self.bin / name
        path.write_text("#!/bin/bash\n" + script + "\n")
        path.chmod(0o755)

    def package(self, arch, pkg, version="1.2.3-1", metadata_arch=None):
        folder = self.work / f"icloud-{arch}"
        folder.mkdir(exist_ok=True)
        (folder / "source-commit.txt").write_text(COMMIT + "\n")
        path = folder / f"{pkg}-{version}-{arch}.pkg.tar.zst"
        data = f"pkgname = {pkg}\npkgver = {version}\narch = {metadata_arch or arch}\n".encode()
        with tarfile.open(path, "w") as archive:
            info = tarfile.TarInfo(".PKGINFO")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))

    def collect(self):
        return subprocess.run([str(ROOT / "bin/collect-packages"), str(self.work), COMMIT, str(self.work / "out")], env=self.env, capture_output=True)

    def populate(self):
        for arch in ("x86_64", "aarch64"):
            for pkg in PACKAGES:
                self.package(arch, pkg)

    def test_collect_valid_native_artifacts(self):
        self.populate()
        result = self.collect()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list((self.work / "out").glob("*"))), 2 * len(PACKAGES))

    def test_wrong_source_never_copies_packages(self):
        self.populate()
        (self.work / "icloud-aarch64/source-commit.txt").write_text("b" * 40)
        self.assertNotEqual(self.collect().returncode, 0)
        self.assertFalse((self.work / "out").exists())

    def test_wrong_architecture_rejected(self):
        self.populate()
        self.package("aarch64", "icloud-notes", metadata_arch="x86_64")
        self.assertNotEqual(self.collect().returncode, 0)
        self.assertFalse((self.work / "out").exists())

    def test_missing_package_rejected(self):
        self.populate()
        next((self.work / "icloud-aarch64").glob("icloud-notes-*.zst")).unlink()
        self.assertNotEqual(self.collect().returncode, 0)

    def test_mismatched_versions_rejected(self):
        self.populate()
        next((self.work / "icloud-aarch64").glob("icloud-notes-*.zst")).unlink()
        self.package("aarch64", "icloud-notes", version="1.2.4-1")
        self.assertNotEqual(self.collect().returncode, 0)

    def installer(self, arch):
        home = self.work / "home"
        hooks = home / ".config/omarchy/hooks/pre-refresh-pacman.d"
        hooks.mkdir(parents=True)
        etc = self.work / "etc"
        (etc / "pacman.d").mkdir(parents=True)
        (etc / "pacman.conf").write_text("[options]\nInclude = /etc/pacman.d/icloud-for-omarchy.conf\n")
        (etc / "pacman.d/icloud-for-omarchy.conf").write_text("old repository")
        (hooks / "icloud-for-omarchy").touch()
        source = (ROOT / "install.sh").read_text().replace("/etc/pacman", str(etc / "pacman"))
        # Existing config refers to transplanted scratch path, too.
        (etc / "pacman.conf").write_text(f"[options]\nInclude = {etc}/pacman.d/icloud-for-omarchy.conf\n")
        script = self.work / "install.sh"
        script.write_text(source)
        self.stub("uname", f"echo {arch}")
        user = pwd.getpwuid(os.getuid()).pw_name
        self.stub("getent", f"echo {user}:x:{os.getuid()}:{os.getgid()}:{user}:{home}:/bin/bash")
        self.stub("sudo", 'exec "$@"')
        self.stub("pacman", 'echo "$*" >>"$LOG"')
        self.stub("omarchy-pkg-add", 'echo "$*" >>"$LOG"')
        self.stub("pacman-key", "exit 0")
        self.stub("curl", 'while (($#)); do if [[ $1 == -o ]]; then touch "$2"; fi; shift; done')
        self.stub("gpg", "echo fpr:::::::::35C47A06567940B6796B4D0F9B3C7BDF85268B31:")
        self.env.update(LOG=str(self.work / "calls"), SUDO_USER=user)
        result = subprocess.run(["bash", str(script), "icloud-photos"], env=self.env, capture_output=True)
        return result, etc, hooks

    def test_arm_installer_migrates_repository(self):
        result, etc, hooks = self.installer("aarch64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((etc / "pacman.d/icloud-for-omarchy.conf").exists())
        self.assertFalse((hooks / "icloud-for-omarchy").exists())
        self.assertTrue((hooks / "icloud-for-omarchy-aarch64").exists())
        self.assertIn("[icloud-for-omarchy-aarch64]", (etc / "pacman.d/icloud-for-omarchy-aarch64.conf").read_text())

    def test_x86_installer_retains_repository(self):
        result, etc, hooks = self.installer("x86_64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((etc / "pacman.d/icloud-for-omarchy.conf").exists())
        self.assertEqual((etc / "pacman.conf").read_text().count("Include ="), 1)

    def test_unsupported_architecture_changes_nothing(self):
        result, etc, _ = self.installer("riscv64")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((etc / "pacman.d/icloud-for-omarchy.conf").read_text(), "old repository")
        self.assertFalse((self.work / "calls").exists())

    def test_verification_uses_exact_tag_for_installer_and_repository(self):
        log = self.work / "docker.log"
        self.stub("docker", 'printf "%s\\n" "$@" >"$LOG"')
        self.env.update(LOG=str(log), VERIFY_ARCH="aarch64", ARM_BUILD_IMAGE="arch:arm")
        result = subprocess.run([str(ROOT / "bin/verify-release"), "notes-v1.2.3", "icloud-notes", "1.2.3"], env=self.env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        invocation = log.read_text()
        self.assertIn("INSTALLER=https://github.com/ferdousbhai/icloud-for-omarchy/releases/download/notes-v1.2.3/install-notes.sh", invocation)
        self.assertIn("ICLOUD_RELEASES=https://github.com/ferdousbhai/icloud-for-omarchy/releases/download/notes-v1.2.3", invocation)
        self.assertNotIn("/latest/", invocation)


if __name__ == "__main__":
    unittest.main()
