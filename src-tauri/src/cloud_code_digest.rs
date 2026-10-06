//! Which cloud code the bundled helper would deploy, as one digest.
//!
//! The same files and the same hash as `deploy/cloud/common/release.py`: the
//! package inits plus the `.py` files directly in `cloud/common/` and
//! `cloud/modal/`, sorted by POSIX path, each as `path NUL sha256 LF`, all
//! hashed again. `build.rs` runs it over the repository's `deploy/` and bakes
//! the answer in as `MC_CLOUD_CODE_DIGEST`; the gateway answers its own.
//!
//! Shared with `build.rs` by `#[path]`, so it uses only `std` and `sha2`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The shipped files under `root`, as relative POSIX paths, sorted.
pub fn shipped_files(root: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    for init in ["__init__.py", "cloud/__init__.py"] {
        if root.join(init).is_file() {
            files.push(init.to_string());
        }
    }
    for directory in ["common", "modal"] {
        let folder: PathBuf = root.join("cloud").join(directory);
        if !folder.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".py") && entry.file_type()?.is_file() {
                files.push(format!("cloud/{directory}/{name}"));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 over each shipped file's path, a NUL, its SHA-256, and a newline.
pub fn code_digest(root: &Path) -> std::io::Result<String> {
    let mut outer = Sha256::new();
    for relative in shipped_files(root)? {
        let inner = hex(&Sha256::digest(std::fs::read(root.join(&relative))?));
        outer.update(format!("{relative}\0{inner}\n").as_bytes());
    }
    Ok(hex(&outer.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture tree `deploy/cloud/tests/test_release.py` builds; both
    /// sides must name the same files and reach the same digest.
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("mc-code-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, data) in [
            ("__init__.py", &b""[..]),
            ("cloud/__init__.py", b"# cloud\n"),
            ("cloud/common/api.py", b"print('gateway')\n"),
            ("cloud/common/notes.txt", b"not shipped\n"),
            ("cloud/common/deep/inner.py", b"not shipped\n"),
            ("cloud/modal/app.py", b"app = 1\r\n"),
            ("cloud/beam/app.py", b"not shipped\n"),
            ("./tools.py", b"not shipped\n"),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        root
    }

    #[test]
    fn matches_the_gateway_digest_on_a_fixture_tree() {
        let root = fixture();
        assert_eq!(shipped_files(&root).unwrap(), ["__init__.py", "cloud/__init__.py", "cloud/common/api.py", "cloud/modal/app.py"]);
        assert_eq!(code_digest(&root).unwrap(), "875a779b6276a7b7ad9827559628c804c86ea4b3703729ca3ac1a54e41c97333");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_baked_digest_is_the_repository_code() {
        let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy");
        assert_eq!(env!("MC_CLOUD_CODE_DIGEST"), code_digest(&deploy).unwrap());
    }
}
