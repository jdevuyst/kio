//! On-disk caches the kio compiler reads and writes.
//!
//! The semantic cache families live here:
//!
//! - [`enriched`] — the enriched-IR cache, keyed by source content
//!   plus referenced module fingerprints.
//! - [`emit`] — per-backend emitted-string caches under
//!   `<cache>/emit/`.
//! - [`artifact`] — complete per-backend output-directory caches under
//!   `<cache>/artifacts/`.
//! - [`equiv`] — the per-`equiv` discharge-result cache for
//!   `kio test`, keyed by the claim and its referenced definition
//!   closure.
//! - [`package_check`] — package-level check-skip entries used by
//!   warm `kio check` runs.
//! - [`typed`] — the per-module typed-AST cache under
//!   `<cache>/typed/`, storing `Module<Prime>` values after the
//!   frontend typer has accepted them.
//! - [`roots`] — shared resolution policy for Kio-semantic cache roots.
//! - [`gc`] — access metadata and garbage collection for semantic cache roots.
//!
//! [`policy`] holds the cross-cache enable / disable contract
//! for the `--no-cache` CLI flag.
//!
//! The rustc-rlib cache the Rust test runner uses to avoid repeated
//! `rustc` invocations lives in the runner itself
//! (`ci/infra/kio-test-runner-rs/src/rlib_cache/`), not here — its
//! keys are independent of any kio-compiler input and it has no
//! consumers inside `kio-rs`.

use std::fs;
use std::path::Path;

use crate::path_display::DisplayPath;

pub mod artifact;
pub mod emit;
pub mod enriched;
#[cfg(feature = "surface")]
pub mod equiv;
pub mod gc;
pub mod identity;
pub mod keys;
#[cfg(feature = "cli")]
pub mod package_check;
pub mod policy;
#[cfg(feature = "cli")]
pub mod roots;
pub mod typed;

/// Publish a fully written same-directory tempfile without accepting a
/// destination merely because it exists. Platforms that do not replace an
/// existing destination validate a concurrent winner, remove an invalid
/// destination, and retry once.
pub(crate) fn publish_temp_file(
    tmp_path: &Path,
    final_path: &Path,
    final_entry_is_valid: impl FnMut() -> bool,
) -> Result<(), String> {
    publish_temp_file_with_rename(tmp_path, final_path, final_entry_is_valid, |from, to| {
        fs::rename(from, to)
    })
}

fn publish_temp_file_with_rename(
    tmp_path: &Path,
    final_path: &Path,
    mut final_entry_is_valid: impl FnMut() -> bool,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    if let Err(rename_error) = rename(tmp_path, final_path) {
        if final_entry_is_valid() {
            let _ = remove_path_if_exists(tmp_path);
            return Ok(());
        }
        if let Err(remove_error) = remove_path_if_exists(final_path) {
            let _ = remove_path_if_exists(tmp_path);
            return Err(format!(
                "rename {} -> {}: {rename_error}; remove invalid destination: {remove_error}",
                DisplayPath(tmp_path),
                DisplayPath(final_path)
            ));
        }
        if let Err(retry_error) = rename(tmp_path, final_path) {
            if final_entry_is_valid() {
                let _ = remove_path_if_exists(tmp_path);
                return Ok(());
            }
            let _ = remove_path_if_exists(tmp_path);
            return Err(format!(
                "rename {} -> {}: {rename_error}; retry: {retry_error}",
                DisplayPath(tmp_path),
                DisplayPath(final_path)
            ));
        }
    }
    Ok(())
}

pub(crate) fn remove_path_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            #[cfg(windows)]
            let is_directory_symlink = {
                use std::os::windows::fs::FileTypeExt;
                file_type.is_symlink_dir()
            };
            #[cfg(not(windows))]
            let is_directory_symlink = false;
            remove_existing_path(path, file_type.is_dir(), is_directory_symlink)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_existing_path(
    path: &Path,
    is_directory: bool,
    is_directory_symlink: bool,
) -> std::io::Result<()> {
    if is_directory {
        fs::remove_dir_all(path)
    } else if is_directory_symlink {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_symlink_dispatch_uses_directory_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = dir.path().join("directory-shaped entry");
        fs::create_dir(&entry).expect("create directory-shaped entry");

        remove_existing_path(&entry, false, true).expect("remove through directory-symlink arm");
        assert!(!entry.exists());
    }

    #[test]
    fn flat_file_publish_accepts_a_valid_same_key_peer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = dir.path().join("entry.tmp");
        let final_path = dir.path().join("entry");
        fs::write(&tmp_path, "ours").expect("write tempfile");
        let attempts = std::cell::Cell::new(0);

        publish_temp_file_with_rename(
            &tmp_path,
            &final_path,
            || matches!(fs::read_to_string(&final_path), Ok(contents) if contents == "peer"),
            |_, to| {
                attempts.set(attempts.get() + 1);
                fs::write(to, "peer")?;
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            },
        )
        .expect("accept valid peer");

        assert_eq!(attempts.get(), 1);
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "peer");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn flat_file_repair_accepts_a_valid_peer_winning_the_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = dir.path().join("entry.tmp");
        let final_path = dir.path().join("entry");
        fs::write(&tmp_path, "ours").expect("write tempfile");
        fs::write(&final_path, "corrupt").expect("write corrupt destination");
        let attempts = std::cell::Cell::new(0);

        publish_temp_file_with_rename(
            &tmp_path,
            &final_path,
            || matches!(fs::read_to_string(&final_path), Ok(contents) if contents == "peer"),
            |_, to| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt == 1 {
                    fs::write(to, "peer")?;
                }
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            },
        )
        .expect("accept peer winning retry");

        assert_eq!(attempts.get(), 2);
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "peer");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn flat_file_repair_rejects_an_invalid_retry_peer_and_cleans_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = dir.path().join("entry.tmp");
        let final_path = dir.path().join("entry");
        fs::write(&tmp_path, "ours").expect("write tempfile");
        fs::write(&final_path, "corrupt").expect("write corrupt destination");
        let attempts = std::cell::Cell::new(0);

        let error = publish_temp_file_with_rename(
            &tmp_path,
            &final_path,
            || matches!(fs::read_to_string(&final_path), Ok(contents) if contents == "peer"),
            |_, to| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt == 1 {
                    fs::write(to, "invalid peer")?;
                }
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            },
        )
        .expect_err("invalid peer must not be accepted");

        assert_eq!(attempts.get(), 2);
        assert!(error.contains("retry"), "{error}");
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "invalid peer");
        assert!(!tmp_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn remove_path_removes_a_directory_symlink_without_touching_its_target() {
        use std::os::windows::fs::{FileTypeExt, symlink_dir};

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("sentinel"), "preserved").expect("write sentinel");
        match symlink_dir(&target, &link) {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && std::env::var_os("GITHUB_ACTIONS").is_none() =>
            {
                return;
            }
            Err(error) => panic!("create directory symlink: {error}"),
        }
        assert!(
            fs::symlink_metadata(&link)
                .expect("directory symlink metadata")
                .file_type()
                .is_symlink_dir()
        );

        remove_path_if_exists(&link).expect("remove directory symlink");
        assert_eq!(
            fs::symlink_metadata(&link)
                .expect_err("directory symlink removed")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(
            fs::read_to_string(target.join("sentinel")).expect("read target sentinel"),
            "preserved"
        );
        remove_path_if_exists(&link).expect("repeat removal is idempotent");
    }
}
