#!/bin/sh
set -eu

cd "$(dirname "$0")"
NDK="${NDK:-${ANDROID_NDK_HOME:-/opt/homebrew/share/android-ndk}}"
case "$(uname -s)" in
Darwin) HOST=darwin-x86_64 ;;
*) HOST=linux-x86_64 ;;
esac
CC="$NDK/toolchains/llvm/prebuilt/$HOST/bin/aarch64-linux-android28-clang"

mkdir -p build
for src in ./*.c; do
    name="$(basename "$src" .c)"
    "$CC" -O2 -Wall -Wextra -Werror -o "build/$name" "$src"
    echo "$(pwd)/build/$name"
done
