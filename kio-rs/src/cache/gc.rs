use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

const RETENTION_SECS: u64 = 3 * 24 * 60 * 60;
const AUTO_GC_INTERVAL_SECS: u64 = 24 * 60 * 60;
const TEMP_ENTRY_GRACE_SECS: u64 = 24 * 60 * 60;
const GC_DIR: &str = ".gc";
const ACCESS_DIR: &str = "access";
const STATE_FILE: &str = "state.json";
const RETIRED_CACHE_FAMILIES: &[&str] = &["user-elaborator"];

static PENDING: OnceLock<Mutex<BTreeMap<PathBuf, BTreeSet<CacheFamily>>>> = OnceLock::new();
static STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CacheFamily {
    PackageCheck,
    Typed,
    EnrichedIr,
    Emit,
    Artifacts,
    Equiv,
    Doc,
}

impl CacheFamily {
    pub fn name(self) -> &'static str {
        match self {
            CacheFamily::PackageCheck => "package-check",
            CacheFamily::Typed => "typed",
            CacheFamily::EnrichedIr => "enriched-ir",
            CacheFamily::Emit => "emit",
            CacheFamily::Artifacts => "artifacts",
            CacheFamily::Equiv => "equiv",
            CacheFamily::Doc => "doc",
        }
    }

    fn dir_name(self) -> &'static str {
        self.name()
    }

    fn all() -> &'static [CacheFamily] {
        &[
            CacheFamily::PackageCheck,
            CacheFamily::Typed,
            CacheFamily::EnrichedIr,
            CacheFamily::Emit,
            CacheFamily::Artifacts,
            CacheFamily::Equiv,
            CacheFamily::Doc,
        ]
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcSummary {
    pub removed_entries: usize,
    pub removed_temp_entries: usize,
    pub swept_families: usize,
}

impl GcSummary {
    fn add(&mut self, other: GcSummary) {
        self.removed_entries += other.removed_entries;
        self.removed_temp_entries += other.removed_temp_entries;
        self.swept_families += other.swept_families;
    }

    pub fn total_removed(&self) -> usize {
        self.removed_entries + self.removed_temp_entries
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct GcState {
    last_gc_attempt: Option<u64>,
    last_gc_success: Option<u64>,
    families: BTreeMap<String, FamilyState>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct FamilyState {
    last_successful_use: Option<u64>,
}

enum MissingLastUse {
    Skip,
    UseNow,
}

pub fn record_cache_open(cache_root: &Path, family: CacheFamily) {
    if !crate::cache::policy::caches_enabled() {
        return;
    }
    let mut pending = PENDING
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("cache GC pending-use registry lock poisoned");
    pending
        .entry(cache_root.to_path_buf())
        .or_default()
        .insert(family);
}

pub fn record_entry_path_access(
    cache_root: &Path,
    family: CacheFamily,
    family_root: &Path,
    entry_path: &Path,
) {
    let Ok(rel) = entry_path.strip_prefix(family_root) else {
        return;
    };
    record_entry_access(cache_root, family, rel);
}

pub fn record_entry_access(cache_root: &Path, family: CacheFamily, rel_path: &Path) {
    if !crate::cache::policy::caches_enabled() {
        return;
    }
    record_cache_open(cache_root, family);
    let _ = write_access_marker(cache_root, family, rel_path, now_secs());
}

pub fn finish_successful_command() {
    if !crate::cache::policy::caches_enabled() {
        return;
    }
    let mut pending_guard = PENDING
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("cache GC pending-use registry lock poisoned");
    let pending = std::mem::take(&mut *pending_guard);
    if pending.is_empty() {
        return;
    }
    let now = now_secs();
    for (cache_root, families) in pending {
        let _ = finish_root_success(&cache_root, &families, now);
    }
}

fn finish_root_success(
    cache_root: &Path,
    families: &BTreeSet<CacheFamily>,
    now: u64,
) -> Result<(), String> {
    let mut state = read_state(cache_root)?;
    for family in families {
        state
            .families
            .entry(family.name().to_owned())
            .or_default()
            .last_successful_use = Some(now);
    }

    let due = state
        .last_gc_success
        .map(|last| now.saturating_sub(last) >= AUTO_GC_INTERVAL_SECS)
        .unwrap_or(true);
    if due {
        let _ = run_gc_with_state(cache_root, &mut state, now, MissingLastUse::Skip)?;
    } else {
        write_state(cache_root, &state)?;
    }
    Ok(())
}

pub fn run_explicit(cache_root: &Path) -> Result<GcSummary, String> {
    let now = now_secs();
    let mut state = read_state(cache_root)?;
    run_gc_with_state(cache_root, &mut state, now, MissingLastUse::UseNow)
}

fn run_gc_with_state(
    cache_root: &Path,
    state: &mut GcState,
    now: u64,
    missing_last_use: MissingLastUse,
) -> Result<GcSummary, String> {
    state.last_gc_attempt = Some(now);
    let mut summary = GcSummary::default();
    for family in RETIRED_CACHE_FAMILIES {
        summary.add(sweep_retired_cache_family(cache_root, state, family)?);
    }
    for family in CacheFamily::all() {
        let last_use = state
            .families
            .get(family.name())
            .and_then(|s| s.last_successful_use);
        let cutoff = match (last_use, &missing_last_use) {
            (Some(last), _) => last.saturating_sub(RETENTION_SECS),
            (None, MissingLastUse::UseNow) => now.saturating_sub(RETENTION_SECS),
            (None, MissingLastUse::Skip) => continue,
        };
        let family_summary = sweep_family(cache_root, *family, cutoff, now)?;
        summary.add(family_summary);
    }
    state.last_gc_success = Some(now);
    write_state(cache_root, state)?;
    Ok(summary)
}

fn sweep_retired_cache_family(
    cache_root: &Path,
    state: &mut GcState,
    family: &str,
) -> Result<GcSummary, String> {
    let family_root = cache_root.join(family);
    let removed = remove_retired_cache_dir(&family_root)?;
    remove_retired_cache_dir(&cache_root.join(GC_DIR).join(ACCESS_DIR).join(family))?;
    state.families.remove(family);
    Ok(GcSummary {
        removed_entries: usize::from(removed),
        removed_temp_entries: 0,
        swept_families: usize::from(removed),
    })
}

fn remove_retired_cache_dir(path: &Path) -> Result<bool, String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "remove {}: {error}",
            crate::path_display::DisplayPath(path)
        )),
    }
}

fn sweep_family(
    cache_root: &Path,
    family: CacheFamily,
    cutoff: u64,
    now: u64,
) -> Result<GcSummary, String> {
    let mut summary = match family {
        CacheFamily::Artifacts => sweep_artifacts(cache_root, family, cutoff, now)?,
        _ => sweep_file_family(cache_root, family, cutoff, now)?,
    };
    if summary.removed_entries > 0 || summary.removed_temp_entries > 0 {
        summary.swept_families = 1;
    }
    Ok(summary)
}

fn sweep_file_family(
    cache_root: &Path,
    family: CacheFamily,
    cutoff: u64,
    now: u64,
) -> Result<GcSummary, String> {
    let family_root = cache_root.join(family.dir_name());
    if !family_root.exists() {
        return Ok(GcSummary::default());
    }
    let mut summary = GcSummary::default();
    sweep_file_dir(
        cache_root,
        family,
        &family_root,
        &family_root,
        cutoff,
        now,
        &mut summary,
    )?;
    remove_empty_dirs(&family_root, &family_root)?;
    Ok(summary)
}

fn sweep_file_dir(
    cache_root: &Path,
    family: CacheFamily,
    family_root: &Path,
    dir: &Path,
    cutoff: u64,
    now: u64,
    summary: &mut GcSummary,
) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!(
                "read {}: {e}",
                crate::path_display::DisplayPath(dir)
            ));
        }
    };
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("read {}: {e}", crate::path_display::DisplayPath(dir)))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", crate::path_display::DisplayPath(&path)))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            sweep_file_dir(cache_root, family, family_root, &path, cutoff, now, summary)?;
            if path != family_root {
                let _ = fs::remove_dir(&path);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if is_temp_path(&path) {
            if is_older_than(&path, now, TEMP_ENTRY_GRACE_SECS) {
                fs::remove_file(&path).map_err(|e| {
                    format!("remove {}: {e}", crate::path_display::DisplayPath(&path))
                })?;
                summary.removed_temp_entries += 1;
            }
            continue;
        }
        let Ok(rel) = path.strip_prefix(family_root) else {
            continue;
        };
        let access = entry_access_secs(cache_root, family, rel)
            .or_else(|| modified_secs(&path))
            .unwrap_or(0);
        if access < cutoff {
            fs::remove_file(&path)
                .map_err(|e| format!("remove {}: {e}", crate::path_display::DisplayPath(&path)))?;
            remove_entry_access(cache_root, family, rel);
            summary.removed_entries += 1;
        }
    }
    Ok(())
}

fn sweep_artifacts(
    cache_root: &Path,
    family: CacheFamily,
    cutoff: u64,
    now: u64,
) -> Result<GcSummary, String> {
    let family_root = cache_root.join(family.dir_name());
    if !family_root.exists() {
        return Ok(GcSummary::default());
    }
    let mut summary = GcSummary::default();
    let targets = fs::read_dir(&family_root).map_err(|e| {
        format!(
            "read {}: {e}",
            crate::path_display::DisplayPath(&family_root)
        )
    })?;
    for target in targets {
        let target = target.map_err(|e| {
            format!(
                "read {}: {e}",
                crate::path_display::DisplayPath(&family_root)
            )
        })?;
        let target_path = target.path();
        let target_type = target.file_type().map_err(|e| {
            format!(
                "stat {}: {e}",
                crate::path_display::DisplayPath(&target_path)
            )
        })?;
        if !target_type.is_dir() || target_type.is_symlink() {
            continue;
        }
        let entries = fs::read_dir(&target_path).map_err(|e| {
            format!(
                "read {}: {e}",
                crate::path_display::DisplayPath(&target_path)
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                format!(
                    "read {}: {e}",
                    crate::path_display::DisplayPath(&target_path)
                )
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("stat {}: {e}", crate::path_display::DisplayPath(&path)))?;
            if file_type.is_dir() && !file_type.is_symlink() && is_temp_path(&path) {
                if is_older_than(&path, now, TEMP_ENTRY_GRACE_SECS) {
                    fs::remove_dir_all(&path).map_err(|e| {
                        format!("remove {}: {e}", crate::path_display::DisplayPath(&path))
                    })?;
                    summary.removed_temp_entries += 1;
                }
                continue;
            }
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&family_root) else {
                continue;
            };
            let manifest = path.join("manifest.txt");
            let access = entry_access_secs(cache_root, family, rel)
                .or_else(|| modified_secs(&manifest))
                .or_else(|| modified_secs(&path))
                .unwrap_or(0);
            if access < cutoff {
                fs::remove_dir_all(&path).map_err(|e| {
                    format!("remove {}: {e}", crate::path_display::DisplayPath(&path))
                })?;
                remove_entry_access(cache_root, family, rel);
                summary.removed_entries += 1;
            }
        }
        let _ = fs::remove_dir(&target_path);
    }
    let _ = fs::remove_dir(&family_root);
    Ok(summary)
}

pub(crate) fn entry_access_secs(
    cache_root: &Path,
    family: CacheFamily,
    rel_path: &Path,
) -> Option<u64> {
    let path = access_marker_path(cache_root, family, rel_path)?;
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub(crate) fn remove_entry_access(cache_root: &Path, family: CacheFamily, rel_path: &Path) {
    if let Some(path) = access_marker_path(cache_root, family, rel_path) {
        let _ = fs::remove_file(path);
    }
}

fn write_access_marker(
    cache_root: &Path,
    family: CacheFamily,
    rel_path: &Path,
    now: u64,
) -> std::io::Result<()> {
    let Some(path) = access_marker_path(cache_root, family, rel_path) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{now}\n"))
}

fn access_marker_path(cache_root: &Path, family: CacheFamily, rel_path: &Path) -> Option<PathBuf> {
    let mut path = cache_root.join(GC_DIR).join(ACCESS_DIR).join(family.name());
    for component in rel_path.components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    let file_name = path.file_name()?.to_string_lossy();
    path.set_file_name(format!("{file_name}.stamp"));
    Some(path)
}

fn read_state(cache_root: &Path) -> Result<GcState, String> {
    let path = state_path(cache_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GcState::default()),
        Err(e) => {
            return Err(format!(
                "read {}: {e}",
                crate::path_display::DisplayPath(&path)
            ));
        }
    };
    serde_json::from_str(&text)
        .map_err(|e| format!("parse {}: {e}", crate::path_display::DisplayPath(&path)))
}

fn write_state(cache_root: &Path, state: &GcState) -> Result<(), String> {
    let dir = cache_root.join(GC_DIR);
    fs::create_dir_all(&dir)
        .map_err(|e| format!("mkdir {}: {e}", crate::path_display::DisplayPath(&dir)))?;
    let final_path = state_path(cache_root);
    let n = STATE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!("state.json.tmp.{}.{}", std::process::id(), n));
    let body =
        serde_json::to_string_pretty(state).map_err(|e| format!("encode cache GC state: {e}"))?;
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| {
                format!(
                    "create {}: {e}",
                    crate::path_display::DisplayPath(&tmp_path)
                )
            })?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("write {}: {e}", crate::path_display::DisplayPath(&tmp_path)))?;
        file.write_all(b"\n")
            .map_err(|e| format!("write {}: {e}", crate::path_display::DisplayPath(&tmp_path)))?;
    }
    if let Err(e) = fs::rename(&tmp_path, &final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "rename {} -> {}: {e}",
            crate::path_display::DisplayPath(&tmp_path),
            crate::path_display::DisplayPath(&final_path)
        ));
    }
    Ok(())
}

fn state_path(cache_root: &Path) -> PathBuf {
    cache_root.join(GC_DIR).join(STATE_FILE)
}

fn remove_empty_dirs(root: &Path, dir: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!(
                "read {}: {e}",
                crate::path_display::DisplayPath(dir)
            ));
        }
    };
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("read {}: {e}", crate::path_display::DisplayPath(dir)))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", crate::path_display::DisplayPath(&path)))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            remove_empty_dirs(root, &path)?;
            if path != root {
                let _ = fs::remove_dir(&path);
            }
        }
    }
    Ok(())
}

fn is_temp_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.contains(".tmp."))
}

fn is_older_than(path: &Path, now: u64, age_secs: u64) -> bool {
    modified_secs(path)
        .map(|modified| now.saturating_sub(modified) >= age_secs)
        .unwrap_or(false)
}

fn modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("kio-cache-gc-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    struct TempRoot(PathBuf);
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_marker(root: &Path, family: CacheFamily, rel: &str, at: u64) {
        write_access_marker(root, family, Path::new(rel), at).unwrap();
    }

    #[test]
    fn explicit_gc_removes_old_file_entries_and_keeps_recent_entries() {
        let root = TempRoot(temp_root("file"));
        let typed = root.0.join("typed").join("pkg");
        fs::create_dir_all(&typed).unwrap();
        fs::write(typed.join("old.full.bin"), b"old").unwrap();
        fs::write(typed.join("new.full.bin"), b"new").unwrap();

        let now = 10 * 24 * 60 * 60;
        write_marker(&root.0, CacheFamily::Typed, "pkg/old.full.bin", 1);
        write_marker(&root.0, CacheFamily::Typed, "pkg/new.full.bin", now);

        let mut state = GcState::default();
        let summary = run_gc_with_state(&root.0, &mut state, now, MissingLastUse::UseNow).unwrap();

        assert_eq!(summary.removed_entries, 1);
        assert!(!typed.join("old.full.bin").exists());
        assert!(typed.join("new.full.bin").is_file());
    }

    #[test]
    fn explicit_gc_handles_flat_typed_entries() {
        let root = TempRoot(temp_root("flat-typed"));
        let typed = root.0.join("typed");
        fs::create_dir_all(&typed).unwrap();
        fs::write(typed.join("old.bin"), b"old").unwrap();
        fs::write(typed.join("new.bin"), b"new").unwrap();

        let now = 10 * 24 * 60 * 60;
        write_marker(&root.0, CacheFamily::Typed, "old.bin", 1);
        write_marker(&root.0, CacheFamily::Typed, "new.bin", now);

        let mut state = GcState::default();
        let summary = run_gc_with_state(&root.0, &mut state, now, MissingLastUse::UseNow).unwrap();

        assert_eq!(summary.removed_entries, 1);
        assert!(!typed.join("old.bin").exists());
        assert!(typed.join("new.bin").is_file());
    }

    #[test]
    fn auto_gc_uses_recorded_family_last_use() {
        let root = TempRoot(temp_root("auto"));
        let doc = root.0.join("doc");
        fs::create_dir_all(&doc).unwrap();
        fs::write(doc.join("old.bin"), b"old").unwrap();
        fs::write(doc.join("recent.bin"), b"recent").unwrap();

        let now = 20 * 24 * 60 * 60;
        let last_use = 15 * 24 * 60 * 60;
        write_marker(
            &root.0,
            CacheFamily::Doc,
            "old.bin",
            last_use - RETENTION_SECS - 1,
        );
        write_marker(
            &root.0,
            CacheFamily::Doc,
            "recent.bin",
            last_use - RETENTION_SECS + 1,
        );

        let mut state = GcState::default();
        state
            .families
            .entry(CacheFamily::Doc.name().to_owned())
            .or_default()
            .last_successful_use = Some(last_use);
        let summary = run_gc_with_state(&root.0, &mut state, now, MissingLastUse::Skip).unwrap();

        assert_eq!(summary.removed_entries, 1);
        assert!(!doc.join("old.bin").exists());
        assert!(doc.join("recent.bin").is_file());
    }

    #[test]
    fn explicit_gc_removes_retired_user_elaborator_layout() {
        let root = TempRoot(temp_root("retired-user-elaborator"));
        let legacy_root = root.0.join("user-elaborator");
        fs::create_dir_all(&legacy_root).unwrap();
        fs::write(legacy_root.join("template.bin"), b"retired").unwrap();

        let live_typed = root.0.join("typed").join("pkg").join("module.full.bin");
        fs::create_dir_all(live_typed.parent().unwrap()).unwrap();
        fs::write(&live_typed, b"live").unwrap();
        let now = now_secs();
        write_marker(&root.0, CacheFamily::Typed, "pkg/module.full.bin", now);
        let live_access = root
            .0
            .join(GC_DIR)
            .join(ACCESS_DIR)
            .join(CacheFamily::Typed.name())
            .join("pkg/module.full.bin.stamp");

        let legacy_access = root.0.join(GC_DIR).join(ACCESS_DIR).join("user-elaborator");
        fs::create_dir_all(&legacy_access).unwrap();
        fs::write(legacy_access.join("template.stamp"), b"1\n").unwrap();

        let mut state = GcState::default();
        state
            .families
            .entry("user-elaborator".to_owned())
            .or_default()
            .last_successful_use = Some(1);
        state
            .families
            .entry(CacheFamily::Typed.name().to_owned())
            .or_default()
            .last_successful_use = Some(now);
        write_state(&root.0, &state).unwrap();

        let summary = run_explicit(&root.0).unwrap();

        assert_eq!(summary.removed_entries, 1);
        assert_eq!(summary.swept_families, 1);
        assert!(!legacy_root.exists());
        assert!(!legacy_access.exists());
        assert!(live_typed.is_file());
        assert!(live_access.is_file());
        let state = read_state(&root.0).unwrap();
        assert!(!state.families.contains_key("user-elaborator"));
        assert!(state.families.contains_key(CacheFamily::Typed.name()));

        assert_eq!(run_explicit(&root.0).unwrap(), GcSummary::default());
        assert!(live_typed.is_file());
        assert!(live_access.is_file());
    }
}
