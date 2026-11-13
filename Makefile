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
export CXX_aarch64_linux_android := $(NDK_BIN)/$(TARGET)$(API)-clang++
export AR_aarch64_linux_android := $(NDK_BIN)/llvm-ar
export PULSAR_MAX_VERSION := $(shell git describe --tags --match 'v*' --always --dirty 2>/dev/null || echo unknown)
export PULSAR_MAX_BUILD_TIME := $(shell LC_ALL=C git log -1 --format=%cd --date=format:'%b %e %Y, %H:%M:%S' 2>/dev/null || echo unknown)

CARGO_ANDROID := cargo --locked
BIN := target/$(TARGET)/release/pulsar-max

# sendspin-cpp and its dependencies: pinned sources fetched once, static libraries built with the
# NDK's CMake toolchain and linked by third_party/pulsar-sendspin's build.rs.
SS_DIR := third_party/sendspin-cpp
SS_BUILD := build/sendspin-cpp
SS_STAMP := $(SS_BUILD)/.built
# fetch.sh skips existing checkouts: a copy of it marks which pins the sources were fetched for.
SS_PINS := $(SS_DIR)/src/.pins
SS_NDK := build/sendspin-cpp.ndk
NPROC := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
export PULSAR_SS_SRC := $(CURDIR)/$(SS_DIR)/src
export PULSAR_SS_BUILD := $(CURDIR)/$(SS_BUILD)
export PULSAR_REQUIRE_SS := 1
# Host builds of pulsar-sendspin compile its stub, without sendspin-cpp.
HOST_CARGO := env -u PULSAR_SS_SRC -u PULSAR_SS_BUILD -u PULSAR_REQUIRE_SS cargo --locked
HOST_CRATES := -p max-core -p max-wake -p pulsar-sendspin

.PHONY: all check fmt clippy test test-helper test-deploy clean distclean ndk

all: build/pulsar-max build/helper.dex

ndk:
	@test -x "$(CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER)" || { echo "NDK not found: set NDK=<path>"; exit 1; }

build/pulsar-max: ndk $(SS_STAMP) FORCE
	$(CARGO_ANDROID) build --release --target $(TARGET) -p pulsar-max
	@mkdir -p build
	$(NDK_BIN)/llvm-strip -o $@ $(BIN)
	cp $(BIN) build/pulsar-max.debug

build/helper.dex: android/helper/build.sh $(wildcard android/helper/*.java android/helper/test/*.java)
	android/helper/build.sh
	@mkdir -p build
	cp android/helper/build/helper.dex $@

$(SS_PINS): $(SS_DIR)/fetch.sh
	rm -rf $(SS_DIR)/src
	$(SS_DIR)/fetch.sh
	cp $< $@

$(SS_NDK): FORCE
	@mkdir -p $(dir $@)
	@printf '%s\n' '$(NDK)' '$(API)' "$$(grep Pkg.Revision '$(NDK)/source.properties' 2>/dev/null)" > $@.new
	@if cmp -s $@.new $@; then rm $@.new; else mv $@.new $@; fi

# Objects do not depend on the NDK or API: any change rebuilds from scratch.
$(SS_STAMP): $(SS_PINS) $(SS_DIR)/CMakeLists.txt $(SS_NDK) | ndk
	rm -rf $(SS_BUILD)
	cmake -S $(SS_DIR) -B $(SS_BUILD) -DCMAKE_BUILD_TYPE=Release \
	  -DCMAKE_TOOLCHAIN_FILE=$(NDK)/build/cmake/android.toolchain.cmake \
	  -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-$(API) -DANDROID_STL=c++_static \
	  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 >/dev/null
	env -u MAKEFLAGS -u MFLAGS cmake --build $(SS_BUILD) --parallel $(NPROC)
	touch $@

check: fmt clippy test test-helper test-deploy

fmt:
	cargo fmt --all --check

clippy: ndk $(SS_STAMP)
	$(CARGO_ANDROID) clippy --target $(TARGET) --all-targets -- -D warnings
	$(HOST_CARGO) clippy $(HOST_CRATES) --all-targets -- -D warnings

test:
	$(HOST_CARGO) test $(HOST_CRATES)

# build.sh runs the helper's host tests before it builds the dex.
test-helper:
	android/helper/build.sh

test-deploy:
	sh deploy/test.sh

clean:
	cargo clean
	rm -rf build android/helper/build

# Also forgets the fetched sendspin-cpp sources.
distclean: clean
	rm -rf $(SS_DIR)/src

FORCE:
