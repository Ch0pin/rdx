"""Release layout and signing checks; every output lives in a temporary root."""
from pathlib import Path
import hashlib
import json
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package_macos
import package_release


EXPECTED_BACKEND = 'androguard-f7fa4a4853d957ebc3c54355d128a3835ef5ae22-rdx2'


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / 'build/release'
        self.binary.mkdir(parents=True)
        for name in ('rdx', 'rdx-source-stats', 'rdx.exe', 'rdx-source-stats.exe'):
            (self.binary / name).write_bytes(name.encode())
        fixtures = {
            'Cargo.toml': '[package]\nversion = "0.8.0"\n',
            'LICENSE': 'RDX license', 'NOTICE': 'RDX notice',
            'assets/icons/rdx.icns': 'icon',
            'docs/install.md': 'Install help',
            'plugins/example/plugin.json': '{}',
            'plugins/example/main.py': 'print("plugin")',
            'plugins/example/tests/private-test.py': 'not distributed',
            'third_party/androguard/LICENSE': 'Apache-2.0 text',
            'third_party/androguard/README.md': 'Credits',
            'third_party/androguard/dependency-audit.json': json.dumps({'missing_license_text': [], 'cargo_lock_sha256': hashlib.sha256(b'audited lock').hexdigest()}),
            'third_party/androguard/vendored-source-audit.json': json.dumps([{'component': 'dep', 'files': [{'path': 'src/lib.rs', 'sha256': hashlib.sha256(b'pinned source').hexdigest()}]}]),
            'tools/androguard-helper/Cargo.lock': 'audited lock',
            'tools/androguard-helper/vendor/dep/src/lib.rs': 'pinned source',
            'third_party/androguard/dependencies/dep/LICENSE': 'Dependency notice',
            'third_party/androguard/mpl-source/colored/src/lib.rs': '// corresponding source',
        }
        for name, content in fixtures.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        verifier = Path(__file__).resolve().parents[2] / 'tools/androguard-helper/package.py'
        shutil.copy2(verifier, self.root / 'tools/androguard-helper/package.py')
        self.helper = self.root / 'tools/androguard-helper/target/release/rdx-androguard-helper'
        self.helper.parent.mkdir(parents=True)
        self.helper.write_bytes(b'helper')

    def release(self, platform, helper=None):
        args = ['package_release.py', '--platform', platform, '--binary-dir', str(self.binary), '--tag', 'v0.8.0']
        if helper is not None:
            args += ['--helper', str(helper)]
        with mock.patch.object(package_release, 'ROOT', self.root), mock.patch.object(sys, 'argv', args), mock.patch.object(package_macos.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=EXPECTED_BACKEND + '\n')):
            package_release.main()

    def test_macos_default_helper_notices_plugins_and_signing_order(self):
        output = self.root / 'isolated/RDX.app'
        with mock.patch.object(package_macos.subprocess, 'check_output', return_value='arm64\n'), mock.patch.object(package_macos.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=EXPECTED_BACKEND + '\n')) as run:
            package_macos.package_app(self.binary, output, root=self.root)
        executable = output / 'Contents/MacOS'
        self.assertEqual((executable / package_macos.HELPER).read_bytes(), b'helper')
        self.assertEqual((executable / 'rdx-source-stats').read_bytes(), b'rdx-source-stats')
        resources = output / 'Contents/Resources'
        self.assertTrue((resources / 'third_party/androguard/mpl-source/colored/src/lib.rs').is_file())
        self.assertTrue((resources / 'third_party/androguard/dependencies/dep/LICENSE').is_file())
        self.assertTrue((resources / 'plugins/example/main.py').is_file())
        self.assertFalse((resources / 'plugins/example/tests').exists())
        info = plistlib.loads((output / 'Contents/Info.plist').read_bytes())
        self.assertEqual(info['CFBundleExecutable'], 'rdx')
        signed = [Path(call.args[0][-1]) for call in run.call_args_list if '--sign' in call.args[0]]
        self.assertLess(signed.index(executable / package_macos.HELPER), signed.index(output))
        self.assertEqual(run.call_args_list[-1].args[0][:4], ['codesign', '--verify', '--deep', '--strict'])
        self.assertFalse((self.root / 'target/RDX.app').exists())

    def test_missing_helper_fails_before_touching_existing_bundle(self):
        self.helper.unlink()
        output = self.root / 'target/RDX.app'
        output.mkdir(parents=True)
        sentinel = output / 'preserve-me'
        sentinel.write_text('existing app')
        with self.assertRaisesRegex(SystemExit, 'Required Androguard release helper is missing'):
            package_macos.package_app(self.binary, output, root=self.root)
        self.assertEqual(sentinel.read_text(), 'existing app')
        self.assertFalse((output / 'Contents').exists())
        with self.assertRaisesRegex(SystemExit, 'Required Androguard release helper is missing'):
            self.release('linux-x86_64')
        self.assertFalse((self.root / 'dist').exists())

    def test_macos_explicit_helper_architecture_is_checked_before_writing(self):
        other = self.root / 'cross-target/helper'
        other.parent.mkdir()
        other.write_bytes(b'other target')
        output = self.root / 'cross/RDX.app'
        with mock.patch.object(package_macos.subprocess, 'check_output', side_effect=['arm64\n', 'arm64\n', 'x86_64\n']), mock.patch.object(package_macos.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=EXPECTED_BACKEND + '\n')):
            with self.assertRaisesRegex(SystemExit, 'architecture mismatch'):
                package_macos.package_app(self.binary, output, other, root=self.root)
        self.assertFalse(output.exists())

    def test_backend_and_source_provenance_must_match_before_distribution(self):
        output = self.root / 'never-written/RDX.app'
        with mock.patch.object(package_macos.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout='different-backend')):
            with self.assertRaisesRegex(SystemExit, 'backend/version does not match'):
                package_macos.package_app(self.binary, output, root=self.root)
        self.assertFalse(output.exists())
        lock = self.root / 'tools/androguard-helper/Cargo.lock'
        with mock.patch.object(package_macos.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=EXPECTED_BACKEND + '\n')):
            lock.write_text('unreviewed lock')
            with self.assertRaisesRegex(SystemExit, 'Cargo.lock changed'):
                package_macos.resolve_helper(self.root)
            lock.write_text('audited lock')
            (self.root / 'tools/androguard-helper/vendor/dep/src/lib.rs').write_text('modified source')
            with self.assertRaisesRegex(SystemExit, 'vendored source changed'):
                package_macos.resolve_helper(self.root)

    def test_linux_archive_has_explicit_helper_and_required_notices(self):
        explicit = self.root / 'matching-linux-target/helper'
        explicit.parent.mkdir()
        explicit.write_bytes(b'linux helper')
        self.release('linux-x86_64', explicit)
        path = self.root / 'dist/rdx-v0.8.0-linux-x86_64.tar.gz'
        with tarfile.open(path) as archive:
            prefix = 'rdx-v0.8.0-linux-x86_64/'
            self.assertEqual(archive.extractfile(prefix + package_macos.HELPER).read(), b'linux helper')
            self.assertIn(prefix + 'rdx-source-stats', archive.getnames())
            self.assertIn(prefix + 'third_party/androguard/mpl-source/colored/src/lib.rs', archive.getnames())
            self.assertIn(prefix + 'plugins/example/main.py', archive.getnames())
            self.assertNotIn(prefix + 'plugins/example/tests/private-test.py', archive.getnames())
        self.assertTrue(path.with_name(path.name + '.sha256').is_file())

    def test_windows_archive_stays_native_only(self):
        self.helper.unlink()
        self.release('windows-x86_64')
        with zipfile.ZipFile(self.root / 'dist/rdx-v0.8.0-windows-x86_64.zip') as archive:
            self.assertIn('rdx-v0.8.0-windows-x86_64/rdx.exe', archive.namelist())
            self.assertFalse(any(Path(name).name.startswith(package_macos.HELPER) for name in archive.namelist()))
        with self.assertRaisesRegex(SystemExit, 'Windows fallback is disabled'):
            self.release('windows-x86_64', self.helper)


if __name__ == '__main__':
    unittest.main()
