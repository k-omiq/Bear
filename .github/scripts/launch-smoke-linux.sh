#!/usr/bin/env bash
# Launch the installed app once under a virtual display and let it check itself
# (src-tauri/src/smoke.rs). Writes the app's JSON report and a screenshot of the
# display into OUT_DIR, prints the report, and exits with the app's exit code.
#
#   launch-smoke-linux.sh OUT_DIR COMMAND [ARGS...]
#
# Needs Xvfb, dbus-run-session and ImageMagick's `import` on PATH.
set -euo pipefail

out=$(realpath -m "$1")
shift
mkdir -p "$out"
report="$out/report.json"
rm -f "$report"

Xvfb :99 -screen 0 1600x1000x24 -nolisten tcp >"$out/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
export DISPLAY=:99
for _ in $(seq 50); do
  [ -e /tmp/.X11-unix/X99 ] && break
  sleep 0.1
done

export MANGA_CLEANER_SMOKE_REPORT="$report"
export MANGA_CLEANER_SMOKE_RUNTIME="${MANGA_CLEANER_SMOKE_RUNTIME:-1}"
dbus-run-session -- "$@" >"$out/app.log" 2>&1 &
app=$!

# The app holds its window for five seconds after writing the report: long
# enough to photograph what the check saw. Its own deadline is 17 minutes.
for _ in $(seq 1200); do
  if [ -e "$report" ]; then
    import -window root "$out/screenshot.png" || echo "screenshot failed" >&2
    break
  fi
  kill -0 "$app" 2>/dev/null || break
  sleep 1
done

status=0
wait "$app" || status=$?
echo "app exit code: $status"
if [ -e "$report" ]; then
  cat "$report"
else
  echo "no report was written; last lines of the app log:" >&2
  tail -n 50 "$out/app.log" >&2 || true
  [ "$status" -ne 0 ] || status=4
fi
exit "$status"
