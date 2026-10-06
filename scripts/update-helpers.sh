#!/usr/bin/env bash
# Bring the desktop helper binaries in src-tauri/binaries up to date with this
# checkout, and optionally put them into an installed local build.
#
#   npm run helpers                 # rebuild what is out of date
#   npm run helpers -- --force      # rebuild the cloud helper even when current
#   npm run helpers:install         # also update /Applications/Manga Cleaner.app
#
# The cloud setup helper (manga-cleaner-provisioner) is a frozen copy of
# provisioner/ and deploy/. Edits there reach the app only after it is frozen
# again: src-tauri/build.rs compares the stamp this writes, a release build
# refuses a stale helper, and a development build runs the live Python instead.
#
# Steps:
#   1. target/cloud-build-venv: Python 3.12 with provisioner/requirements.lock,
#      hash-checked, reinstalled when the lock changes.
#   2. Cloud helper: frozen and smoke tested when its stamp no longer matches.
#   3. uv (managed Python for FLUX): staged when missing. It is a pinned
#      download, not built from this checkout, so it never goes stale.
#   4. --install (macOS): refuses an app built from other deploy/ code, copies
#      changed helpers into the app, re-signs it ad hoc and verifies the
#      signature. MC_APP picks another app bundle.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)

FORCE=0
INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    --install) INSTALL=1 ;;
    *) echo "usage: scripts/update-helpers.sh [--force] [--install]" >&2; exit 2 ;;
  esac
done

fail() { echo "ERROR: $*" >&2; exit 1; }

TRIPLE=$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')
[ -n "$TRIPLE" ] || fail "rustc not found; the helper is named after the Rust host target"
SUFFIX=""
case "$TRIPLE" in *windows*) SUFFIX=".exe" ;; esac
BIN="src-tauri/binaries"
HELPER="$BIN/manga-cleaner-provisioner-$TRIPLE$SUFFIX"
STAMP="$HELPER.sha256"
UV_SIDECAR="$BIN/manga-cleaner-uv-$TRIPLE$SUFFIX"
VENV="target/cloud-build-venv"
if [ -n "$SUFFIX" ]; then VENV_PY="$VENV/Scripts/python.exe"; else VENV_PY="$VENV/bin/python"; fi

echo "== build environment =="
if [ ! -x "$VENV_PY" ]; then
  if command -v uv >/dev/null; then
    uv venv --python 3.12 "$VENV"
  else
    command -v python3.12 >/dev/null || fail "need uv or python3.12 to create $VENV"
    python3.12 -m venv "$VENV"
  fi
fi
LOCK_SUM=$(shasum -a 256 provisioner/requirements.lock | cut -d' ' -f1)
if [ "$(cat "$VENV/.requirements.sha256" 2>/dev/null)" != "$LOCK_SUM" ]; then
  if command -v uv >/dev/null; then
    uv pip install --python "$VENV_PY" --require-hashes -r provisioner/requirements.lock
  else
    "$VENV_PY" -m pip install --require-hashes -r provisioner/requirements.lock
  fi
  echo "$LOCK_SUM" > "$VENV/.requirements.sha256"
else
  echo "$VENV matches provisioner/requirements.lock"
fi

echo "== cloud helper =="
if [ "$FORCE" = 0 ] && [ -f "$HELPER" ] && [ -f "$STAMP" ] && shasum -a 256 -c --status "$STAMP" 2>/dev/null; then
  echo "$HELPER matches this checkout"
else
  [ -f "$STAMP" ] && shasum -a 256 -c --quiet "$STAMP" 2>/dev/null | sed 's/^/  changed: /' || true
  "$VENV_PY" .github/scripts/build-cloud-provisioner.py "$TRIPLE" --no-config
fi

echo "== managed Python installer (uv) =="
if [ -f "$UV_SIDECAR" ]; then
  echo "$UV_SIDECAR present"
else
  UV=$(command -v uv) || fail "uv is missing; install it, then run this again"
  cp "$UV" "$UV_SIDECAR"
  chmod 755 "$UV_SIDECAR"
  echo "staged $("$UV_SIDECAR" --version)"
fi

[ "$INSTALL" = 1 ] || { echo "Done. npm run helpers:install also updates the installed app."; exit 0; }

echo "== installed app =="
[ "$(uname -s)" = "Darwin" ] || fail "--install updates a macOS app bundle; on other systems rebuild the app"
APP="${MC_APP:-/Applications/Manga Cleaner.app}"
[ -d "$APP/Contents/MacOS" ] || fail "no app at $APP (set MC_APP to its path)"
if pgrep -f "$APP/Contents/MacOS/manga-cleaner" >/dev/null; then
  fail "Manga Cleaner is running; quit it, then run this again"
fi
# The app compares a deployment with the cloud code it was built with
# (MC_CLOUD_CODE_DIGEST). A helper that deploys other code would make
# Settings ask for an update that can never match.
CODE_DIGEST=$("$VENV_PY" -c 'import sys; from pathlib import Path; from deploy.cloud.common.release import code_digest; print(code_digest(Path("deploy")))')
if ! LC_ALL=C grep -aq "$CODE_DIGEST" "$APP/Contents/MacOS/manga-cleaner"; then
  fail "$APP was built from other cloud code (deploy/). Rebuild the app from this checkout instead of copying the helper into it"
fi
CHANGED=0
for pair in "$HELPER:manga-cleaner-provisioner" "$UV_SIDECAR:manga-cleaner-uv"; do
  source="${pair%%:*}"
  dest="$APP/Contents/MacOS/${pair##*:}"
  if cmp -s "$source" "$dest"; then
    echo "${pair##*:} already current"
    continue
  fi
  # Copied as is: PyInstaller signs the frozen helper and uv ships signed, and
  # signing here would change the bytes this comparison relies on.
  cp "$source" "$dest"
  echo "${pair##*:} updated"
  CHANGED=1
done
if [ "$CHANGED" = 1 ]; then
  # The bundle seal covers Contents/MacOS, so the app itself is signed again.
  codesign --force --sign - "$APP"
  codesign --verify --deep --strict "$APP"
  echo "Done. The app has a new signature: on its next launch, click Always Allow on each cloud key prompt."
else
  echo "Done. Nothing in the app changed."
fi
