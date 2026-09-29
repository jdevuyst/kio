use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const IMPLEMENTATION_TAG: &str = "kio-rs";
const FEATURES: &[&str] = &["cli", "lsp", "parallel", "prime", "repl", "surface"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=src");

    for feature in FEATURES {
        println!(
            "cargo:rerun-if-env-changed=CARGO_FEATURE_{}",
            feature.to_ascii_uppercase()
        );
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let feature_set = feature_set();
    let mut h = blake3::Hasher::new();
    write_framed(&mut h, b"compiler-cache-id-v1");
    write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
    write_framed(
        &mut h,
        env::var("CARGO_PKG_VERSION")
            .expect("package version")
            .as_bytes(),
    );
    write_framed(&mut h, feature_set.as_bytes());
    hash_file(&mut h, &manifest_dir, Path::new("Cargo.toml"));
    hash_file(&mut h, &manifest_dir, Path::new("Cargo.lock"));
    hash_file(&mut h, &manifest_dir, Path::new("build.rs"));
    for rel in source_files(&manifest_dir.join("src")) {
        println!("cargo:rerun-if-changed={}", rel.display());
        hash_file(&mut h, &manifest_dir, &rel);
    }

    println!("cargo:rustc-env=KIO_FEATURE_SET={feature_set}");
    println!(
        "cargo:rustc-env=KIO_COMPILER_CACHE_ID={}",
        h.finalize().to_hex()
    );

    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
    println!("cargo:rustc-env=KIO_GIT_INFO={}", git_provenance());
}

/// Best-effort build provenance for `kio --version`. Empty means a
/// *release* build — either no git repository (a crates.io tarball, which
/// only ever holds a published release) or a clean checkout sitting exactly
/// on this version's `releases/v<CARGO_PKG_VERSION>` tag. Otherwise a short commit
/// hash, suffixed `-dirty` when tracked files are modified.
fn git_provenance() -> String {
    let Some(sha) = git(&["rev-parse", "--short=8", "HEAD"]) else {
        return String::new();
    };
    // `git()` returns `None` on empty stdout, so a clean tree (empty
    // `--porcelain` output) reads as not-dirty.
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let release_tag = format!("releases/v{version}");
    let on_release_tag = git(&[
        "describe",
        "--tags",
        "--exact-match",
        "--match",
        &release_tag,
        "HEAD",
    ])
    .is_some_and(|tag| tag == release_tag);
    if on_release_tag && !dirty {
        return String::new();
    }
    if dirty { format!("{sha}-dirty") } else { sha }
}

/// Run `git` with `args`, returning trimmed stdout on success and `None`
/// on any failure, non-zero exit, or empty output. Provenance stamping
/// only; a missing `git` degrades to a bare release version.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn feature_set() -> String {
    FEATURES
        .iter()
        .map(|feature| {
            let env_name = format!("CARGO_FEATURE_{}", feature.to_ascii_uppercase());
            format!("{feature}={}", env::var_os(env_name).is_some())
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn source_files(src_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_source_files(src_dir, src_dir, &mut files);
    files.sort();
    files
}

fn collect_source_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source directory") {
        let entry = entry.expect("read source directory entry");
        let path = entry.path();
        let ty = entry.file_type().expect("read source directory entry type");
        if ty.is_dir() {
            collect_source_files(root, &path, files);
        } else if ty.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("source file under source root");
            files.push(Path::new("src").join(rel));
        }
    }
}

fn hash_file(h: &mut blake3::Hasher, manifest_dir: &Path, rel: &Path) {
    let bytes = fs::read(manifest_dir.join(rel)).unwrap_or_else(|e| {
        panic!(
            "read compiler cache identity input `{}`: {e}",
            rel.display()
        )
    });
    write_framed(h, rel.to_string_lossy().as_bytes());
    write_framed(h, &bytes);
}

fn write_framed(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}
