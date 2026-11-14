"""Vendored support checks against disposable checkouts, never real account data."""
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

# Loading the checker by path would leave a compiled copy in bin/.
sys.dont_write_bytecode = True
CHECKER = Path(__file__).resolve().parents[1] / 'bin/check-shared-support'
loader = importlib.machinery.SourceFileLoader('shared_support_checker', str(CHECKER))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)

OWNER = 'icloud-for-omarchy'  # holds the canonical copy of both groups
COPY = 'cloud-mail'  # holds a copy of both groups
FILES = sum(len(members) for members in module.GROUPS.values())


class SharedSupportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name, members in module.GROUPS.items():
            # Synthetic source bytes keep tests independent of sibling checkouts.
            for member in members:
                target = self.root / member
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(f'fixture source for {name}\n'.encode())
        for repo in module.REPOS:
            target = self.root / repo / 'bin/check-shared-support'
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(CHECKER, target)
            manifest = {name: hashlib.sha256((self.root / members[0]).read_bytes()).hexdigest()
                        for name, members in module.GROUPS.items()
                        if any(member.startswith(repo + '/') for member in members)}
            (self.root / repo / 'shared-support.json').write_text(json.dumps({'product': repo, 'groups': manifest}))
        self.checker = self.root / OWNER / 'bin/check-shared-support'

    def run_check(self, *args):
        return subprocess.run([sys.executable, str(self.checker),
                               '--workspace-root', str(self.root), *args], text=True, capture_output=True)

    def member(self, group, repo):
        return self.root / next(m for m in module.GROUPS[group] if m.startswith(repo + '/'))

    def keep_only(self, *repos):
        for repo in module.REPOS:
            if repo not in repos:
                shutil.rmtree(self.root / repo)

    def test_full_workspace_matches(self):
        result = self.run_check('--require-all')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f'{FILES} files', result.stdout)

    def test_drift_fails_and_record_refuses_to_hide_different_sources(self):
        path = self.member('agent_skill.rs', COPY)
        path.write_bytes(path.read_bytes() + b'\n# fixture change\n')
        before = self.checker.read_bytes()
        self.assertNotEqual(self.run_check().returncode, 0)
        self.assertNotEqual(self.run_check('--record', 'agent_skill.rs').returncode, 0)
        self.assertEqual(self.checker.read_bytes(), before)

    def test_record_updates_scoped_manifests_without_changing_sources(self):
        for member in module.GROUPS['agent_skill.rs']:
            path = self.root / member
            path.write_bytes(path.read_bytes() + b'\n# agreed fixture change\n')
        before = {member: (self.root / member).read_bytes()
                  for members in module.GROUPS.values() for member in members}
        self.assertEqual(self.run_check('--record', 'agent_skill.rs').returncode, 0)
        result = self.run_check('--require-all')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(before, {member: (self.root / member).read_bytes() for member in before})

    def test_standalone_clone_checks_digests_and_strict_mode_requires_siblings(self):
        self.keep_only(OWNER)
        self.assertEqual(self.run_check().returncode, 0)
        self.assertNotEqual(self.run_check('--require-all').returncode, 0)
        path = self.member('agent_skill.rs', OWNER)
        path.write_bytes(path.read_bytes() + b'\n# fixture change\n')
        self.assertIn('Product source digest mismatch', self.run_check().stderr)

    def test_group_record_does_not_require_repositories_outside_the_group(self):
        # Omacloud holds no copy of recall_client.rs.
        shutil.rmtree(self.root / 'omacloud')
        for member in module.GROUPS['recall_client.rs']:
            path = self.root / member
            path.write_bytes(path.read_bytes() + b'\n# agreed fixture change\n')
        result = self.run_check('--record', 'recall_client.rs')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_check().returncode, 0)

    def test_standalone_noncanonical_copy_still_checks_recorded_digest(self):
        self.keep_only(COPY)
        command = [sys.executable, str(self.root / COPY / 'bin/check-shared-support'),
                   '--workspace-root', str(self.root)]
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        path = self.member('agent_skill.rs', COPY)
        path.write_bytes(path.read_bytes() + b'\n# local drift\n')
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Product source digest mismatch', result.stderr)

    def test_renamed_standalone_checkout_uses_local_product_manifest(self):
        self.keep_only(OWNER)
        source = self.member('agent_skill.rs', OWNER).relative_to(self.root / OWNER)
        renamed = self.root / 'release-0.2.0'
        (self.root / OWNER).rename(renamed)
        self.checker = renamed / 'bin/check-shared-support'
        # Both automatic root selection and an explicit workspace root work.
        self.assertEqual(subprocess.run([sys.executable, str(self.checker)], capture_output=True).returncode, 0)
        self.assertEqual(self.run_check().returncode, 0)
        (renamed / source).write_bytes((renamed / source).read_bytes() + b'\n# drift\n')
        self.assertIn('Product source digest mismatch', self.run_check().stderr)

    def test_renamed_checkout_checks_present_sibling_and_records_scoped_hashes(self):
        source = self.member('agent_skill.rs', OWNER).relative_to(self.root / OWNER)
        renamed = self.root / 'my-icloud-worktree'
        (self.root / OWNER).rename(renamed)
        self.checker = renamed / 'bin/check-shared-support'
        self.assertEqual(self.run_check('--require-all').returncode, 0)
        changed = b'fixture source, changed everywhere\n'
        peers = [self.root / m for m in module.GROUPS['agent_skill.rs'][1:]]
        peers[0].write_bytes(changed)
        self.assertIn('Product source copies differ', self.run_check().stderr)
        for path in [renamed / source, *peers]:
            path.write_bytes(changed)
        result = self.run_check('--record', 'agent_skill.rs')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_check('--require-all').returncode, 0)

    def test_missing_source_and_checker_drift_fail(self):
        self.member('recall_client.rs', COPY).unlink()
        self.assertIn('Missing shared file', self.run_check().stderr)
        checker = self.root / 'omacloud/bin/check-shared-support'
        checker.write_bytes(checker.read_bytes() + b'\n# fixture checker change\n')
        self.assertIn('Checker sync mismatch', self.run_check('--require-all').stderr)


if __name__ == '__main__':
    unittest.main()
