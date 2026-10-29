# Local API v1

[Back to XRealD](../README.md)

Private Unix stream socket. Each packet is `uint32 BE length`, `uint8 kind`, body;
length includes kind. Kind 1 is UTF-8 JSON, kind 2 is native telemetry. Maximum
length 256 KiB. Unknown/oversized inbound frames close only the affected client.

Requests use JSON-RPC 2.0 fields, a request ID and object params:

```json
{"jsonrpc":"2.0","id":1,"method":"subscribe","params":{"topics":["imu","buttons","state"],"rate_hz":30}}
```

Responses contain the matching ID and `result` or `error`. Server events contain
`method` / `params` without an ID. Request notifications/batches are not supported.

| Method | Params | Result |
| --- | --- | --- |
| `status` | `{}` | Discovery, channels, wearing state, fresh data, input access/errors, rates, CPU/RSS, counters |
| `capabilities` | `{}` | Known reads, disabled features, live successes/errors |
| `get` | `name`, optional bool `cached` | Value and time obtained; default requests live GET |
| `set` | object | Error -32030; no device write is sent |
| `subscribe` | `topics`, `format=json/native`, `rate_hz=1..240` | Replaces subscriptions on this connection |
| `unsubscribe` | `{}` | Removes all subscriptions on this connection |

Topics: `imu`, `buttons`, `gestures`, `wearing`, `state`. Native format requires IMU; native input rate
is not throttled by `rate_hz`. One backend IMU connection serves all subscribers.
Default `--imu-batch-ms 8` adds up to 8 ms of intentional receive buffering, plus
normal scheduling delay. Every native frame is retained. Use `--imu-batch-ms 0`
for immediate reads; allowed range 0–20 ms is reported in `status.transport`.
Control/evdev processing is not subject to this delay. Socket writes combine queued
packets using writev without changing the wire framing.

Control GETs have unique 31-bit transactions and 4 s timeout; at most 16 outstanding.
Unknown opcodes/parameter names cannot be submitted through the API.

GET names: `software.version`, `dsp.version`, `factory.config`, `proximity.enabled`,
`eis.enabled`, `display.edid`. Cached results retain `received_monotonic_ns` and
are invalidated when control disconnects. No old One/Pro EDID enum is applied to 1S.
Health snapshots summarize factory availability instead of copying its full table;
use explicit `get factory.config`. Config is limited to 256 KiB / 32768 JSON nodes;
oversized replies produce an error rather than unbounded allocation.

## Wearing events (0.1.3)

Subscribe to `wearing` to receive the current state immediately after the
subscription response, followed by device events. `status.wearing` contains
the same snapshot. This subscription does not open the IMU or poll the glasses.

```json
{"jsonrpc":"2.0","id":1,"method":"subscribe","params":{"topics":["wearing"]}}
```

Example event:

```json
{"jsonrpc":"2.0","method":"wearing","params":{"state":"worn","received_monotonic_ns":2000000000}}
```

States are `worn`, `not_worn` and `unknown`. The timestamp is nanoseconds since
daemon startup, or `null` before a device event. Starting the daemon or losing
and restoring the control connection resets the state to `unknown`; malformed
wearing messages also clear the previous state. A new subscriber receives only
the current connection's state, not a value left over from a disconnected pair.

Pause automatic actions on `unknown` or a lost API connection. Require a stable
`worn` state and deliberate user input before resuming automatic clicks.
The `proximity.enabled` GET is a device setting, not the current wearing state.

Wearing events retain their order and are not coalesced. A client that cannot
keep up is disconnected when its output queue fills. `unsubscribe`, or replacing
the subscription without `wearing`, stops future wearing events.

## Telemetry and time

Each sensor reading has value, `device_timestamp_ns` and `received_monotonic_ns`.
Invalid mask bits never replace another sensor. Unmasked NaNs are ignored; NaNs
in valid fields reject that frame. Device timestamp units follow the inspected
protocol. Host monotonic values are relative to daemon start; compare against
`sent_monotonic_ns`/`uptime_ns`, not Unix time.

`fresh` means a gyro update within 500 ms and an online IMU link. Every other sensor
has its own receive time; consumers must assess per-field age. An epoch increments
and latest readings reset on each IMU connection. Position/6DoF, magnetic units,
physical north and factory-calibration applicability are not inferred.

Native kind 2 body:

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | ASCII `XRI1` |
| 4 | 8 | Epoch, uint64 BE |
| 12 | 8 | Valid-frame sequence within epoch, uint64 BE |
| 20 | 8 | Host receive monotonic ns, uint64 BE |
| 28 | 2 | Original device opcode, uint16 BE |
| 30 | 4 | Device payload length, uint32 BE |
| 34 | 128 | Original IMU payload, little-endian fields |

CLI native output preserves packet framing, making concatenated captures decodable.

## Buttons and bounded delivery

`button` events include global sequence, source `evdev/control`, code, label, state
`down/up/repeat`, host receive time, optional raw device time and kernel event time,
hold duration/source and physical-mapping verification. TCP 0x272e code 4 is the
Quick Button on 1S (`XREAL_QUICK`). Do not assign physical button labels to other
TCP codes. Events from each source remain separate; the daemon does not inject input.

Evdev hold intervals use kernel event timestamps when available. TCP intervals use
receive-monotonic time and can be distorted by batching/network delay; long-press
is an observation, not proof of firmware behavior. Holds are bounded at 64 and reset
on disconnect/input gap. Input IDs are rechecked on the opened descriptor before
reading; no other keyboard is opened by the daemon.

`SYN_DROPPED`: publish gap in event order, ignore until `SYN_REPORT`, query the current
bitmap, publish `input_resync`. Failed bitmap queries are explicit, not an empty-state
claim. No synthetic button presses/releases are sent to Linux.
Buttons-only and gestures-only subscribers also receive input gap/reset/resync events and control
connection transitions, so they can discard held-button state after a disconnect.

### Quick Button gestures (0.1.2)

Subscribe to `gestures` for `button_gesture` events. IMU need not be subscribed.
Only TCP control code 4 participates; evdev events cannot produce duplicate gestures.
The `physical_mapping_verified` flag identifies the Quick Button mapping recorded
on USB `3318:043e`, firmware `1.9.1.20260914175625`.

- `single`: a press shorter than 600 ms, emitted 350 ms after release if no second
  press begins in that window.
- `double`: second press begins less than 350 ms after first release and both holds
  are shorter than 600 ms. Emitted on second release; no extra `single` events.
- `long`: held at least 600 ms, emitted on release. A preceding short click remains
  independent when a second press becomes long.

Example event params (nanoseconds relative to daemon start):

```json
{"button":"quick","source":"control","code":4,"kind":"single","first_down_monotonic_ns":1000000000,"last_up_monotonic_ns":1100000000,"emitted_monotonic_ns":1450000000,"press_durations_ms":[100],"timing_source":"receive_monotonic","physical_mapping_verified":true,"sequence":1}
```

Raw `button` down/up events remain immediate. Their legacy `gesture` field describes
one hold only; action consumers should use the separate `gestures` subscription
to avoid treating the first half of a double click as an independent action.
The daemon maintains one bounded recognizer and clears pending/held state on control
disconnect; incomplete gestures are discarded. `status.input.gesture_events` counts
derived events, including those with no subscriber. New subscribers get future events,
not a replay; a click already pending at subscription time may complete afterward.

Gesture timing uses host receive time; TCP delivery and scheduling can affect it.
The raw device timestamp is passed through without unit conversion. Gesture
thresholds belong to XRealD; the glasses' firmware actions still run independently.

Limits: 8 clients, 512 KiB output per client, 16 KiB read/write scheduling chunks
(writes at most 64 KiB per turn). Sampled JSON telemetry coalesces only wholly unsent snapshots.
Native/control/button records are not silently coalesced; overflow disconnects the
lagging client and increments eviction counters. Reconnect and inspect sequence/epoch
after a gap. Delivery to an indefinitely stalled client is not guaranteed.

Disconnect/unknown packet does not imply sleep. The API exposes channel facts and
freshness. No audio capture, framebuffer ownership or automatic device SET occurs.
