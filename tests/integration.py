#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Black-box API/lifecycle tests against independent synthetic TCP peers; no glasses."""
import contextlib
import json
import os
import pathlib
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
DAEMON = pathlib.Path(os.environ.get('XREALD_BIN', ROOT / 'target/release/xreald'))
CLI = pathlib.Path(os.environ.get('XREALCTL_BIN', ROOT / 'target/release/xrealctl'))


def varint(value):
    output = bytearray()
    while value > 127:
        output.append((value & 127) | 128)
        value >>= 7
    output.append(value)
    return bytes(output)


def device_frame(opcode, body):
    return struct.pack('>HI', opcode, len(body)) + body


def property_reply(opcode, transaction, value):
    if isinstance(value, int):
        inner = b'\x10' + varint(value)
    else:
        data = value.encode()
        inner = b'\x12' + varint(len(data)) + data
    return device_frame(opcode, struct.pack('>I', transaction & 0x7fffffff)
                        + b'\x22' + varint(len(inner)) + inner)


def exact(stream, count):
    data = bytearray()
    while len(data) < count:
        chunk = stream.recv(count - len(data))
        if not chunk:
            raise EOFError()
        data.extend(chunk)
    return bytes(data)


class API:
    def __init__(self, path):
        self.stream = socket.socket(socket.AF_UNIX)
        self.stream.settimeout(5)
        self.stream.connect(str(path))
        self.next_id = 0

    def send(self, method, **params):
        self.next_id += 1
        body = json.dumps({'jsonrpc': '2.0', 'id': self.next_id,
                           'method': method, 'params': params}).encode()
        self.stream.sendall(struct.pack('>I', len(body) + 1) + b'\x01' + body)
        return self.next_id

    def recv(self):
        size, = struct.unpack('>I', exact(self.stream, 4))
        assert 0 < size <= 262144
        body = exact(self.stream, size)
        return json.loads(body[1:]) if body[0] == 1 else body[1:]

    def call(self, method, **params):
        ident = self.send(method, **params)
        for _ in range(1000):
            result = self.recv()
            if isinstance(result, dict) and result.get('id') == ident:
                return result
        raise AssertionError('no matching response')

    def close(self):
        self.stream.close()


class Peer:
    def __init__(self, rate=1400):
        self.stop = threading.Event()
        self.rate = rate
        self.listeners = []
        self.connections = {'control': [], 'imu': []}
        self.writes = []
        self.accepted_imu = 0
        self.send_lock = threading.Lock()
        self.close_config = False
        self.ignore_requests = False
        for channel in ('control', 'imu'):
            listener = socket.socket()
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(.1)
            setattr(self, channel + '_port', listener.getsockname()[1])
            self.listeners.append(listener)
            threading.Thread(target=self.accept, args=(channel, listener), daemon=True).start()

    def accept(self, channel, listener):
        while not self.stop.is_set():
            try:
                connection, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            self.connections[channel].append(connection)
            if channel == 'imu':
                self.accepted_imu += 1
            target = self.control if channel == 'control' else self.imu
            threading.Thread(target=self.worker, args=(target, channel, connection), daemon=True).start()

    def worker(self, target, channel, connection):
        try:
            target(connection)
        except (OSError, EOFError):
            pass
        finally:
            with contextlib.suppress(ValueError): self.connections[channel].remove(connection)
            connection.close()

    def control(self, connection):
        while not self.stop.is_set():
            opcode, size = struct.unpack('>HI', exact(connection, 6))
            body = exact(connection, size)
            transaction, = struct.unpack('>I', body[:4])
            self.writes.append((opcode, body))
            assert size == 6 and transaction & 0x80000000 and body[4:] == b'\x18\x00'
            assert opcode in (0x271d, 0x272d, 0x271f, 0x2718, 0x28a6, 0x275e)
            if self.ignore_requests:
                continue
            values = {0x271d: 'fixture-sw', 0x272d: 'fixture-dsp',
                      0x271f: json.dumps({'FSN': 'SERIAL_TO_REDACT', 'future': {'table': 'x' * 210000}}),
                      0x2718: 1, 0x28a6: 0, 0x275e: 29}
            packet = property_reply(opcode, transaction, values[opcode])
            # Exercise split headers and bodies independently of the Rust framer.
            with self.send_lock:
                connection.sendall(packet[:3]); connection.sendall(packet[3:])
                if opcode == 0x271f and self.close_config:
                    connection.shutdown(socket.SHUT_WR)
                    return

    def imu(self, connection):
        started = time.monotonic(); count = 0
        while not self.stop.is_set():
            due = int((time.monotonic() - started) * self.rate)
            frames = bytearray()
            while count < due:
                mask = 4 if count % 7 < 2 else 11
                body = bytearray(128)
                struct.pack_into('<Q', body, 8, 1_000_000_000 + count * 1_000_000_000 // self.rate)
                struct.pack_into('<I', body, 24, mask)
                struct.pack_into('<10f', body, 28, 1, 2, 3, 0, 0, 9.80665, 10, 20, 30, 42)
                frames.extend(device_frame(0x2836, body)); count += 1
            if frames:
                connection.sendall(frames)
            self.stop.wait(.005)

    def buttons(self, state, code=1):
        body = bytearray(64); struct.pack_into('<III', body, 0, code, state, 42)
        with self.send_lock:
            self.connections['control'][0].sendall(device_frame(0x272e, body))

    def disconnect(self, channel):
        for connection in list(self.connections[channel]):
            with contextlib.suppress(OSError): connection.shutdown(socket.SHUT_RDWR)
            connection.close()

    def close(self):
        self.stop.set()
        for channel in self.connections: self.disconnect(channel)
        for listener in self.listeners: listener.close()


class Service:
    def __init__(self, rate=1400):
        self.temp = tempfile.TemporaryDirectory(prefix='xr-', dir='/tmp')
        self.socket = pathlib.Path(self.temp.name) / 'run/control.sock'
        self.peer = Peer(rate)
        self.process = subprocess.Popen([str(DAEMON), '--socket', str(self.socket),
            '--no-discovery', '--peer', '127.0.0.1', '--imu-port', str(self.peer.imu_port),
            '--control-port', str(self.peer.control_port)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(self.process.communicate()[1].decode())
            if self.socket.exists():
                try:
                    api = API(self.socket)
                    status = api.call('status')['result']; api.close()
                    if status['control']['online']: return
                except (OSError, KeyError): pass
            time.sleep(.02)
        self.close(); raise AssertionError('daemon not ready')

    def api(self): return API(self.socket)
    def close(self):
        self.process.terminate()
        try: self.process.wait(timeout=5)
        except subprocess.TimeoutExpired: self.process.kill(); self.process.wait()
        self.peer.close()
        self.process.communicate()
        self.temp.cleanup()


class Integration(unittest.TestCase):
    def setUp(self): self.service = Service()
    def tearDown(self): self.service.close()

    def test_get_catalog_large_reply_fin_and_cli(self):
        api = self.service.api()
        self.assertFalse(api.call('capabilities')['result']['set_enabled'])
        for name in ('software.version', 'dsp.version', 'proximity.enabled', 'eis.enabled', 'display.edid'):
            self.assertIn('result', api.call('get', name=name))
        cached=api.call('get',name='software.version',cached=True)['result']
        self.assertEqual(cached['value'],'fixture-sw'); self.assertGreater(cached['received_monotonic_ns'],0)
        self.service.peer.close_config = True
        config = api.call('get', name='factory.config')['result']['value']
        self.assertEqual(config['FSN'], '<redacted>'); self.assertEqual(len(config['future']['table']), 210000)
        self.assertIn('error', api.call('set', name='display.edid', value=17))
        self.assertIn('error', api.call('get', name='arbitrary_opcode'))
        result = subprocess.run([str(CLI), '--socket', str(self.service.socket), 'status'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['api_version'], 1); api.close()

    def test_demand_driven_stream_and_reconnect(self):
        api = self.service.api()
        self.assertEqual(self.service.peer.accepted_imu, 0)
        api.call('subscribe', topics=['imu', 'state'], rate_hz=30)
        epoch = None
        while epoch is None:
            message = api.recv()
            if message.get('method') == 'imu':
                snapshot = message['params']['snapshot']; epoch = snapshot['epoch']
                self.assertEqual(snapshot['gyro']['value'], [1, 2, 3])
        self.service.peer.disconnect('imu')
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            message = api.recv()
            if message.get('method') == 'imu' and message['params']['snapshot']['epoch'] > epoch: break
        else: self.fail('stream did not recover')
        api.call('unsubscribe')
        query = self.service.api()
        for _ in range(100):
            if not query.call('status')['result']['imu']['online']: break
            time.sleep(.01)
        else: self.fail('IMU stayed open with no subscribers')
        query.close(); api.close()

    def test_tcp_buttons_with_streaming(self):
        api = self.service.api(); api.call('subscribe', topics=['buttons', 'imu'], rate_hz=60)
        self.service.peer.buttons(1); time.sleep(.65); self.service.peer.buttons(2)
        seen = []
        while len(seen) < 2:
            message = api.recv()
            if message.get('method') == 'button': seen.append(message['params'])
        self.assertEqual([b['state'] for b in seen], ['down', 'up'])
        self.assertEqual(seen[1]['gesture'], 'long_press')
        self.assertFalse(seen[1]['physical_mapping_verified']); api.close()

    def test_native_stream_and_cli_watch(self):
        api = self.service.api(); api.call('subscribe', topics=['imu'], format='native')
        packet = api.recv(); self.assertEqual(packet[:4], b'XRI1')
        epoch, sequence, received = struct.unpack('>QQQ', packet[4:28])
        self.assertGreater(epoch, 0); self.assertGreater(sequence, 0); self.assertGreater(received, 0)
        opcode, length = struct.unpack('>HI', packet[28:34])
        self.assertEqual((opcode, length), (0x2836, 128)); api.close()
        result = subprocess.run([str(CLI), '--socket', str(self.service.socket), 'watch', 'imu', '--seconds', '1'], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(any(json.loads(line).get('method') == 'imu' for line in result.stdout.splitlines()))

    def test_quick_button_gestures_are_exclusive_without_imu(self):
        api = self.service.api(); api.call('subscribe', topics=['gestures'])
        def press(seconds):
            self.service.peer.buttons(1, code=4); time.sleep(seconds); self.service.peer.buttons(2, code=4)
        def gesture(expected):
            event = api.recv(); self.assertEqual(event['method'], 'button_gesture')
            self.assertEqual(event['params']['kind'], expected)
            self.assertEqual(event['params']['button'], 'quick')
            self.assertTrue(event['params']['physical_mapping_verified'])
        press(.08); gesture('single')
        press(.07); time.sleep(.1); press(.08); gesture('double')
        press(.68); gesture('long')
        time.sleep(.4)
        state = api.call('status')['result']
        self.assertEqual(state['input']['gesture_events'], 3)
        self.assertEqual(self.service.peer.accepted_imu, 0)
        api.close()

    def test_quick_pending_click_is_cancelled_by_control_disconnect(self):
        api = self.service.api(); api.call('subscribe', topics=['gestures'])
        self.service.peer.buttons(1, code=4); time.sleep(.05); self.service.peer.buttons(2, code=4)
        self.service.peer.disconnect('control')
        self.assertEqual(api.recv()['method'], 'connection')
        self.assertEqual(api.recv()['method'], 'connection')
        time.sleep(.4)
        self.assertEqual(api.call('status')['result']['input']['gesture_events'], 0)
        api.close()

    def test_buttons_only_observes_control_gap_and_resets_hold(self):
        api = self.service.api(); api.call('subscribe', topics=['buttons'])
        self.service.peer.buttons(1)
        self.assertEqual(api.recv()['params']['state'], 'down')
        self.service.peer.disconnect('control')
        gap = api.recv()
        self.assertEqual(gap['method'], 'connection'); self.assertFalse(gap['params']['online'])
        recovered = api.recv()
        self.assertEqual(recovered['method'], 'connection'); self.assertTrue(recovered['params']['online'])
        deadline = time.monotonic() + 2
        while not self.service.peer.connections['control'] and time.monotonic() < deadline: time.sleep(.01)
        self.service.peer.buttons(2)
        released = api.recv()['params']
        self.assertEqual(released['state'], 'up'); self.assertIsNone(released['duration_ms'])
        api.close()

    def test_bad_client_does_not_stop_daemon(self):
        bad = self.service.api(); bad.stream.sendall(b'\xff\xff\xff\xff'); time.sleep(.05); bad.close()
        api = self.service.api(); self.assertGreater(api.call('status')['result']['metrics']['api_errors'], 0)
        self.assertIn('error', api.call('subscribe', topics=['other']))
        self.assertIn('error', api.call('subscribe', topics=['imu'], rate_hz=0))
        self.assertIn('error', api.call('subscribe',topics=['imu'],rate_hz='fast'))
        self.assertIn('error', api.call('get',name='software.version',cached='yes')); api.close()

    def test_pending_request_limit_and_timeout(self):
        api = self.service.api(); self.service.peer.ignore_requests = True
        for _ in range(17): api.send('get', name='software.version')
        results = [api.recv() for _ in range(17)]
        codes = [r['error']['code'] for r in results]
        self.assertIn(-32011, codes); self.assertIn(-32012, codes); api.close()

    def test_daemon_socket_cleanup_and_collision(self):
        other = subprocess.run([str(DAEMON), '--socket', str(self.service.socket), '--run-for', '0'], capture_output=True, text=True)
        self.assertNotEqual(other.returncode, 0)
        self.assertTrue(self.service.socket.exists())
        self.service.process.terminate(); self.service.process.wait(timeout=5)
        self.assertFalse(self.service.socket.exists())

    def test_slow_native_client_is_bounded(self):
        self.service.peer.rate = 12000
        slow = self.service.api(); slow.stream.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
        slow.call('subscribe', topics=['imu'], format='native')
        observer = self.service.api(); deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if observer.call('status')['result']['metrics']['clients_evicted'] > 0: break
            time.sleep(.02)
        else: self.fail('slow client retained an unbounded stream')
        self.assertEqual(self.service.process.poll(), None); observer.close(); slow.close()


if __name__ == '__main__': unittest.main(verbosity=2)
