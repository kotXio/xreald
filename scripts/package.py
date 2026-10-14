#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Create a self-contained release with binaries, matching source and licenses."""
import argparse
import gzip
import io
from pathlib import Path
import tarfile

import artifacts as a


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=a.ROOT/'releases')
    args = parser.parse_args()
    bundle = a.ROOT/'binaries'/f'{a.version()}-linux-arm64'
    a.validate(bundle)
    prefix = f'xreald-{a.version()}-linux-arm64'
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output/(prefix+'.tar.gz')
    if archive.exists(): parser.error('Archive exists; choose a new output directory')
    names = ['Cargo.toml','Cargo.lock','README.md','NOTICE','LICENSE','install.py',
             'src','scripts','tests','docs','packaging','binaries','LICENSES']
    paths = []
    for name in names:
        path = a.ROOT/name
        paths.extend(sorted(path.rglob('*')) if path.is_dir() else [path])
    with archive.open('xb') as raw, gzip.GzipFile(filename='',mode='wb',fileobj=raw,mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode='w') as tar:
            for path in paths:
                if '__pycache__' in path.parts or path.suffix in ('.pyc','.pyo'): continue
                if path.is_symlink(): raise ValueError('Symlink in release: '+str(path))
                if not path.is_file(): continue
                data = path.read_bytes()
                entry = tarfile.TarInfo(prefix+'/'+str(path.relative_to(a.ROOT)))
                entry.size = len(data); entry.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
                tar.addfile(entry, io.BytesIO(data))
    digest = a.sha(archive.read_bytes())
    (args.output/(archive.name+'.sha256')).write_text(f'{digest}  {archive.name}\n')
    print(str(archive)+'\nSHA-256 '+digest)


if __name__ == '__main__': main()
