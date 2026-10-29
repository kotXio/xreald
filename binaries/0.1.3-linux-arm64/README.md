# XRealD 0.1.3 — Linux ARM64

Our static, stripped builds of `xreald` and `xrealctl`, compiled from the matching
source in this repository with Rust 1.99.0 and target `aarch64-unknown-linux-musl`.

- [Install prebuilt binaries](../../README.md#install-prebuilt-binaries)
- [Build from source](../../README.md#build-from-source)
- [Build/source manifest](manifest.json)
- [SHA-256 checksums](SHA256SUMS)
- [Dependency licenses and notices](../../docs/sources.md#license-scope)

Both files are Linux ARM64 ELF executables without a dynamic interpreter.
Rust and shared application libraries are not required on the target.
No vendor executable or firmware is included.
