#!/bin/sh
# Host tests of supervise.sh, ctl.sh and pulsar-max.rc: a scripted fake daemon and stubs for the
# Android commands.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d)
trap 'cleanup_procs; rm -rf "$T"' EXIT
failed=0 passed=0
ORIG_PATH=$PATH

cleanup_procs() {
  for f in "$T"/*.pid "$T"/d/supervise.pid "$T"/fake/daemon.pid "$T"/fake/sleep.pid; do
    [ -f "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null
  done
  return 0
}

stub() {
  printf '#!/bin/sh\n%s\n' "$2" > "$T/stub/$1"
  chmod +x "$T/stub/$1"
}

# Plan lines: "<exit code> <seconds> [bytes to print]" per run, "stop" to end the loop.
fake_daemon() { # $1: dir, $2: variant
  mkdir -p "$1"
  echo "$2" > "$1/variant"
  : > "$1/helper.dex"
  cat > "$1/pulsar-max" <<'EOF'
#!/bin/sh
here=$(dirname "$0")
if [ "${1:-}" = --version ]; then echo "pulsar-max $(cat "$here/variant")"; exit 0; fi
echo $$ > "$FAKE/daemon.pid"
echo "$(cat "$here/variant")" >> "$FAKE/runs"
line=$(head -n 1 "$FAKE/plan")
tail -n +2 "$FAKE/plan" > "$FAKE/plan.tmp" && mv "$FAKE/plan.tmp" "$FAKE/plan"
set -- $line
if [ "${1:-stop}" = stop ]; then rm -f "$PULSAR_MAX_DIR/enabled"; exit 0; fi
echo "fake running as test"
[ -n "${3:-}" ] && head -c "$3" /dev/zero | tr '\0' x
trap 'echo term >> "$FAKE/signals"; kill $! 2>/dev/null; exit 0' TERM
if [ "$2" -gt 0 ]; then /bin/sleep "$2" & echo $! > "$FAKE/sleep.pid"; wait $!; fi
exit "$1"
EOF
  echo "# $2" >> "$1/pulsar-max"
  chmod +x "$1/pulsar-max"
}

setup() {
  cleanup_procs
  rm -rf "$T/d" "$T/stub" "$T/fake"
  mkdir -p "$T/d" "$T/stub" "$T/fake"
  export PULSAR_MAX_DIR="$T/d" FAKE="$T/fake" PULSAR_MAX_BACKOFF=0 PULSAR_MAX_MINUP=0
  export PULSAR_MAX_LONG=300 PULSAR_MAX_ROTATE=60
  unset PULSAR_MAX_MAXLOG
  export PATH="$T/stub:$ORIG_PATH"
  : > "$FAKE/runs"
  : > "$FAKE/signals"
  echo "RECORD_AUDIO: ignore" > "$FAKE/appops"
  ( /bin/sleep 300 & echo $! > "$T/iosdk.pid" )
  stub cmd 'case "$*" in
  "appops get"*) cat "$FAKE/appops" ;;
  "appops set"*) echo "RECORD_AUDIO: $5" > "$FAKE/appops" ;;
esac'
  stub pidof 'case "$1" in
  pulsar-max) f="$FAKE/daemon.pid" ;;
  com.yandex.io.sdk) f="'"$T"'/iosdk.pid" ;;
  *) exit 1 ;;
esac
p=$(cat "$f" 2>/dev/null) && kill -0 "$p" 2>/dev/null && echo "$p"'
  stub setsid '"$@"'
  cp "$HERE/supervise.sh" "$HERE/ctl.sh" "$T/d/"
}

plan() { printf '%s\n' "$@" > "$FAKE/plan"; }
runs() { tr '\n' ' ' < "$FAKE/runs" | sed 's/ $//'; }
supervise() { sh "$T/d/supervise.sh"; echo $?; }
ctl() { sh "$T/d/ctl.sh" "$@"; }
exists() { if [ -e "$1" ]; then echo yes; else echo no; fi; }
iosdk_alive() { if kill -0 "$(cat "$T/iosdk.pid")" 2>/dev/null; then echo yes; else echo no; fi; }

check() { # $1: name, $2: got, $3: want
  if [ "$2" = "$3" ]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL $1: got '$2', want '$3'"
  fi
}

wait_for() { # $1: shell condition, up to 5 s
  n=0
  while [ $n -lt 50 ] && ! eval "$1"; do /bin/sleep 0.1; n=$((n + 1)); done
}

setup
check "disabled: exit" "$(supervise)" 0
check "disabled: mic back" "$(cat "$FAKE/appops")" "RECORD_AUDIO: allow"
check "disabled: io.sdk restarted" "$(iosdk_alive)" no
check "disabled: no run" "$(runs)" ""

setup
echo "RECORD_AUDIO: allow" > "$FAKE/appops"
check "disabled, mic already back: exit" "$(supervise)" 0
check "disabled, mic already back: io.sdk untouched" "$(iosdk_alive)" yes

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 0" stop
check "crash loop: exit" "$(supervise)" 0
check "crash loop: rolled back" "$(runs)" "new new new new new old"
check "crash loop: bin is prev" "$(cat "$T/d/bin/variant")" old
check "crash loop: bad kept" "$(cat "$T/d/bad/variant")" new
check "crash loop: logged" "$(grep -c 'switched to prev/' "$T/d/pulsar-max.log")" 1

setup
fake_daemon "$T/d/bin" new
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 0" stop
check "no prev: gives up" "$(supervise)" 1
check "no prev: runs" "$(runs)" "new new new new new"
check "no prev: disabled" "$(exists "$T/d/enabled")" no
check "no prev: mic back" "$(cat "$FAKE/appops")" "RECORD_AUDIO: allow"
check "no prev: io.sdk restarted" "$(iosdk_alive)" no
check "no prev: logged" "$(grep -c 'no previous version' "$T/d/pulsar-max.log")" 1
check "no prev: pid file gone" "$(exists "$T/d/supervise.pid")" no

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 0" "1 0" "1 0" "1 0" "1 0" "1 0" stop
check "prev crash-loops too: gives up" "$(supervise)" 1
check "prev crash-loops too: runs" "$(runs)" "new new new new new old old old old old"
check "prev crash-loops too: logged" "$(grep -c 'also with prev/' "$T/d/pulsar-max.log")" 1

setup
unset PULSAR_MAX_BACKOFF
stub sleep '[ "$1" = "$PULSAR_MAX_ROTATE" ] && exec /bin/sleep 1
echo "$1" >> "$FAKE/sleeps"'
fake_daemon "$T/d/bin" new
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 0" stop
supervise > /dev/null
check "backoff doubles from 3 s" "$(tr '\n' ' ' < "$FAKE/sleeps")" "3 6 12 24 "

setup
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan stop
check "missing bin: exit" "$(supervise)" 0
check "missing bin: prev runs" "$(runs)" old

setup
fake_daemon "$T/d/bin" new
rm "$T/d/bin/helper.dex"
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan stop
supervise > /dev/null
check "bin without helper.dex: prev runs" "$(runs)" old

setup
mkdir -p "$T/d/bin"
touch "$T/d/enabled"
check "nothing usable: gives up" "$(supervise)" 1
check "nothing usable: logged" "$(grep -c 'no usable version' "$T/d/pulsar-max.log")" 1

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan "0 0" "0 0" "0 0" "0 0" "0 0" stop
check "clean exits: exit" "$(supervise)" 0
check "clean exits are not crashes" "$(runs)" "new new new new new new"

setup
export PULSAR_MAX_MINUP=5
fake_daemon "$T/d/bin" new
touch "$T/d/enabled"
plan "0 0" "0 0" "0 0" "0 0" "0 0" stop
check "quick clean exits count: gives up" "$(supervise)" 1

setup
export PULSAR_MAX_LONG=2
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 2" "1 0" "1 0" "1 0" "1 0" stop
check "long run resets: exit" "$(supervise)" 0
check "long run resets: no rollback" "$(runs)" "new new new new new new new new new new"
check "long run marks bin healthy" "$(exists "$T/d/bin/.healthy")" yes

setup
export PULSAR_MAX_LONG=2
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
touch "$T/d/enabled"
plan "1 0" "1 0" "1 0" "1 0" "1 0" "1 2" "1 0" "1 0" "1 0" "1 0" "1 0" stop
check "a long run after a rollback forgets it: gives up" "$(supervise)" 1
check "a long run after a rollback forgets it: why" \
  "$(grep -c 'no previous version' "$T/d/pulsar-max.log")" 1

setup
export PULSAR_MAX_MAXLOG=100 PULSAR_MAX_ROTATE=1
fake_daemon "$T/d/bin" new
touch "$T/d/enabled"
plan "0 4 500" stop
sh "$T/d/supervise.sh" &
wait_for '[ -f "$T/d/pulsar-max.log.1" ]'
check "the log is rotated during a long run" \
  "$(exists "$T/d/pulsar-max.log.1") $(pidof pulsar-max > /dev/null && echo running)" "yes running"
check "rotated log was cut" "$([ "$(wc -c < "$T/d/pulsar-max.log")" -lt 500 ] && echo yes)" yes
wait

setup
fake_daemon "$T/d/bin" new
touch "$T/d/enabled"
plan "0 30"
sh "$T/d/supervise.sh" &
wait_for '[ -n "$(runs)" ]'
first=$(cat "$T/d/supervise.pid")
check "a second supervisor leaves" "$(supervise)" 0
check "a second supervisor keeps the pid file" "$(cat "$T/d/supervise.pid")" "$first"
check "a second supervisor runs nothing" "$(runs)" new
check "a second supervisor keeps the mic" "$(cat "$FAKE/appops")" "RECORD_AUDIO: ignore"
kill -TERM "$first"
kill -TERM "$(cat "$FAKE/daemon.pid")"
wait_for '[ ! -f "$T/d/supervise.pid" ]'
check "TERM ends the supervisor" "$(exists "$T/d/supervise.pid")" no
check "TERM: no restart" "$(runs)" new

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
plan "0 30" "0 30"
ctl enable
wait_for '[ -n "$(runs)" ]'
check "enable: daemon runs" "$(runs)" new
check "enable: supervisor pid" "$(exists "$T/d/supervise.pid")" yes
ctl enable
/bin/sleep 0.3
check "enable twice: one daemon" "$(runs)" new
check "status" "$(ctl status | head -3 | tr '\n' '|')" "enabled|version: pulsar-max new|previous: pulsar-max old|"
ctl restart
wait_for '[ "$(runs)" = "new new" ]'
check "restart: TERM then a new run" "$(cat "$FAKE/signals") $(runs)" "term new new"
start=$(date +%s)
ctl disable
check "disable: by TERM, not the 10 s timeout" "$([ $(($(date +%s) - start)) -lt 5 ] && echo yes)" yes
check "disable: daemon got TERM" "$(grep -c term "$FAKE/signals")" 2
check "disable: flag gone" "$(exists "$T/d/enabled")" no
check "disable: supervisor gone" "$(exists "$T/d/supervise.pid")" no
check "disable: daemon stopped" "$(pidof pulsar-max || echo none)" none
check "disable: mic back" "$(cat "$FAKE/appops")" "RECORD_AUDIO: allow"
check "disable: io.sdk restarted" "$(iosdk_alive)" no

setup
echo "RECORD_AUDIO: allow" > "$FAKE/appops"
ctl disable
check "disable repairs a deaf io.sdk" "$(iosdk_alive)" no

setup
fake_daemon "$T/d/bin" new
( /bin/sleep 300 & echo $! > "$T/victim.pid" )
cat "$T/victim.pid" > "$T/d/supervise.pid"
plan "0 30"
ctl enable
wait_for '[ -n "$(runs)" ]'
check "stale pid: enable starts" "$(runs)" new
ctl disable
check "stale pid: unrelated process survives" \
  "$(kill -0 "$(cat "$T/victim.pid")" 2>/dev/null && echo yes)" yes

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
check "restart while disabled" "$(ctl restart > /dev/null; echo $?)" 1
plan "0 30" "0 30"
touch "$T/d/enabled"
ctl rollback
wait_for '[ -n "$(runs)" ]'
check "rollback: prev runs" "$(runs)" old
check "rollback: bad kept" "$(cat "$T/d/bad/variant")" new
check "rollback: still enabled" "$(exists "$T/d/enabled")" yes
ctl disable
check "rollback without prev" "$(ctl rollback > /dev/null; echo $?)" 1

setup
fake_daemon "$T/d/bin" new
fake_daemon "$T/d/prev" old
ctl rollback
check "rollback while disabled: stays off" "$(exists "$T/d/enabled") $(runs)" "no "
check "rollback while disabled: mic back" "$(cat "$FAKE/appops")" "RECORD_AUDIO: allow"
check "usage" "$(ctl bogus > /dev/null; echo $?)" 2

setup
check "activate: needs new/" "$(ctl activate > /dev/null; echo $?)" 1
check "health: no install yet" "$(ctl health)" "fail: no install mark in the log"
fake_daemon "$T/d/new" v1
plan "0 30" "0 30"
ctl activate
wait_for '[ "$(ctl health)" = ok ]'
check "activate: first install" "$(cat "$T/d/bin/variant") $(exists "$T/d/prev") $(runs)" "v1 no v1"
check "health: ok" "$(ctl health)" ok
check "activate: install mark" "$(grep -c '^=== install pulsar-max v1' "$T/d/pulsar-max.log")" 1

fake_daemon "$T/d/new" v1
ctl activate
wait_for '[ "$(ctl health)" = ok ]'
check "activate: same version keeps bin and prev" \
  "$(cat "$T/d/bin/variant") $(exists "$T/d/prev") $(exists "$T/d/new")" "v1 no no"

fake_daemon "$T/d/new" v2
plan "0 30" "0 30"
ctl activate
wait_for '[ "$(ctl health)" = ok ]'
check "activate: an unproven bin is replaced, prev kept" \
  "$(cat "$T/d/bin/variant") $(exists "$T/d/prev")" "v2 no"

touch "$T/d/bin/.healthy"
fake_daemon "$T/d/new" v3
plan "1 0" "0 30"
ctl activate
wait_for '[ "$(ctl health)" != wait ]'
check "activate: a healthy bin becomes prev" "$(cat "$T/d/prev/variant") $(cat "$T/d/bin/variant")" "v2 v3"
check "health: a crash fails" "$(ctl health)" fail
ctl disable

setup
echo "=== install pulsar-max v1 now" > "$T/d/pulsar-max.log"
check "health: before the start" "$(ctl health)" wait
printf '=== start\nfake running as test\n=== exit 0 after 0s\n=== start\n' >> "$T/d/pulsar-max.log"
check "health: a restart fails" "$(ctl health)" fail
printf '=== install pulsar-max v2 now\n=== start\n=== start\nrunning as\n' >> "$T/d/pulsar-max.log"
check "health: only the last install counts, two starts wait" "$(ctl health)" wait

rc_service=$(grep '^service ' "$HERE/pulsar-max.rc")
check "rc: no \$ for init to expand" "$(grep -v '^#' "$HERE/pulsar-max.rc" | grep -c '\$')" 0
check "rc: runs the supervisor only when readable" \
  "$(echo "$rc_service" | grep -c '\[ -r /data/pulsar-max/supervise.sh \] && exec')" 1
check "rc: gives the mic back and restarts io.sdk" \
  "$(echo "$rc_service" | grep -c 'RECORD_AUDIO allow; am crash com.yandex.io.sdk"$')" 1

echo "$passed passed, $failed failed"
[ $failed = 0 ]
