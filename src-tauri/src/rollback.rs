//! Going back to an earlier released version, from inside the app.
//!
//! Two commands. [`rollback_versions`] lists what was released, with the facts
//! the interface needs to warn about each version. [`rollback_prepare`] checks
//! one version's own update manifest and hands the interface the same update
//! handle the updater plugin's `check` does, so the download, the signature
//! check against the configured public key, the install and the restart are
//! the plugin's own code and the normal update dialog's progress.
//!
//! ## Where a version comes from
//!
//! The list is the GitHub releases list, which every release since
//! `v1.0.0-beta.2` is mirrored to (`github-release.yml`), with its date and
//! notes. It is not an index on the update host: that host serves everything
//! but `latest.json` as immutable for a year (`infra/site`), so a file that
//! changes with each release would go stale there.
//!
//! The update manifest is always `releases/v<version>/latest.json` on the
//! same host as `latest.json`, which the release workflow writes from
//! `v1.0.0-beta.21` on and `scripts/backfill-rollback-manifests.py` writes for
//! the releases before it. It is derived here from the version and never taken
//! from a listed URL: nothing the interface or a list says can point the
//! updater at another host. The signature is checked by the plugin whatever
//! the manifest says, so a manifest can at worst name a build that is refused.
//!
//! ## What is safe to go back to
//!
//! The facts are the data formats, read off each release tag:
//!
//! - Chapter manifests: `v1.0.0-beta.2` writes format 2. `v1.0.0-beta.3`
//!   to `v1.0.0-beta.20` write and read format 5. This build writes format 6
//!   for a chapter with a Nano Banana patch. An older build **refuses** a
//!   manifest from a later format and leaves it untouched (`Job::open`).
//! - Cloud profiles (`inference.json`) arrived in `v1.0.0-beta.3` and have
//!   not changed since. `v1.0.0-beta.2` does not know the file at all.
//! - `settings.json` is opaque JSON merged by key: an older build keeps the
//!   keys it does not know.
//! - The library index and the undo journal are format 1 in every release.
//!
//! So the floor is `v1.0.0-beta.3`. Below it every chapter made since would be
//! refused and cloud setup would be lost to the older build.

use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Runtime};
use tauri_plugin_updater::UpdaterExt;

/// The host the updater already trusts; `tauri.conf.json` names its
/// `latest.json`.
const UPDATE_HOST: &str = "https://cleaner.komiq.cc";

/// The releases every tag is mirrored to (`github-release.yml`). The
/// repository was renamed from `manga-cleaner`; the old name redirects.
const GITHUB_RELEASES: &str = "https://api.github.com/repos/k-omiq/Bear/releases?per_page=100";

/// Each version's own page, for a platform that cannot roll back in place.
const RELEASE_PAGE: &str = "https://github.com/k-omiq/Bear/releases/tag/v";

/// The oldest version it is safe to go back to (see the module docs).
pub const FLOOR: &str = "1.0.0-beta.3";

/// The last version that reads each chapter manifest format, oldest first.
/// A version after the last row reads this build's
/// [`cleaner_core::project::FORMAT_VERSION`]. When that number moves, the
/// last version that read the old one is added here (a test holds this).
const READ_LIMITS: &[(&str, u32)] = &[("1.0.0-beta.2", 2), ("1.0.0-beta.20", 5)];

/// The last version released without the skip rule: it shows the update
/// badge for the version just left, though it installs nothing by itself.
const LAST_WITHOUT_SKIP: &str = "1.0.0-beta.20";

/// One released version, as the list shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub version: String,
    pub date: Option<String>,
    pub notes: Option<String>,
    pub page_url: String,
    /// The newest chapter manifest format this version opens.
    pub reads_format: u32,
    /// Whether this version keeps the skip and so does not offer the version
    /// left behind.
    pub honours_skip: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub current_version: String,
    /// The installer kind of this copy: `app`, `nsis`, `msi`, `appimage`,
    /// `deb`, `rpm` or `unknown` (a development build).
    pub bundle: &'static str,
    /// Whether the updater can replace this copy in place.
    pub in_place: bool,
    pub floor: &'static str,
    /// The chapter manifest format this build writes.
    pub manifest_format: u32,
    pub versions: Vec<Entry>,
}

/// What `plugin:updater|check` answers, so the interface builds the plugin's
/// own `Update` from it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    rid: tauri::ResourceId,
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
    raw_json: serde_json::Value,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
}

/// `1.0.0-beta.3` or `v1.0.0-beta.3`; anything else is not a version.
fn parse_version(raw: &str) -> Option<Version> {
    Version::parse(raw.trim().trim_start_matches('v')).ok()
}

fn floor() -> Version {
    Version::parse(FLOOR).expect("the floor is a version")
}

/// The newest chapter manifest format `version` opens.
fn reads_format(version: &Version) -> u32 {
    READ_LIMITS
        .iter()
        .find(|(last, _)| version <= &Version::parse(last).expect("a version"))
        .map_or(cleaner_core::project::FORMAT_VERSION, |(_, format)| *format)
}

fn honours_skip(version: &Version) -> bool {
    version > &Version::parse(LAST_WITHOUT_SKIP).expect("a version")
}

/// The one manifest the updater is pointed at for `version`.
fn manifest_url(version: &Version) -> String {
    format!("{UPDATE_HOST}/releases/v{version}/latest.json")
}

fn entry(version: Version, date: Option<String>, notes: Option<String>) -> Entry {
    let notes = notes.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
    Entry {
        page_url: format!("{RELEASE_PAGE}{version}"),
        reads_format: reads_format(&version),
        honours_skip: honours_skip(&version),
        version: version.to_string(),
        date,
        notes,
    }
}

/// Rows with a version, each version once, in the order given.
fn dedupe(rows: impl IntoIterator<Item = Entry>) -> Vec<Entry> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter().filter(|row| seen.insert(row.version.clone())).collect()
}

fn parse_github(body: &str) -> Result<Vec<Entry>, String> {
    let releases: Vec<GithubRelease> = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(dedupe(releases.into_iter().filter(|r| !r.draft).filter_map(|r| {
        parse_version(&r.tag_name).map(|v| entry(v, r.published_at, r.body))
    })))
}

/// Whether the updater may offer `remote` when `target` was asked for: only
/// that version, and only when it is older than what is running. The
/// plugin's own comparator would refuse any older version.
fn is_rollback_target(current: &Version, remote: &Version, target: &Version) -> bool {
    remote == target && remote < current
}

/// Why `target` may not be installed over `current`, as a `rollback_*` code.
fn refuse_target(current: &Version, target: &Version) -> Option<String> {
    if target >= current {
        return Some(format!("rollback_not_older: {target} is not older than {current}"));
    }
    if target < &floor() {
        return Some(format!("rollback_below_floor: {target} is older than {FLOOR}"));
    }
    None
}

fn bundle_name() -> &'static str {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    match bundle_type() {
        Some(BundleType::App) | Some(BundleType::Dmg) => "app",
        Some(BundleType::Nsis) => "nsis",
        Some(BundleType::Msi) => "msi",
        Some(BundleType::AppImage) => "appimage",
        Some(BundleType::Deb) => "deb",
        Some(BundleType::Rpm) => "rpm",
        _ => "unknown",
    }
}

/// The installer kinds the published manifests carry a build for. A `.deb`
/// copy would be handed the AppImage and refuse it, as a normal update would.
fn replaces_in_place(bundle: &str) -> bool {
    matches!(bundle, "app" | "nsis" | "appimage")
}

fn fetch(url: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("MangaCleaner/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    response.text().map_err(|e| e.to_string())
}

fn released_versions() -> Result<Vec<Entry>, String> {
    fetch(GITHUB_RELEASES)
        .and_then(|body| parse_github(&body))
        .map_err(|e| format!("rollback_offline: {e}"))
}

#[tauri::command]
pub async fn rollback_versions(app: tauri::AppHandle) -> Result<Catalog, String> {
    let versions = tauri::async_runtime::spawn_blocking(released_versions)
        .await
        .map_err(|e| format!("rollback_offline: {e}"))??;
    let bundle = bundle_name();
    Ok(Catalog {
        current_version: app.package_info().version.to_string(),
        bundle,
        in_place: replaces_in_place(bundle),
        floor: FLOOR,
        manifest_format: cleaner_core::project::FORMAT_VERSION,
        versions,
    })
}

#[tauri::command]
pub async fn rollback_prepare<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    version: String,
) -> Result<Metadata, String> {
    let target = parse_version(&version).ok_or_else(|| format!("rollback_invalid: {version}"))?;
    let current = app.package_info().version.clone();
    if let Some(refusal) = refuse_target(&current, &target) {
        return Err(refusal);
    }
    if !replaces_in_place(bundle_name()) {
        return Err(format!("rollback_in_place_unsupported: {}", bundle_name()));
    }
    let url = manifest_url(&target).parse().map_err(|e| format!("rollback_invalid: {e}"))?;
    let wanted = target.clone();
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| format!("rollback_invalid: {e}"))?
        .version_comparator(move |current, remote| is_rollback_target(&current, &remote.version, &wanted))
        .build()
        .map_err(|e| format!("rollback_network: {e}"))?;
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => return Err(format!("rollback_unreachable: the manifest for {target} names another version")),
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
            return Err(format!("rollback_unreachable: no manifest for {target}"))
        }
        Err(tauri_plugin_updater::Error::TargetsNotFound(targets)) => {
            return Err(format!("rollback_platform: {target} has no build for {}", targets.join(", ")))
        }
        Err(e) => return Err(format!("rollback_network: {e}")),
    };
    let date = update.raw_json.get("pub_date").and_then(|d| d.as_str()).map(str::to_owned);
    Ok(Metadata {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date,
        body: update.body.clone(),
        raw_json: update.raw_json.clone(),
        rid: webview.resources_table().add(update),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    #[test]
    fn rollback_comparator_takes_only_the_asked_older_version() {
        let current = v("1.0.0-beta.21");
        let target = v("1.0.0-beta.19");
        assert!(is_rollback_target(&current, &v("1.0.0-beta.19"), &target));
        assert!(!is_rollback_target(&current, &v("1.0.0-beta.18"), &target), "another version");
        assert!(!is_rollback_target(&current, &v("1.0.0-beta.21"), &v("1.0.0-beta.21")), "the same version");
        assert!(!is_rollback_target(&current, &v("1.0.0-beta.22"), &v("1.0.0-beta.22")), "a newer one");
        // Prerelease numbers order as numbers: beta.9 is older than beta.10.
        assert!(is_rollback_target(&v("1.0.0-beta.10"), &v("1.0.0-beta.9"), &v("1.0.0-beta.9")));
    }

    #[test]
    fn rollback_refuses_newer_equal_and_below_the_floor() {
        let current = v("1.0.0-beta.21");
        assert!(refuse_target(&current, &v("1.0.0-beta.20")).is_none());
        assert!(refuse_target(&current, &v(FLOOR)).is_none(), "the floor itself is allowed");
        assert!(refuse_target(&current, &v("1.0.0-beta.2")).unwrap().starts_with("rollback_below_floor"));
        assert!(refuse_target(&current, &v("1.0.0-beta.21")).unwrap().starts_with("rollback_not_older"));
        assert!(refuse_target(&current, &v("1.0.0")).unwrap().starts_with("rollback_not_older"));
    }

    #[test]
    fn rollback_manifest_url_is_derived_on_the_update_host() {
        assert_eq!(
            manifest_url(&v("1.0.0-beta.19")),
            "https://cleaner.komiq.cc/releases/v1.0.0-beta.19/latest.json"
        );
        assert!(parse_version("v1.0.0-beta.19").is_some());
        assert!(parse_version("../evil").is_none());
        assert!(parse_version("1.0.0-beta.19/../../x").is_none());
    }

    #[test]
    fn rollback_reads_formats_follow_the_release_tags() {
        assert_eq!(reads_format(&v("1.0.0-beta.2")), 2);
        assert_eq!(reads_format(&v("1.0.0-beta.3")), 5);
        assert_eq!(reads_format(&v("1.0.0-beta.20")), 5);
        assert_eq!(reads_format(&v("1.0.0-beta.21")), cleaner_core::project::FORMAT_VERSION);
        assert!(!honours_skip(&v("1.0.0-beta.20")));
        assert!(honours_skip(&v("1.0.0-beta.21")));
    }

    /// When the manifest format moves, the last version that read the old
    /// one has to be added to `READ_LIMITS`, or the warning goes quiet.
    #[test]
    fn rollback_read_limits_cover_every_older_format() {
        let (_, last) = READ_LIMITS.last().unwrap();
        assert_eq!(
            last + 1,
            cleaner_core::project::FORMAT_VERSION,
            "FORMAT_VERSION moved: add the last version that read format {} to READ_LIMITS",
            cleaner_core::project::FORMAT_VERSION - 1
        );
        assert!(v(FLOOR) > v(READ_LIMITS[0].0), "the floor reads the formats after beta.2");
    }

    #[test]
    fn rollback_github_list_parses() {
        let github = r#"[
            {"tag_name":"v1.0.0-beta.20","published_at":"2026-11-05T01:55:30Z","body":"  Fixes.  ","draft":false,"prerelease":true},
            {"tag_name":"v1.0.0-beta.20","body":"a second row for the same tag"},
            {"tag_name":"v1.0.0-beta.19","body":""},
            {"tag_name":"v1.0.0-beta.21","draft":true},
            {"tag_name":"nightly"}
        ]"#;
        let rows = parse_github(github).unwrap();
        assert_eq!(rows.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["1.0.0-beta.20", "1.0.0-beta.19"]);
        assert_eq!(rows[0].notes.as_deref(), Some("Fixes."));
        assert_eq!(rows[0].date.as_deref(), Some("2026-11-05T01:55:30Z"));
        assert_eq!(rows[1].notes, None);
        assert_eq!(rows[0].reads_format, 5);
        assert_eq!(rows[0].page_url, "https://github.com/k-omiq/Bear/releases/tag/v1.0.0-beta.20");
        assert!(parse_github("{}").is_err());
    }

    #[test]
    fn rollback_entries_cross_the_seam_in_camel_case() {
        let json = serde_json::to_string(&entry(v("1.0.0-beta.9"), None, None)).unwrap();
        for key in ["pageUrl", "readsFormat", "honoursSkip"] {
            assert!(json.contains(key), "{json}");
        }
    }

    #[test]
    fn rollback_in_place_only_for_published_installer_kinds() {
        assert!(replaces_in_place("app") && replaces_in_place("nsis") && replaces_in_place("appimage"));
        for other in ["deb", "rpm", "msi", "unknown"] {
            assert!(!replaces_in_place(other), "{other}");
        }
    }
}
