# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Source binding and checksums shared by build, packaging and installation."""
import hashlib
import json
from pathlib import Path
import re
import struct

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'aarch64-unknown-linux-musl'
BINARIES = ('xreald', 'xrealctl')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def version():
    return re.search(r'^version = "([0-9A-Za-z.+-]+)"$',
                     (ROOT/'Cargo.toml').read_text(), re.M).group(1)


def sources():
    files = [ROOT/'Cargo.toml', ROOT/'Cargo.lock', *sorted((ROOT/'src').rglob('*.rs'))]
    return {str(p.relative_to(ROOT)): sha(p.read_bytes()) for p in files}


def static_arm64(data):
    if (len(data) < 64 or data[:6] != b'\x7fELF\x02\x01'
            or struct.unpack_from('<H', data, 18)[0] != 183):
        raise ValueError('Expected a 64-bit little-endian ARM64 ELF executable')
    offset = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    if size < 56 or not count or offset + size * count > len(data):
        raise ValueError('Invalid ELF program headers')
    for i in range(count):
        if struct.unpack_from('<I', data, offset + i * size)[0] == 3:
            raise ValueError('Expected a static binary without a dynamic interpreter')


def validate(bundle):
    bundle = Path(bundle)
    manifest = json.loads((bundle/'manifest.json').read_text())
    if (manifest.get('format') != 1 or manifest.get('version') != version()
            or manifest.get('target') != TARGET or manifest.get('sources') != sources()
            or set(manifest.get('binaries', {})) != set(BINARIES)):
        raise ValueError('Binary bundle does not match this source version')
    result = {}
    for name in BINARIES:
        path = bundle/name
        if path.is_symlink() or not path.is_file(): raise ValueError('Invalid binary: '+name)
        data = path.read_bytes()
        if sha(data) != manifest['binaries'][name]: raise ValueError('Checksum mismatch: '+name)
        static_arm64(data)
        result[name] = data
    expected = ''.join(f'{manifest["binaries"][name]}  {name}\n' for name in BINARIES)
    if (bundle/'SHA256SUMS').read_text() != expected: raise ValueError('Invalid SHA256SUMS')
    return result
