#!/usr/bin/env bash
# Build, sign, and publish an update to R2 (cleaner.komiq.cc).
# Usage: ./scripts/release.sh ["release notes"]
# Run on mac -> publishes darwin entry. Run on windows (git-bash) -> publishes windows entry.
# latest.json is merged, so platforms built on other machines are preserved.
#
# Node handles the manifest below. Python 3.11+ is still needed to freeze the
# bundled cloud helper, and is checked explicitly before the build.
set -euo pipefail
cd "$(dirname "$0")/.."

NOTES="${1:-}"
BASE_URL="https://cleaner.komiq.cc"
PUBLISH_URL="https://cleaner-publish.komiq.cc"
KEY_PATH="$HOME/.tauri/cleaner-updater.key"
TOKEN_PATH="$HOME/.tauri/cleaner-publish.token"

# One scratch directory for the whole run, removed on any exit. `/tmp/name.$$`
# is not a temporary file: the pid is guessable and /tmp is not private on a
# shared machine, and there is no /tmp worth the name under git-bash.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

[ -f "$KEY_PATH" ] || { echo "signing key missing: $KEY_PATH"; exit 1; }
[ -f "$TOKEN_PATH" ] || { echo "publish token missing: $TOKEN_PATH"; exit 1; }
TOKEN=$(tr -d '\r\n' < "$TOKEN_PATH")

# Tauri v2 reads TAURI_SIGNING_PRIVATE_KEY (path or key content); the
# _PATH-suffixed name is not recognized.
export TAURI_SIGNING_PRIVATE_KEY="$KEY_PATH"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

# The platform key, resolved before the build rather than after it: an
# unsupported host should cost a second, not a twenty minute build. `uname -m`
# is read on windows too. It used to be ignored there, so an arm64 git-bash
# would have published an arm64 build under the x86_64 key and handed every
# x64 user an installer that cannot run.
OS=$(uname -s); ARCH=$(uname -m)
case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64)  PLATFORM="darwin-aarch64"; TARGET="aarch64-apple-darwin" ;;
      x86_64) PLATFORM="darwin-x86_64"; TARGET="x86_64-apple-darwin" ;;
      *) echo "unsupported platform $OS-$ARCH"; exit 1 ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN*)
    case "$ARCH" in
      x86_64) PLATFORM="windows-x86_64"; TARGET="x86_64-pc-windows-msvc" ;;
      # runtime::package carries a win-arm64 row and nothing builds it: no
      # arm64 windows machine has run this application and none of the
      # acceleration figures were measured on one. Publishing from here would
      # put an entry in latest.json for an installer the release workflow
      # never produces.
      aarch64 | arm64) echo "arm64 windows is not a shipped platform; nothing here has ever been built or measured on one"; exit 1 ;;
      *) echo "unsupported platform $OS-$ARCH"; exit 1 ;;
    esac
    ;;
  *) echo "unsupported platform $OS-$ARCH"; exit 1 ;;
esac

# A local release must stage the same two executables as the tagged release
# workflow. Otherwise the installer may build while cloud setup and managed
# Python installation fail on a clean machine.
PYTHON=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null && "$candidate" -c 'import sys; assert sys.version_info >= (3, 11)' >/dev/null 2>&1; then
    PYTHON="$candidate"
    break
  fi
done
[ -n "$PYTHON" ] || { echo "Python 3.11+ is required to build the bundled helpers"; exit 1; }
"$PYTHON" -m pip install --require-hashes -r provisioner/requirements.lock
"$PYTHON" .github/scripts/build-cloud-provisioner.py "$TARGET"
"$PYTHON" -m pip install uv==0.12.19
"$PYTHON" .github/scripts/stage-flux-python.py "$TARGET"

# The Visual C++ runtime the downloaded ONNX Runtime imports, copied beside the
# executable. The release workflow does this in its own step; a build started
# from here would otherwise produce an installer that leaves a clean windows
# machine with a runtime that cannot load (windows error 126).
if [ "${PLATFORM#windows-}" != "$PLATFORM" ]; then
  echo "== staging the visual c++ runtime =="
  powershell -NoProfile -ExecutionPolicy Bypass \
    -File .github/scripts/stage-vc-runtime.ps1 \
    -Destination src-tauri/vendor/windows-crt
fi

VERSION=$(node -p "require('./src-tauri/tauri.conf.json').version")
echo "== building v$VERSION =="
npm ci
npx tauri build --target "$TARGET"

BUNDLE_DIR="target/$TARGET/release/bundle"
case "$PLATFORM" in
  darwin-*)
    ARTIFACT=$(ls "$BUNDLE_DIR"/macos/*.app.tar.gz | head -1)
    EXTRA=$(ls "$BUNDLE_DIR"/dmg/*.dmg 2>/dev/null | head -1 || true)
    EXT="app.tar.gz"
    ;;
  windows-*)
    ARTIFACT=$(ls "$BUNDLE_DIR"/nsis/*-setup.exe | head -1)
    EXTRA=""
    EXT="setup.exe"
    ;;
esac
SIG="$ARTIFACT.sig"
[ -f "$SIG" ] || { echo "signature missing: $SIG (is bundle.createUpdaterArtifacts true?)"; exit 1; }

# Platform-tagged, space-free name: keeps mac arches distinct in R2 and keeps
# update URLs safe without percent-encoding. Must match release.yml's naming.
FNAME="MangaCleaner_${VERSION}_${PLATFORM}.${EXT}"
REMOTE="releases/v$VERSION/$FNAME"
echo "== uploading $FNAME =="
curl -sSf -X PUT -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/octet-stream" --data-binary @"$ARTIFACT" "$PUBLISH_URL/$REMOTE"
if [ -n "$EXTRA" ]; then
  curl -sSf -X PUT -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/octet-stream" --data-binary @"$EXTRA" "$PUBLISH_URL/releases/v$VERSION/MangaCleaner_${VERSION}_${PLATFORM}.dmg"
fi

echo "== merging latest.json =="
# 404 means first release ever; any other failure must abort rather than
# silently rebuilding latest.json from nothing.
HTTP=$(curl -s -o "$WORK/existing-latest.json" -w "%{http_code}" "$BASE_URL/latest.json" || echo 000)
case "$HTTP" in
  200) EXISTING=$(cat "$WORK/existing-latest.json") ;;
  404) EXISTING='{}' ;;
  *) echo "fetching existing latest.json failed (HTTP $HTTP), aborting"; exit 1 ;;
esac

# Written to a file and run by path rather than piped to `node -`: the script
# has to work under git-bash, and a file leaves nothing to argue about.
cat > "$WORK/merge-latest.js" <<'JS'
const fs = require('fs');
const [version, platform, url, sigPath, notes] = process.argv.slice(2);

// Anything that is not a JSON object is treated as no manifest at all: a
// truncated or replaced latest.json must not stop a release, and must not be
// merged into either.
let data;
try {
  data = JSON.parse(process.env.EXISTING_JSON || '{}');
  if (data === null || typeof data !== 'object' || Array.isArray(data)) data = {};
} catch {
  data = {};
}

// Platforms are only carried over within one version. A build of a new version
// starts an empty platform set, so the manifest can never offer this version's
// installer to one platform and the last version's to another.
const sameVersion = data.version === version;
const platforms =
  sameVersion && data.platforms !== null && typeof data.platforms === 'object' ? data.platforms : {};
platforms[platform] = { signature: fs.readFileSync(sigPath, 'utf8').trim(), url };

console.log(
  JSON.stringify(
    {
      version,
      notes: notes || (sameVersion ? data.notes || '' : ''),
      // The same shape python's "%Y-%m-%dT%H:%M:%SZ" produced, so an existing
      // manifest and a new one are not written two different ways.
      pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
      platforms,
    },
    null,
    2,
  ),
);
JS

LATEST=$(EXISTING_JSON="$EXISTING" node "$WORK/merge-latest.js" "$VERSION" "$PLATFORM" "$BASE_URL/$REMOTE" "$SIG" "$NOTES")
printf '%s' "$LATEST" > "$WORK/latest.json"
curl -sSf -X PUT -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" --data-binary @"$WORK/latest.json" "$PUBLISH_URL/latest.json"

echo "== done =="
echo "$LATEST"
echo "check: curl $BASE_URL/latest.json"
