#!/bin/sh
set -eu

cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ANDROID_JAR="$SDK/platforms/android-33/android.jar"
BUILD_TOOLS="$SDK/build-tools/36.0.0"
D8="$BUILD_TOOLS/d8"
JAVA_HOME="$(/usr/libexec/java_home -v 17)"
export JAVA_HOME PATH="$JAVA_HOME/bin:$PATH"
CORE="PlayerCore.java Capture.java"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/test" "$TMP/classes" build

# Without android.jar, so an android.* import in a core fails here.
javac --release 8 -Xlint:all -Xlint:-options -Werror -d "$TMP/test" $CORE test/*.java
java -cp "$TMP/test" Tests

javac -source 8 -target 8 -Xlint:all -Xlint:-options -Werror -bootclasspath "$ANDROID_JAR:$BUILD_TOOLS/core-lambda-stubs.jar" \
    -d "$TMP/classes" MaxHelper.java $CORE
"$D8" --min-api 28 --lib "$ANDROID_JAR" --output "$TMP" "$TMP"/classes/*.class
mv "$TMP/classes.dex" build/helper.dex
echo "$(pwd)/build/helper.dex"
