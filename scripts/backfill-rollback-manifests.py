#!/usr/bin/env python3
"""Write the per-version update manifests the in-app rollback reads.

    python3 scripts/backfill-rollback-manifests.py [--out DIR] [--upload] [--force]

The in-app rollback (src-tauri/src/rollback.rs) goes back to a version by
checking https://cleaner.komiq.cc/releases/v<version>/latest.json. The release
workflow writes that file from the first release that has the rollback; the
releases before it published only the moving latest.json. This builds the
missing ones from what every release already has on GitHub: each installer's
.sig asset, the release notes and the publish date.

Each platform's url is the copy on the update host when it is there, else the
GitHub asset. The signature is the one published beside the installer, and the
app checks it against its own public key whatever this file says, so a wrong
manifest can only name a build the app refuses.

Without --upload nothing leaves this computer: the manifests are written under
--out for a look first. --upload sends each through the publish worker
(.github/scripts/publish-upload.py, CLEANER_PUBLISH_TOKEN). A version whose
manifest is already on the update host is skipped unless --force: release
objects there are served as immutable.

GH_TOKEN, when set, is sent to the GitHub API to lift its rate limit.
"""
import argparse
import importlib.util
import json
import os
import re
import sys
import urllib.error
import urllib.request

REPO = "k-omiq/Bear"
UPDATE_HOST = "https://cleaner.komiq.cc"
UA = "manga-cleaner-release"
INSTALLER = re.compile(r"^MangaCleaner_(?P<version>[^_]+)_(?P<platform>[a-z]+-[a-z0-9_]+)(?:\.app\.tar\.gz|-setup\.exe|\.AppImage)$")


def request(url, method="GET", api=False):
    headers = {"User-Agent": UA}
    if api:
        headers["Accept"] = "application/vnd.github+json"
        if os.environ.get("GH_TOKEN"):
            headers["Authorization"] = f"Bearer {os.environ['GH_TOKEN']}"
    return urllib.request.Request(url, method=method, headers=headers)


def get(url, api=False):
    with urllib.request.urlopen(request(url, api=api), timeout=60) as response:
        return response.read().decode()


def exists(url):
    try:
        with urllib.request.urlopen(request(url, method="HEAD"), timeout=60) as response:
            return 200 <= response.status < 300
    except urllib.error.HTTPError:
        return False


def manifest_for(release):
    version = release["tag_name"].lstrip("v")
    assets = {a["name"]: a for a in release.get("assets", [])}
    platforms = {}
    sources = {}
    for name, asset in sorted(assets.items()):
        if not name.endswith(".sig"):
            continue
        artifact = name[: -len(".sig")]
        match = INSTALLER.match(artifact)
        if not match or match["version"] != version or artifact not in assets:
            continue
        hosted = f"{UPDATE_HOST}/releases/v{version}/{artifact}"
        url = hosted if exists(hosted) else assets[artifact]["browser_download_url"]
        platforms[match["platform"]] = {"signature": get(asset["browser_download_url"]).strip(), "url": url}
        sources[match["platform"]] = "update host" if url == hosted else "github"
    manifest = {
        "version": version,
        "notes": (release.get("body") or "").strip(),
        "pub_date": release.get("published_at"),
        "platforms": platforms,
    }
    return version, manifest, sources


def publisher():
    path = os.path.join(os.path.dirname(__file__), "..", ".github", "scripts", "publish-upload.py")
    spec = importlib.util.spec_from_file_location("publish_upload", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", default="rollback-manifests")
    parser.add_argument("--upload", action="store_true")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    if args.upload and not os.environ.get("CLEANER_PUBLISH_TOKEN"):
        sys.exit("--upload needs CLEANER_PUBLISH_TOKEN")
    upload = publisher().upload if args.upload else None

    releases = json.loads(get(f"https://api.github.com/repos/{REPO}/releases?per_page=100", api=True))
    for release in releases:
        if release.get("draft"):
            continue
        version, manifest, sources = manifest_for(release)
        key = f"releases/v{version}/latest.json"
        if not manifest["platforms"]:
            print(f"{version:<18} no signed installers, skipped")
            continue
        listed = ", ".join(f"{p} ({s})" for p, s in sorted(sources.items()))
        if exists(f"{UPDATE_HOST}/{key}") and not args.force:
            print(f"{version:<18} already published, skipped: {listed}")
            continue
        folder = os.path.join(args.out, f"v{version}")
        os.makedirs(folder, exist_ok=True)
        local = os.path.join(folder, "latest.json")
        with open(local, "w") as handle:
            json.dump(manifest, handle, indent=2)
        if upload:
            upload(local, key, "application/json")
            print(f"{version:<18} uploaded {key}: {listed}")
        else:
            print(f"{version:<18} written {local}: {listed}")


if __name__ == "__main__":
    main()
