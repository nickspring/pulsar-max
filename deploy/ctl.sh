#!/system/bin/sh
# pulsar-max control on the station: enable|disable|restart|rollback|status|log [lines]
# For tools/install.sh: activate (new/ becomes bin/, then enable), health (verdict on the run it
# started: ok, wait or fail).
# PULSAR_MAX_DIR overrides the install dir (host tests).
D=${PULSAR_MAX_DIR:-/data/pulsar-max}
LOG=$D/pulsar-max.log
IOSDK=com.yandex.io.sdk
STOP_WAIT=10

is_supervisor() {
  if [ -r "/proc/$1/cmdline" ]; then
    # toybox tr spins on the read error of a process exiting under it; grep just fails.
    grep -qs supervise.sh "/proc/$1/cmdline"
  else
    ps -p "$1" -o args= 2>/dev/null | grep -q supervise.sh
  fi
}

# A supervisor killed by reboot leaves its pid behind; the number may now be another process.
supervisor_pid() {
  pid=$(cat "$D/supervise.pid" 2>/dev/null) || return 1
  if [ -n "$pid" ] && is_supervisor "$pid"; then
    echo "$pid"
    return 0
  fi
  rm -f "$D/supervise.pid"
  return 1
}

daemon_pid() { pidof pulsar-max; }

wait_gone() {
  n=0
  while [ $n -lt $STOP_WAIT ] && supervisor_pid >/dev/null; do
    sleep 1
    n=$((n + 1))
  done
  ! supervisor_pid >/dev/null
}

stop_all() {
  for pid in $(daemon_pid); do kill -TERM "$pid"; done
  if ! wait_gone; then
    pid=$(supervisor_pid) && kill -9 "$pid"
    for pid in $(daemon_pid); do kill -9 "$pid"; done
    rm -f "$D/supervise.pid"
  fi
}

# Unconditional: io.sdk started under a denial stays deaf after `allow` until it restarts.
give_back_mic() {
  cmd appops set $IOSDK RECORD_AUDIO allow
  for pid in $(pidof $IOSDK); do kill -9 "$pid"; done
}

start_supervisor() {
  supervisor_pid >/dev/null && return 0
  setsid sh "$D/supervise.sh" < /dev/null > /dev/null 2>&1 &
}

same_version() {
  cmp -s "$1/pulsar-max" "$2/pulsar-max" && cmp -s "$1/helper.dex" "$2/helper.dex"
}

case "$1" in
  enable)
    touch "$D/enabled"
    start_supervisor
    ;;
  disable)
    rm -f "$D/enabled"
    stop_all
    give_back_mic
    ;;
  restart)
    if [ ! -f "$D/enabled" ]; then
      echo "disabled: run $0 enable"
      exit 1
    fi
    for pid in $(daemon_pid); do kill -TERM "$pid"; done
    start_supervisor
    ;;
  rollback)
    if [ ! -x "$D/prev/pulsar-max" ]; then
      echo "no previous version in $D/prev"
      exit 1
    fi
    was_enabled=0
    [ -f "$D/enabled" ] && was_enabled=1
    rm -f "$D/enabled"
    stop_all
    rm -rf "$D/bad"
    mv "$D/bin" "$D/bad" && mv "$D/prev" "$D/bin"
    echo "=== ctl: rolled back to prev/, the replaced version is in bad/ $(date)" >> "$LOG"
    if [ $was_enabled = 1 ]; then
      touch "$D/enabled"
      start_supervisor
    else
      give_back_mic
    fi
    ;;
  activate)
    if [ ! -d "$D/new" ]; then
      echo "nothing uploaded to $D/new"
      exit 1
    fi
    rm -f "$D/enabled"
    stop_all
    if [ -d "$D/bin" ] && same_version "$D/bin" "$D/new"; then
      rm -rf "$D/new"
    elif [ -f "$D/bin/.healthy" ]; then
      rm -rf "$D/prev" && mv "$D/bin" "$D/prev" && mv "$D/new" "$D/bin"
    else
      rm -rf "$D/bin" && mv "$D/new" "$D/bin"
    fi
    echo "=== install $("$D/bin/pulsar-max" --version 2>&1) $(date)" >> "$LOG"
    touch "$D/enabled"
    start_supervisor
    ;;
  health)
    run=$(sed -n '/^=== install /h; /^=== install /!H; ${x;p;}' "$LOG" 2>/dev/null)
    case "$run" in
      "=== install "*) ;;
      *) echo "fail: no install mark in the log"; exit 1 ;;
    esac
    if echo "$run" | grep -q -E '^=== (exit|supervise)'; then
      echo "fail"
      exit 1
    fi
    starts=$(echo "$run" | grep -c '^=== start')
    if [ "$starts" = 1 ] && echo "$run" | grep -q 'running as'; then
      echo "ok"
    else
      echo "wait"
    fi
    ;;
  status)
    if [ -f "$D/enabled" ]; then echo "enabled"; else echo "disabled"; fi
    echo "version: $("$D/bin/pulsar-max" --version 2>/dev/null || echo none)"
    [ -x "$D/prev/pulsar-max" ] && echo "previous: $("$D/prev/pulsar-max" --version 2>/dev/null)"
    echo "supervisor: $(supervisor_pid || echo stopped)"
    echo "daemon: $(daemon_pid || echo stopped)"
    echo "io.sdk microphone: $(cmd appops get $IOSDK RECORD_AUDIO 2>/dev/null | cut -d';' -f1)"
    ;;
  log)
    tail -n "${2:-50}" "$LOG"
    ;;
  *)
    echo "usage: $0 enable|disable|restart|rollback|status|log [lines]"
    exit 2
    ;;
esac
