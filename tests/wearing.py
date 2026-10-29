#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Wire/API wearing lifecycle with synthetic TCP peers only; no glasses."""
import json
import subprocess
import time
import unittest
from integration import CLI, Service, device_frame


class WearingIntegration(unittest.TestCase):
    def setUp(self):
        self.service = Service()
        self.api = self.service.api()

    def tearDown(self):
        self.api.close(); self.service.close()

    def event(self):
        for _ in range(20):
            event = self.api.recv()
            if event.get('method') == 'wearing': return event['params']
        self.fail('No wearing event')

    def send(self, body):
        peer = self.service.peer
        with peer.send_lock:
            peer.connections['control'][0].sendall(device_frame(0x273d, body))

    def test_initial_cached_malformed_and_no_polling_or_imu(self):
        self.assertEqual(self.api.call('status')['result']['wearing']['state'], 'unknown')
        self.api.call('subscribe', topics=['wearing'])
        self.assertEqual(self.event()['state'], 'unknown')
        self.send(b'\x18\x01'); self.assertEqual(self.event()['state'], 'worn')
        # A subscriber joining later gets the current connection's cached event.
        newcomer = self.service.api()
        try:
            newcomer.call('subscribe', topics=['wearing'])
            self.assertEqual(newcomer.recv()['params']['state'], 'worn')
        finally: newcomer.close()
        for body in (b'\x18\x02', b'', b'\x18\x03', b'\x18\x01\x18\x02'):
            self.send(body)
            self.assertEqual(self.event()['state'], 'not_worn' if body == b'\x18\x02' else 'unknown')
        self.assertEqual(self.service.peer.writes, [])
        self.assertEqual(self.service.peer.accepted_imu, 0)

    def test_disconnect_and_new_connection_never_retain_worn(self):
        self.api.call('subscribe', topics=['wearing']); self.event()
        self.send(b'\x18\x01'); self.assertEqual(self.event()['state'], 'worn')
        self.service.peer.disconnect('control')
        self.assertEqual(self.event()['state'], 'unknown')
        deadline = time.monotonic()+5
        while time.monotonic() < deadline:
            status = self.api.call('status')['result']
            if status['control']['online'] and status['metrics']['connections'] >= 2: break
            time.sleep(.02)
        else: self.fail('No reconnect')
        self.assertEqual(status['wearing']['state'], 'unknown')
        self.assertIsNone(status['wearing']['received_monotonic_ns'])

    def test_existing_buttons_subscription_does_not_receive_wearing(self):
        self.api.call('subscribe', topics=['buttons'])
        self.send(b'\x18\x01'); time.sleep(.1)
        ident = self.api.send('status')
        self.assertEqual(self.api.recv().get('id'), ident)

    def wait_status_without_events(self, expected):
        deadline = time.monotonic()+2
        while time.monotonic() < deadline:
            ident = self.api.send('status')
            response = self.api.recv()
            self.assertEqual(response.get('id'), ident, 'Unexpected event after subscription ended')
            if response['result']['wearing']['state'] == expected: return
            time.sleep(.01)
        self.fail('Status did not reflect the device event')

    def test_unsubscribe_stops_events_but_status_keeps_updating(self):
        self.api.call('subscribe', topics=['wearing']); self.event()
        self.send(b'\x18\x01'); self.assertEqual(self.event()['state'], 'worn')
        self.api.call('unsubscribe')
        self.send(b'\x18\x02')
        self.wait_status_without_events('not_worn')

    def test_replacing_subscription_stops_wearing_events(self):
        self.api.call('subscribe', topics=['wearing']); self.event()
        self.send(b'\x18\x01'); self.assertEqual(self.event()['state'], 'worn')
        self.api.call('subscribe', topics=['buttons'])
        self.send(b'\x18\x02')
        self.wait_status_without_events('not_worn')

    def test_adjacent_device_frames_preserve_every_transition(self):
        self.api.call('subscribe', topics=['wearing']); self.event()
        peer = self.service.peer
        with peer.send_lock:
            peer.connections['control'][0].sendall(b''.join(
                device_frame(0x273d, body) for body in (b'\x18\x01', b'\x18\x02', b'\x18\x01')))
        events = [self.event() for _ in range(3)]
        self.assertEqual([e['state'] for e in events], ['worn', 'not_worn', 'worn'])
        times = [e['received_monotonic_ns'] for e in events]
        self.assertTrue(all(isinstance(t, int) and t >= 0 for t in times))
        self.assertEqual(times, sorted(times))

    def test_documented_cli_watch_returns_snapshot_without_device_polling(self):
        result = subprocess.run([str(CLI), '--socket', str(self.service.socket),
            'watch', 'wearing', '--seconds', '1'], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['method'], 'wearing')
        self.assertEqual(events[0]['params'], {'state':'unknown', 'received_monotonic_ns':None})
        self.assertEqual(self.service.peer.writes, [])
        self.assertEqual(self.service.peer.accepted_imu, 0)


if __name__ == '__main__': unittest.main(verbosity=2)
