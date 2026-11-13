#!/system/bin/sh
# Keeps pulsar-max running while $D/enabled exists; started by init (pulsar-max.rc) or ctl.sh.
# Log: $D/pulsar-max.log, rotated at 1 MB (checked every minute), one .1 kept.
#
# pulsar's supervise.sh rules: exit 0 (SIGTERM, HA's Restart) restarts after 1 s and is not a crash
# unless the run was shorter than MINUP s; anything else is a crash. CRASHES short crashes in a
# row (runs < LONG s) swap prev/ in for bin/ (bin/ is kept as bad/); if prev/ crash-loops too,
# `enabled` goes and the microphone goes back to io.sdk, so the station is not left deaf.
# A run of LONG s marks bin/ healthy: tools/install.sh moves only a healthy bin/ to prev/.
#
# PULSAR_MAX_DIR, _LONG, _MINUP, _BACKOFF, _MAXLOG, _ROTATE override device values (host tests).
D=${PULSAR_MAX_DIR:-/data/pulsar-max}
LOG=$D/pulsar-max.log
IOSDK=com.yandex.io.sdk
MAXLOG=${PULSAR_MAX_MAXLOG:-1000000}
ROTATE=${PULSAR_MAX_ROTATE:-60}
LONG=${PULSAR_MAX_LONG:-300}
CRASHES=5
MINUP=${PULSAR_MAX_MINUP:-5}
BACKOFF_MIN=${PULSAR_MAX_BACKOFF:-3}
BACKOFF_MAX=30

# init starts a second copy when system_server restarts; its daemon could not bind and would "crash".
is_supervisor() {
  if [ -r "/proc/$1/cmdline" ]; then
    # toybox tr spins on the read error of a process exiting under it; grep just fails.
    grep -qs supervise.sh "/proc/$1/cmdline"
  else
    ps -p "$1" -o args= 2>/dev/null | grep -q supervise.sh
  fi
}

other=$(cat "$D/supervise.pid" 2>/dev/null)
if [ -n "$other" ] && [ "$other" != $$ ] && is_supervisor "$other"; then
  exit 0
fi

uptime_s() {
  if [ -r /proc/uptime ]; then
    read -r u _ < /proc/uptime
    echo "${u%%.*}"
  else
    date +%s
  fi
}

# No `&&`: on a full /data the copy fails and the log must still shrink.
rotate() {
  if [ -f "$LOG" ] && [ "$(wc -c < "$LOG")" -gt "$MAXLOG" ]; then
    cp "$LOG" "$LOG.1"
    : > "$LOG"
  fi
}

say() { echo "=== supervise: $* $(date)" >> "$LOG"; }

# io.sdk started without the permission does not ask again after a grant, only a restart helps.
# An unreadable state counts as revoked: a spare restart is harmless, a deaf io.sdk is not.
give_back_mic() {
  had=no
  dumpsys package $IOSDK 2>/dev/null | grep -q 'RECORD_AUDIO: granted=true' && had=yes
  pm grant $IOSDK android.permission.RECORD_AUDIO
  case "$had $(cmd appops get $IOSDK RECORD_AUDIO 2>/dev/null)" in
    yes*allow*) return 0 ;;
  esac
  cmd appops set $IOSDK RECORD_AUDIO allow
  for pid in $(pidof $IOSDK); do kill -9 "$pid"; done
}

finish() {
  kill "$ROT" 2>/dev/null
  rm -f "$D/supervise.pid"
  exit "$1"
}

give_up() {
  rm -f "$D/enabled"
  give_back_mic
  say "$1; disabled, the microphone is back with $IOSDK"
  finish 1
}

usable() { [ -x "$1/pulsar-max" ] && [ -f "$1/helper.dex" ]; }

swap_in_prev() {
  rm -rf "$D/bad"
  mv "$D/bin" "$D/bad" 2>/dev/null
  mv "$D/prev" "$D/bin"
}

echo $$ > "$D/supervise.pid"
( while sleep "$ROTATE"; do rotate; done ) > /dev/null 2>&1 &
ROT=$!
trap 'finish 143' TERM INT HUP

fails=0 swapped=0 backoff=$BACKOFF_MIN
while [ -f "$D/enabled" ]; do
  rotate
  if ! usable "$D/bin"; then
    say "$D/bin has no usable pulsar-max and helper.dex"
    if [ $swapped = 0 ] && usable "$D/prev"; then
      swap_in_prev && swapped=1 && fails=0 && say "switched to prev/" && continue
    fi
    give_up "no usable version"
  fi
  echo "=== start $(date)" >> "$LOG"
  t0=$(uptime_s)
  "$D/bin/pulsar-max" "$D" >> "$LOG" 2>&1
  rc=$?
  up=$(($(uptime_s) - t0))
  echo "=== exit $rc after ${up}s $(date)" >> "$LOG"

  if [ $up -ge "$LONG" ]; then
    touch "$D/bin/.healthy"
    fails=0 swapped=0 backoff=$BACKOFF_MIN
    if [ $rc -eq 0 ]; then sleep 1; else sleep "$backoff"; fi
    continue
  fi
  if [ $rc -eq 0 ] && [ $up -ge "$MINUP" ]; then
    sleep 1
    continue
  fi
  fails=$((fails + 1))
  if [ $fails -ge $CRASHES ]; then
    if [ $swapped = 0 ] && usable "$D/prev"; then
      swap_in_prev
      swapped=1 fails=0 backoff=$BACKOFF_MIN
      say "$CRASHES short crashes in a row, switched to prev/ (the crashing version is kept as bad/)"
      continue
    fi
    if [ $swapped = 1 ]; then
      give_up "$CRASHES short crashes in a row, also with prev/"
    fi
    give_up "$CRASHES short crashes in a row, no previous version to fall back to"
  fi
  sleep "$backoff"
  backoff=$((backoff * 2))
  [ $backoff -gt $BACKOFF_MAX ] && backoff=$BACKOFF_MAX
done
give_back_mic
finish 0
