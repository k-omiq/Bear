#!/usr/bin/env bash
# One-command release. Bumps the version, tags, pushes, watches the Release
# workflow on GitHub Actions, and verifies the published manifest.
#
#   ./scripts/ship.sh 0.2.0 "Fixed the drag flinch, faster exports"
#   ./scripts/ship.sh 1.0.0-beta.2 "Second beta"
#   ./scripts/ship.sh 0.2.0 "notes" --dry-run   # stop before pushing anything
#
# Steps it performs:
#   1. Preflight: gh auth, clean tree, on main, synced with origin, secrets set.
#   2. Bump version in src-tauri/tauri.conf.json, package.json, and Cargo.toml.
#   3. Commit "release: vX.Y.Z", push main.
#   4. Annotated tag vX.Y.Z (message = release notes shown in the app), push it.
#   5. Watch the Release run; on failure print the failing step's log and exit 1.
#   6. Verify https://cleaner.komiq.cc/latest.json serves the new version
#      and count the published platforms.
#
# scripts/release.sh is the OTHER path: it builds and publishes from THIS
# machine only (current platform). Use ship.sh for normal releases.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-}"
NOTES="${2:-}"
DRY_RUN="${3:-}"
REPO="k-omiq/manga-cleaner"
BASE_URL="https://cleaner.komiq.cc"

fail() { echo "ERROR: $*" >&2; exit 1; }

# X.Y.Z, optionally with a semver prerelease tail: this project shipped its
# first beta as 1.0.0-beta.1, and a pattern that only accepted three numbers
# meant the repository could not ship its own current version.
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail "usage: ship.sh X.Y.Z[-prerelease] \"notes\" [--dry-run] (got '${VERSION}')"
[ -n "$NOTES" ] || fail "release notes required; users see them in the update dialog"

echo "== preflight =="
command -v gh >/dev/null || fail "gh CLI not installed"
gh auth status >/dev/null 2>&1 || fail "gh not authenticated; run: gh auth login"
[ "$(git branch --show-current)" = "main" ] || fail "not on main"
# Untracked files don't block a release; modified tracked files do.
[ -z "$(git status --porcelain --untracked-files=no)" ] || fail "working tree has uncommitted changes; commit or stash first"
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not synced with origin/main; push or pull first"
git rev-parse "v$VERSION" >/dev/null 2>&1 && fail "tag v$VERSION already exists"

CURRENT=$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
# Semver 2.0.0 precedence, not `int(x) for x in v.split('.')`: the current
# version is 1.0.0-beta.1 and that raises ValueError, so before this no
# prerelease could be shipped and no release could follow one.
#
# The versions arrive through the environment rather than being interpolated
# into the source, so a version string can never be read as Python.
NEW_VERSION="$VERSION" CURRENT_VERSION="$CURRENT" python3 <<'PY' || fail "version $VERSION is not greater than current $CURRENT"
import os, sys

def key(version):
    """A tuple that sorts by semver precedence.

    Build metadata (anything after '+') is dropped first and from the whole
    string, which is what the specification says: it takes no part in
    precedence, and it can appear on a version that has no prerelease at all.
    """
    core, _, prerelease = version.split('+', 1)[0].partition('-')
    major, minor, patch = (int(part) for part in core.split('.'))
    if not prerelease:
        # A release outranks every prerelease of the same core version, so the
        # release sorts above with 1 and the prereleases below with 0.
        return (major, minor, patch, 1, ())
    identifiers = []
    for identifier in prerelease.split('.'):
        if identifier.isdigit():
            # Numeric identifiers compare numerically and always rank below
            # alphanumeric ones, hence the leading 0 against the leading 1.
            identifiers.append((0, int(identifier), ''))
        else:
            identifiers.append((1, 0, identifier))
    # A shorter identifier list ranks below a longer one that shares its
    # prefix, which is what comparing the tuples already does.
    return (major, minor, patch, 0, tuple(identifiers))

sys.exit(0 if key(os.environ['NEW_VERSION']) > key(os.environ['CURRENT_VERSION']) else 1)
PY

# One fetch, then grep the string: `gh | grep -q` under pipefail dies of
# SIGPIPE whenever the match is not the last line grep reads.
SECRETS=$(gh secret list --repo "$REPO")
for s in TAURI_SIGNING_PRIVATE_KEY CLEANER_PUBLISH_TOKEN; do
  grep -q "^$s" <<<"$SECRETS" || fail "repo secret $s missing"
done
# Authenticode is optional and the release does not fail without it, so this
# reports rather than blocks. It is worth saying out loud every time: an
# unsigned installer means every Windows user meets a SmartScreen warning
# before they meet the application.
if grep -q "^WINDOWS_CERTIFICATE" <<<"$SECRETS"; then
  echo "windows installer: signed (WINDOWS_CERTIFICATE is set)"
else
  echo "windows installer: UNSIGNED - users will see a SmartScreen warning. Set the WINDOWS_CERTIFICATE and WINDOWS_CERTIFICATE_PASSWORD repo secrets to sign."
fi
echo "preflight ok: $CURRENT -> $VERSION"

echo "== bump version =="
python3 - "$VERSION" <<'PY'
import json, sys
v = sys.argv[1]
p = 'src-tauri/tauri.conf.json'
c = json.load(open(p))
c['version'] = v
open(p, 'w').write(json.dumps(c, indent=2) + '\n')
PY
python3 - "$VERSION" <<'PY'
import json, sys
v = sys.argv[1]
p = 'package.json'
c = json.load(open(p))
c['version'] = v
open(p, 'w').write(json.dumps(c, indent=2) + '\n')
PY
python3 - "$VERSION" <<'PY'
import json, sys
v = sys.argv[1]
p = 'package-lock.json'
c = json.load(open(p))
c['version'] = v
c['packages']['']['version'] = v
open(p, 'w').write(json.dumps(c, indent=2) + '\n')
PY
python3 - "$VERSION" <<'PY'
import re, sys
v = sys.argv[1]
p = 'Cargo.toml'
text = open(p).read()
open(p, 'w').write(re.sub(r'^version = "[^"]+"', f'version = "{v}"', text, count=1, flags=re.M))
PY
# Keep Cargo.lock's own entry in sync so the release build does not dirty the tree.
(cargo update -q --package manga-cleaner 2>/dev/null) || true
git diff --stat | sed 's/^/  /'

if [ "$DRY_RUN" = "--dry-run" ]; then
  echo "== dry run: reverting bump, nothing pushed =="
  git checkout -- src-tauri/tauri.conf.json package.json Cargo.toml Cargo.lock 2>/dev/null || true
  exit 0
fi

echo "== commit and tag =="
git add src-tauri/tauri.conf.json package.json Cargo.toml Cargo.lock package-lock.json 2>/dev/null || true
git commit -m "release: v$VERSION"
git tag -a "v$VERSION" -m "$NOTES"
git push origin main
git push origin "v$VERSION"

echo "== waiting for the Release workflow to start =="
RUN_ID=""
for _ in $(seq 1 30); do
  RUN_ID=$(gh run list --repo "$REPO" --workflow Release --event push --limit 5 \
    --json databaseId,headBranch --jq ".[] | select(.headBranch == \"v$VERSION\") | .databaseId" | head -1)
  [ -n "$RUN_ID" ] && break
  sleep 10
done
[ -n "$RUN_ID" ] || fail "release run for v$VERSION never appeared; check: gh run list --repo $REPO"
echo "run: https://github.com/$REPO/actions/runs/$RUN_ID"

echo "== watching (mac + windows builds, usually 10-25 min) =="
if ! gh run watch "$RUN_ID" --repo "$REPO" --exit-status --interval 30; then
  echo ""
  echo "== RELEASE FAILED - failing step log =="
  gh run view "$RUN_ID" --repo "$REPO" --log-failed | tail -60
  fail "workflow failed; tag v$VERSION is pushed but nothing was published. Fix the cause, delete the tag (git push origin :refs/tags/v$VERSION && git tag -d v$VERSION), and re-run ship.sh"
fi

echo "== verifying published manifest =="
sleep 5
MANIFEST=$(curl -sf "$BASE_URL/latest.json") || fail "workflow succeeded but $BASE_URL/latest.json is unreachable"
MANIFEST_JSON="$MANIFEST" python3 - "$VERSION" <<'PY' || fail "manifest verification failed"
import json, os, sys
m = json.loads(os.environ["MANIFEST_JSON"])
want = sys.argv[1]
assert m.get("version") == want, f"manifest has {m.get('version')}, expected {want}"
plats = sorted(m.get("platforms", {}).keys())
print(f"published v{want} with platforms: {', '.join(plats)}")
# The platforms release.yml builds. windows-aarch64 is deliberately not
# here: runtime::package carries the win-arm64 row, nothing builds it, and an
# arm64 entry in the manifest would offer an installer that does not exist.
missing = {"darwin-aarch64", "windows-x86_64", "linux-x86_64"} - set(plats)
if missing:
    print(f"WARNING: missing platforms: {', '.join(sorted(missing))}", file=sys.stderr)
PY

echo "== done: v$VERSION is live; apps see it on next launch =="
