# XRealD

XRealD connects **XREAL 1S glasses to Linux applications**. It provides button
events, wearing status, motion readings and device information through a small
background service and the `xrealctl` command. Applications share one connection
to the glasses.

Part of [Perideck](https://github.com/kotXio/Perideck), an Orange Pi/XREAL desktop
project. XRealD can also run independently using the glasses' USB network and
input interfaces. Supported hardware: **XREAL 1S, USB `3318:043e`**.

## Contents

- [What it does](#what-it-does)
- [Install prebuilt binaries](#install-prebuilt-binaries)
- [Build from source](#build-from-source)
- [Use it](#use-it)
- [Installation, permissions and removal](docs/installation.md)
- [Local API](docs/api-v1.md)
- [Sources and licenses](docs/sources.md)

## What it does

- **Quick Button:** reports single, double and long presses for an application
  to act on. Raw press/release events are available for push-to-talk controls.
- **Wearing status:** tells applications when the glasses are put on or taken off,
  so they can pause and resume their controls. Uses device events, without polling.
- **Motion sensors:** gyro, accelerometer, magnetometer and temperature, with
  timestamps and freshness. The IMU connection opens only while a client needs it.
- **Device information:** reads software/DSP versions, factory configuration,
  proximity/EIS flags and the current EDID value. Factory serial identity is redacted.
- **Reconnect:** rediscovers the same USB device after unplugging and reconnecting.
  Reports missing/ambiguous devices instead of mixing readings from several glasses.
- **Local integration:** a private Unix socket, JSON events and an optional native
  IMU stream. No web server, dashboard or runtime interpreter is required.

XRealD reports events; your application chooses the action. It does not inject
keys, change volume, configure displays or override the glasses' own shortcuts.
Device writes, head tracking and Eye camera video are outside its scope.

## Install prebuilt binaries

Use our [Linux ARM64 bundle](releases/xreald-0.1.3-linux-arm64.tar.gz)
and its [checksum](releases/xreald-0.1.3-linux-arm64.tar.gz.sha256).
It contains both static binaries, the installer, matching source and licenses.
No Rust installation is needed on the target.

```sh
sha256sum -c xreald-0.1.3-linux-arm64.tar.gz.sha256 && \
tar -xzf xreald-0.1.3-linux-arm64.tar.gz && \
cd xreald-0.1.3-linux-arm64 && \
python3 -B install.py install && \
python3 -B install.py install --apply
```

Run as your normal user on Linux ARM64. The installer refuses to overwrite an
existing XRealD setup. Then follow [USB networking, permissions and startup](docs/installation.md).
For an existing installation, use the [upgrade steps](docs/installation.md#upgrade-from-012).

## Build from source

With Rust installed through [rustup](https://rust-lang.org/tools/install/):

```sh
rustup toolchain install 1.99.0 --profile minimal
rustup target add --toolchain 1.99.0 aarch64-unknown-linux-musl
RUSTUP_TOOLCHAIN=1.99.0 python3 -B scripts/build.py
python3 -B install.py install --bundle build/linux-arm64
python3 -B install.py install --bundle build/linux-arm64 --apply
```

Build on Linux or macOS; install on Linux ARM64. The build uses Rust's bundled
linker and locked Cargo dependencies, so no separate cross compiler is required.
[Build details and checks](docs/build.md).

## Use it

After [starting the service](docs/installation.md#start-and-stop):

```sh
export PATH="$HOME/.local/bin:$PATH"
xrealctl status
xrealctl capabilities
xrealctl get software.version
xrealctl watch gestures
xrealctl watch buttons
xrealctl watch wearing
xrealctl watch imu --rate 30 --seconds 10
```

Stop a continuous watch with **Ctrl+C**. Quick Button `single` is delayed 350 ms
to distinguish a double press. A `long` gesture requires a 600 ms hold and is
reported on release; use raw button events to react immediately on press.
The glasses' configured firmware actions still run alongside these events.

`watch wearing` first shows the current state, then each update: `worn`,
`not_worn` or `unknown`. `status` includes the same information. After starting
XRealD, take the glasses off and put them on if the state remains `unknown`.
Applications should pause automatic actions while the state is unknown.

The default IMU batching window is 8 ms to reduce CPU wakeups. Sensor history is
not saved. Commands print to the terminal; if you record a stream on an SD-based
device, use a bounded capture in `$XDG_RUNTIME_DIR`, which is RAM under systemd.
Service messages go to the system journal.

**Author:** Kostiantyn Andriiuk <konstantin@andriyuk.com>.
[Apache 2.0](LICENSE) for our code; [CC BY 4.0](LICENSES/CC-BY-4.0.txt) for our
documentation. Dependencies keep their licenses. An independent community
project, not an official XREAL product.
