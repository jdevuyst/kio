use std::fs;
use std::path::{Path, PathBuf};

/// Keep each synthesized package separate from the source tree and other snippets.
pub(super) fn create_scratch_dir(source: &Path, open_line: usize) -> std::io::Result<PathBuf> {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("kiodoc");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let prefix = format!("kiodoc-{stem}-l{open_line}-{nonce}-{}", std::process::id());
    create_scratch_dir_in(&std::env::temp_dir(), &prefix)
}

fn create_scratch_dir_in(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    let mut path = parent.join(prefix);
    let mut suffix = 0_u64;
    loop {
        // Clock resolution cannot establish ownership among parallel validators.
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix += 1;
                path = parent.join(format!("{prefix}-{suffix}"));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_directory_is_not_reused_or_removed() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("snippet");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("utils.kio"), "existing source").unwrap();

        let scratch = create_scratch_dir_in(root.path(), "snippet").unwrap();
        assert_ne!(scratch, existing);
        fs::write(scratch.join("utils.kio"), "new source").unwrap();
        fs::remove_dir_all(scratch).unwrap();
        assert_eq!(
            fs::read_to_string(existing.join("utils.kio")).unwrap(),
            "existing source"
        );
    }

    #[test]
    fn existing_file_is_not_reused() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("snippet");
        fs::write(&existing, "existing file").unwrap();

        let scratch = create_scratch_dir_in(root.path(), "snippet").unwrap();
        assert!(scratch.is_dir());
        assert_ne!(scratch, existing);
        assert_eq!(fs::read_to_string(existing).unwrap(), "existing file");
    }

    #[test]
    fn simultaneous_identical_candidates_keep_sources_isolated() {
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(2);
        let allocations = std::thread::scope(|scope| {
            let workers: Vec<_> = ["first source", "second source"]
                .into_iter()
                .map(|source| {
                    let root = root.path();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        let scratch = create_scratch_dir_in(root, "snippet").unwrap();
                        fs::write(scratch.join("utils.kio"), source).unwrap();
                        barrier.wait();
                        let contents = fs::read_to_string(scratch.join("utils.kio")).unwrap();
                        (scratch, source, contents)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_ne!(allocations[0].0, allocations[1].0);
        for (_, source, contents) in allocations {
            assert_eq!(contents, source);
        }
    }

    #[test]
    fn non_collision_errors_are_returned() {
        let root = tempfile::tempdir().unwrap();
        let parent_file = root.path().join("file");
        fs::write(&parent_file, "existing file").unwrap();
        assert!(create_scratch_dir_in(&parent_file, "snippet").is_err());
    }
}
