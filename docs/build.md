# Build and package

[Back to XRealD](../README.md)

Use Rust **1.99.0**, Python **3.10+** and the locked Cargo dependencies.
The [README recipe](../README.md#build-from-source) builds static ARM64 Linux
binaries on a Linux or macOS host, using Rust's bundled `rust-lld`.

Output: `build/linux-arm64/{xreald,xrealctl,manifest.json,SHA256SUMS}`.
Use `--output` to choose a new directory, or `--offline` when dependencies are
already cached. Existing output directories are not replaced.
The manifest binds both binaries to the exact Cargo/source hashes; the installer
checks it for both installation paths.

## Local checks

```sh
rustup component add --toolchain 1.99.0 rustfmt clippy
RUSTUP_TOOLCHAIN=1.99.0 sh scripts/check
```

Tests use synthetic loopback peers and temporary Unix sockets. They do not open
glasses, microphones or desktop input devices. `cargo build --release` also
produces native host binaries under `target/release` for these tests.
macOS builds are development tools; USB discovery/input support targets Linux.

## Release bundle

For maintainers, copy the two binaries and manifest/checksums from a successful
build into `binaries/0.1.3-linux-arm64`, then run:

```sh
python3 -B scripts/package.py --output build/releases
```

The archive contains matching sources, installer, documentation, dependency
notices and the ARM64 binaries. Use a new output directory when packaging again.
