#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
"""Measure the native daemon with synthetic1400Hz input and two consumers; not OPi hardware evidence."""
import argparse
import contextlib
import json
import pathlib
import subprocess
import threading
import time
from integration import Service


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seconds', type=int, default=10)
    parser.add_argument('--output', type=pathlib.Path)
    args = parser.parse_args()
    if args.seconds<1:parser.error("seconds must be positive")
    service = Service(); native = service.api(); sampled = service.api(); observer = service.api()
    native.call('subscribe', topics=['imu'], format='native')
    sampled.call('subscribe', topics=['imu'], rate_hz=30)
    counts = [0, 0]; stop = threading.Event(); threads = []
    def consume(api, index):
        while not stop.is_set():
            try: api.recv(); counts[index] += 1
            except (OSError, EOFError): return
    for index, api in enumerate([native, sampled]):
        thread = threading.Thread(target=consume, args=(api, index), daemon=True); thread.start(); threads.append(thread)
    try:
        time.sleep(.5); before = observer.call('status')['result']; started = time.monotonic(); latencies = []; rss = []
        while time.monotonic() - started < args.seconds:
            call = time.monotonic(); after = observer.call('status')['result']; latencies.append((time.monotonic() - call) * 1000)
            if after['metrics']['resident_bytes'] is not None: rss.append(after['metrics']['resident_bytes'])
            else: rss.append(int(subprocess.check_output(['ps','-o','rss=','-p',str(service.process.pid)],text=True).strip()) * 1024)
            time.sleep(.1)
        elapsed = time.monotonic() - started
        after = observer.call('status')['result']; metrics = after['metrics']
        result = {'kind':'synthetic peer on development host','seconds':round(elapsed,3),'consumers':'one native + one JSON30Hz',
                  'cpu_percent_one_core':round((metrics['cpu_total_ns']-before['metrics']['cpu_total_ns']) / elapsed / 1e7,3),
                  'rss_max_bytes':max(rss),'api_latency_p95_ms':round(sorted(latencies)[int(len(latencies)*.95)],3),
                  'native_packets':counts[0],'sampled_packets':counts[1],'metrics':metrics,'glasses_used':False}
        print(json.dumps(result,indent=2))
        if args.output: args.output.write_text(json.dumps(result,indent=2)+'\n')
    finally:
        stop.set()
        for api in (native,sampled,observer):
            with contextlib.suppress(OSError): api.stream.shutdown(2)
            api.close()
        for thread in threads: thread.join(timeout=1)
        service.close()


if __name__ == '__main__': main()
