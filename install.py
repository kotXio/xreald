#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Preview-first, per-user XRealD installation and optional scoped input access."""
import argparse
import json
import os
from pathlib import Path
import platform
import pwd
import re
import stat
import subprocess
import sys

from scripts import artifacts as a

UNIT = 'xreald.service'
RULE = Path('/etc/udev/rules.d/70-xreald-input.rules')


def layout(user):
    home = Path(user.pw_dir)
    return {'xreald':home/'.local/bin/xreald', 'xrealctl':home/'.local/bin/xrealctl',
            'unit':home/'.config/systemd/user'/UNIT,
            'receipt':home/'.local/state/xreald-install/receipt.json'}


def trusted(path, uid):
    for p in [path, *path.parents]:
        if not os.path.lexists(p): continue
        st = p.lstat()
        if stat.S_ISLNK(st.st_mode) or st.st_uid not in (0, uid) or st.st_mode & 0o022:
            raise RuntimeError('Unsafe installation path: '+str(p))


def idle(user, installing=False):
    active = subprocess.run(['pgrep', '-u', str(user.pw_uid), '-x', 'xreald'],
                            capture_output=True, timeout=5)
    if active.returncode != 1: raise RuntimeError('Stop all your XRealD processes first')
    state = subprocess.run(['systemctl', '--user', 'show', UNIT, '-p', 'LoadState', '-p', 'ActiveState'],
                           capture_output=True, text=True, timeout=5)
    props = dict(line.split('=',1) for line in state.stdout.splitlines() if '=' in line)
    if state.returncode or props.get('ActiveState') not in ('inactive', 'failed'):
        raise RuntimeError('Log in as the intended user and stop xreald.service first')
    if props.get('LoadState') == 'not-found': return
    if installing:
        raise RuntimeError('An XRealD unit already exists; remove the previous installation first')
    enabled = subprocess.run(['systemctl', '--user', 'is-enabled', UNIT],
                             capture_output=True, text=True, timeout=5)
    if enabled.stdout.strip() != 'disabled': raise RuntimeError('Disable xreald.service first')


def new_file(path, data, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data); os.fchmod(stream.fileno(), mode)
    except BaseException:
        path.unlink(missing_ok=True); raise


def plan_install(user, bundle):
    files = layout(user)
    data = a.validate(bundle)
    targets = {files[name]:(data[name], 0o755) for name in a.BINARIES}
    targets[files['unit']] = ((a.ROOT/'packaging'/UNIT).read_bytes(), 0o644)
    for path in [*targets, files['receipt']]:
        trusted(path.parent, user.pw_uid)
        if os.path.lexists(path): raise RuntimeError('Already exists; refusing to replace '+str(path))
    return targets


def apply_install(user, targets):
    idle(user, installing=True)
    created = []
    try:
        for path,(data,mode) in targets.items():
            new_file(path, data, mode); created.append(path)
        record = {'format':1, 'uid':user.pw_uid, 'files':{
            str(path):{'sha256':a.sha(data), 'mode':mode} for path,(data,mode) in targets.items()}}
        new_file(layout(user)['receipt'], (json.dumps(record,indent=2)+'\n').encode(), 0o600)
    except BaseException:
        for path in reversed(created): path.unlink(missing_ok=True)
        raise


def plan_remove(user):
    files = layout(user)
    trusted(files['receipt'], user.pw_uid)
    record = json.loads(files['receipt'].read_text())
    expected = {str(files[n]) for n in (*a.BINARIES, 'unit')}
    if record.get('format') != 1 or record.get('uid') != user.pw_uid or set(record.get('files', {})) != expected:
        raise RuntimeError('Invalid installation receipt')
    for name,info in record['files'].items():
        path = Path(name); trusted(path, user.pw_uid)
        if (not path.is_file() or stat.S_IMODE(path.stat().st_mode) != info['mode']
                or a.sha(path.read_bytes()) != info['sha256']):
            raise RuntimeError('Managed file changed; preserve and review it before removal: '+name)
    return [Path(p) for p in record['files']] + [files['receipt']]


def apply_remove(user, targets):
    idle(user)
    for path in targets: path.unlink()
    # Keep the shared bin/unit directories and anything the user added.
    try: layout(user)['receipt'].parent.rmdir()
    except OSError: pass


def rule_bytes(user):
    if user.pw_uid == 0 or not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_.-]*\$?', user.pw_name):
        raise ValueError('Choose a non-root Linux account with a simple local username')
    return ('# XRealD: read only the XREAL 1S input nodes, including before desktop login.\n'
            'SUBSYSTEM=="input", KERNEL=="event*", ATTRS{idVendor}=="3318", '
            'ATTRS{idProduct}=="043e", OWNER="'+user.pw_name+'", MODE="0400"\n').encode()


def permissions(user, remove, apply):
    data = rule_bytes(user)
    trusted(RULE.parent, 0)
    if remove:
        trusted(RULE, 0)
        if not RULE.is_file() or RULE.read_bytes() != data:
            raise RuntimeError('Rule differs; review it manually before removing')
        print('Remove '+str(RULE))
        if apply: RULE.unlink()
    else:
        if os.path.lexists(RULE): raise RuntimeError('Rule already exists; refusing to replace it')
        print(data.decode(), end='')
        print('Create '+str(RULE))
        if apply: new_file(RULE, data, 0o644)
    print('Reload udev rules and reconnect XREAL explicitly; no devices are retriggered.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('install','remove','permissions','remove-permissions'))
    parser.add_argument('--bundle', type=Path, default=a.ROOT/'binaries/0.1.2-linux-arm64')
    parser.add_argument('--user', help='Target account for the root-only input rule operation')
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    if args.action.endswith('permissions'):
        if not args.user: parser.error('--user is required for input permissions')
        if args.apply and (os.getuid() != 0 or platform.system() != 'Linux'):
            parser.error('Applying input permissions requires Linux root')
        permissions(pwd.getpwnam(args.user), args.action == 'remove-permissions', args.apply)
        return
    if args.user: parser.error('Run install/remove as the intended user, without --user')
    if os.getuid() == 0: parser.error('Run install/remove as the intended desktop user, without sudo')
    if args.apply and (platform.system() != 'Linux' or platform.machine() != 'aarch64'):
        parser.error('This installation targets Linux ARM64')
    user = pwd.getpwuid(os.getuid())
    targets = plan_install(user, args.bundle) if args.action == 'install' else plan_remove(user)
    for path in targets: print(str(path))
    if args.apply:
        if args.action == 'install': apply_install(user, targets)
        else: apply_remove(user, targets)
    print('No service started. Run systemctl --user daemon-reload after applying changes.')


if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as exc: sys.exit(str(exc))
