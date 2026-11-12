#!/bin/sh
set -eu

cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ANDROID_JAR="$SDK/platforms/android-33/android.jar"
D8="$SDK/build-tools/36.0.0/d8"
JAVA_HOME="$(/usr/libexec/java_home -v 17)"
export JAVA_HOME PATH="$JAVA_HOME/bin:$PATH"

rm -rf build
mkdir -p build/classes
javac -source 8 -target 8 -Xlint:-options -cp "$ANDROID_JAR" -d build/classes ./*.java
"$D8" --min-api 28 --lib "$ANDROID_JAR" --output build build/classes/*.class
echo "$(pwd)/build/classes.dex"
