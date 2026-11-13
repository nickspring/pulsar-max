# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

pulsar-max is a Home Assistant voice satellite for the Yandex Station Max (`yandexstation_2`), an
Android 9 (YaOS) device. It reuses protocol crates of [pulsar](https://github.com/Morvex885/pulsar)
(Buildroot stations) and adds its own Android hardware layer and daemon. Station facts measured on
the device live in `docs/max.md`; target design and stages in `docs/architecture.md`.

## Commands

Homebrew `rustup` is keg-only; the Makefile puts `/opt/homebrew/opt/rustup/bin` on PATH, plain
`cargo` calls in a shell need it too. The NDK is auto-detected (`ANDROID_NDK_HOME`, Homebrew cask,
SDK); override with `make NDK=<path>`. The first `make` also needs `cmake` and network: it fetches
the pinned sendspin-cpp sources into `third_party/sendspin-cpp/src/` and builds them with the NDK's
CMake toolchain into `build/sendspin-cpp/` (`make distclean` forgets the sources).

```sh
make                      # build/pulsar-max (+ .debug, aarch64-linux-android API 28) and build/helper.dex
                          # (helpers need JDK 17 and the Android SDK: platforms/android-33, build-tools/36.0.0)
make check                # cargo fmt --check, clippy -D warnings (Android target + host crates), tests
make test                 # cargo test -p max-core -p max-wake -p pulsar-sendspin (host; Sendspin as a stub)
make test-deploy          # deploy/test.sh: supervise.sh and ctl.sh against a fake daemon (host)
tools/install.sh <station>               # install/update from build/ (writes /odm/etc/init/pulsar-max.rc once)
tools/install.sh --uninstall <station>   # back to stock, keeps api.key and sendspin/ (--purge removes them)
cargo --locked test -p max-core identity::tests::name_from_sysfs_line   # single test
probes/java/build.sh      # Java probe -> probes/java/build/classes.dex (JDK 17, SDK android-33, d8)
probes/c/build.sh         # C probes (NDK clang) -> probes/c/build/; evgrab <dev> <sec> tests EVIOCGRAB,
                          # tflite_check <lib> <model> loads a model the way io.sdk does and times it,
                          # aaudio_check [sec] [low] plays silence and prints AAudio timestamps/latency
python3 -I probes/analyze_capture.py <file.raw> 16 [rate] [tone_hz] ["a-b,c-d"]   # per-channel levels
```

Spotter references (`probes/spotter/`) need a venv with `numpy` and `ai-edge-litert`:
`python3 -m venv /tmp/litert-venv && /tmp/litert-venv/bin/pip install numpy ai-edge-litert`.
`golden_signal.py` regenerates `crates/max-wake/tests/fixtures/`, `chen14_ref.py` prints the
golden chen14 cases; `proto.py <capture.raw> <model.tflite>` runs the whole spotter on a recording
with a local copy of the station's model (kept in `dump/`, never in the repo). For the command
spotter, `golden_command.py` regenerates its synthetic fixtures (a tiny random nnet3 in the stock
format) and `cmd_ref.py <model_dir> <vqe16k.raw>` (or `--capture <48k 16ch.raw>`) is the reference;
`MAX_CMD_MODEL=<dir> MAX_CMD_RAW=<16k.raw> cargo --locked test --release -p max-wake --test
command_model -- --ignored --nocapture` runs the Rust one on a recording.
`probes/ledpattern/golden_led.py` regenerates `crates/max-core/tests/fixtures/ledpattern.txt` from
`ledref.py` (synthetic patterns only; stock `.led` files stay out of the repo).

`cargo --locked` is used everywhere: commit `Cargo.lock` changes deliberately.

Running a test build on the station (root ssh on port 2222; absshd has no scp/sftp, so files go
through `cat`). If pulsar-max is installed, `sh /data/pulsar-max/ctl.sh disable` first and
`enable` afterwards: only one daemon may hold the mic and the API port. Everything stays in
`/data/local/tmp/pulsar-max/` and is removed afterwards. The daemon takes the mic from io.sdk at
start and gives it back on SIGTERM/SIGINT/SIGHUP/SIGQUIT or HA Restart; run it with `ssh -t` so
Ctrl-C arrives, and always end with the restore line, which is harmless when nothing was taken:

```sh
st() { ssh -p 2222 root@<station> "$@"; }; T=/data/local/tmp/pulsar-max
st "mkdir -p $T"
st "cat > $T/pulsar-max && chmod 755 $T/pulsar-max" < build/pulsar-max
st "cat > $T/helper.dex" < build/helper.dex     # found next to the binary
ssh -t -p 2222 root@<station> "$T/pulsar-max $T/data"   # Ctrl-C to stop
st "cat > $T/rec.raw" < rec.raw; st "$T/pulsar-max --spot $T/rec.raw"   # wake word on a recording
st "cat > $T/probe.dex" < probes/java/build/classes.dex
st "CLASSPATH=$T/probe.dex app_process /system/bin MaxProbe record 8 $T/out.raw ffff 1 0.05"
st "cat $T/out.raw" > out.raw                    # binary only on this stream
st 'cmd appops set com.yandex.io.sdk RECORD_AUDIO allow; kill $(pidof com.yandex.io.sdk)'
st "rm -rf $T"
```

The capture probe fails while io.sdk records; take the mic first (`RECORD_AUDIO ignore` + kill)
and restore it as above.

## Architecture

- **Workspace split.** `crates/max-core` (dialog, audio, helper protocol, media) and
  `crates/max-wake` (spotter) hold all logic without IO and without pulsar crates, so they build
  and are tested on the macOS host. `crates/pulsar-max` is the Android-only daemon (glue, IO,
  `sys.rs`/`tflite.rs`/`aaudio.rs` FFI); it depends on `pulsar-lan`, which uses netlink and does
  not build off Linux. New logic goes into the host crates with tests; the daemon stays thin.
- **pulsar as a dependency.** `pulsar-core`, `pulsar-esphome`, `pulsar-lan`, `pulsar-sendspin`
  come from the pulsar git tag in `Cargo.toml` (`[workspace.dependencies]`). External deps are
  pinned exactly to the versions of `satellite/Cargo.toml` of that tag; when bumping the tag,
  re-sync the pins. `pulsar-lan` and `pulsar-sendspin` are overridden by `[patch]` with copies in
  `third_party/` (Android build fixes; Sendspin's ALSA sink replaced by the `AudioOutput` trait,
  `third_party/README.md`); keep their diffs minimal and drop them once upstream has the changes.
  `pulsar-sendspin` is a workspace member so its host tests run (without sendspin-cpp).
- **Sendspin.** `max-core/src/music_out.rs` is the player's output logic (pulsar's sink contract,
  finish time from AAudio timestamps, gain ramp) over a `Device` trait, host-tested with a fake;
  `crates/pulsar-max/src/{aaudio,sendspin}.rs` are the AAudio FFI and the glue.
- **Execution model** follows pulsar's `docs/architecture.md`, "Модель исполнения": one tokio
  `current_thread` runtime with a `LocalSet`, services are actors spawned through
  `pulsar_core::actors` returning non-blocking `Clone` handles plus event receivers; blocking or
  realtime loops run on named `std::thread`s; `unsafe` and libc only in `sys.rs`/`tflite.rs`/
  `aaudio.rs` with `// SAFETY:` (enforced by `#![deny(clippy::undocumented_unsafe_blocks)]`).
- **Helpers.** The HAL is owned by `audioserver`; Yandex VQE runs there as a preprocessing
  effect for source `MIC`. `android/helper/MaxHelper.java` runs as root under `app_process` in two
  processes: `capture` (`AudioRecord` with `setChannelIndexMask(0xffff)`, needs a fake
  `ActivityThread`/`opPackageName`; streams channels 1,2,3 = `vqe_0`, `vqe_bnr`, raw mic) and
  `player` (`MediaPlayer` per channel, `STREAM_MUSIC` through reflected `IAudioService`). The line protocol is in
  docs/architecture.md, "Протокол помощников"; its Rust side is `max-core/src/player_proto.rs`.
  `crates/pulsar-max/src/{capture,player}.rs` spawn and restart them; `max-core` decimates
  48 -> 16 kHz and keeps the exactly-one-`done`-per-play bookkeeping (`inflight.rs`).
- **Dialog.** `max-core/src/dialog.rs` is pulsar's voice logic as a pure state machine over the
  `Effects` trait; `crates/pulsar-max/src/controller.rs` implements `Effects` over pulsar-esphome
  and the player. Runs start from HA (`assist_satellite.start_conversation`, announcements);
  pulsar-esphome hardcodes flags=0, so HA-side wake word is not available. The action button
  starts runs too. Earcons and the HA timer ring play io.sdk's own sound files in place
  (`Sounds`, built in main.rs), the timer falling back to a generated `timer_alert.wav`.
- **Wake word.** `crates/max-wake` is io.sdk's own "Алиса" spotter reimplemented (fbank,
  chen14 decision, anti-ad frequency filter; docs/max.md, "Ключевое слово"), host-tested against
  the Python references in `probes/spotter/`. On the station the model and `flags.txt` are read in
  place from io.sdk's `spotter.json` catalog and run by io.sdk's `libtflite.so`, dlopened straight
  from its APK (`crates/pulsar-max/src/tflite.rs`). The capture thread tees positioned 16 kHz
  `vqe_0`/`vqe_bnr`/raw-mic chunks to the wake thread (`wake.rs`, bounded `try_send`); a hit
  carries the stream position, and the controller sends the `Preroll` after it, then live audio.
- **Voice commands.** The same thread runs io.sdk's command spotter (`max-wake` `nnet.rs`,
  `chen14_multi.rs`, `command.rs`: a Kaldi nnet3 Q16 model run in f32, multi-word chen14;
  docs/max.md, "Команды стока") on `vqe_0`, model read in place (`command` in `spotter.json`).
  pulsar's command rules are `max-core/src/commands.rs` and `Dialog::command`; `music_ops` turns
  a `MusicAction` (`Effects::music`) into player, Sendspin and volume operations that the controller
  only executes.
- **Ring.** `max-core/src/ledpattern.rs` plays io.sdk's own `.led` patterns (format and player
  reverse-engineered, references in `probes/ledpattern/`), read in place from io.sdk's
  `ledpatterns/` dir; `max-core/src/ring.rs` maps the dialog's `Ui` states to them as stock does and
  schedules output (`Output::step`); `crates/pulsar-max/src/ring.rs` only writes fb5 (bytes B, G, R)
  and waits. io.sdk still draws the knob volume on the ring: the daemon reads the knob ungrabbed
  and leaves the ring to io.sdk for 1.2 s.
- **Buttons.** `crates/pulsar-max/src/input.rs` EVIOCGRABs `gpio_keypad` (io.sdk would answer the
  action button too); `max-core/src/input.rs` decodes events and classifies presses (pulsar-hw
  timing; presses are timed in the reader thread). The knob stays with io.sdk, which drives
  `STREAM_MUSIC` itself. evgrab fails with EBUSY while the daemon holds the grab.
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
- Station safety: never touch the root method in `/odm` (`rootd`, `absshd`, `adbguard.sh`, its
  `/odm/etc/init/*.rc`) or `/data/local/tmp` files that are not ours; `/system` and `/vendor` are
  dm-verity. Our only file in `/odm` is `/odm/etc/init/pulsar-max.rc`, written and removed only by
  `tools/install.sh`. Tests that take the mic from io.sdk must restore it (`RECORD_AUDIO allow` + `kill`),
  and test files go to `/data/local/tmp/pulsar-max/` and are removed afterwards. Sound tests stay at
  volume <= 0.2 unless the owner allows more.
- Live tests with the owner: one step at a time, every prompt in Russian, say exactly what to
  watch or say before running it.
