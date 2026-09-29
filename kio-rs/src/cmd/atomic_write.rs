//! Crash-safe file write: render to a sibling temp file, fsync it, then
//! atomically rename it over the target.
//!
//! Used wherever an interrupted write (kill / panic / `ENOSPC`) must not
//! leave a half-written file at the target path. The formatter
//! ([`crate::cmd::fmt`]) uses the plain temp-then-rename variant for its
//! recomputable output; the `*.sig.kio` changelog ([`crate::cmd::sig`])
//! is a committed, no-GC, not-recomputable history file, so it uses the
//! fsync-before-rename variant for crash durability — a torn write of
//! the changelog is silent loss of removed-item history.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Write `bytes` to `target` atomically: a sibling `<name>.tmp-<pid>`
/// temp file receives the bytes, is `fsync`ed, then `rename`d over the
/// target. POSIX guarantees rename atomicity within a filesystem (the
/// temp file lives in the target's directory, so they share one), so a
/// reader observes either the old contents or the complete new
/// contents, never a truncation. The `fsync` flushes the bytes to disk
/// before the rename publishes them, so a crash after the rename cannot
/// leave a renamed-but-empty file. On any failure the temp file is
/// cleaned up.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .ok_or_else(|| io::Error::other("target path has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp_path = parent.join(tmp_name);

    let write_result = (|| -> io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_publishes_full_contents() {
        let dir = std::env::temp_dir().join(format!("kio-atomic-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.sig.kio");
        write_atomic(&target, b"signature app v(1);\n").unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "signature app v(1);\n"
        );
        // Overwriting replaces the contents atomically.
        write_atomic(&target, b"signature app v(2);\n").unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "signature app v(2);\n"
        );
        // No temp file is left behind at the target's directory.
        let leftover = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!leftover, "no leftover temp file should remain");
        fs::remove_dir_all(&dir).ok();
    }
}
