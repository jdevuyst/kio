#[path = "../manual_pack.rs"]
mod manual_pack;

fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !manual_pack::is_checkout(crate_root) {
        return Err(std::io::Error::other(
            "manual packing requires a source checkout",
        ));
    }
    let bundle = manual_pack::assemble(crate_root.parent().unwrap())?;
    manual_pack::validate(&bundle)?;
    match args.as_slice() {
        [flag] if flag == "--check" => {
            manual_pack::snapshot(&bundle, &crate_root.join("manual"), true)
        }
        [flag] if flag == "--write" => {
            manual_pack::snapshot(&bundle, &crate_root.join("manual"), false)
        }
        [flag, path] if flag == "--check-package" => check_package(
            &bundle,
            std::path::Path::new(path),
            concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION")),
        ),
        _ => Err(std::io::Error::other(
            "usage: pack_manual --check|--write|--check-package <archive>",
        )),
    }
}

fn check_package(
    bundle: &manual_pack::Bundle,
    path: &std::path::Path,
    prefix: &str,
) -> std::io::Result<()> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(std::fs::File::open(path)?);
    let mut archive = tar::Archive::new(decoder);
    let expected = [
        (
            format!("{prefix}/manual/index.tsv"),
            bundle.index.as_bytes(),
        ),
        (
            format!("{prefix}/manual/payload.tar.gz-concat"),
            bundle.payload.as_slice(),
        ),
    ];
    let mut found = [false; 2];
    for entry in archive.entries()? {
        let mut entry = entry?;
        for (index, (name, bytes)) in expected.iter().enumerate() {
            if entry.path()?.as_ref() == std::path::Path::new(name) {
                let mut packaged = Vec::new();
                entry.read_to_end(&mut packaged)?;
                if found[index] || !entry.header().entry_type().is_file() || packaged != *bytes {
                    return Err(std::io::Error::other(format!(
                        "stale or invalid packaged manual: {name}"
                    )));
                }
                found[index] = true;
            }
        }
    }
    if !found.into_iter().all(|present| present) {
        return Err(std::io::Error::other(
            "source package is missing the complete manual",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_archive_requires_both_current_manual_files() {
        let root = tempfile::tempdir().unwrap();
        let bundle = manual_pack::Bundle {
            index: "current index".into(),
            payload: b"current payload".to_vec(),
        };
        let path = root.path().join("source.crate");
        for (index, payload, duplicate, passes) in [
            (
                bundle.index.as_bytes(),
                Some(bundle.payload.as_slice()),
                false,
                true,
            ),
            (
                b"stale".as_slice(),
                Some(bundle.payload.as_slice()),
                false,
                false,
            ),
            (
                bundle.index.as_bytes(),
                Some(b"stale".as_slice()),
                false,
                false,
            ),
            (bundle.index.as_bytes(), None, false, false),
            (
                bundle.index.as_bytes(),
                Some(bundle.payload.as_slice()),
                true,
                false,
            ),
        ] {
            let encoder = flate2::write::GzEncoder::new(
                std::fs::File::create(&path).unwrap(),
                flate2::Compression::default(),
            );
            let mut archive = tar::Builder::new(encoder);
            let mut append = |name: &str, bytes: &[u8]| {
                let mut header = tar::Header::new_ustar();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive.append_data(&mut header, name, bytes).unwrap();
            };
            append("fixture-version/manual/index.tsv", index);
            if let Some(payload) = payload {
                append("fixture-version/manual/payload.tar.gz-concat", payload);
            }
            if duplicate {
                append("fixture-version/manual/index.tsv", index);
            }
            archive.into_inner().unwrap().finish().unwrap();
            assert_eq!(
                check_package(&bundle, &path, "fixture-version").is_ok(),
                passes
            );
        }
    }
}
