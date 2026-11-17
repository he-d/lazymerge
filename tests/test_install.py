#!/usr/bin/env python3
"""Offline integration tests for the Unix binary installer (stdlib only)."""
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    ('Linux', 'x86_64'): 'x86_64-unknown-linux-musl',
    ('Linux', 'aarch64'): 'aarch64-unknown-linux-musl',
    ('Darwin', 'x86_64'): 'x86_64-apple-darwin',
    ('Darwin', 'arm64'): 'aarch64-apple-darwin',
}


@unittest.skipIf(os.name == 'nt', 'Unix installer requires a Unix shell')
class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='lazymerge-installer-test-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.fixtures = self.root / 'release assets'
        self.fixtures.mkdir()
        self.mock_bin = self.root / 'commands'
        self.mock_bin.mkdir()
        self.install = self.root / 'my tools' / 'bin'
        self.log = self.root / 'requests.log'
        self.env = {
            **os.environ,
            'PATH': str(self.mock_bin) + os.pathsep + os.environ['PATH'],
            'LAZYMERGE_INSTALL_DIR': str(self.install),
            'TEST_RELEASE_ASSETS': str(self.fixtures),
            'TEST_DOWNLOAD_LOG': str(self.log),
            'TEST_OS': 'Linux', 'TEST_MACHINE': 'x86_64', 'TEST_ROSETTA': '0',
        }
        self.env.pop('LAZYMERGE_VERSION', None)
        self.command('uname', '#!/bin/sh\ncase "$1" in -s) printf "%s\\n" "$TEST_OS";; -m) printf "%s\\n" "$TEST_MACHINE";; esac\n')
        self.command('sysctl', '#!/bin/sh\nprintf "%s\\n" "$TEST_ROSETTA"\n')
        self.command('curl', '''#!/usr/bin/env python3
import os, pathlib, shutil, sys
arguments = sys.argv[1:]
url = arguments[-1]
destination = arguments[arguments.index('--output') + 1]
with open(os.environ['TEST_DOWNLOAD_LOG'], 'a') as log: log.write(url + '\\n')
source = pathlib.Path(os.environ['TEST_RELEASE_ASSETS']) / url.rsplit('/', 1)[-1]
if not source.is_file(): sys.exit(22)
shutil.copyfile(source, destination)
''')
        for target in TARGETS.values():
            self.archive(target)
        self.checksums()

    def command(self, name, text):
        path = self.mock_bin / name
        path.write_text(text)
        path.chmod(0o755)

    def archive(self, target, name='lazymerge'):
        content = f'#!/bin/sh\nprintf "lazymerge 0.1.2 {target}\\n"\n'.encode()
        with tarfile.open(self.fixtures / f'lazymerge-{target}.tar.gz', 'w:gz') as archive:
            info = tarfile.TarInfo(name)
            info.mode = 0o755
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
        return content

    def checksums(self):
        entries = [f'{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.name}\n'
                   for file in sorted(self.fixtures.glob('*.tar.gz'))]
        (self.fixtures / 'SHA256SUMS').write_text(''.join(entries))

    def run_installer(self, **environment):
        return subprocess.run(['sh', str(ROOT / 'install.sh')], env={**self.env, **environment},
                              text=True, capture_output=True, timeout=15)

    def test_selects_all_unix_architectures_and_installs_into_paths_with_spaces(self):
        for (system, machine), target in TARGETS.items():
            with self.subTest(system=system, machine=machine):
                result = self.run_installer(TEST_OS=system, TEST_MACHINE=machine)
                self.assertEqual(result.returncode, 0, result.stderr)
                installed = self.install / 'lazymerge'
                self.assertTrue(os.access(installed, os.X_OK))
                self.assertIn(target, installed.read_text())
                self.assertIn('releases/latest/download/', self.log.read_text())
                self.assertIn(str(self.install), result.stdout)

    def test_rosetta_prefers_native_apple_silicon(self):
        result = self.run_installer(TEST_OS='Darwin', TEST_MACHINE='x86_64', TEST_ROSETTA='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('aarch64-apple-darwin', (self.install / 'lazymerge').read_text())

    def test_an_older_command_earlier_on_path_gets_prepend_guidance(self):
        self.command('lazymerge', '#!/bin/sh\nprintf "older binary\\n"\n')
        result = self.run_installer(PATH=self.env['PATH'] + os.pathsep + str(self.install))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Another lazymerge currently takes precedence:', result.stdout)
        self.assertIn('Prepend ', result.stdout)
        self.assertNotIn('Run: lazymerge\n', result.stdout)

    def test_path_guidance_quotes_apostrophes(self):
        self.install = self.root / "a user's tools"
        result = self.run_installer(LAZYMERGE_INSTALL_DIR=str(self.install))
        self.assertEqual(result.returncode, 0, result.stderr)
        guidance = next(line.removeprefix('For this shell: ') for line in result.stdout.splitlines() if line.startswith('For this shell: '))
        probe = subprocess.run(['sh', '-c', guidance + '\ncommand -v lazymerge'], env=self.env, text=True, capture_output=True, check=True)
        self.assertEqual(probe.stdout.strip(), str(self.install / 'lazymerge'))

    def test_pinned_version_updates_existing_executable_without_staging_debris(self):
        self.install.mkdir(parents=True)
        destination = self.install / 'lazymerge'
        destination.write_bytes(b'old binary')
        result = self.run_installer(LAZYMERGE_VERSION='v0.1.2')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('releases/download/v0.1.2/', self.log.read_text())
        self.assertIn('lazymerge 0.1.2', destination.read_text())
        self.assertEqual(list(self.install.iterdir()), [destination])

    def test_bad_checksum_preserves_existing_install(self):
        self.install.mkdir(parents=True)
        destination = self.install / 'lazymerge'
        destination.write_bytes(b'keep this executable')
        (self.fixtures / 'SHA256SUMS').write_text('0' * 64 + '  lazymerge-x86_64-unknown-linux-musl.tar.gz\n')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Checksum mismatch', result.stderr)
        self.assertEqual(destination.read_bytes(), b'keep this executable')

    def test_missing_or_duplicate_checksums_fail_before_install(self):
        manifest = self.fixtures / 'SHA256SUMS'
        original = manifest.read_text()
        for content in ['', original + original]:
            with self.subTest(content=content[:20]):
                manifest.write_text(content)
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('No unique checksum', result.stderr)
                self.assertFalse((self.install / 'lazymerge').exists())

    def test_missing_binary_does_not_replace_install(self):
        self.install.mkdir(parents=True)
        destination = self.install / 'lazymerge'
        destination.write_bytes(b'old')
        self.archive('x86_64-unknown-linux-musl', name='unrelated.txt')
        self.checksums()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(destination.read_bytes(), b'old')

    def test_invalid_versions_and_architectures_fail_without_downloading(self):
        for environment in [{'LAZYMERGE_VERSION': '../../bad'}, {'TEST_MACHINE': 'i386'}, {'TEST_OS': 'Plan9'}]:
            with self.subTest(environment=environment):
                result = self.run_installer(**environment)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.log.exists())

    def test_directory_collision_does_not_receive_a_staged_executable(self):
        destination = self.install / 'lazymerge'
        destination.mkdir(parents=True)
        (destination / 'keep.txt').write_text('keep')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('is a directory', result.stderr)
        self.assertEqual([path.name for path in destination.iterdir()], ['keep.txt'])


if __name__ == '__main__':
    unittest.main()
