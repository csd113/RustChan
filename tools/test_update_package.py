"""Offline signing/packaging regression tests with disposable synthetic ELF assets."""
import gzip
import io
import json
import pathlib
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

import update_package as tool


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.version = tomllib.loads(pathlib.Path('Cargo.toml').read_text())['package']['version']

    def binary(self, target):
        data = bytearray(128)
        data[:7] = b'\x7fELF\x02\x01\x01'
        data[16:18] = (2).to_bytes(2, 'little')
        data[18:20] = tool.TARGETS[target].to_bytes(2, 'little')
        path = self.root / target
        path.write_bytes(data)
        return path

    def package(self, target='x86_64-unknown-linux-gnu'):
        info = tool.package(self.binary(target), target, self.version, self.root)
        artifact = (self.root / info['filename']).read_bytes()
        return info, artifact

    def test_deterministic_package(self):
        info, artifact = self.package()
        again, second = self.package()
        self.assertEqual(info, again)
        self.assertEqual(artifact, second)
        # Earlier controllers lack the same-binary handoff/startup protocol.
        self.assertEqual(info['minimum_updater'], '1.6.6')
        tool.validate(info, artifact)

    def test_signatures_and_both_architectures(self):
        for target in tool.TARGETS:
            self.package(target)
        key = self.root / 'key.pem'
        subprocess.run(['openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(key)], check=True, capture_output=True)
        tool.sign(self.root, key, 123, self.version)
        self.assertEqual(len((self.root / 'rustchan-update-public-key.hex').read_text().strip()), 64)
        for target in tool.TARGETS:
            manifest = json.loads((self.root / f'rustchan-update-{target}.json').read_text())
            self.assertEqual(manifest['release_id'], 123)
            self.assertEqual(set(manifest), tool.FIELDS)

    def test_invalid_hash_size_schema_target(self):
        info, artifact = self.package()
        for field, value in [('sha256', '00' * 32), ('size', 1), ('minimum_schema', '99.0.0'), ('target', 'unsupported')]:
            changed = dict(info, **{field: value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                tool.validate(changed, artifact)

    def test_hidden_metadata_traversal_links_duplicates(self):
        info, _ = self.package()
        binary = self.binary('x86_64-unknown-linux-gnu').read_bytes()
        for name, kind, count in [('rustchan-cli', tarfile.XHDTYPE, 1), ('../rustchan-cli', tarfile.REGTYPE, 1), ('rustchan-cli', tarfile.SYMTYPE, 1), ('rustchan-cli', tarfile.REGTYPE, 2)]:
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode='w', format=tarfile.USTAR_FORMAT) as archive:
                for _ in range(count):
                    entry = tarfile.TarInfo(name)
                    entry.type, entry.size = kind, len(binary)
                    archive.addfile(entry, io.BytesIO(binary))
            artifact = gzip.compress(raw.getvalue(), mtime=0)
            changed = dict(info, size=len(artifact), sha256=tool.digest(artifact))
            with self.subTest(name=name, kind=kind), self.assertRaises((ValueError, tarfile.TarError)):
                tool.validate(changed, artifact)

    def test_wrong_executable_architecture(self):
        with self.assertRaisesRegex(ValueError, 'architecture or format'):
            tool.package(self.binary('aarch64-unknown-linux-gnu'), 'x86_64-unknown-linux-gnu', self.version, self.root)

    def test_release_tag_must_match_package_version(self):
        major, minor, patch = tool.stable(self.version)
        mismatched = f'{major}.{minor}.{patch + 1}'
        with self.assertRaisesRegex(ValueError, 'package version must match the release tag'):
            tool.package(self.binary('x86_64-unknown-linux-gnu'), 'x86_64-unknown-linux-gnu', mismatched, self.root)

    def test_prerelease_and_malformed_version_rejected(self):
        for version in ['1.5.0-rc.1', '01.5.0', '../1.5.0', '1.5']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                tool.stable(version)
