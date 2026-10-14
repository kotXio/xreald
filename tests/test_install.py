# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import install
from scripts import artifacts


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)/'another-home'
        self.home.mkdir()
        self.user = SimpleNamespace(pw_dir=str(self.home), pw_uid=2345, pw_name='desktop')

    def test_user_paths_and_scoped_read_only_rule(self):
        self.assertEqual(install.layout(self.user)['xreald'], self.home/'.local/bin/xreald')
        rule = install.rule_bytes(self.user).decode()
        self.assertIn('OWNER="desktop", MODE="0400"', rule)
        self.assertIn('ATTRS{idVendor}=="3318", ATTRS{idProduct}=="043e"', rule)
        self.assertNotIn('GROUP=', rule)
        for name in ('root', 'bad"name', 'bad\nname'):
            u = SimpleNamespace(pw_name=name, pw_uid=0 if name=='root' else 2345)
            with self.assertRaises(ValueError): install.rule_bytes(u)

    def test_first_install_accepts_absent_unit(self):
        with patch.object(install.subprocess, 'run', side_effect=[
            SimpleNamespace(returncode=1),
            SimpleNamespace(returncode=0, stdout='LoadState=not-found\nActiveState=inactive\n')]) as run:
            install.idle(self.user)
        self.assertEqual(run.call_count, 2)

    def test_running_process_blocks_changes(self):
        with patch.object(install.subprocess, 'run', return_value=SimpleNamespace(returncode=0)):
            with self.assertRaisesRegex(RuntimeError, 'Stop all'): install.idle(self.user)

    def test_first_install_cannot_shadow_a_global_unit(self):
        with patch.object(install.subprocess, 'run', side_effect=[
            SimpleNamespace(returncode=1),
            SimpleNamespace(returncode=0, stdout='LoadState=loaded\nActiveState=inactive\n')]):
            with self.assertRaisesRegex(RuntimeError, 'unit already exists'):
                install.idle(self.user, installing=True)

    def test_existing_file_is_never_overwritten(self):
        path = self.home/'keep'; path.write_text('original')
        with self.assertRaises(FileExistsError): install.new_file(path,b'replacement',0o644)
        self.assertEqual(path.read_text(),'original')

    def test_partial_install_removes_only_new_files(self):
        paths = install.layout(self.user)
        keep = self.home/'keep'; keep.write_text('unrelated')
        targets = {paths['xreald']:(b'first',0o755), paths['xrealctl']:(b'second',0o755)}
        real_write = install.new_file
        def fail(path,data,mode):
            if path == paths['xrealctl']: raise OSError('injected failure')
            real_write(path,data,mode)
        with patch.object(install,'idle'), patch.object(install,'new_file',side_effect=fail):
            with self.assertRaises(OSError): install.apply_install(self.user,targets)
        self.assertFalse(paths['xreald'].exists()); self.assertFalse(paths['receipt'].exists())
        self.assertEqual(keep.read_text(),'unrelated')

    def test_round_trip_and_modified_file_guard(self):
        files = install.layout(self.user)
        targets = {files[n]:(n.encode(),0o644 if n=='unit' else 0o755)
                   for n in ('xreald','xrealctl','unit')}
        with patch.object(install,'idle'), patch.object(install,'trusted'):
            install.apply_install(self.user,targets)
            paths = install.plan_remove(self.user)
            files['unit'].write_text('user edit')
            with self.assertRaisesRegex(RuntimeError,'changed'): install.plan_remove(self.user)
            files['unit'].write_bytes(targets[files['unit']][0])
            extra = files['receipt'].parent/'keep'; extra.write_text('keep')
            install.apply_remove(self.user,install.plan_remove(self.user))
            self.assertEqual(extra.read_text(),'keep')
            self.assertTrue(all(not p.exists() for p in paths))

    def test_removal_receipt_cannot_add_arbitrary_paths(self):
        receipt = install.layout(self.user)['receipt']
        receipt.parent.mkdir(parents=True)
        keep = self.home/'keep';keep.write_text('keep')
        receipt.write_text(json.dumps({'format':1,'uid':2345,'files':{
            str(keep):{'sha256':artifacts.sha(keep.read_bytes()),'mode':0o644}}}))
        with patch.object(install,'trusted'):
            with self.assertRaisesRegex(RuntimeError,'receipt'):install.plan_remove(self.user)
        self.assertTrue(keep.exists())

    def test_input_rule_never_overwrites_and_removal_checks_content(self):
        rule = self.home/'70-xreald-input.rules'
        with patch.object(install,'RULE',rule), patch.object(install,'trusted'):
            install.permissions(self.user,False,True)
            with self.assertRaisesRegex(RuntimeError,'exists'):install.permissions(self.user,False,True)
            original = rule.read_bytes();rule.write_text('another rule')
            with self.assertRaisesRegex(RuntimeError,'differs'):install.permissions(self.user,True,True)
            rule.write_bytes(original);install.permissions(self.user,True,True)
        self.assertFalse(rule.exists())

    def test_symlink_path_is_rejected(self):
        path = self.home/'link';path.symlink_to(self.home/'other')
        with self.assertRaisesRegex(RuntimeError,'Unsafe'):install.trusted(path,os.getuid())

    def test_bundle_rejects_binary_tampering_and_source_drift(self):
        bundle = self.home/'bundle'
        original = ROOT/'binaries/0.1.2-linux-arm64'
        shutil.copytree(original,bundle)
        artifacts.validate(bundle)
        path=bundle/'xreald';data=path.read_bytes();path.write_bytes(data+b'edit')
        with self.assertRaisesRegex(ValueError,'Checksum'):artifacts.validate(bundle)
        path.write_bytes(data)
        manifest=json.loads((bundle/'manifest.json').read_text());manifest['sources']['Cargo.toml']='0'*64
        (bundle/'manifest.json').write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError,'source version'):artifacts.validate(bundle)


if __name__ == '__main__': unittest.main()
