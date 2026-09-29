//! Resolution of Git dependencies with an optional repository-relative manifest path.
//!
//! A git dependency is fetched into a per-user, content-addressed cache
//! and checked out at a pinned commit, then handed to the same re-root
//! logic a local `path` dependency uses (see
//! [`crate::package_collection::materialize_dependencies`]). This module
//! owns the fetch-and-pin half:
//!
//! - **Cache home.** Clones live under `$KIO_CACHE_HOME/git-deps/`,
//!   falling back to `$XDG_CACHE_HOME/kio/git-deps/` and then
//!   `$HOME/.cache/kio/git-deps/`, mirroring the REPL's XDG resolution.
//! - **Content addressing.** Each URL gets its own subtree keyed by a
//!   hash of the URL (`<cache>/git-deps/<url-hash>/`): a bare mirror
//!   clone at `repo.git`, plus one checked-out worktree per resolved
//!   commit at `<sha>/`. A cached mirror for the same URL is fetched and
//!   reused, never re-cloned; a worktree for an already-checked-out sha
//!   is reused as-is.
//! - **The lockfile is the pin.** A `<local>.lock.kio` beside the
//!   `<local>.dep.kio` records the `(url, ref, optional path, commit)` the ref resolved
//!   to. When it exists, resolution checks out the locked commit and
//!   never re-resolves the ref — reproducible builds. When it is absent
//!   (first resolve), the ref is resolved against the mirror and the
//!   lockfile is written. The lockfile is **committed** by users — it is
//!   the reproducibility pin, recording the resolved commit; the
//!   materialized module tree it gates is committed too.
//!
//! Git is driven through `std::process::Command`; no git library is
//! linked. Git failures are dependency-tier diagnostics at the URL or ref;
//! explicit manifest selection failures point at the selector.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::{GitSource, LockFile};
use crate::error::Error;
use crate::file_kind;
use crate::pass::resolve::LocatedError;
use crate::path_display::DisplayPath;
use crate::span::Span;

/// Resolve a `source { git; ref }` dependency to a checked-out
/// `*.pkg.kio` on disk.
///
/// `consumer_root` is the consuming package's root; `dep_file_path` is
/// the `<local>.dep.kio` declaring this dependency (the lockfile sits
/// beside it). `source` retains each declared field's diagnostic span.
///
/// On first resolve (no lockfile) the ref is resolved against a freshly
/// fetched mirror clone and the resulting `<local>.lock.kio` is written.
/// On a subsequent resolve the locked commit is checked out directly,
/// with no ref resolution — the build is reproducible.
pub fn resolve_git_dependency(
    consumer_root: &Path,
    dep_file_path: &Path,
    local_name: &str,
    source: &GitSource,
) -> Result<PathBuf, LocatedError> {
    let url = source.url.as_str();
    let git_ref = source.git_ref.as_str();
    let dep_err = |message| source_error(dep_file_path, source.url_span, message);
    let ref_err = |message| source_error(dep_file_path, source.ref_span, message);

    let cache_root = cache_home().ok_or_else(|| {
        dep_err(format!(
            "dependency `{local_name}`: cannot resolve a git-clone cache directory — set \
             `$KIO_CACHE_HOME`, `$XDG_CACHE_HOME`, or `$HOME`"
        ))
    })?;
    let git_deps_root = cache_root.join("git-deps");
    let url_dir = git_deps_root.join(url_hash(url));
    let mirror_dir = url_dir.join("repo.git");

    // The lockfile (`<local>.lock.kio`) is the pin. When present, its
    // recorded commit is used verbatim and the ref is never re-resolved.
    let lock_path = lock_path_for(dep_file_path);
    let existing_lock = read_lock(&lock_path, local_name)?;

    // Determine the commit to check out, fetching / cloning the mirror
    // only when we actually need to resolve a ref. A present lock with a
    // matching url + ref + selector short-circuits the network entirely *if* the
    // commit is already checked out; otherwise we still need the mirror
    // to populate the worktree.
    let commit = match &existing_lock {
        Some(lock) => {
            // A lockfile whose declared source identity no longer matches the
            // `.dep.kio` is stale: the declared source changed but the
            // pin was not refreshed. Reject rather than silently honor a
            // pin for a different source.
            if lock.url != url {
                return Err(dep_err(format!(
                    "dependency `{local_name}`: the lockfile `{}` pins git URL `{}`, but \
                     `{}` declares `{}`. Delete the lockfile to re-resolve, or restore the \
                     URL.",
                    DisplayPath(&lock_path),
                    lock.url,
                    DisplayPath(dep_file_path),
                    url,
                )));
            }
            if lock.git_ref != git_ref {
                return Err(ref_err(format!(
                    "dependency `{local_name}`: the lockfile `{}` pins ref `{}`, but `{}` \
                     declares `{}`. Delete the lockfile to re-resolve against the new ref.",
                    DisplayPath(&lock_path),
                    lock.git_ref,
                    DisplayPath(dep_file_path),
                    git_ref,
                )));
            }
            if lock.manifest_path.as_deref() != manifest_path(source) {
                let span = source
                    .manifest_path
                    .as_ref()
                    .map_or(source.url_span, |path| path.span);
                return Err(source_error(
                    dep_file_path,
                    span,
                    format!(
                        "dependency `{local_name}`: the lockfile `{}` pins manifest path {}, but \
                     `{}` declares {}. Run `kio dep update {local_name}` to select the new \
                     source, or restore the declaration.",
                        DisplayPath(&lock_path),
                        describe_manifest_path(lock.manifest_path.as_deref()),
                        DisplayPath(dep_file_path),
                        describe_manifest_path(manifest_path(source)),
                    ),
                ));
            }
            lock.commit.clone()
        }
        None => {
            // First resolve: fetch the mirror and resolve the ref to a
            // concrete commit. The lock is written *after* the checkout
            // below so the recorded `sig` (the dependency's contract-surface
            // digest at this commit) can be computed from the checked-out
            // package — the digest needs the tree on disk.
            ensure_mirror(&mirror_dir, url, FetchPolicy::OfflineReuseOk, &dep_err)?;
            resolve_ref_to_commit(&mirror_dir, git_ref, &ref_err)?
        }
    };

    // Check out the pinned commit into its content-addressed worktree.
    // A worktree already holding this commit is reused; otherwise the
    // mirror is fetched (if not already present) and the tree extracted.
    let checkout_dir = url_dir.join(&commit);
    if !is_complete_checkout(&checkout_dir) {
        ensure_mirror(&mirror_dir, url, FetchPolicy::OfflineReuseOk, &dep_err)?;
        checkout_commit(&mirror_dir, &checkout_dir, &commit, &dep_err)?;
    }

    let pkg_file = locate_source_package(
        &checkout_dir,
        source,
        local_name,
        consumer_root,
        dep_file_path,
    )?;

    // First resolve only: pin the commit *and* the dependency's
    // contract-surface digest at it. A present lock is honored verbatim
    // (its commit is reproducible; re-pinning is `kio dep update`'s job),
    // so the digest is recorded once, here, and refreshed only by update.
    if existing_lock.is_none() {
        let sig = dependency_contract_digest(&pkg_file, local_name, &dep_err)?;
        write_lock(&lock_path, local_name, source, &commit, &sig, &dep_err)?;
    }

    Ok(pkg_file)
}

/// The outcome of re-resolving (re-pinning) a git dependency's `ref`
/// during `kio dep update`: the commit the lockfile held before, and the
/// commit the `ref` resolves to now.
pub struct LockUpdate {
    /// The commit the `<local>.lock.kio` pinned before this update, or
    /// `None` when no lockfile existed yet (an unlocked dependency that
    /// `update` pins for the first time).
    pub old_commit: Option<String>,
    /// The commit the `ref` resolves to now — the lockfile's new pin.
    pub new_commit: String,
    /// The contract-compatibility gate result for the re-pin, present only
    /// when the gate ran and found a **breaking** change that the lock was
    /// nonetheless rewritten across — i.e. an unsealed break, or a sealed
    /// break overridden by `--allow-breaking`. The command turns it into a
    /// warning. A compatible move, an unchanged pin, or a first lock
    /// carries `None` (nothing to warn about). A *blocked* sealed break
    /// never reaches here: it returns an error and the lock is left intact.
    pub gate: Option<ContractGate>,
    /// The lockfile write this re-pin computed, **not yet committed** —
    /// [`update_git_lock`] stages it so the caller can write every selected
    /// dependency's lock only after all have passed their gate, keeping the
    /// re-pin atomic.
    pub staged_lock: StagedLock,
}

impl LockUpdate {
    /// Whether the re-pin left the commit unchanged: a lockfile existed
    /// and already held the freshly-resolved commit.
    pub fn is_unchanged(&self) -> bool {
        self.old_commit.as_deref() == Some(self.new_commit.as_str())
    }
}

/// Re-resolve a `source { git; ref }` dependency's `ref` to the commit it
/// designates **now**, run the contract-compatibility honesty gate, and
/// (when the gate permits) rewrite the `<local>.lock.kio` to that commit
/// and its fresh contract digest, **ignoring** any commit a present
/// lockfile already pins. This is the re-pinning half of `kio dep update`:
/// unlike [`resolve_git_dependency`] — which honors an existing lock for
/// reproducibility and only resolves a ref on the first (unlocked) resolve
/// — this always fetches the mirror and resolves the ref afresh.
///
/// **The honesty gate.** When the re-pin moves the commit and a prior pin
/// exists, the dependency's contract surface at the old commit is compared
/// (via the same [`crate::sig::compare`] the package-versioning relation
/// uses) against the new commit's:
///
/// - **compatible** → the lock is rewritten (commit + new digest); exit 0.
/// - **breaking** on a **sealed** dependency contract → an **error**; the
///   lock is left intact, so the consumer stays pinned to the old commit,
///   unless `allow_breaking` downgrades it to a warning and proceeds.
/// - **breaking** on an **unsealed** dependency contract → a **warning**;
///   the lock is rewritten.
///
/// The old and new contracts are recovered by checking out each commit
/// through the shared checkout machinery and projecting its surface (the
/// sealed `<pkg>.sig.kio` interface, or the live bridge-reachable surface
/// when the dependency ships no sealed changelog). `consumer_root` is the
/// consuming package's root, needed by [`locate_package_file`].
pub fn update_git_lock(
    consumer_root: &Path,
    dep_file_path: &Path,
    local_name: &str,
    source: &GitSource,
    allow_breaking: bool,
) -> Result<LockUpdate, LocatedError> {
    let url = source.url.as_str();
    let git_ref = source.git_ref.as_str();
    let dep_err = |message| source_error(dep_file_path, source.url_span, message);
    let ref_err = |message| source_error(dep_file_path, source.ref_span, message);

    let cache_root = cache_home().ok_or_else(|| {
        dep_err(format!(
            "dependency `{local_name}`: cannot resolve a git-clone cache directory — set \
             `$KIO_CACHE_HOME`, `$XDG_CACHE_HOME`, or `$HOME`"
        ))
    })?;
    let url_dir = cache_root.join("git-deps").join(url_hash(url));
    let mirror_dir = url_dir.join("repo.git");

    // The commit the lock pinned before, if any. A lockfile whose source
    // identity no longer matches the `.dep.kio` is stale; `update` is the
    // operation that *resolves* such drift, so rather than rejecting it
    // (as the reproducible resolve path does) we re-pin against the
    // current declaration and treat the old commit as the prior pin only
    // when the lock's URL + ref + selector still match — a re-pin onto a different
    // source is a fresh pin, not an A→B move of the same line.
    let lock_path = lock_path_for(dep_file_path);
    let old_commit = read_lock(&lock_path, local_name)?
        .and_then(|lock| lock_matches_source(&lock, source).then_some(lock.commit));

    // Always fetch + resolve afresh: `update` re-pins to the ref's
    // current commit, never the lock's old one. A fetch failure here is
    // fatal — re-pinning to a stale cached commit when the
    // remote is unreachable would silently contradict resolving the `ref`
    // to "now".
    ensure_mirror(&mirror_dir, url, FetchPolicy::RequireFetch, &dep_err)?;
    let new_commit = resolve_ref_to_commit(&mirror_dir, git_ref, &ref_err)?;

    // Derive the new commit's contract surface (its sealed status gates
    // the break, its digest is recorded in the lock).
    let new_pkg = checkout_and_locate(
        &url_dir,
        &mirror_dir,
        &new_commit,
        source,
        local_name,
        consumer_root,
        dep_file_path,
    )?;
    let new_contract = dependency_contract(&new_pkg, local_name, &dep_err)?;
    let new_sig = crate::sig::contract_digest(&new_contract.snapshot);

    // The honesty gate runs only on an actual A→B move with a prior pin.
    // A first lock has no baseline to break; an unchanged pin moves
    // nothing.
    let gate = if let Some(old) = &old_commit
        && old != &new_commit
    {
        let old_pkg = checkout_and_locate(
            &url_dir,
            &mirror_dir,
            old,
            source,
            local_name,
            consumer_root,
            dep_file_path,
        )?;
        let old_contract = dependency_contract(&old_pkg, local_name, &dep_err)?;
        let report = crate::sig::compare(&old_contract.snapshot, &new_contract.snapshot);
        if report.is_breaking() {
            let reasons = report.breaking().map(|c| c.detail.clone()).collect();
            Some(ContractGate {
                breaking: true,
                sealed: new_contract.sealed,
                reasons,
            })
        } else {
            None
        }
    } else {
        None
    };

    // A breaking change on a sealed dependency contract blocks the re-pin
    // unless `--allow-breaking` overrides it. When blocked, the lock is
    // left intact — the consumer stays on the reproducible old commit.
    if let Some(gate) = &gate
        && gate.breaking
        && gate.sealed
        && !allow_breaking
    {
        let mut message = format!(
            "dependency `{local_name}`: re-pinning `{ref}` from {old} to {new} would adopt a \
             breaking change to its sealed contract surface:",
            ref = git_ref,
            old = short(old_commit.as_deref().unwrap_or("")),
            new = short(&new_commit),
        );
        for reason in &gate.reasons {
            message.push_str("\n  - ");
            message.push_str(reason);
        }
        message.push_str(
            "\nthe lock was left unchanged (still pinned to the old commit). Re-run with \
             `kio dep update --allow-breaking` to adopt the break anyway.",
        );
        return Err(dep_err(message));
    }

    // Stage — do not write — the new lock. `kio dep update` writes every
    // selected dependency's staged lock only after all have passed their
    // gate, so a later dependency's blocked break leaves this one's lock
    // un-advanced (and its committed tree consistent). The write itself is
    // content-addressed at commit time.
    let staged_lock = StagedLock {
        lock_path,
        local_name: local_name.to_owned(),
        span: source.url_span,
        dep_file_path: dep_file_path.to_path_buf(),
        text: render_lock(local_name, source, &new_commit, &new_sig),
    };

    Ok(LockUpdate {
        old_commit,
        new_commit,
        gate,
        staged_lock,
    })
}

/// Check out `commit` into its content-addressed worktree (reusing a
/// populated one) and locate the dependency's `*.pkg.kio` within it.
/// Shared by the new- and old-commit contract derivations in
/// [`update_git_lock`].
fn checkout_and_locate(
    url_dir: &Path,
    mirror_dir: &Path,
    commit: &str,
    source: &GitSource,
    local_name: &str,
    consumer_root: &Path,
    dep_file_path: &Path,
) -> Result<PathBuf, LocatedError> {
    let dep_err = |message| source_error(dep_file_path, source.url_span, message);
    let checkout_dir = url_dir.join(commit);
    if !is_complete_checkout(&checkout_dir) {
        ensure_mirror(
            mirror_dir,
            &source.url,
            FetchPolicy::OfflineReuseOk,
            &dep_err,
        )?;
        checkout_commit(mirror_dir, &checkout_dir, commit, &dep_err)?;
    }
    locate_source_package(
        &checkout_dir,
        source,
        local_name,
        consumer_root,
        dep_file_path,
    )
}

fn source_error(dep_file_path: &Path, span: Span, message: String) -> LocatedError {
    LocatedError {
        file_path: dep_file_path.to_path_buf(),
        error: Error::dep(span, message),
    }
}

fn manifest_path(source: &GitSource) -> Option<&str> {
    source.manifest_path.as_ref().map(|path| path.path.as_str())
}

fn lock_matches_source(lock: &LockFile, source: &GitSource) -> bool {
    lock.url == source.url
        && lock.git_ref == source.git_ref
        && lock.manifest_path.as_deref() == manifest_path(source)
}

fn describe_manifest_path(path: Option<&str>) -> String {
    path.map_or_else(
        || "<automatic discovery>".to_owned(),
        |path| format!("`{path}`"),
    )
}

/// The 12-char abbreviation of a 40-char commit SHA for a diagnostic; a
/// non-SHA-shaped string is shown verbatim. Mirrors the display
/// abbreviation `cmd::dep` uses for its report lines.
fn short(commit: &str) -> &str {
    if commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        &commit[..12]
    } else {
        commit
    }
}

/// The per-user git-clone cache directory: `$KIO_CACHE_HOME`, falling
/// back to `$XDG_CACHE_HOME/kio` and then `$HOME/.cache/kio`. `None`
/// when none of the three environment variables yields an absolute base
/// (the XDG spec ignores a relative `$XDG_CACHE_HOME`).
fn cache_home() -> Option<PathBuf> {
    cache_home_from(
        std::env::var_os("KIO_CACHE_HOME").map(PathBuf::from),
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Pure cache-home resolution, factored out of [`cache_home`] so it can
/// be unit-tested without mutating the process environment. `$KIO_CACHE_HOME`
/// wins when absolute; otherwise `$XDG_CACHE_HOME/kio` when `$XDG_CACHE_HOME`
/// is absolute (per the XDG Base Directory spec a relative value is
/// ignored); otherwise `$HOME/.cache/kio`.
fn cache_home_from(
    kio_cache_home: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(k) = kio_cache_home
        && k.is_absolute()
    {
        return Some(k);
    }
    if let Some(xdg) = xdg_cache_home
        && xdg.is_absolute()
    {
        return Some(xdg.join("kio"));
    }
    let home = home?;
    Some(home.join(".cache").join("kio"))
}

/// A filesystem-safe, collision-resistant key for a clone URL. BLAKE3 of
/// the URL bytes, hex-encoded — the same hash family the enriched-IR and
/// kiodoc caches use. URLs vary wildly in shape (`file://`, `https://`,
/// `git@…:…`), so hashing sidesteps every path-character pitfall.
fn url_hash(url: &str) -> String {
    blake3::hash(url.as_bytes()).to_hex().to_string()
}

/// The `<local>.lock.kio` path beside a `<local>.dep.kio`.
fn lock_path_for(dep_file_path: &Path) -> PathBuf {
    let stem = dep_file_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(file_kind::dep_stem)
        .unwrap_or("dep");
    let dir = dep_file_path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}{}", file_kind::LOCK_KIO_SUFFIX))
}

/// Read and parse an existing `<local>.lock.kio`, returning `None` when
/// the file is absent. A present-but-malformed lockfile is a dependency
/// error (the committed pin is corrupt).
fn read_lock(lock_path: &Path, local_name: &str) -> Result<Option<LockFile>, LocatedError> {
    let source = match std::fs::read_to_string(lock_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(LocatedError {
                file_path: lock_path.to_path_buf(),
                error: Error::dep(
                    Span::new(0, 0),
                    format!(
                        "dependency `{local_name}`: cannot read lockfile `{}`: {e}",
                        DisplayPath(lock_path)
                    ),
                ),
            });
        }
    };
    let lock = crate::pass::parser::parse_lock_file(&source, Some(local_name)).map_err(|err| {
        let (span, message) = err.diag();
        LocatedError {
            file_path: lock_path.to_path_buf(),
            error: Error::dep(
                span,
                format!(
                    "dependency `{local_name}`: lockfile `{}` is malformed: {message}",
                    DisplayPath(lock_path)
                ),
            ),
        }
    })?;
    Ok(Some(lock))
}

/// Render a freshly resolved `<local>.lock.kio` to its canonical
/// (`kio fmt`) text. Shared by the immediate first-resolve write
/// ([`write_lock`]) and the deferred `kio dep update` write
/// ([`StagedLock`]).
fn render_lock(local_name: &str, source: &GitSource, commit: &str, sig: &str) -> String {
    let lock = LockFile {
        field_leading_trivia: Default::default(),
        resolved_leading_trivia: Vec::new(),
        trailing_trivia: Vec::new(),
        name: local_name.to_owned(),
        url: source.url.clone(),
        git_ref: source.git_ref.clone(),
        manifest_path: manifest_path(source).map(str::to_owned),
        commit: commit.to_owned(),
        sig: sig.to_owned(),
        meta: crate::ast::Meta::new(Span::new(0, 0)),
    };
    crate::pretty::pretty_lock_file(&lock)
}

/// Write `text` to `lock_path`, content-addressed: a lockfile already
/// holding the identical bytes is left untouched.
fn write_lock_text(
    lock_path: &Path,
    local_name: &str,
    text: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<(), LocatedError> {
    if let Ok(existing) = std::fs::read_to_string(lock_path)
        && existing == text
    {
        return Ok(());
    }
    std::fs::write(lock_path, text).map_err(|e| {
        dep_err(format!(
            "dependency `{local_name}`: cannot write lockfile `{}`: {e}",
            DisplayPath(lock_path)
        ))
    })
}

/// Write a freshly resolved `<local>.lock.kio`, pretty-printed so it is
/// canonical Kio. Written content-addressed: a lockfile already holding
/// the identical bytes is left untouched.
fn write_lock(
    lock_path: &Path,
    local_name: &str,
    source: &GitSource,
    commit: &str,
    sig: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<(), LocatedError> {
    let text = render_lock(local_name, source, commit, sig);
    write_lock_text(lock_path, local_name, &text, dep_err)
}

/// A `<local>.lock.kio` write that `kio dep update` has computed but not
/// yet committed. Staging the write — rather than performing it inside
/// [`update_git_lock`] — is what lets `kio dep update` apply every
/// dependency's re-pin **atomically**: the writes happen only after every
/// selected dependency has passed its honesty gate, so a blocked sealed
/// break leaves no lock advanced and therefore no committed tree stale
/// against an advanced lock. See [`crate::cmd::dep`]'s
/// `update_at`.
pub struct StagedLock {
    lock_path: PathBuf,
    local_name: String,
    /// The defining-token span of the dependency's origin, so a write
    /// failure anchors its diagnostic the same place a resolve failure does.
    span: Span,
    dep_file_path: PathBuf,
    text: String,
}

impl StagedLock {
    /// Commit the staged lockfile to disk (content-addressed: an identical
    /// existing file is left untouched). A write failure is a dependency
    /// error anchored at the dependency's origin token.
    pub fn commit(&self) -> Result<(), LocatedError> {
        let dep_err = |message: String| LocatedError {
            file_path: self.dep_file_path.clone(),
            error: Error::dep(self.span, message),
        };
        write_lock_text(&self.lock_path, &self.local_name, &self.text, &dep_err)
    }
}

/// Whether a fetch failure against an already-present mirror is fatal.
///
/// `kio dep update` must re-resolve a dependency's `ref` against the
/// remote's **current** state, so a failed fetch (e.g. offline) is a
/// dependency error: it must not silently re-pin to a stale cached commit.
/// The reproducible resolve / build path instead reuses an
/// already-fetched ref offline, so its fetch failure is non-fatal — a
/// genuinely missing ref still surfaces later at ref-resolution time.
#[derive(Clone, Copy)]
enum FetchPolicy {
    /// Reuse the cached mirror when a fetch fails (offline resolve / build).
    OfflineReuseOk,
    /// Require the fetch to succeed; a failure is a dependency error
    /// (`kio dep update` re-pinning to the ref's current commit).
    RequireFetch,
}

/// Ensure a bare mirror clone of `url` exists at `mirror_dir`, fetching
/// updates when it is already present. A mirror (`--mirror`) tracks every
/// ref, so any branch / tag the `ref` names is resolvable from it.
/// `fetch_policy` governs whether a fetch failure on an existing mirror is
/// fatal (see [`FetchPolicy`]).
fn ensure_mirror(
    mirror_dir: &Path,
    url: &str,
    fetch_policy: FetchPolicy,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<(), LocatedError> {
    if is_git_dir(mirror_dir) {
        // Reuse the cached mirror; fetch so a moved branch / new tag is
        // visible. The fetch outcome is fatal only under
        // `FetchPolicy::RequireFetch` (`kio dep update`): re-pinning to a
        // stale cached commit when the remote is unreachable would be a
        // silent lie about resolving the `ref` to "now". Under
        // `OfflineReuseOk` a fetch failure is ignored — an offline reuse of
        // an already-fetched ref must still work, and a genuinely missing
        // ref surfaces later at resolve / checkout time with a precise
        // message.
        let fetched = run_git(
            mirror_dir.parent().unwrap_or(mirror_dir),
            [
                "--git-dir",
                path_arg(mirror_dir),
                "fetch",
                "--prune",
                "origin",
            ],
        );
        if let Err(stderr) = fetched
            && matches!(fetch_policy, FetchPolicy::RequireFetch)
        {
            return Err(dep_err(format!(
                "cannot fetch the latest commits of the git dependency from `{url}`: {stderr}. \
                 `kio dep update` re-resolves `ref` against the remote, so a fetch failure \
                 cannot silently re-pin to a stale cached commit; re-run when the remote is \
                 reachable, or use `kio dep fetch` to build against the locked commit."
            )));
        }
        return Ok(());
    }
    if let Some(parent) = mirror_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            dep_err(format!(
                "cannot create git-clone cache directory `{}`: {e}",
                DisplayPath(parent)
            ))
        })?;
    }
    let work = mirror_dir.parent().unwrap_or(mirror_dir);
    // `--end-of-options` separates the flags from the `<repo> <dir>`
    // positionals so a `-`-leading URL can't be parsed as a git flag —
    // the same hardening `resolve_ref_to_commit` already applies to its
    // ref argument.
    run_git(
        work,
        [
            "clone",
            "--mirror",
            "--quiet",
            "--end-of-options",
            url,
            path_arg(mirror_dir),
        ],
    )
    .map_err(|stderr| {
        dep_err(format!(
            "cannot clone git dependency from `{url}` into `{}`: {stderr}",
            DisplayPath(mirror_dir)
        ))
    })?;
    Ok(())
}

/// Resolve a ref (branch, tag, or commit SHA) against a mirror to the
/// full 40-char commit SHA. `git rev-parse <ref>^{commit}` resolves a
/// branch / tag / abbreviated SHA to the commit it designates uniformly.
fn resolve_ref_to_commit(
    mirror_dir: &Path,
    git_ref: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<String, LocatedError> {
    // `<ref>^{commit}` peels an annotated tag to its commit and is a
    // no-op for a branch / lightweight tag / commit. The mirror is bare,
    // so `--git-dir` is given explicitly.
    let spec = format!("{git_ref}^{{commit}}");
    let out = run_git_stdout(
        mirror_dir,
        [
            "--git-dir",
            path_arg(mirror_dir),
            "rev-parse",
            "--verify",
            "--end-of-options",
            &spec,
        ],
    )
    .map_err(|stderr| {
        dep_err(format!(
            "cannot resolve ref `{git_ref}` in the git dependency: {stderr}"
        ))
    })?;
    let commit = out.trim().to_owned();
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(dep_err(format!(
            "ref `{git_ref}` resolved to `{commit}`, which is not a 40-character commit SHA"
        )));
    }
    Ok(commit)
}

/// Extract `commit` from the mirror into a fresh worktree at
/// `checkout_dir`. Uses a temporary index and `git --work-tree …
/// checkout` so no branch is created in the bare mirror and the worktree
/// is a clean snapshot of the commit.
fn checkout_commit(
    mirror_dir: &Path,
    checkout_dir: &Path,
    commit: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<(), LocatedError> {
    // A stale, half-populated checkout (an interrupted previous run)
    // would shadow this one; remove it first so the extraction starts
    // clean. A *complete* checkout (one carrying the completeness
    // sentinel) was already short-circuited by the caller, so this only
    // ever clears an incomplete tree.
    if checkout_dir.exists() {
        std::fs::remove_dir_all(checkout_dir).map_err(|e| {
            dep_err(format!(
                "cannot clear stale git checkout `{}`: {e}",
                DisplayPath(checkout_dir)
            ))
        })?;
    }
    std::fs::create_dir_all(checkout_dir).map_err(|e| {
        dep_err(format!(
            "cannot create git checkout directory `{}`: {e}",
            DisplayPath(checkout_dir)
        ))
    })?;
    // `checkout <commit> -- .` populates the work tree from the commit
    // without moving HEAD in the bare mirror. A throwaway index keeps the
    // mirror's own (empty) index untouched and avoids cross-run races.
    let index_file = checkout_dir.join(".kio-checkout-index");
    let run = Command::new("git")
        .current_dir(mirror_dir)
        .env("GIT_DIR", mirror_dir)
        .env("GIT_WORK_TREE", checkout_dir)
        .env("GIT_INDEX_FILE", &index_file)
        .args(["checkout", "--quiet", commit, "--", "."])
        .output();
    let _ = std::fs::remove_file(&index_file);
    let out = run.map_err(|e| {
        dep_err(format!(
            "cannot run `git checkout {commit}` for the git dependency: {e}"
        ))
    })?;
    if !out.status.success() {
        // Clean up the partial checkout so a retry starts fresh.
        let _ = std::fs::remove_dir_all(checkout_dir);
        return Err(dep_err(format!(
            "cannot check out commit `{commit}` of the git dependency: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // Mark the checkout complete only after a fully-successful extraction.
    // A process killed mid-`git checkout` leaves a populated-but-partial
    // tree with no sentinel, which `is_complete_checkout` treats as not
    // usable so it is re-extracted rather than silently reused.
    std::fs::write(checkout_dir.join(CHECKOUT_COMPLETE_SENTINEL), b"").map_err(|e| {
        dep_err(format!(
            "cannot finalize the git checkout `{}`: {e}",
            DisplayPath(checkout_dir)
        ))
    })?;
    Ok(())
}

fn locate_source_package(
    checkout_dir: &Path,
    source: &GitSource,
    local_name: &str,
    consumer_root: &Path,
    dep_file_path: &Path,
) -> Result<PathBuf, LocatedError> {
    match &source.manifest_path {
        Some(selector) => {
            locate_selected_package(checkout_dir, &selector.path, local_name, &|message| {
                source_error(dep_file_path, selector.span, message)
            })
        }
        None => locate_package_file(checkout_dir, local_name, consumer_root, &|message| {
            source_error(dep_file_path, source.url_span, message)
        }),
    }
}

/// Resolve the written path without lexical normalization: a `..` after a
/// symlink must retain the filesystem's meaning before containment is checked.
fn locate_selected_package(
    checkout_dir: &Path,
    selector: &str,
    local_name: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<PathBuf, LocatedError> {
    let checkout = std::fs::canonicalize(checkout_dir).map_err(|e| {
        dep_err(format!(
            "dependency `{local_name}`: cannot resolve the git checkout `{}`: {e}",
            DisplayPath(checkout_dir),
        ))
    })?;
    let candidate = std::fs::canonicalize(checkout_dir.join(selector)).map_err(|e| {
        dep_err(format!(
            "dependency `{local_name}`: manifest path `{selector}` does not resolve to an \
             existing package file inside the git checkout: {e}",
        ))
    })?;
    if !candidate.starts_with(&checkout) {
        return Err(dep_err(format!(
            "dependency `{local_name}`: manifest path `{selector}` resolves outside the \
             git checkout; choose a `*.pkg.kio` file inside the checkout",
        )));
    }
    if !candidate.is_file()
        || !candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(file_kind::is_package_file)
    {
        return Err(dep_err(format!(
            "dependency `{local_name}`: manifest path `{selector}` must name an existing \
             `*.pkg.kio` file, not a directory or another file kind (resolved to `{}`)",
            DisplayPath(&candidate),
        )));
    }
    Ok(candidate)
}

/// Find the dependency's `*.pkg.kio` inside a checked-out worktree. A
/// `*.pkg.kio` at the worktree root is the package file; if exactly one
/// lives in an immediate subdirectory instead, that subdirectory is the
/// package root (a repo whose package is one level down). More than one
/// candidate is ambiguous and rejected.
fn locate_package_file(
    checkout_dir: &Path,
    local_name: &str,
    consumer_root: &Path,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<PathBuf, LocatedError> {
    // The checkout lives in the cache, never under the consumer root —
    // assert it so a future cache-layout change can't silently re-root a
    // dependency inside the consumer tree.
    debug_assert!(
        !checkout_dir.starts_with(consumer_root),
        "git checkout must live in the cache, outside the consumer root"
    );
    let mut candidates = Vec::new();
    collect_package_files(checkout_dir, 1, &mut candidates).map_err(|e| {
        dep_err(format!(
            "dependency `{local_name}`: cannot scan the git checkout `{}` for a `*.pkg.kio`: {e}",
            DisplayPath(checkout_dir)
        ))
    })?;
    match candidates.len() {
        0 => Err(dep_err(format!(
            "dependency `{local_name}`: the git checkout `{}` contains no `*.pkg.kio` package \
             file (at its root or one directory down)",
            DisplayPath(checkout_dir)
        ))),
        1 => Ok(candidates.into_iter().next().unwrap()),
        _ => {
            candidates.sort();
            Err(dep_err(format!(
                "dependency `{local_name}`: the git checkout `{}` contains multiple `*.pkg.kio` \
                 package files ({}); a git dependency must resolve to exactly one package",
                DisplayPath(checkout_dir),
                candidates
                    .iter()
                    .map(|p| DisplayPath(p).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    }
}

/// Collect `*.pkg.kio` files at `dir` and (when `depth_left > 0`) one
/// level into each immediate subdirectory. The two-level search lets a
/// repo place its package either at the root or in a single
/// subdirectory, the two natural layouts, without an unbounded walk.
fn collect_package_files(
    dir: &Path,
    depth_left: usize,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_file() {
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(file_kind::is_package_file)
            {
                out.push(path);
            }
        } else if file_type.is_dir() && depth_left > 0 {
            // Skip the `.git` administrative directory and any
            // `.kio-cache` build output that might have been committed.
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            collect_package_files(&path, depth_left - 1, out)?;
        }
    }
    Ok(())
}

// ---- contract surface (the lock's `sig` digest + the update gate) -----------

/// A git dependency's contract surface at a pinned commit, plus whether
/// that surface is **sealed** (the dependency ships a `<pkg>.sig.kio` with
/// at least one committed version). The honesty gate keys error-vs-warning
/// on the new commit's sealed flag; the digest is recorded in the lock.
struct DepContract {
    snapshot: crate::sig::ContractSnapshot,
    sealed: bool,
}

/// Derive a checked-out dependency package's contract surface and sealed
/// status from the package file `pkg_file` (a `<pkg>.sig.kio` is looked
/// for beside it):
///
/// - **Sealed** — the dependency ships a `<pkg>.sig.kio` whose header
///   generation is `> v(1)` (at least one version is committed). The
///   contract is the **last sealed** interface, recovered by replaying the
///   changelog through `generation - 1`. Replay validates the fresh Kio'
///   signature artifact (including resolution, kinds, and recursive-type
///   soundness) but never typechecks module function bodies, so the result is
///   identical on `kio` and `kio-prime`.
/// - **Unsealed** — no `<pkg>.sig.kio`, or one still on its first
///   (uncommitted) draft `v(1)`. The contract is the dependency's live
///   bridge-reachable surface, projected from a typecheck of the package.
///
/// The contract surface is the same object the `kio sig` compatibility
/// relation compares; reusing it keeps the lock's `sig` and the update
/// gate consistent with the package-versioning contract.
fn dependency_contract(
    pkg_file: &Path,
    local_name: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<DepContract, LocatedError> {
    let pkg_dir = pkg_file.parent().unwrap_or_else(|| Path::new("."));
    let pkg_name = pkg_file
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(file_kind::package_stem)
        .ok_or_else(|| {
            dep_err(format!(
                "dependency `{local_name}`: cannot read the package name from `{}`",
                DisplayPath(pkg_file)
            ))
        })?;

    if let Some(file) = read_dep_sig_file(pkg_dir, pkg_name, local_name, dep_err)? {
        let generation = file.version;
        if generation > 1 {
            // Sealed: the contract a consumer commits to is the last
            // sealed interface (the open draft `v(generation)` is excluded
            // by replaying through `generation - 1`).
            let replayed = crate::sig::replay_through(&file, generation - 1).map_err(|e| {
                dep_err(format!(
                    "dependency `{local_name}`: replaying the sealed contract of `{pkg_name}`: {}",
                    e.diag().1
                ))
            })?;
            return Ok(DepContract {
                snapshot: replayed.current,
                sealed: true,
            });
        }
    }

    // Unsealed (no changelog, or a still-draft v(1)): the contract is the
    // live bridge-reachable surface, which needs a typecheck.
    let snapshot = live_contract_snapshot(pkg_dir, local_name, dep_err)?;
    Ok(DepContract {
        snapshot,
        sealed: false,
    })
}

/// The dependency's contract-surface digest at a pinned commit — what the
/// lock's `sig "<…>";` records. A thin wrapper over [`dependency_contract`]
/// that drops the sealed flag and hashes the snapshot.
fn dependency_contract_digest(
    pkg_file: &Path,
    local_name: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<String, LocatedError> {
    let contract = dependency_contract(pkg_file, local_name, dep_err)?;
    Ok(crate::sig::contract_digest(&contract.snapshot))
}

/// Read + parse a checked-out dependency's `<pkg>.sig.kio`, or `None` when
/// it ships none. A malformed changelog is a dependency error (the
/// dependency's committed contract record is corrupt).
fn read_dep_sig_file(
    pkg_dir: &Path,
    pkg_name: &str,
    local_name: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<Option<crate::ast::SignatureFile>, LocatedError> {
    let sig_path = pkg_dir.join(format!("{pkg_name}{}", file_kind::SIG_KIO_SUFFIX));
    let source = match std::fs::read_to_string(&sig_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(dep_err(format!(
                "dependency `{local_name}`: cannot read the contract changelog `{}`: {e}",
                DisplayPath(&sig_path)
            )));
        }
    };
    let file =
        crate::pass::parser::parse_signature_file(&source, Some(pkg_name)).map_err(|err| {
            let (_, message) = err.diag();
            dep_err(format!(
                "dependency `{local_name}`: the contract changelog `{}` is malformed: {message}",
                DisplayPath(&sig_path)
            ))
        })?;
    Ok(Some(file))
}

/// Project a checked-out dependency package's live bridge-reachable
/// contract surface from its **signatures**, without typechecking function
/// bodies. The package is parsed and reduced to its signature surface
/// (function and `rec`-member bodies blanked, surface-only auxiliary
/// declarations dropped — see
/// [`crate::cmd::check::contract_snapshot_via_lower`]), then lowered by the
/// running binary's front-end: the full `kio` pipeline when `surface` is
/// built, else the Kio'-only `kio-prime` pipeline.
///
/// Because the digest depends only on the signature surface — never on a
/// body — the two binaries compute the **identical** contract, including
/// for a dependency whose bodies use surface forms (`if`/`do`/`match`/
/// elaborators) that the surface-less `kio-prime` lowering would reject in
/// a full compile (`specs/versioning.md`
/// § Git-dependency contract gate).
fn live_contract_snapshot(
    pkg_dir: &Path,
    local_name: &str,
    dep_err: &impl Fn(String) -> LocatedError,
) -> Result<crate::sig::ContractSnapshot, LocatedError> {
    let parsed = crate::package_collection::walk(pkg_dir).map_err(|(walk_err, _)| {
        dep_err(format!(
            "dependency `{local_name}`: cannot read its package to compute its contract \
             surface: {}",
            walk_err.into_located().error.diag().1
        ))
    })?;
    let root = parsed.packages.get(&parsed.root).ok_or_else(|| {
        dep_err(format!(
            "dependency `{local_name}`: its package produced no root to compute a contract \
             surface from"
        ))
    })?;
    let wrap = |located: LocatedError| {
        dep_err(format!(
            "dependency `{local_name}`: its contract surface cannot be computed: {}",
            located.error.diag().1
        ))
    };
    // Both binaries consume the *same* body-blanked signature surface and
    // read only signatures, so they produce the identical contract. Exactly
    // one branch is live per build (`git_dep` is `cli`-gated, and `cli`
    // always pairs with `surface` or `prime`).
    #[cfg(feature = "surface")]
    {
        crate::cmd::check::contract_snapshot_via_lower::<crate::pass::full::FullPipeline>(root)
            .map_err(wrap)
    }
    #[cfg(all(not(feature = "surface"), feature = "prime"))]
    {
        crate::cmd::check::contract_snapshot_via_lower::<crate::prime::pipeline::PrimePipeline>(
            root,
        )
        .map_err(wrap)
    }
}

/// The outcome of the `kio dep update` contract-compatibility honesty
/// gate: whether the dependency's contract surface broke across the
/// re-pin, and whether the new commit's surface is sealed.
pub struct ContractGate {
    /// Whether the new commit's contract surface breaks the old commit's
    /// (an export dropped / narrowed, or a host requirement added).
    pub breaking: bool,
    /// Whether the new commit's contract surface is **sealed** (the
    /// dependency ships a committed `<pkg>.sig.kio`). A breaking change on
    /// a sealed contract is an error (overridable by `--allow-breaking`);
    /// on an unsealed contract it is a warning.
    pub sealed: bool,
    /// Human-readable one-line reasons for each breaking change, surfaced
    /// in the error / warning the command prints.
    pub reasons: Vec<String>,
}

// ---- git plumbing ----------------------------------------------------------

/// True when `dir` is a git repository (bare or not) — `repo.git/HEAD`
/// exists. A cheap, network-free check used to decide clone-vs-fetch.
fn is_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() || dir.join(".git").exists()
}

/// Marker file written into a checkout directory once its extraction has
/// fully succeeded. Its presence is the completeness signal
/// [`is_complete_checkout`] checks; a populated directory lacking it is a
/// partial extraction. Named under the same `.kio-checkout-`
/// prefix as the throwaway index, and — like it — not a `*.kio` module, so
/// re-rooting never picks it up.
const CHECKOUT_COMPLETE_SENTINEL: &str = ".kio-checkout-complete";

/// True when `checkout_dir` holds a **complete** extracted tree: the
/// completeness sentinel [`CHECKOUT_COMPLETE_SENTINEL`], written by
/// [`checkout_commit`] only after a fully-successful extraction, is
/// present. A directory that holds files but no sentinel is a partial
/// checkout — a previous run killed mid-`git checkout` — and is treated as
/// not usable so the caller re-extracts it rather than reusing the partial
/// tree. An empty or absent directory is likewise not a
/// usable checkout.
fn is_complete_checkout(checkout_dir: &Path) -> bool {
    checkout_dir.join(CHECKOUT_COMPLETE_SENTINEL).is_file()
}

/// `&Path` → `&str` for a git argv slot. A non-UTF-8 path would be a
/// pathological cache location; lossy rendering keeps the type simple
/// and the diagnostic readable if git then complains.
fn path_arg(p: &Path) -> &str {
    p.to_str().unwrap_or("")
}

/// Run a git command in `cwd`, returning `Ok(())` on success and the
/// trimmed stderr on failure.
fn run_git<'a, I>(cwd: &Path, args: I) -> Result<(), String>
where
    I: IntoIterator<Item = &'a str>,
{
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn `git`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Run a git command in `cwd`, returning its stdout on success and the
/// trimmed stderr on failure.
fn run_git_stdout<'a, I>(cwd: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = &'a str>,
{
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn `git`: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_with_path(path: Option<&str>) -> GitSource {
        GitSource {
            url: "repo".to_owned(),
            url_span: Span::new(10, 14),
            url_leading_trivia: Vec::new(),
            git_ref: "main".to_owned(),
            ref_span: Span::new(20, 24),
            ref_leading_trivia: Vec::new(),
            manifest_path: path.map(|path| crate::ast::GitManifestPath {
                path: path.to_owned(),
                span: Span::new(30, 40),
                leading_trivia: Vec::new(),
            }),
        }
    }

    #[test]
    fn git_manifest_malformed_lock_is_a_dependency_error_at_the_selector() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lib.lock.kio");
        let text = render_lock(
            "lib",
            &source_with_path(Some("C:/lib.pkg.kio")),
            "commit",
            "digest",
        );
        std::fs::write(&path, &text).unwrap();
        let error = read_lock(&path, "lib").unwrap_err();
        assert_eq!(error.file_path, path);
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Dep);
        let (span, message) = error.error.diag();
        assert_eq!(
            &text[span.start as usize..span.end as usize],
            "\"C:/lib.pkg.kio\""
        );
        assert!(message.contains("malformed"), "{message}");
    }

    #[test]
    fn git_manifest_path_is_part_of_lock_identity() {
        for old in [None, Some("a/lib.pkg.kio"), Some("b/lib.pkg.kio")] {
            let source = source_with_path(old);
            let text = render_lock("lib", &source, "commit", "digest");
            let lock = crate::pass::parser::parse_lock_file(&text, Some("lib")).unwrap();
            assert_eq!(lock.manifest_path.as_deref(), old);
            for new in [
                None,
                Some("a/lib.pkg.kio"),
                Some("b/lib.pkg.kio"),
                Some("./a/lib.pkg.kio"),
            ] {
                assert_eq!(
                    lock_matches_source(&lock, &source_with_path(new)),
                    old == new
                );
            }
            let mut other = source.clone();
            other.git_ref = "other".to_owned();
            assert!(!lock_matches_source(&lock, &other));
            other = source;
            other.url = "other".to_owned();
            assert!(!lock_matches_source(&lock, &other));
        }
    }

    #[test]
    fn git_manifest_selection_bypasses_decoy_but_discovery_stays_shallow() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let consumer = temp.path().join("consumer");
        let package = checkout.join("packages/nested/lib/lib.pkg.kio");
        std::fs::create_dir_all(package.parent().unwrap()).unwrap();
        std::fs::write(&package, "package lib;").unwrap();
        let decoy = checkout.join("decoy.pkg.kio");
        std::fs::write(&decoy, "package decoy;").unwrap();
        let dep_file = consumer.join("lib.dep.kio");
        let source = source_with_path(Some("packages/nested/lib/lib.pkg.kio"));
        assert_eq!(
            locate_source_package(&checkout, &source, "lib", &consumer, &dep_file).unwrap(),
            std::fs::canonicalize(&package).unwrap()
        );
        assert_eq!(
            locate_source_package(
                &checkout,
                &source_with_path(None),
                "lib",
                &consumer,
                &dep_file
            )
            .unwrap(),
            decoy
        );
        for selector in [
            "missing.pkg.kio",
            "packages/nested/lib",
            "packages/nested/lib/main.kio",
        ] {
            std::fs::write(
                checkout.join("packages/nested/lib/main.kio"),
                "module main;",
            )
            .unwrap();
            let source = source_with_path(Some(selector));
            let error =
                locate_source_package(&checkout, &source, "lib", &consumer, &dep_file).unwrap_err();
            assert_eq!(error.file_path, dep_file);
            assert_eq!(error.error.diag().0, source.manifest_path.unwrap().span);
            assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Dep);
        }
        let source = source_with_path(Some("packages/./nested/../nested/lib/lib.pkg.kio"));
        assert_eq!(
            locate_source_package(&checkout, &source, "lib", &consumer, &dep_file).unwrap(),
            std::fs::canonicalize(package).unwrap()
        );
    }

    #[test]
    fn git_manifest_selection_rejects_component_escape() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let sibling = temp.path().join("checkout-other");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("lib.pkg.kio"), "package lib;").unwrap();
        let sink = std::cell::RefCell::new(Vec::new());
        let error = locate_selected_package(
            &checkout,
            "../checkout-other/lib.pkg.kio",
            "lib",
            &capturing_dep_err(&sink),
        )
        .unwrap_err();
        assert!(error.error.diag().1.contains("outside the git checkout"));
    }

    #[cfg(unix)]
    #[test]
    fn git_manifest_selection_canonicalizes_symlinks_before_containment() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(checkout.join("inside")).unwrap();
        std::fs::create_dir_all(outside.join("child")).unwrap();
        std::fs::write(checkout.join("inside/lib.pkg.kio"), "package lib;").unwrap();
        std::fs::write(outside.join("lib.pkg.kio"), "package lib;").unwrap();
        std::os::unix::fs::symlink("inside", checkout.join("alias")).unwrap();
        std::os::unix::fs::symlink(&outside, checkout.join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.join("child"), checkout.join("jump")).unwrap();
        std::os::unix::fs::symlink(outside.join("lib.pkg.kio"), checkout.join("linked.pkg.kio"))
            .unwrap();
        let sink = std::cell::RefCell::new(Vec::new());
        let dep_err = capturing_dep_err(&sink);
        assert_eq!(
            locate_selected_package(&checkout, "alias/lib.pkg.kio", "lib", &dep_err).unwrap(),
            std::fs::canonicalize(checkout.join("inside/lib.pkg.kio")).unwrap()
        );
        for selector in [
            "escape/lib.pkg.kio",
            "jump/../lib.pkg.kio",
            "linked.pkg.kio",
        ] {
            let error = locate_selected_package(&checkout, selector, "lib", &dep_err).unwrap_err();
            assert!(
                error.error.diag().1.contains("outside the git checkout"),
                "{selector}"
            );
        }
    }

    // Build an OS-absolute path from slash-separated segments, so these
    // tests exercise `is_absolute()` correctly on Windows too (where a
    // leading `/` is not absolute).
    fn abs(p: &str) -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(format!("C:\\{}", p.replace('/', "\\")))
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(format!("/{p}"))
        }
    }

    #[test]
    fn cache_home_prefers_kio_cache_home_when_absolute() {
        let got = cache_home_from(
            Some(abs("abs/kio-cache")),
            Some(abs("abs/xdg")),
            Some(abs("home/u")),
        );
        assert_eq!(got, Some(abs("abs/kio-cache")));
    }

    #[test]
    fn cache_home_ignores_relative_kio_cache_home() {
        // A relative `$KIO_CACHE_HOME` is ignored; resolution falls
        // through to `$XDG_CACHE_HOME/kio`.
        let got = cache_home_from(
            Some(PathBuf::from("relative/cache")),
            Some(abs("abs/xdg")),
            Some(abs("home/u")),
        );
        assert_eq!(got, Some(abs("abs/xdg").join("kio")));
    }

    #[test]
    fn cache_home_falls_back_to_xdg_then_home() {
        assert_eq!(
            cache_home_from(None, Some(abs("abs/xdg")), None),
            Some(abs("abs/xdg").join("kio"))
        );
        assert_eq!(
            cache_home_from(None, None, Some(abs("home/u"))),
            Some(abs("home/u").join(".cache").join("kio"))
        );
        assert_eq!(
            cache_home_from(None, Some(PathBuf::from("relative")), None),
            None
        );
        assert_eq!(cache_home_from(None, None, None), None);
    }

    #[test]
    fn url_hash_is_stable_and_distinct() {
        let a = url_hash("file:///tmp/foo");
        let b = url_hash("file:///tmp/foo");
        let c = url_hash("file:///tmp/bar");
        assert_eq!(a, b, "same URL hashes identically");
        assert_ne!(a, c, "distinct URLs hash distinctly");
        assert_eq!(a.len(), 64, "blake3 hex is 64 chars");
    }

    #[test]
    fn lock_path_is_sibling_of_dep_file() {
        let dep = PathBuf::from("/pkg/foobar.dep.kio");
        assert_eq!(lock_path_for(&dep), PathBuf::from("/pkg/foobar.lock.kio"));
    }

    // ---- git-backed regression tests ----------
    //
    // These spawn `git` against a throwaway local repository in a temp
    // dir; `git` is always on the toolchain in this repo. A plain local
    // path (not `file://`) is used as the clone URL so the local
    // transport applies and no `protocol.file.allow` policy can intervene.

    /// Run a git command in `dir` with host config neutralized, asserting
    /// success — used only to *set up* the source repository under test.
    fn run_setup_git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// Create a one-commit source repository at `dir`.
    fn init_source_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        run_setup_git(dir, &["init", "-q"]);
        std::fs::write(dir.join("lib.pkg.kio"), "package lib;\n").unwrap();
        run_setup_git(dir, &["add", "-A"]);
        run_setup_git(
            dir,
            &[
                "-c",
                "user.name=Kio Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
    }

    /// A `dep_err` closure that also records every message it is handed,
    /// so a test can assert on the diagnostic text.
    fn capturing_dep_err(
        sink: &std::cell::RefCell<Vec<String>>,
    ) -> impl Fn(String) -> LocatedError + '_ {
        move |message: String| {
            sink.borrow_mut().push(message.clone());
            LocatedError {
                file_path: PathBuf::from("test.dep.kio"),
                error: Error::dep(Span::new(0, 0), message),
            }
        }
    }

    #[test]
    fn sealed_dependency_rejects_an_invalid_fresh_signature_artifact() {
        let tmp = tempfile::tempdir().expect("create dependency fixture");
        let package_file = tmp.path().join("lib.pkg.kio");
        std::fs::write(&package_file, "package lib;\nbridge { api; }\n")
            .expect("write package file");
        std::fs::write(
            tmp.path().join("lib.sig.kio"),
            r#"signature lib v(2);
v(1) {
  with { module api {
    pub rec newtype Bad : (Bad -> .) { pub constructor mk; pub projector un; };
  } };
  nonbreaking { add { api.Bad; } }
}
"#,
        )
        .expect("write malformed sealed signature");
        let sink = std::cell::RefCell::new(Vec::new());
        let dep_err = capturing_dep_err(&sink);

        let error = match dependency_contract(&package_file, "dep", &dep_err) {
            Err(error) => error,
            Ok(_) => panic!("sealed dependency materialization accepted its invalid signature"),
        };
        let message = error.error.diag().1;
        assert!(
            message.contains("replaying the sealed contract"),
            "{message}"
        );
        assert!(message.contains("strictly positive"), "{message}");
    }

    #[test]
    fn update_require_fetch_is_fatal_offline_reuse_is_not() {
        // Under `RequireFetch` (`kio dep update`) a failed fetch is a
        // dependency error — it must not silently re-pin to a stale cached
        // commit. Under `OfflineReuseOk` (resolve / build) the same failure
        // reuses the cache.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        init_source_repo(&src);
        let url = src.to_str().unwrap().to_owned();
        let mirror = tmp.path().join("cache").join("repo.git");
        let sink = std::cell::RefCell::new(Vec::new());
        let dep_err = capturing_dep_err(&sink);

        // Online: the mirror clones.
        ensure_mirror(&mirror, &url, FetchPolicy::RequireFetch, &dep_err)
            .expect("clone the mirror while the remote is reachable");
        assert!(is_git_dir(&mirror));

        // Go offline: remove the source so any fetch fails.
        std::fs::remove_dir_all(&src).unwrap();

        ensure_mirror(&mirror, &url, FetchPolicy::RequireFetch, &dep_err)
            .expect_err("RequireFetch must be fatal when the remote fetch fails");
        ensure_mirror(&mirror, &url, FetchPolicy::OfflineReuseOk, &dep_err)
            .expect("OfflineReuseOk reuses the cached mirror offline");
    }

    #[test]
    fn checkout_writes_completeness_sentinel_partial_is_not_reused() {
        // A fully-successful extraction writes the completeness
        // sentinel; a tree whose sentinel is missing (a checkout killed
        // mid-extraction) is not a usable checkout and is re-extracted.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        init_source_repo(&src);
        let url = src.to_str().unwrap().to_owned();
        let mirror = tmp.path().join("cache").join("repo.git");
        let sink = std::cell::RefCell::new(Vec::new());
        let dep_err = capturing_dep_err(&sink);

        ensure_mirror(&mirror, &url, FetchPolicy::OfflineReuseOk, &dep_err).expect("clone mirror");
        let commit = resolve_ref_to_commit(&mirror, "HEAD", &dep_err).expect("resolve HEAD");
        let checkout = tmp.path().join("cache").join(&commit);
        checkout_commit(&mirror, &checkout, &commit, &dep_err).expect("checkout commit");

        assert!(
            checkout.join(CHECKOUT_COMPLETE_SENTINEL).is_file(),
            "a successful extraction writes the sentinel"
        );
        assert!(is_complete_checkout(&checkout));
        assert!(checkout.join("lib.pkg.kio").is_file(), "tree extracted");

        // Simulate an interrupted checkout: files remain, sentinel gone.
        std::fs::remove_file(checkout.join(CHECKOUT_COMPLETE_SENTINEL)).unwrap();
        assert!(
            checkout.join("lib.pkg.kio").is_file(),
            "files still present"
        );
        assert!(
            !is_complete_checkout(&checkout),
            "a populated-but-sentinel-less tree is a partial checkout, not reusable"
        );
    }

    #[test]
    fn populated_without_sentinel_is_not_complete() {
        // (predicate, no git): files without the sentinel = partial.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("checkout");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("some.kio"), b"module x;").unwrap();
        assert!(
            !is_complete_checkout(&dir),
            "files but no sentinel = partial"
        );
        std::fs::write(dir.join(CHECKOUT_COMPLETE_SENTINEL), b"").unwrap();
        assert!(is_complete_checkout(&dir), "sentinel present = complete");
        assert!(
            !is_complete_checkout(&tmp.path().join("absent")),
            "an absent directory is not a usable checkout"
        );
    }

    #[test]
    fn clone_separates_dash_leading_url_from_options() {
        // A `-`-leading URL must reach git as the `<repo>` positional,
        // never as a flag. Without `--end-of-options` git reports "unknown
        // switch"; with it, git treats `-x` as a (missing) repository.
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("cache").join("repo.git");
        let sink = std::cell::RefCell::new(Vec::new());
        let dep_err = capturing_dep_err(&sink);

        ensure_mirror(&mirror, "-x", FetchPolicy::OfflineReuseOk, &dep_err)
            .expect_err("a `-x` URL has no repository to clone");
        let msg = sink.borrow().last().cloned().unwrap_or_default();
        assert!(
            !msg.contains("unknown switch") && !msg.contains("unknown option"),
            "the `-`-leading URL must not be parsed as a git flag: {msg}"
        );
    }
}
