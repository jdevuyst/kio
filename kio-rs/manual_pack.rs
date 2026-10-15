use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

pub struct Bundle {
    pub index: String,
    pub payload: Vec<u8>,
}

pub fn assemble(root: &Path) -> io::Result<Bundle> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--stage",
            "-z",
            "--",
            "docs",
            "specs",
            "KIO-FOR-AGENTS.md",
            "TERMINOLOGY.md",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("cannot inventory tracked manual sources"));
    }
    let mut paths = Vec::new();
    for record in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|r| !r.is_empty())
    {
        let record = std::str::from_utf8(record).map_err(io::Error::other)?;
        let (metadata, path) = record
            .split_once('\t')
            .ok_or_else(|| io::Error::other("invalid Git inventory"))?;
        if !metadata.starts_with("100644 ") && !metadata.starts_with("100755 ") {
            return Err(io::Error::other(format!(
                "unsupported manual entry: {path}"
            )));
        }
        if !metadata.ends_with(" 0") || !valid_path(path) {
            return Err(io::Error::other(format!(
                "invalid manual inventory path: {path}"
            )));
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    for required in ["KIO-FOR-AGENTS.md", "TERMINOLOGY.md"] {
        if !paths.iter().any(|path| path == required) {
            return Err(io::Error::other(format!(
                "{required} must be tracked before bundling"
            )));
        }
    }
    pack_paths(root, &paths)
}

fn pack_paths(root: &Path, paths: &[String]) -> io::Result<Bundle> {
    let mut index = String::from("kio-manual-v1\n");
    let mut payload = Vec::new();
    let mut identity = blake3::Hasher::new();
    for path in paths {
        let source = root.join(path);
        let mut ancestor = source.parent();
        while let Some(directory) = ancestor.filter(|directory| *directory != root) {
            if !fs::symlink_metadata(directory)?.file_type().is_dir() {
                return Err(io::Error::other(format!(
                    "manual source has an unsupported ancestor: {path}"
                )));
            }
            ancestor = directory.parent();
        }
        if !fs::symlink_metadata(&source)?.file_type().is_file() {
            return Err(io::Error::other(format!(
                "manual source is not a regular file: {path}"
            )));
        }
        let bytes = fs::read(source)?;
        identity.update(&(path.len() as u64).to_le_bytes());
        identity.update(path.as_bytes());
        identity.update(&(bytes.len() as u64).to_le_bytes());
        identity.update(&bytes);
        let offset = payload.len();
        let gzip = flate2::GzBuilder::new()
            .mtime(0)
            .operating_system(255)
            .write(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(gzip);
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive.append_data(&mut header, path, bytes.as_slice())?;
        let compressed = archive.into_inner()?.finish()?;
        writeln!(
            index,
            "{}\t{}\t{}\t{}",
            path,
            bytes.len(),
            offset,
            compressed.len()
        )
        .unwrap();
        payload.extend_from_slice(&compressed);
    }
    writeln!(index, "identity\t{}", identity.finalize().to_hex()).unwrap();
    Ok(Bundle { index, payload })
}

/// Check a source package's self-contained snapshot without consulting Git.
pub fn validate(bundle: &Bundle) -> io::Result<()> {
    use std::io::Read;
    let invalid = || io::Error::other("invalid packaged manual snapshot");
    let mut lines = bundle.index.lines();
    if lines.next() != Some("kio-manual-v1") {
        return Err(invalid());
    }
    let mut identity = blake3::Hasher::new();
    let mut offset = 0usize;
    let mut previous = "";
    let mut found_guide = false;
    let mut found_terminology = false;
    while let Some(line) = lines.next() {
        if let Some(expected) = line.strip_prefix("identity\t") {
            return if lines.next().is_none()
                && offset == bundle.payload.len()
                && found_guide
                && found_terminology
                && expected == identity.finalize().to_hex().as_str()
            {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || !valid_path(fields[0]) || fields[0] <= previous {
            return Err(invalid());
        }
        let size: u64 = fields[1].parse().map_err(|_| invalid())?;
        let indexed_offset: usize = fields[2].parse().map_err(|_| invalid())?;
        let length: usize = fields[3].parse().map_err(|_| invalid())?;
        let end = offset.checked_add(length).ok_or_else(invalid)?;
        if indexed_offset != offset {
            return Err(invalid());
        }
        let compressed = bundle.payload.get(offset..end).ok_or_else(invalid)?;
        let gzip = flate2::bufread::GzDecoder::new(compressed);
        let header = gzip.header().ok_or_else(invalid)?;
        if header.mtime() != 0
            || header.operating_system() != 255
            || header.filename().is_some()
            || header.extra().is_some()
            || header.comment().is_some()
        {
            return Err(invalid());
        }
        let mut archive = tar::Archive::new(gzip);
        let mut files = archive.entries()?;
        let mut file = files.next().ok_or_else(invalid)??;
        if file.path()?.as_ref() != Path::new(fields[0])
            || file.size() != size
            || !file.header().entry_type().is_file()
            || file.header().mode()? != 0o644
            || file.header().uid()? != 0
            || file.header().gid()? != 0
            || file.header().mtime()? != 0
        {
            return Err(invalid());
        }
        identity.update(&(fields[0].len() as u64).to_le_bytes());
        identity.update(fields[0].as_bytes());
        identity.update(&size.to_le_bytes());
        let mut buffer = [0u8; 8192];
        let mut actual_size = 0u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            identity.update(&buffer[..count]);
            actual_size += count as u64;
        }
        if actual_size != size {
            return Err(invalid());
        }
        drop(file);
        if files.next().is_some() {
            return Err(invalid());
        }
        let mut gzip = archive.into_inner();
        let mut padding = 0usize;
        loop {
            let count = gzip.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if buffer[..count].iter().any(|byte| *byte != 0) {
                return Err(invalid());
            }
            padding += count;
        }
        // The iterator consumed the first zero header. Require the second
        // end marker and permit only complete zero-padding blocks after it.
        if padding < 512 || !padding.is_multiple_of(512) || !gzip.into_inner().is_empty() {
            return Err(invalid());
        }
        found_guide |= fields[0] == "KIO-FOR-AGENTS.md";
        found_terminology |= fields[0] == "TERMINOLOGY.md";
        previous = fields[0];
        offset = end;
    }
    Err(invalid())
}

pub fn valid_path(path: &str) -> bool {
    (matches!(path, "KIO-FOR-AGENTS.md" | "TERMINOLOGY.md")
        || path.starts_with("docs/")
        || path.starts_with("specs/"))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
}

pub fn is_checkout(core_root: &Path) -> bool {
    let Some(root) = core_root.parent() else {
        return false;
    };
    if core_root.file_name().is_none_or(|name| name != "kio-rs") {
        return false;
    }
    let Ok(top) = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return false;
    };
    if !top.status.success()
        || Path::new(String::from_utf8_lossy(&top.stdout).trim())
            .canonicalize()
            .ok()
            != root.canonicalize().ok()
    {
        return false;
    }
    Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--error-unmatch",
            "--",
            "AGENTS.md",
            "DESIGN.md",
            "ci/cargo.sh",
            "kio-rs/Cargo.toml",
        ])
        .output()
        .is_ok_and(|result| result.status.success())
}

pub fn snapshot(bundle: &Bundle, directory: &Path, check: bool) -> io::Result<()> {
    if check {
        if fs::read(directory.join("index.tsv"))? != bundle.index.as_bytes()
            || fs::read(directory.join("payload.tar.gz-concat"))? != bundle.payload
        {
            return Err(io::Error::other(
                "stale manual snapshot; run cargo run --example pack_manual -- --write",
            ));
        }
    } else {
        fs::create_dir_all(directory)?;
        fs::write(directory.join("index.tsv"), &bundle.index)?;
        fs::write(directory.join("payload.tar.gz-concat"), &bundle.payload)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write as IoWrite};

    fn git(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    fn fixture() -> (tempfile::TempDir, Vec<String>) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs/assets")).unwrap();
        fs::create_dir_all(root.path().join("specs")).unwrap();
        let long = format!("docs/assets/{}.bin", "asset-".repeat(30));
        let paths = vec![
            "KIO-FOR-AGENTS.md".into(),
            "TERMINOLOGY.md".into(),
            long,
            "specs/empty.md".into(),
        ];
        for (path, bytes) in
            paths
                .iter()
                .zip([b"guide".as_slice(), b"terminology", b"\0\xff\r\n", b""])
        {
            fs::write(root.path().join(path), bytes).unwrap();
        }
        (root, paths)
    }

    #[test]
    fn snapshot_validates_long_paths_binary_assets_and_empty_files() {
        let (root, paths) = fixture();
        let bundle = pack_paths(root.path(), &paths).unwrap();
        validate(&bundle).unwrap();
        let second = pack_paths(root.path(), &paths).unwrap();
        assert_eq!(bundle.index, second.index);
        assert_eq!(bundle.payload, second.payload);
        let snapshot_dir = root.path().join("snapshot");
        snapshot(&bundle, &snapshot_dir, false).unwrap();
        snapshot(&bundle, &snapshot_dir, true).unwrap();
        fs::write(root.path().join("TERMINOLOGY.md"), b"changed").unwrap();
        assert!(
            snapshot(
                &pack_paths(root.path(), &paths).unwrap(),
                &snapshot_dir,
                true
            )
            .is_err()
        );
    }

    #[test]
    fn packaged_validation_rejects_corruption_and_mismatched_index() {
        let (root, paths) = fixture();
        let bundle = pack_paths(root.path(), &paths).unwrap();
        let mut corrupt = Bundle {
            index: bundle.index.clone(),
            payload: bundle.payload.clone(),
        };
        corrupt.payload[20] ^= 0xff;
        assert!(validate(&corrupt).is_err());
        let changed = Bundle {
            index: bundle.index.replace("KIO-FOR-AGENTS.md", "docs/wrong.md"),
            payload: bundle.payload,
        };
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn both_root_references_are_required_in_checkout_and_packaged_inventories() {
        let (root, paths) = fixture();
        git(root.path(), &["init", "--quiet"]);
        for omitted in ["KIO-FOR-AGENTS.md", "TERMINOLOGY.md"] {
            let partial: Vec<_> = paths
                .iter()
                .filter(|path| *path != omitted)
                .cloned()
                .collect();
            assert!(validate(&pack_paths(root.path(), &partial).unwrap()).is_err());
            git(root.path(), &["read-tree", "--empty"]);
            let mut add = vec!["add", "--"];
            add.extend(partial.iter().map(String::as_str));
            git(root.path(), &add);
            assert!(assemble(root.path()).is_err());
        }
    }

    #[test]
    fn packaged_validation_rejects_hidden_entries_and_incomplete_end_markers() {
        let (root, _) = fixture();
        let bundle = pack_paths(
            root.path(),
            &["KIO-FOR-AGENTS.md".into(), "TERMINOLOGY.md".into()],
        )
        .unwrap();
        let mut original = Vec::new();
        flate2::read::GzDecoder::new(bundle.payload.as_slice())
            .read_to_end(&mut original)
            .unwrap();
        let repack = |decoded: &[u8]| {
            let mut encoder = flate2::GzBuilder::new()
                .mtime(0)
                .operating_system(255)
                .write(Vec::new(), flate2::Compression::default());
            encoder.write_all(decoded).unwrap();
            let mut payload = encoder.finish().unwrap();
            let mut lines = bundle.index.lines();
            let mut index = format!("{}\n", lines.next().unwrap());
            let first: Vec<_> = lines.next().unwrap().split('\t').collect();
            let second: Vec<_> = lines.next().unwrap().split('\t').collect();
            writeln!(index, "{}\t{}\t0\t{}", first[0], first[1], payload.len()).unwrap();
            writeln!(
                index,
                "{}\t{}\t{}\t{}",
                second[0],
                second[1],
                payload.len(),
                second[3]
            )
            .unwrap();
            writeln!(index, "{}", lines.next().unwrap()).unwrap();
            payload.extend_from_slice(&bundle.payload[first[3].parse::<usize>().unwrap()..]);
            Bundle { index, payload }
        };
        validate(&repack(&original)).unwrap();
        for path in ["docs/hidden.md", "../escape.md"] {
            let mut hidden = original.clone();
            let mut header = tar::Header::new_ustar();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // Write the traversal spelling directly: the adversarial archive
            // is deliberately outside tar::Builder's safe path interface.
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            hidden.extend_from_slice(header.as_bytes());
            hidden.push(b'x');
            hidden.resize(hidden.len() + 511 + 1024, 0);
            assert!(validate(&repack(&hidden)).is_err(), "hidden {path}");
        }
        for removed in [512, 1024] {
            assert!(validate(&repack(&original[..original.len() - removed])).is_err());
        }
        let mut partial = original;
        partial.push(0);
        assert!(validate(&repack(&partial)).is_err());
    }

    #[test]
    fn packaged_build_does_not_inventory_ambient_project_directories() {
        let (root, _) = fixture();
        let core = root.path().join("kio-rs");
        fs::create_dir(&core).unwrap();
        fs::write(root.path().join("AGENTS.md"), b"ambient instructions").unwrap();
        assert!(!is_checkout(&core));
        git(root.path(), &["init", "--quiet"]);
        git(root.path(), &["add", "--", "AGENTS.md", "docs", "specs"]);
        assert!(!is_checkout(&core));
        fs::create_dir(root.path().join("ci")).unwrap();
        for path in ["DESIGN.md", "ci/cargo.sh", "kio-rs/Cargo.toml"] {
            fs::write(root.path().join(path), b"checkout marker").unwrap();
        }
        git(
            root.path(),
            &["add", "--", "DESIGN.md", "ci/cargo.sh", "kio-rs/Cargo.toml"],
        );
        assert!(is_checkout(&core));
        let nested = root.path().join("nested/kio-rs");
        fs::create_dir_all(&nested).unwrap();
        assert!(!is_checkout(&nested));
    }

    #[test]
    fn paths_reject_escape_and_every_control_character() {
        for path in [
            "docs/../escape",
            "/docs/file",
            "docs//file",
            "docs/./file",
            "docs\\file",
            "README.md",
        ] {
            assert!(!valid_path(path));
        }
        for character in (0..=0x9f)
            .filter_map(char::from_u32)
            .filter(|c| c.is_control())
        {
            assert!(!valid_path(&format!("docs/{character}file")));
        }
    }

    #[test]
    fn tracked_inventory_handles_ignored_assets_and_source_mutations() {
        let (root, paths) = fixture();
        git(root.path(), &["init", "--quiet"]);
        fs::write(root.path().join(".gitignore"), b"docs/assets/\n").unwrap();
        git(
            root.path(),
            &[
                "add",
                "-f",
                "--",
                "KIO-FOR-AGENTS.md",
                "TERMINOLOGY.md",
                "docs",
                "specs",
            ],
        );
        fs::write(root.path().join("docs/untracked.md"), b"not bundled").unwrap();
        let first = assemble(root.path()).unwrap();
        assert!(first.index.contains(&paths[2]));
        assert!(first.index.contains("TERMINOLOGY.md"));
        assert!(!first.index.contains("untracked.md"));
        validate(&first).unwrap();
        let snapshot_dir = root.path().join("snapshot");
        snapshot(&first, &snapshot_dir, false).unwrap();
        fs::write(root.path().join("docs/added.md"), b"new tracked source").unwrap();
        git(root.path(), &["add", "--", "docs/added.md"]);
        let added = assemble(root.path()).unwrap();
        assert!(added.index.contains("docs/added.md"));
        assert!(snapshot(&added, &snapshot_dir, true).is_err());
        git(
            root.path(),
            &["mv", "--", "docs/added.md", "docs/renamed.md"],
        );
        let renamed = assemble(root.path()).unwrap();
        assert!(!renamed.index.contains("docs/added.md"));
        assert!(renamed.index.contains("docs/renamed.md"));
        assert!(snapshot(&renamed, &snapshot_dir, true).is_err());
        git(
            root.path(),
            &["rm", "-f", "--quiet", "--", "docs/renamed.md"],
        );
        assert_eq!(assemble(root.path()).unwrap().index, first.index);
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlinks_and_replaced_ancestor_directories_are_rejected() {
        use std::os::unix::fs::symlink;
        let (root, _) = fixture();
        git(root.path(), &["init", "--quiet"]);
        symlink("../KIO-FOR-AGENTS.md", root.path().join("docs/link.md")).unwrap();
        git(
            root.path(),
            &[
                "add",
                "--",
                "KIO-FOR-AGENTS.md",
                "TERMINOLOGY.md",
                "docs",
                "specs",
            ],
        );
        assert!(assemble(root.path()).is_err());
        git(root.path(), &["rm", "-f", "--quiet", "--", "docs/link.md"]);
        fs::rename(
            root.path().join("docs/assets"),
            root.path().join("moved-assets"),
        )
        .unwrap();
        symlink("../moved-assets", root.path().join("docs/assets")).unwrap();
        assert!(assemble(root.path()).is_err());
    }
}
