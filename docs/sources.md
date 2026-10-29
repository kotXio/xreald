# Sources and licenses

[Back to XRealD](../README.md)

Original implementation and packaging:
**Kostiantyn Andriiuk <konstantin@andriyuk.com>**,
[kotXio/xreald](https://github.com/kotXio/xreald).
The wider Linux/XREAL setup is [Perideck](https://github.com/kotXio/Perideck).

## Protocol references

Our Rust implementation uses protocol facts and independently captured XREAL 1S
observations. No external driver code or manufacturer SDK is copied into `src/`.

| Reference | Author | Use |
| --- | --- | --- |
| [xr-tools network protocol, `65c1110`](https://github.com/0xcaff/xr-tools/tree/65c11105434a998f09ac4a5f8e4de8736bb4d36c/packages/xreal_one_driver/src/proto/net) | 0xcaff and contributors | Frame layout, known GETs and TCP button report format |
| [one-xr, `c7b0004`](https://github.com/Skarian/one-xr/tree/c7b000444cacc23090a71867e46cebc549205dd5) | Neil Skaria and contributors | One/Pro control protocol research |
| [ar-glass-lib IMU reference, `678c4d5`](https://github.com/taowen/ar-glass-lib/blob/678c4d552ef123022683a1ce6bdd9c7780c74017/library/src/main/cpp/xreal_one_imu_jni.cpp) | Tao Wen and contributors | Historical reference for sensor validity and interpretation; upstream is unavailable, so the original URL is retained for attribution |
| [Quick Button guide](https://tutorials.xreal.com/docs/glasses/one-series/osd/quick-button/) | XREAL | Firmware shortcut behavior; XRealD does not alter these bindings |
| [Linux input events](https://docs.kernel.org/input/event-codes.html) | Linux kernel contributors | EV_KEY and SYN_DROPPED handling |

The Quick Button timing fixture is an anonymized relative-time capture from a
1S running firmware `1.9.1.20260914175625`. It contains button timings, not device
identity or sensor recordings. Gesture recognition uses only Quick Button code 4.

Wearing detection uses independently observed XREAL 1S control events:
opcode `0x273d`, body `18 01` for worn and `18 02` for not worn.
Other payloads are treated as unknown.

## License scope

- Our code and examples: [Apache 2.0](../LICENSE), with [NOTICE](../NOTICE).
- Our documentation: [CC BY 4.0](../LICENSES/CC-BY-4.0.txt).
- Cargo dependencies: versions/checksums in [Cargo.lock](../Cargo.lock), original
  license texts in [LICENSES/dependencies](../LICENSES/dependencies/).
  The [dependency inventory](../LICENSES/dependencies.json) records authors,
  upstream repositories and declared licenses, including build-time crates.
- Static binary runtime: [Rust standard-library notices](../LICENSES/Rust-1.99.0-library.html)
  and [musl copyright/license](../LICENSES/musl-COPYRIGHT.txt).

The binaries are compiled by us from the supplied source. Their build manifest
and checksums are in [binaries/0.1.3-linux-arm64](../binaries/0.1.3-linux-arm64/README.md).
