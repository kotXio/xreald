#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Compile static Linux ARM64 binaries with the installed Rust 1.99.0 toolchain."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess

import artifacts as a


def output(*args):
    return subprocess.check_output(args, text=True, cwd=a.ROOT).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=a.ROOT/'build/linux-arm64')
    parser.add_argument('--offline', action='store_true', help='Use cached Cargo dependencies')
    args = parser.parse_args()
    if os.getuid() == 0: parser.error('Build as your normal user, without sudo')
    if args.output.exists(): parser.error('Output exists; choose a new directory')
    rust = output('rustc', '--version')
    if not rust.startswith('rustc 1.99.0 '): parser.error('Select Rust 1.99.0 before building')
    sysroot = Path(output('rustc', '--print', 'sysroot'))
    host = next(line[6:] for line in output('rustc', '-vV').splitlines() if line.startswith('host: '))
    linker = sysroot/'lib/rustlib'/host/'bin/rust-lld'
    target_lib = Path(output('rustc', '--print', 'target-libdir', '--target', a.TARGET))
    if not target_lib.is_dir() or not linker.is_file():
        parser.error('Install the aarch64-unknown-linux-musl target with rustup first')
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home()/'.cargo')).resolve()
    flags = ['-C', 'linker-flavor=ld.lld', '--remap-path-prefix='+str(a.ROOT)+'=/xreald',
             '--remap-path-prefix='+str(cargo_home)+'=/cargo',
             '--remap-path-prefix='+str(sysroot)+'=/rust-toolchain']
    env = {**os.environ, 'CARGO_ENCODED_RUSTFLAGS':'\x1f'.join(flags),
           'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER':str(linker),
           'CARGO_TARGET_DIR':str(a.ROOT/'target')}
    env.pop('RUSTFLAGS', None)
    command = ['cargo', 'build', '--locked', '--release', '--target', a.TARGET]
    if args.offline: command.append('--offline')
    subprocess.run(command, env=env, cwd=a.ROOT, check=True)
    args.output.mkdir(parents=True)
    hashes = {}
    for name in a.BINARIES:
        source = a.ROOT/'target'/a.TARGET/'release'/name
        a.static_arm64(source.read_bytes())
        shutil.copyfile(source, args.output/name); (args.output/name).chmod(0o755)
        hashes[name] = a.sha(source.read_bytes())
    manifest = {'format':1, 'version':a.version(), 'target':a.TARGET, 'rustc':rust,
                'profile':'release; thin LTO; one codegen unit; stripped; remapped source paths',
                'sources':a.sources(), 'binaries':hashes}
    (args.output/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    (args.output/'SHA256SUMS').write_text(''.join(f'{hashes[n]}  {n}\n' for n in a.BINARIES))
    a.validate(args.output)
    print('Built and checked '+str(args.output))


if __name__ == '__main__': main()
