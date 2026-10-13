#!/usr/bin/env python3
"""Package provenance, ARM migration and exact-release checks without root."""
import io
import os
import pwd
import shutil
import tomllib
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
COMMIT = "a" * 40
PACKAGES = ("icloud-session", "icloud-photos", "icloud-findmy", "icloud-notes", "icloud-reminders", "icloud-contacts")


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

    def suite_checkout(self):
        root = self.work / "suite"
        (root / "packaging").mkdir(parents=True)
        (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.8.0"\n')
        shutil.copy2(ROOT / "packaging/PKGBUILD", root / "packaging/PKGBUILD")
        subprocess.run(["git", "init", "-q", "-b", "main", str(root)], check=True)
        for key, value in (("user.name", "Suite test"), ("user.email", "test@localhost"), ("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")):
            subprocess.run(["git", "config", key, value], cwd=root, check=True)
        subprocess.run(["git", "add", "."], cwd=root, check=True)
        subprocess.run(["git", "commit", "-qm", "Initial suite"], cwd=root, check=True)
        return root

    def test_suite_versions_ignore_old_app_tags_and_follow_suite_tags(self):
        root = self.suite_checkout()
        subprocess.run(["git", "tag", "notes-v99.0.0"], cwd=root, check=True)
        def version():
            return subprocess.check_output(["bash", "-ec", 'source packaging/PKGBUILD; pkgver'], cwd=root, text=True).strip()
        self.assertRegex(version(), r"^0\.8\.0\.r1\.[0-9a-f]+$")
        subprocess.run(["git", "tag", "v0.8.0"], cwd=root, check=True)
        self.assertEqual(version(), "0.8.0")
        subprocess.run(["git", "commit", "--allow-empty", "-qm", "Development"], cwd=root, check=True)
        self.assertRegex(version(), r"^0\.8\.0\.r1\.[0-9a-f]+$")

    @unittest.skipUnless(shutil.which("makepkg"), "native Arch makepkg unavailable")
    def test_makepkg_can_rewrite_and_reload_suite_version(self):
        root = self.suite_checkout()
        self.stub("cargo", "exit 0")
        self.stub("rustc", "echo 'host: aarch64-unknown-linux-gnu'")
        versions = []
        for _ in range(2):
            result = subprocess.run(["makepkg", "--nobuild", "--nodeps"], cwd=root / "packaging", env=self.env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            versions.append(subprocess.check_output(["bash", "-ec", 'source packaging/PKGBUILD; echo "$pkgver"'], cwd=root, text=True))
        self.assertEqual(versions[0], versions[1])

    def test_all_rust_crates_inherit_one_locked_suite_version(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        expected = workspace["package"]["version"]
        versions = {p["name"]: p["version"] for p in tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]}
        for member in workspace["members"]:
            package = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
            self.assertEqual(package["version"], {"workspace": True})
            self.assertEqual(versions[package["name"]], expected)

    def release_fixture(self):
        root = self.suite_checkout()
        (root / "bin").mkdir()
        for name in ("release", "collect-packages", "make-installers"):
            shutil.copy2(ROOT / "bin" / name, root / "bin" / name)
        (root / "bin/test").write_text("#!/bin/bash\nexit 0\n")
        (root / "bin/test").chmod(0o755)
        (root / "install.sh").write_text("#!/bin/bash\nSIGNING_KEY_FINGERPRINT=test-key\nDEFAULT_PACKAGES=(icloud-notes)\n")
        subprocess.run(["git", "add", "."], cwd=root, check=True)
        subprocess.run(["git", "commit", "-qm", "Release scripts"], cwd=root, check=True)
        self.stub("git", 'if [[ $1 == push ]]; then echo "git $*" >>"$LOG"; else exec /usr/bin/git "$@"; fi')
        self.stub("cargo", 'echo "cargo $*" >>"$LOG"')
        self.stub("vercmp", "echo 1")
        self.stub("gpg", r'''
            echo "gpg $*" >>"$LOG"
            while (($#)); do
                if [[ $1 == --output ]]; then touch "$2"; fi
                shift
            done
        ''')
        self.stub("repo-add", r'''
            for name in "${3%.db.tar.gz}".{db,files}.tar.gz; do touch "$name" "$name.sig"; done
        ''')
        self.stub("gh", r'''
            echo "gh $*" >>"$LOG"
            case "$1 $2" in
                "run list") echo 42 ;;
                "run view") echo true ;;
                "run download")
                    while [[ $1 != --dir ]]; do shift; done
                    python3 "$FIXTURES" "$2"
                    ;;
            esac
        ''')
        fixture = self.work / "artifacts.py"
        fixture.write_text('''import io, os, pathlib, subprocess, sys, tarfile
root = pathlib.Path(sys.argv[1])
commit = subprocess.check_output(["/usr/bin/git", "rev-parse", "HEAD"], text=True)
for arch in ("x86_64", "aarch64"):
    folder = root / f"icloud-{arch}"
    folder.mkdir(parents=True)
    (folder / "source-commit.txt").write_text(commit)
    for pkg in ''' + repr(PACKAGES) + ''':
        version = os.environ.get("ARTIFACT_VERSION", "1.2.3-1")
        data = f"pkgname = {pkg}\\npkgver = {version}\\narch = {arch}\\n".encode()
        with tarfile.open(folder / f"{pkg}-{version}-{arch}.pkg.tar.zst", "w") as archive:
            info = tarfile.TarInfo(".PKGINFO")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
''')
        self.env.update(LOG=str(self.work / "release.log"), FIXTURES=str(fixture))
        cache = root / "dist/cargo-aarch64/keep"
        cache.parent.mkdir(parents=True)
        cache.touch()
        # The real repository also ignores build output.
        (root / ".gitignore").write_text("/dist\n")
        subprocess.run(["/usr/bin/git", "add", ".gitignore"], cwd=root, check=True)
        subprocess.run(["/usr/bin/git", "commit", "-qm", "Ignore output"], cwd=root, check=True)
        return root

    def test_release_publishes_one_tag_and_complete_suite_without_local_install(self):
        root = self.release_fixture()
        result = subprocess.run([str(root / "bin/release"), "1.2.3"], env=self.env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = Path(self.env["LOG"]).read_text()
        self.assertIn("git push -q origin v1.2.3", log)
        self.assertIn("gh workflow run packages.yml", log)
        self.assertEqual(log.count("--detach-sign"), 12)
        self.assertIn("gh release create v1.2.3", log)
        self.assertNotIn("verify-release", log)
        self.assertNotIn("cargo publish", log)
        self.assertTrue((root / "dist/cargo-aarch64/keep").exists())
        self.assertEqual(tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"], "1.2.3")

    def test_release_refuses_wrong_version_before_signing_or_publishing(self):
        root = self.release_fixture()
        result = subprocess.run([str(root / "bin/release"), "1.2.3"], env=dict(self.env, ARTIFACT_VERSION="1.2.4-1"), capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        log = Path(self.env["LOG"]).read_text()
        self.assertNotIn("--detach-sign", log)
        self.assertNotIn("gh release create", log)

    def test_native_build_shares_target_and_preserves_provenance(self):
        log = self.work / "build.log"
        self.stub("uname", "echo x86_64")
        self.stub("cargo", "exit 0")
        self.stub("rustc", "exit 0")
        self.stub("makepkg", 'printf "%s %s %s %s %s\\n" "$PWD" "$CARGO_TARGET_DIR" "$CARGO_BUILD_JOBS" "$CARGO_PROFILE_DEV_DEBUG" "$CARGO_INCREMENTAL" >>"$LOG"')
        self.stub("dbus-run-session", "exit 0")
        output = self.work / "packages"
        result = subprocess.run(
            [str(ROOT / "bin/build-packages"), "x86_64", str(output)],
            env=dict(self.env, LOG=str(log), CARGO_BUILD_JOBS="3"), capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [line.split() for line in log.read_text().splitlines()]
        self.assertEqual(len(calls), 1)
        self.assertEqual(len({call[1] for call in calls}), 1)
        self.assertEqual({call[2] for call in calls}, {"3"})
        self.assertEqual({tuple(call[3:]) for call in calls}, {("0", "0")})
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True)
        self.assertEqual((output / "source-commit.txt").read_text(), commit)
        self.assertFalse(Path(calls[0][1]).exists())

    def test_native_build_rejects_other_architecture(self):
        self.stub("uname", "echo x86_64")
        self.stub("makepkg", "exit 99")
        result = subprocess.run(
            [str(ROOT / "bin/build-packages"), "aarch64", str(self.work / "packages")],
            env=self.env, capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"requested architecture", result.stderr)
        self.assertFalse((self.work / "packages").exists())

    def test_native_build_can_clone_a_checkout_owned_by_another_uid(self):
        probe = subprocess.run(["unshare", "--user", "--map-auto", "--map-root-user", "--mount", "--fork", "true"], capture_output=True)
        if probe.returncode:
            self.skipTest("subordinate UID namespaces unavailable")
        root = self.suite_checkout()
        (root / "bin").mkdir()
        shutil.copy2(ROOT / "bin/build-packages", root / "bin/build-packages")
        self.stub("uname", "echo x86_64")
        self.stub("cargo", "exit 0")
        self.stub("rustc", "exit 0")
        self.stub("makepkg", "exit 0")
        # A real chroot's builder reads the checkout as a different UID.
        # Restore ownership before leaving the namespace for normal cleanup.
        result = subprocess.run([
            "unshare", "--user", "--map-auto", "--map-root-user", "--mount", "--fork",
            "bash", "-euc", r'''
                chmod 755 "$1"
                mkdir "$2"
                trap 'chown -R 0:0 "$2"' EXIT
                chown 1000:1000 "$2"
                setpriv --reuid 1000 --regid 1000 --clear-groups "$3" x86_64 "$2"
            ''', "ownership-test", str(self.work), str(self.work / "output"), str(root / "bin/build-packages"),
        ], env=self.env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.work / "output/source-commit.txt").is_file())

    def test_arch_root_hides_host_runtime_and_preserves_dns(self):
        probe = subprocess.run(["unshare", "--user", "--map-auto", "--map-root-user", "--mount", "--fork", "true"], capture_output=True)
        if probe.returncode:
            self.skipTest("subordinate UID namespaces unavailable")
        root = self.suite_checkout()
        (root / "bin").mkdir()
        shutil.copy2(ROOT / "bin/arch-run", root / "bin/arch-run")
        archive = self.work / "bootstrap.tar"
        with tarfile.open(archive, "w") as tar:
            for name in ("root.x86_64", "root.x86_64/etc", "root.x86_64/etc/pacman.d"):
                info = tarfile.TarInfo(name)
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                tar.addfile(info)
        resolver = self.work / "resolver"
        resolver.write_bytes(Path("/etc/resolv.conf").read_bytes())
        runtime = os.stat("/run")
        self.stub("uname", "echo x86_64")
        self.stub("curl", 'cp "$BOOTSTRAP" "${@: -1}"')
        self.stub("arch-chroot", r'''
            set -e
            [[ $1 != -* ]]
            [[ -z $(ls -A /run) ]]
            cmp "$1/etc/resolv.conf" "$RESOLVER"
            [[ ,$(findmnt -no OPTIONS "$1/source"), == *,ro,* ]]
            echo isolated >>"$LOG"
        ''')
        result = subprocess.run([
            "unshare", "--user", "--map-auto", "--map-root-user", "--mount", "--fork",
            str(root / "bin/arch-run"), "true",
        ], env=dict(self.env, BOOTSTRAP=str(archive), RESOLVER=str(resolver), LOG=str(self.work / "chroot.log")), capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.work / "chroot.log").read_text().splitlines(), ["isolated", "isolated"])
        self.assertEqual((runtime.st_dev, runtime.st_ino), (os.stat("/run").st_dev, os.stat("/run").st_ino))

    def test_recipes_share_flags_and_package_selected_target(self):
        flags = set()
        for target in ("", str(self.work / "shared-target")):
            for pkg in PACKAGES:
                with self.subTest(target=target, package=pkg):
                    source = self.work / pkg
                    source.mkdir(exist_ok=True)
                    (source / "icloud-for-omarchy").symlink_to(ROOT, target_is_directory=True)
                    result = subprocess.run([
                        "bash", "-euc", r'''
                        source "$1"
                        pkgname="$5"
                        startdir="$2"
                        srcdir="$3"
                        pkgdir="$3/staging"
                        export CARGO_TARGET_DIR="$4"
                        _cargo_env
                        first=$RUSTFLAGS
                        _cargo_env
                        [[ $RUSTFLAGS == "$first" ]]
                        printf '%s\n' "$RUSTFLAGS"
                        install() {
                          if [[ $1 == -Dm755 && $2 != build/icloud-notes ]]; then
                            [[ $2 == "$CARGO_TARGET_DIR"/release/* ]]
                            printf '%s\n' "$2"
                          fi
                        }
                        ln() { :; }
                        "package_$pkgname"
                        ''', "test", str(ROOT / "packaging/PKGBUILD"),
                        str(ROOT / "packaging"), str(source), target, pkg,
                    ], env=dict(self.env, RUSTFLAGS=""), capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    lines = result.stdout.splitlines()
                    flags.add(lines[0])
                    self.assertGreater(len(lines), 1)
                    (source / "icloud-for-omarchy").unlink()
        self.assertEqual(len(flags), 1, flags)

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
        self.stub("gpg", "echo fpr:::::::::52130299581DAD685226900CA89CA1A6A1E74251:")
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

    def verification_fixture(self):
        root = self.work / "repo"
        (root / "bin").mkdir(parents=True)
        (root / "packaging").mkdir()
        for name in ("verify-release", "make-installers"):
            shutil.copy2(ROOT / "bin" / name, root / "bin" / name)
        (root / "packaging/PKGBUILD").write_text((ROOT / "packaging/PKGBUILD").read_text())
        (root / "Cargo.toml").write_text((ROOT / "Cargo.toml").read_text())
        (root / "install.sh").write_text('#!/bin/bash\nDEFAULT_PACKAGES=(icloud-notes)\necho "$ICLOUD_RELEASES" >>"$LOG"\n')
        subprocess.run([str(root / "bin/make-installers"), str(root / "assets")], check=True, capture_output=True)
        self.stub("curl", 'echo "$*" >>"$LOG"; name=${2##*/}; cp "$FIXTURE/assets/$name" "$4"')
        (root / "assets/install.sh").write_text((root / "install.sh").read_text())
        self.stub("pacman", 'echo "$2 1.2.3-1"')
        self.env.update(LOG=str(self.work / "verify.log"), ICLOUD_ARCH_ROOT="1", FIXTURE=str(root))
        return root

    def test_verification_checks_exact_installer_bytes_and_all_six_packages(self):
        root = self.verification_fixture()
        result = subprocess.run([str(root / "bin/verify-release"), "v1.2.3"], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = Path(self.env["LOG"]).read_text()
        self.assertEqual(log.count("-fsSL"), 6)
        self.assertIn("https://github.com/ferdousbhai/icloud-for-omarchy/releases/download/v1.2.3/install-notes.sh", log)
        self.assertNotIn("/latest/", log)
        self.assertEqual(result.stdout.count("Verified:"), 6)
        for pkg in PACKAGES:
            self.assertIn(f"Verified: {pkg} 1.2.3", result.stdout)

    def test_changed_published_installer_prevents_installation(self):
        root = self.verification_fixture()
        (root / "assets/install-notes.sh").write_text("changed installer")
        result = subprocess.run([str(root / "bin/verify-release"), "v1.2.3"], env=self.env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Verified:", result.stdout.decode())
        self.assertNotIn("\nhttps://", Path(self.env["LOG"]).read_text())

    def test_suite_mismatch_on_both_architectures_rejected(self):
        self.populate()
        for arch in ("x86_64", "aarch64"):
            next((self.work / f"icloud-{arch}").glob("icloud-notes-*.zst")).unlink()
            self.package(arch, "icloud-notes", version="1.2.4-1")
        self.assertNotEqual(self.collect().returncode, 0)
        self.assertFalse((self.work / "out").exists())

    def test_release_version_must_match_collected_suite(self):
        self.populate()
        result = subprocess.run([str(ROOT / "bin/collect-packages"), str(self.work), COMMIT, str(self.work / "out"), "9.9.9-1"], env=self.env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.work / "out").exists())

    def test_verification_enters_disposable_root_before_installing(self):
        log = self.work / "sudo.log"
        self.stub("sudo", 'printf "%s\\n" "$@" >"$LOG"')
        env = dict(self.env, LOG=str(log))
        env.pop("ICLOUD_ARCH_ROOT", None)
        result = subprocess.run([str(ROOT / "bin/verify-release"), "v1.2.3"], env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(log.read_text().splitlines(), [str(ROOT / "bin/arch-run"), "bin/verify-release", "v1.2.3"])


if __name__ == "__main__":
    unittest.main()
