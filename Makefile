TARGET := aarch64-linux-android
# Android 9 on the station.
API := 28

NDK ?= $(firstword $(wildcard $(ANDROID_NDK_HOME) /opt/homebrew/share/android-ndk) $(lastword $(sort $(wildcard $(HOME)/Library/Android/sdk/ndk/*))))
NDK_HOST := $(if $(filter Darwin,$(shell uname -s)),darwin-x86_64,linux-x86_64)
NDK_BIN := $(NDK)/toolchains/llvm/prebuilt/$(NDK_HOST)/bin

# Homebrew's rustup is keg-only and not on PATH.
RUSTUP_BIN := $(wildcard /opt/homebrew/opt/rustup/bin)
export PATH := $(if $(RUSTUP_BIN),$(RUSTUP_BIN):)$(PATH)

export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER := $(NDK_BIN)/$(TARGET)$(API)-clang
export CC_aarch64_linux_android := $(NDK_BIN)/$(TARGET)$(API)-clang
export AR_aarch64_linux_android := $(NDK_BIN)/llvm-ar
export PULSAR_MAX_VERSION := $(shell git describe --tags --match 'v*' --always --dirty 2>/dev/null || echo unknown)
export PULSAR_MAX_BUILD_TIME := $(shell LC_ALL=C git log -1 --format=%cd --date=format:'%b %e %Y, %H:%M:%S' 2>/dev/null || echo unknown)

CARGO_ANDROID := cargo --locked
BIN := target/$(TARGET)/release/pulsar-max

.PHONY: all check fmt clippy test test-helper clean ndk

all: build/pulsar-max build/helper.dex

ndk:
	@test -x "$(CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER)" || { echo "NDK not found: set NDK=<path>"; exit 1; }

build/pulsar-max: ndk FORCE
	$(CARGO_ANDROID) build --release --target $(TARGET) -p pulsar-max
	@mkdir -p build
	$(NDK_BIN)/llvm-strip -o $@ $(BIN)
	cp $(BIN) build/pulsar-max.debug

build/helper.dex: android/helper/build.sh $(wildcard android/helper/*.java android/helper/test/*.java)
	android/helper/build.sh
	@mkdir -p build
	cp android/helper/build/helper.dex $@

check: fmt clippy test test-helper

fmt:
	cargo fmt --all --check

clippy: ndk
	$(CARGO_ANDROID) clippy --target $(TARGET) --all-targets -- -D warnings
	$(CARGO_ANDROID) clippy -p max-core -p max-wake --all-targets -- -D warnings

test:
	$(CARGO_ANDROID) test -p max-core -p max-wake

# build.sh runs the helper's host tests before it builds the dex.
test-helper:
	android/helper/build.sh

clean:
	cargo clean
	rm -rf build android/helper/build

FORCE:
