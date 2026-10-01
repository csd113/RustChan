#!/usr/bin/env python3
"""Deterministic signed RustChan native update artifacts (stdlib + OpenSSL)."""
import argparse
import gzip
import hashlib
import io
import json
import pathlib
import re
import subprocess
import tarfile
import tomllib

TARGETS = {'x86_64-unknown-linux-gnu': 62, 'aarch64-unknown-linux-gnu': 183}
LIMIT = 256 * 1024 * 1024
FIELDS = {'format', 'version', 'release_id', 'target', 'filename', 'size', 'sha256',
          'executable_sha256', 'executable_size', 'schema', 'minimum_schema', 'minimum_updater'}


def stable(value):
    if not isinstance(value, str) or not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', value):
        raise ValueError('a canonical stable semantic version is required')
    return tuple(map(int, value.split('.')))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def executable(data, target):
    if target not in TARGETS or len(data) < 64 or data[:7] != b'\x7fELF\x02\x01\x01' or int.from_bytes(data[16:18], 'little') not in (2, 3) or int.from_bytes(data[18:20], 'little') != TARGETS[target]:
        raise ValueError('unsupported executable architecture or format')


def package(binary, target, version, output):
    version = version.removeprefix('v')
    stable(version)
    project = tomllib.loads(pathlib.Path('Cargo.toml').read_text())
    if project['package']['version'] != version:
        raise ValueError('package version must match the release tag')
    data = binary.read_bytes()
    if not 64 <= len(data) <= LIMIT:
        raise ValueError('executable size is outside limits')
    executable(data, target)
    output.mkdir(parents=True, exist_ok=True)
    name = f'rustchan-update-{target}.tar.gz'
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        entry = tarfile.TarInfo('rustchan-cli')
        entry.size, entry.mode, entry.mtime = len(data), 0o755, 0
        archive.addfile(entry, io.BytesIO(data))
    artifact = gzip.compress(raw.getvalue(), mtime=0)
    (output / name).write_bytes(artifact)
    info = dict(version=version, target=target, filename=name, size=len(artifact), sha256=digest(artifact),
                executable_size=len(data), executable_sha256=digest(data), schema=version,
                minimum_schema='1.5.0', minimum_updater='1.5.0', format=1)
    (output / f'build-info-{target}.json').write_text(json.dumps(info, sort_keys=True))
    return info


def validate(info, artifact):
    if info['format'] != 1 or info['target'] not in TARGETS or info['filename'] != f"rustchan-update-{info['target']}.tar.gz":
        raise ValueError('invalid manifest identity')
    if stable(info['schema']) != stable(info['version']) or stable(info['minimum_schema']) > stable(info['schema']):
        raise ValueError('invalid schema compatibility')
    stable(info['minimum_updater'])
    if info['size'] != len(artifact) or not 0 < len(artifact) <= LIMIT or info['sha256'] != digest(artifact):
        raise ValueError('artifact size or hash mismatch')
    # Bound decompression and inspect the raw first header, never let tarfile hide PAX/GNU records.
    with gzip.GzipFile(fileobj=io.BytesIO(artifact)) as stream:
        raw = stream.read(info['executable_size'] + 65537)
    if len(raw) > info['executable_size'] + 65536:
        raise ValueError('archive exceeds expansion limit')
    if raw[:100].split(b'\0')[0] != b'rustchan-cli' or raw[156:157] not in (b'0', b'\0') or raw[345:500].strip(b'\0'):
        raise ValueError('archive contains unexpected or hidden metadata')
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        entries = archive.getmembers()
        if len(entries) != 1 or entries[0].name != 'rustchan-cli' or not entries[0].isfile() or entries[0].pax_headers:
            raise ValueError('archive layout mismatch')
        data = archive.extractfile(entries[0]).read()
    end = 512 + ((len(data) + 511) // 512) * 512
    if any(raw[end:]):
        raise ValueError('archive has trailing hidden contents')
    if len(data) != info['executable_size'] or info['executable_sha256'] != digest(data):
        raise ValueError('executable size or hash mismatch')
    executable(data, info['target'])


def sign(directory, key, release_id, version):
    version = version.removeprefix('v')
    stable(version)
    if release_id <= 0:
        raise ValueError('invalid official release identity')
    public_der = subprocess.check_output(['openssl', 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'])
    if len(public_der) != 44 or public_der[:12] != bytes.fromhex('302a300506032b6570032100'):
        raise ValueError('signing key must be Ed25519')
    (directory / 'rustchan-update-public-key.hex').write_text(public_der[12:].hex() + '\n')
    public_pem = directory / 'rustchan-update-public-key.pem'
    subprocess.run(['openssl', 'pkey', '-in', str(key), '-pubout', '-out', str(public_pem)], check=True)
    for target in TARGETS:
        info = json.loads((directory / f'build-info-{target}.json').read_text())
        if info['version'] != version or info['target'] != target:
            raise ValueError('build identity differs from release')
        validate(info, (directory / info['filename']).read_bytes())
        manifest = dict(info, release_id=release_id)
        if set(manifest) != FIELDS:
            raise ValueError('manifest fields differ from protocol')
        path = directory / f'rustchan-update-{target}.json'
        path.write_bytes(json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode())
        signature = path.with_suffix('.json.sig')
        subprocess.run(['openssl', 'pkeyutl', '-sign', '-rawin', '-inkey', str(key), '-in', str(path), '-out', str(signature)], check=True)
        if signature.stat().st_size != 64:
            raise ValueError('signature size mismatch')
        subprocess.run(['openssl', 'pkeyutl', '-verify', '-rawin', '-pubin', '-inkey', str(public_pem), '-in', str(path), '-sigfile', str(signature)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='operation', required=True)
    build = sub.add_parser('package')
    build.add_argument('--binary', type=pathlib.Path, required=True)
    build.add_argument('--target', choices=TARGETS, required=True)
    build.add_argument('--version', required=True)
    build.add_argument('--output', type=pathlib.Path, required=True)
    signing = sub.add_parser('sign')
    signing.add_argument('--directory', type=pathlib.Path, required=True)
    signing.add_argument('--key', type=pathlib.Path, required=True)
    signing.add_argument('--release-id', type=int, required=True)
    signing.add_argument('--version', required=True)
    args = parser.parse_args()
    if args.operation == 'package':
        package(args.binary, args.target, args.version, args.output)
    else:
        sign(args.directory, args.key, args.release_id, args.version)


if __name__ == '__main__':
    main()
