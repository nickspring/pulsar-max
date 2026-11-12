# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

pulsar-max is a Home Assistant voice satellite for the Yandex Station Max (`yandexstation_2`), an
Android 9 (YaOS) device. It reuses protocol crates of [pulsar](https://github.com/Morvex885/pulsar)
(Buildroot stations) and adds its own Android hardware layer and daemon. Station facts measured on
the device live in `docs/max.md`; target design and stages in `docs/architecture.md`.

## Commands

Homebrew `rustup` is keg-only; the Makefile puts `/opt/homebrew/opt/rustup/bin` on PATH, plain
`cargo` calls in a shell need it too. The NDK is auto-detected (`ANDROID_NDK_HOME`, Homebrew cask,
SDK); override with `make NDK=<path>`.

```sh
make                      # build/pulsar-max (stripped) + build/pulsar-max.debug, aarch64-linux-android API 28
make check                # cargo fmt --check, clippy -D warnings (Android target + max-core on host), tests
make test                 # cargo test -p max-core (host)
cargo --locked test -p max-core identity::tests::name_from_sysfs_line   # single test
probes/java/build.sh      # Java probe -> probes/java/build/classes.dex (JDK 17, SDK android-33, d8)
python3 -I probes/analyze_capture.py <file.raw> 16 [rate] [tone_hz] ["a-b,c-d"]   # per-channel levels
```

`cargo --locked` is used everywhere: commit `Cargo.lock` changes deliberately.

Running on the station (root ssh on port 2222; absshd has no scp/sftp, so files go through
`cat`). Everything stays in `/data/local/tmp/pulsar-max/` and is removed afterwards:

```sh
st() { ssh -p 2222 root@<station> "$@"; }; T=/data/local/tmp/pulsar-max
st "mkdir -p $T"
st "cat > $T/pulsar-max && chmod 755 $T/pulsar-max" < build/pulsar-max
st "$T/pulsar-max $T/data"                       # daemon with a throwaway data dir
st "cat > $T/probe.dex" < probes/java/build/classes.dex
st "CLASSPATH=$T/probe.dex app_process /system/bin MaxProbe record 8 $T/out.raw ffff 1 0.05"
st "cat $T/out.raw" > out.raw                    # binary only on this stream
st "rm -rf $T"
```

The capture probe fails while io.sdk records; take the mic first and give it back afterwards
(see Rules).

## Architecture

- **Workspace split.** `crates/max-core` holds all logic without IO and without pulsar crates, so
  it builds and is tested on the macOS host. `crates/pulsar-max` is the Android-only daemon (glue,
  IO, `sys.rs` FFI); it depends on `pulsar-lan`, which uses netlink and does not build off Linux.
  New logic goes into `max-core` with tests; the daemon stays thin.
- **pulsar as a dependency.** `pulsar-core`, `pulsar-esphome`, `pulsar-lan` come from the pulsar
  git tag in `Cargo.toml` (`[workspace.dependencies]`). External deps are pinned exactly to the
  versions of `satellite/Cargo.toml` of that tag; when bumping the tag, re-sync the pins.
  `pulsar-lan` is overridden by `[patch]` with `third_party/pulsar-lan` (Android build fix,
  `third_party/README.md`); keep that copy's diff minimal and drop it once upstream has the fix.
- **Execution model** follows pulsar's `docs/architecture.md`, "Модель исполнения": one tokio
  `current_thread` runtime with a `LocalSet`, services are actors spawned through
  `pulsar_core::actors` returning non-blocking `Clone` handles plus event receivers; blocking or
  realtime loops run on named `std::thread`s; `unsafe` and libc only in `sys.rs` with
  `// SAFETY:` (enforced by `#![deny(clippy::undocumented_unsafe_blocks)]`).
- **Audio path (planned, proven by probes).** The HAL is owned by `audioserver`; Yandex VQE runs
  there as a preprocessing effect for source `MIC`. Capture is Java `AudioRecord` with
  `setChannelIndexMask(0xffff)` from a root `app_process` (needs a fake `ActivityThread` /
  `opPackageName`, see `probes/java/MaxProbe.java`); AAudio caps at 8 channels on Android 9.
  Channel 1 is `vqe_0` (the ASR channel), 0 is the same ×30, 3-10 raw mics, 13-14 speaker
  reference (full table in `docs/max.md`).
- **Coexistence with the Yandex stack.** `com.yandex.io.sdk` cannot be disabled (protected
  package) or force-stopped (bound by system apps). The daemon takes the mic with
  `cmd appops set com.yandex.io.sdk RECORD_AUDIO ignore` + `kill` of its process; Android 9 allows
  only one capture at a time. io.sdk keeps drawing the clock into `fb4` and holds `fb5`, evdev
  devices and `/dev/lp_uart`.
- **Identity.** ESPHome name `yandex-max-` + last 6 hex digits of the `wlan0` MAC (same scheme
  as pulsar); the platform is checked via `ro.yndx.quasar.platform`, since the devicetree has no
  `yandex,*` compatible.

## Rules

- Code comments are English and explain only why (gotchas, device facts, links); docs (`README.md`,
  `docs/`) are Russian. Do not translate upstream text inside `third_party/`.
- Proprietary Yandex files (APK, `.so`, configs, recordings) never enter the repo; analysis copies
  live in the gitignored `dump/` of the sibling pulsar checkout or `/tmp`.
- No personal data in the repo: real IPs, MACs, device names with MAC suffix, serials, logs.
- Station safety: never touch the root method in `/odm` (`rootd`, `absshd`, `adbguard.sh`,
  `/odm/etc/init/*.rc`) or `/data/local/tmp` files that are not ours; `/system` and `/vendor` are
  dm-verity. Tests that take the mic from io.sdk must restore it (`RECORD_AUDIO allow` + `kill`),
  and test files go to `/data/local/tmp/pulsar-max/` and are removed afterwards. Sound tests stay at
  volume <= 0.2 unless the owner allows more.
- Live tests with the owner: one step at a time, every prompt in Russian, say exactly what to
  watch or say before running it.
