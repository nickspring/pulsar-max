#!/bin/sh
# Install, update or remove pulsar-max on a Station Max over root ssh (docs/architecture.md,
# "Автозапуск").
#   tools/install.sh <station>                    install or update from build/ (run make first)
#   tools/install.sh --uninstall <station>        back to stock; keeps /data/pulsar-max/api.key
#   tools/install.sh --uninstall --purge <station>
# PORT (default 2222) and SSH_OPTS are passed to ssh.
set -eu

SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
usage() { sed -n '2,7p' "$SELF" >&2; exit 2; }
cd "$(dirname "$SELF")/.."
PORT=${PORT:-2222}
D=/data/pulsar-max
RC=/odm/etc/init/pulsar-max.rc
RC_LABEL=u:object_r:vendor_configs_file:s0
PLATFORM=yandexstation_2
# Long enough for the supervisor's first rollback (3+6+12+24 s of backoff) to show in the log.
SETTLE=50

mode=install purge=0
while [ $# -gt 1 ]; do
  case "$1" in
    --uninstall) mode=uninstall ;;
    --purge) purge=1 ;;
    *) usage ;;
  esac
  shift
done
[ $# -eq 1 ] || usage
[ $purge = 0 ] || [ $mode = uninstall ] || { echo "--purge needs --uninstall" >&2; exit 2; }
HOST=$1

st() { ssh -o ConnectTimeout=10 ${SSH_OPTS:-} -p "$PORT" "root@$HOST" "$@"; }

preflight() {
  platform=$(st getprop ro.yndx.quasar.platform)
  [ "$platform" = $PLATFORM ] || { echo "not a Station Max: platform '$platform'" >&2; exit 1; }
  [ "$(st id -u)" = 0 ] || { echo "ssh does not give root" >&2; exit 1; }
}

# The only write to /odm. No stdin while it is rw, and the read-only check runs whatever happened.
odm_rc() {
  failed=0
  st "set -e; mount -o rw,remount /odm; trap 'mount -o ro,remount /odm' EXIT HUP INT TERM PIPE; $1" \
    < /dev/null || failed=1
  if ! st "grep -q ' /odm [^ ]* ro[, ]' /proc/mounts"; then
    echo "/odm did not go back to read-only: run 'mount -o ro,remount /odm' on the station" >&2
    exit 1
  fi
  [ $failed = 0 ] || { echo "writing $RC failed" >&2; exit 1; }
}

health() { st "sh $D/ctl.sh health" || true; }

install() {
  for f in build/pulsar-max build/helper.dex; do
    [ -f "$f" ] || { echo "$f missing: run make" >&2; exit 1; }
  done
  preflight
  echo "uploading"
  st "rm -rf $D/new && mkdir -p $D/new"
  st "cat > $D/new/pulsar-max && chmod 755 $D/new/pulsar-max" < build/pulsar-max
  st "cat > $D/new/helper.dex" < build/helper.dex
  version=$(st "$D/new/pulsar-max --version" 2>&1) || version=
  case "$version" in
    "pulsar-max "*) echo "new: $version" ;;
    *) st "rm -rf $D/new"; echo "the uploaded binary does not run: '$version'" >&2; exit 1 ;;
  esac
  for f in supervise.sh ctl.sh; do
    st "cat > $D/$f.new && chmod 755 $D/$f.new && mv $D/$f.new $D/$f" < "deploy/$f"
  done
  st "cat > $D/pulsar-max.rc.new" < deploy/pulsar-max.rc
  if ! st "cmp -s $D/pulsar-max.rc.new $RC"; then
    echo "writing $RC"
    odm_rc "cp $D/pulsar-max.rc.new $RC; chown 0:0 $RC; chmod 644 $RC; chcon $RC_LABEL $RC"
  fi
  st "rm -f $D/pulsar-max.rc.new"

  st "sh $D/ctl.sh activate"
  started=$(date +%s)
  echo "waiting for the daemon"
  verdict=wait
  while [ "$verdict" = wait ] && [ $(($(date +%s) - started)) -lt 30 ]; do
    sleep 2
    verdict=$(health)
  done
  if [ "$verdict" = ok ]; then
    left=$((SETTLE - ($(date +%s) - started)))
    [ $left -le 0 ] || sleep $left
    verdict=$(health)
  fi
  st "sh $D/ctl.sh status"
  if [ "$verdict" != ok ]; then
    st "sh $D/ctl.sh log 30"
    echo "the daemon is not healthy ($verdict). The supervisor rolls back to prev/ by itself;" \
      "do not re-run the install, check 'ctl.sh log'." >&2
    exit 1
  fi
}

uninstall() {
  preflight
  st "if [ -f $D/ctl.sh ]; then sh $D/ctl.sh disable; fi"
  if st "test -e $RC"; then
    echo "removing $RC"
    odm_rc "rm -f $RC"
  fi
  if [ $purge = 1 ]; then
    st "rm -rf $D"
  else
    st "cd $D 2>/dev/null || exit 0; for f in *; do [ \"\$f\" = api.key ] || rm -rf \"\$f\"; done"
  fi
  echo "removed; Alice has the microphone again"
}

$mode
