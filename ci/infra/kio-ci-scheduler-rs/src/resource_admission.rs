//! Cross-platform admission storage for scheduler-owned resources.
//!
//! Each resource has one state lock and a `claims` directory. Immutable claim
//! metadata is encoded in a claim id shared by three separate files: a locked,
//! zero-byte `.lease` and zero-byte `.pending` / `.active` markers. Scanners do
//! not read or rename the locked lease: Windows can reject both operations on
//! a file another process holds open. Activation creates `.active` before it
//! removes `.pending`, and a scan that sees both treats the claim as active.
//!
//! The state lock serializes publication, transition, and cleanup. A scanner
//! proves only liveness by trying the lease lock; the filename proves immutable
//! metadata, and the markers prove state only while that lock is held. Unknown
//! live claims fail closed; suffix discovery is lossless at the `OsStr` level,
//! so a non-Unicode id cannot disappear from the live set. An unlocked lease
//! is stale and its three files are removed after the open handle is closed.
//! Queue tickets are one greater than
//! the newest live claim, so an interrupted counter write cannot poison later
//! admission and an empty queue can safely reuse ticket 1. They preserve FIFO
//! inside each eligible class; work barriers deliberately take priority over
//! normal work. On Unix an admitted command inherits the lease descriptor and
//! a strict descriptor inventory;
//! helper processes that must not pin admission close every inventoried
//! descriptor before exec. On non-Unix hosts the process-tree supervisor
//! retains the `ClaimLease` for the tree lifetime; the Windows supervisor
//! releases it only after its Job Object has drained.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::process::Command;

const CLAIM_VERSION: &str = "v1";
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) const LEASE_FDS_ENV: &str = "KIO_CI_SCHEDULE_LEASE_FDS";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FixedResource {
    Work,
    Cargo,
}

impl FixedResource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Cargo => "cargo",
        }
    }
}

impl From<FixedResource> for SchedulerResource {
    fn from(resource: FixedResource) -> Self {
        match resource {
            FixedResource::Work => Self::Work,
            FixedResource::Cargo => Self::Cargo,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchedulerResource {
    Work,
    Cargo,
    Compiler,
}

impl SchedulerResource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Cargo => "cargo",
            Self::Compiler => "compiler",
        }
    }

    fn accepts(self, kind: ClaimKind) -> bool {
        matches!(
            (self, kind),
            (Self::Work, ClaimKind::Normal | ClaimKind::Barrier)
                | (Self::Cargo, ClaimKind::Normal)
                | (Self::Compiler, ClaimKind::Normal | ClaimKind::Adaptive)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkMode {
    Normal,
    Barrier,
}

#[derive(Clone, Debug)]
pub struct FixedResourceAdmission {
    store: ClaimStore,
    resource: FixedResource,
    capacity: NonZeroUsize,
    claim_kind: ClaimKind,
}

impl FixedResourceAdmission {
    pub fn work(
        schedule_root: PathBuf,
        capacity: usize,
        mode: WorkMode,
    ) -> Result<Self, ResourceAdmissionError> {
        let claim_kind = match mode {
            WorkMode::Normal => ClaimKind::Normal,
            WorkMode::Barrier => ClaimKind::Barrier,
        };
        Self::new(schedule_root, FixedResource::Work, capacity, claim_kind)
    }

    pub fn cargo(schedule_root: PathBuf) -> Result<Self, ResourceAdmissionError> {
        Self::new(schedule_root, FixedResource::Cargo, 1, ClaimKind::Normal)
    }

    fn new(
        schedule_root: PathBuf,
        resource: FixedResource,
        capacity: usize,
        claim_kind: ClaimKind,
    ) -> Result<Self, ResourceAdmissionError> {
        let capacity = NonZeroUsize::new(capacity).ok_or_else(|| {
            ResourceAdmissionError::invalid(
                schedule_root.clone(),
                format!(
                    "{} admission capacity must be at least one",
                    resource.as_str()
                ),
            )
        })?;
        let store = ClaimStore::open(schedule_root, resource.into())?;
        Ok(Self {
            store,
            resource,
            capacity,
            claim_kind,
        })
    }

    pub fn acquire(&self) -> Result<FixedResourcePermit, ResourceAdmissionError> {
        let (lease, outcome) =
            self.store
                .create_pending(self.capacity, self.claim_kind, |snapshot, ticket| {
                    if admission_decision(self.resource, snapshot, ticket) {
                        StoreDecision::Admit
                    } else {
                        StoreDecision::Wait
                    }
                })?;
        let permit = FixedResourcePermit { lease };
        if outcome.decision == StoreDecision::Admit {
            return Ok(permit);
        }

        loop {
            std::thread::sleep(POLL_INTERVAL);
            let outcome = self.store.reconsider(&permit.lease, |snapshot, ticket| {
                if admission_decision(self.resource, snapshot, ticket) {
                    StoreDecision::Admit
                } else {
                    StoreDecision::Wait
                }
            })?;
            if outcome.decision == StoreDecision::Admit {
                return Ok(permit);
            }
        }
    }
}

pub struct FixedResourcePermit {
    lease: ClaimLease,
}

impl FixedResourcePermit {
    pub fn schedule_root(&self) -> &Path {
        self.lease.schedule_root()
    }

    pub fn resource(&self) -> FixedResource {
        match self.lease.resource() {
            SchedulerResource::Work => FixedResource::Work,
            SchedulerResource::Cargo => FixedResource::Cargo,
            SchedulerResource::Compiler => {
                unreachable!("a fixed-resource permit cannot own a compiler claim")
            }
        }
    }

    #[cfg(unix)]
    pub fn prepare_inherited_lease(
        &self,
        command: &mut Command,
    ) -> Result<(), ResourceAdmissionError> {
        self.lease.prepare_inherited_lease(command)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ClaimStore {
    schedule_root: PathBuf,
    resource: SchedulerResource,
}

impl ClaimStore {
    pub(crate) fn open(
        schedule_root: PathBuf,
        resource: SchedulerResource,
    ) -> Result<Self, ResourceAdmissionError> {
        fs::create_dir_all(&schedule_root)
            .map_err(|error| ResourceAdmissionError::io(&schedule_root, error))?;
        let schedule_root = fs::canonicalize(&schedule_root)
            .map_err(|error| ResourceAdmissionError::io(&schedule_root, error))?;
        let claims_dir = schedule_root.join(resource.as_str()).join("claims");
        fs::create_dir_all(&claims_dir)
            .map_err(|error| ResourceAdmissionError::io(&claims_dir, error))?;
        Ok(Self {
            schedule_root,
            resource,
        })
    }

    pub(crate) fn create_pending(
        &self,
        capacity: NonZeroUsize,
        kind: ClaimKind,
        decide: impl FnOnce(&ClaimSnapshot, u64) -> StoreDecision,
    ) -> Result<(ClaimLease, StoreOutcome), ResourceAdmissionError> {
        if !self.resource.accepts(kind) {
            return Err(ResourceAdmissionError::invalid(
                self.resource_dir(),
                format!(
                    "{} claims are invalid for the {} resource",
                    kind.label(),
                    self.resource.as_str()
                ),
            ));
        }
        let state_lock_path = self.state_lock_path();
        let state_lock = lock_state(&state_lock_path)?;
        let outcome = (|| {
            let mut snapshot = self.scan()?;
            let ticket = snapshot.next_ticket().ok_or_else(|| {
                ResourceAdmissionError::invalid(
                    self.claims_dir(),
                    format!(
                        "{} admission ticket space exhausted",
                        self.resource.as_str()
                    ),
                )
            })?;
            let metadata = ClaimMetadata {
                ticket,
                capacity,
                kind,
            };
            let claim_id = metadata.claim_id();
            let lease_path = claim_path(&self.claims_dir(), &claim_id, ClaimFile::Lease);
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&lease_path)
                .map_err(|error| ResourceAdmissionError::io(&lease_path, error))?;
            #[cfg(unix)]
            let lease = keep_lease_above_standard_descriptors(lease, &lease_path)?;
            if let Err(error) = lease.lock() {
                drop(lease);
                let _ = fs::remove_file(&lease_path);
                return Err(ResourceAdmissionError::io(&lease_path, error));
            }
            let pending_path = claim_path(&self.claims_dir(), &claim_id, ClaimFile::Pending);
            if let Err(error) = create_marker(&pending_path) {
                drop(lease);
                let _ = fs::remove_file(&lease_path);
                return Err(error);
            }
            snapshot.claims.push(LiveClaim {
                metadata,
                state: ClaimState::Pending,
            });
            let outcome = if snapshot.has_unknown_live_claim() {
                StoreOutcome {
                    decision: StoreDecision::Wait,
                    wait_reason: Some(StoreWaitReason::UnknownLiveClaim),
                    ticket,
                }
            } else {
                StoreOutcome {
                    decision: decide(&snapshot, ticket),
                    wait_reason: None,
                    ticket,
                }
            };
            if outcome.decision == StoreDecision::Admit {
                activate_claim(&self.claims_dir(), &claim_id)?;
            }
            Ok((
                ClaimLease {
                    store: self.clone(),
                    claim_id,
                    metadata,
                    file: Some(lease),
                },
                outcome,
            ))
        })();
        drop(state_lock);
        outcome
    }

    pub(crate) fn reconsider(
        &self,
        lease: &ClaimLease,
        decide: impl FnOnce(&ClaimSnapshot, u64) -> StoreDecision,
    ) -> Result<StoreOutcome, ResourceAdmissionError> {
        if lease.store.schedule_root != self.schedule_root || lease.store.resource != self.resource
        {
            return Err(ResourceAdmissionError::invalid(
                self.resource_dir(),
                "resource claim belongs to a different store",
            ));
        }
        let state_lock = lock_state(&self.state_lock_path())?;
        let outcome = (|| {
            let snapshot = self.scan()?;
            let live = snapshot.claims.iter().any(|claim| {
                claim.metadata == lease.metadata && claim.state == ClaimState::Pending
            });
            if !live {
                return Err(ResourceAdmissionError::invalid(
                    claim_path(&self.claims_dir(), &lease.claim_id, ClaimFile::Lease),
                    "pending resource claim disappeared or changed state",
                ));
            }
            let outcome = if snapshot.has_unknown_live_claim() {
                StoreOutcome {
                    decision: StoreDecision::Wait,
                    wait_reason: Some(StoreWaitReason::UnknownLiveClaim),
                    ticket: lease.metadata.ticket,
                }
            } else {
                StoreOutcome {
                    decision: decide(&snapshot, lease.metadata.ticket),
                    wait_reason: None,
                    ticket: lease.metadata.ticket,
                }
            };
            if outcome.decision == StoreDecision::Admit {
                activate_claim(&self.claims_dir(), &lease.claim_id)?;
            }
            Ok(outcome)
        })();
        drop(state_lock);
        outcome
    }

    fn scan(&self) -> Result<ClaimSnapshot, ResourceAdmissionError> {
        let mut snapshot = scan_live_claims(&self.claims_dir())?;
        for claim in &snapshot.claims {
            if !self.resource.accepts(claim.metadata.kind) {
                snapshot.has_unknown_live_claim = true;
            }
        }
        Ok(snapshot)
    }

    fn resource_dir(&self) -> PathBuf {
        self.schedule_root.join(self.resource.as_str())
    }

    pub(crate) fn compiler_feedback_path(&self) -> PathBuf {
        assert_eq!(self.resource, SchedulerResource::Compiler);
        self.resource_dir().join("feedback-v1")
    }

    fn claims_dir(&self) -> PathBuf {
        self.resource_dir().join("claims")
    }

    fn state_lock_path(&self) -> PathBuf {
        self.resource_dir().join("state.lock")
    }

    pub(crate) fn schedule_root(&self) -> &Path {
        &self.schedule_root
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreDecision {
    Admit,
    Wait,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreWaitReason {
    UnknownLiveClaim,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoreOutcome {
    pub(crate) decision: StoreDecision,
    pub(crate) wait_reason: Option<StoreWaitReason>,
    pub(crate) ticket: u64,
}

pub(crate) struct ClaimLease {
    store: ClaimStore,
    claim_id: String,
    metadata: ClaimMetadata,
    file: Option<File>,
}

impl ClaimLease {
    pub(crate) fn schedule_root(&self) -> &Path {
        &self.store.schedule_root
    }

    pub(crate) const fn resource(&self) -> SchedulerResource {
        self.store.resource
    }

    #[cfg(unix)]
    pub(crate) fn prepare_inherited_lease(
        &self,
        command: &mut Command,
    ) -> Result<(), ResourceAdmissionError> {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let lease = self
            .file
            .as_ref()
            .unwrap_or_else(|| unreachable!("a live claim always owns its lease"));
        let fd = lease.as_raw_fd();
        append_command_lease_fd(command, fd)?;
        // The descendant tree, rather than its short-lived supervisor, must
        // keep the permit live until its last process exits.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn inherited_lease_fds_from_env() -> Result<Vec<RawFd>, ResourceAdmissionError> {
    match env::var_os(LEASE_FDS_ENV) {
        Some(value) => parse_lease_fds(&value),
        None => Ok(Vec::new()),
    }
}

#[cfg(unix)]
fn append_command_lease_fd(command: &mut Command, fd: RawFd) -> Result<(), ResourceAdmissionError> {
    let ambient = inherited_lease_fds_from_env()?;
    merge_command_lease_fds_with_ambient(command, ambient, Some(fd))
}

#[cfg(all(unix, test))]
fn append_command_lease_fd_with_ambient(
    command: &mut Command,
    fd: RawFd,
    ambient: Vec<RawFd>,
) -> Result<(), ResourceAdmissionError> {
    merge_command_lease_fds_with_ambient(command, ambient, Some(fd))
}

#[cfg(unix)]
pub(crate) fn propagate_inherited_lease_fds(
    command: &mut Command,
    ambient: &[RawFd],
) -> Result<(), ResourceAdmissionError> {
    merge_command_lease_fds_with_ambient(command, ambient.to_vec(), None)
}

#[cfg(unix)]
fn merge_command_lease_fds_with_ambient(
    command: &mut Command,
    mut fds: Vec<RawFd>,
    acquired: Option<RawFd>,
) -> Result<(), ResourceAdmissionError> {
    if let Some(fd) = acquired
        && fd < 3
    {
        return Err(ResourceAdmissionError::invalid(
            PathBuf::from(LEASE_FDS_ENV),
            format!("scheduler lease descriptor must be at least 3 (got {fd})"),
        ));
    }
    let key = OsStr::new(LEASE_FDS_ENV);
    let configured = command
        .get_envs()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.map(OsStr::to_os_string));
    // Removing, clearing, or overriding a command's environment does not close
    // ambient inherited descriptors. Union both strictly parsed inventories so
    // the child never owns an unlisted scheduler lease.
    if let Some(Some(value)) = configured {
        for configured_fd in parse_lease_fds(&value)? {
            if !fds.contains(&configured_fd) {
                fds.push(configured_fd);
            }
        }
    }
    if let Some(fd) = acquired
        && !fds.contains(&fd)
    {
        fds.push(fd);
    }
    let encoded = fds
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    command.env(key, OsString::from(encoded));
    Ok(())
}

#[cfg(unix)]
pub(crate) fn parse_lease_fds(value: &OsStr) -> Result<Vec<RawFd>, ResourceAdmissionError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let Some(value) = value.to_str() else {
        return Err(ResourceAdmissionError::invalid(
            PathBuf::from(LEASE_FDS_ENV),
            format!("{LEASE_FDS_ENV} must be valid Unicode"),
        ));
    };
    let mut seen = HashSet::new();
    let mut fds = Vec::new();
    for part in value.split(',') {
        let fd = part
            .parse::<RawFd>()
            .ok()
            .filter(|fd| *fd >= 3)
            .filter(|fd| fd.to_string() == part)
            .ok_or_else(|| {
                ResourceAdmissionError::invalid(
                    PathBuf::from(LEASE_FDS_ENV),
                    format!("{LEASE_FDS_ENV} contains an invalid descriptor {part:?}"),
                )
            })?;
        if !seen.insert(fd) {
            return Err(ResourceAdmissionError::invalid(
                PathBuf::from(LEASE_FDS_ENV),
                format!("{LEASE_FDS_ENV} contains duplicate descriptor {fd}"),
            ));
        }
        fds.push(fd);
    }
    Ok(fds)
}

#[cfg(unix)]
fn keep_lease_above_standard_descriptors(
    file: File,
    path: &Path,
) -> Result<File, ResourceAdmissionError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    if file.as_raw_fd() >= 3 {
        return Ok(file);
    }
    // Lease descriptors cross exec; reserving the standard descriptor range
    // keeps a closed stdin/stdout/stderr topology from entering the inventory.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd == -1 {
        return Err(ResourceAdmissionError::io(
            path,
            std::io::Error::last_os_error(),
        ));
    }
    drop(file);
    // SAFETY: `F_DUPFD_CLOEXEC` returned a fresh descriptor now owned here.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Drop for ClaimLease {
    fn drop(&mut self) {
        let Ok(state_lock) = lock_state(&self.store.state_lock_path()) else {
            drop(self.file.take());
            return;
        };
        drop(self.file.take());
        let lease_path = claim_path(&self.store.claims_dir(), &self.claim_id, ClaimFile::Lease);
        let lease = OpenOptions::new().read(true).write(true).open(&lease_path);
        if let Ok(lease) = lease
            && lease.try_lock().is_ok()
        {
            drop(lease);
            let _ = remove_claim_files(&self.store.claims_dir(), &self.claim_id);
        }
        drop(state_lock);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClaimKind {
    Normal,
    Barrier,
    Adaptive,
}

impl ClaimKind {
    const fn tag(self) -> &'static str {
        match self {
            Self::Normal => "n",
            Self::Barrier => "b",
            Self::Adaptive => "a",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Barrier => "barrier",
            Self::Adaptive => "adaptive",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "n" => Some(Self::Normal),
            "b" => Some(Self::Barrier),
            "a" => Some(Self::Adaptive),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClaimState {
    Pending,
    Active,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClaimMetadata {
    pub(crate) ticket: u64,
    pub(crate) capacity: NonZeroUsize,
    pub(crate) kind: ClaimKind,
}

impl ClaimMetadata {
    fn claim_id(self) -> String {
        format!(
            "{CLAIM_VERSION}-{:020}-{}-{:020}",
            self.ticket,
            self.kind.tag(),
            self.capacity
        )
    }

    fn parse(claim_id: &str, path: &Path) -> Result<Self, ResourceAdmissionError> {
        let mut parts = claim_id.split('-');
        let version = parts.next();
        let ticket = parts.next();
        let kind = parts.next();
        let capacity = parts.next();
        if version != Some(CLAIM_VERSION) || parts.next().is_some() {
            return Err(ResourceAdmissionError::invalid(
                path.to_path_buf(),
                "malformed resource claim identifier",
            ));
        }
        let ticket = ticket
            .filter(|value| value.len() == 20)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                ResourceAdmissionError::invalid(path.to_path_buf(), "invalid resource claim ticket")
            })?;
        let kind = kind.and_then(ClaimKind::parse).ok_or_else(|| {
            ResourceAdmissionError::invalid(path.to_path_buf(), "invalid resource claim kind")
        })?;
        let capacity = capacity
            .filter(|value| value.len() == 20)
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(NonZeroUsize::new)
            .ok_or_else(|| {
                ResourceAdmissionError::invalid(
                    path.to_path_buf(),
                    "invalid resource claim capacity",
                )
            })?;
        Ok(Self {
            ticket,
            capacity,
            kind,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LiveClaim {
    pub(crate) metadata: ClaimMetadata,
    pub(crate) state: ClaimState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClaimSnapshot {
    claims: Vec<LiveClaim>,
    has_unknown_live_claim: bool,
}

impl ClaimSnapshot {
    #[cfg(test)]
    pub(crate) fn from_claims(claims: Vec<LiveClaim>) -> Self {
        Self {
            claims,
            has_unknown_live_claim: false,
        }
    }

    pub(crate) fn claims(&self) -> &[LiveClaim] {
        &self.claims
    }

    pub(crate) const fn has_unknown_live_claim(&self) -> bool {
        self.has_unknown_live_claim
    }

    pub(crate) fn active_total(&self) -> usize {
        self.claims
            .iter()
            .filter(|claim| claim.state == ClaimState::Active)
            .count()
    }

    #[cfg(test)]
    pub(crate) fn active_adaptive(&self) -> usize {
        self.claims
            .iter()
            .filter(|claim| {
                claim.state == ClaimState::Active && claim.metadata.kind == ClaimKind::Adaptive
            })
            .count()
    }

    pub(crate) fn fixed_capacity_min(&self) -> Option<usize> {
        self.claims
            .iter()
            .filter(|claim| claim.metadata.kind != ClaimKind::Adaptive)
            .map(|claim| claim.metadata.capacity.get())
            .min()
    }

    pub(crate) fn oldest_pending_ticket(&self) -> Option<u64> {
        oldest_pending_ticket(&self.claims, |_| true)
    }

    pub(crate) fn newest_live_ticket(&self) -> Option<u64> {
        self.claims.iter().map(|claim| claim.metadata.ticket).max()
    }

    pub(crate) fn has_active_barrier(&self) -> bool {
        self.claims.iter().any(|claim| {
            claim.state == ClaimState::Active && claim.metadata.kind == ClaimKind::Barrier
        })
    }

    pub(crate) fn has_pending_barrier(&self) -> bool {
        self.claims.iter().any(|claim| {
            claim.state == ClaimState::Pending && claim.metadata.kind == ClaimKind::Barrier
        })
    }

    fn next_ticket(&self) -> Option<u64> {
        self.newest_live_ticket().unwrap_or(0).checked_add(1)
    }
}

#[derive(Clone, Copy)]
enum ClaimFile {
    Lease,
    Pending,
    Active,
}

impl ClaimFile {
    const fn suffix(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::Pending => "pending",
            Self::Active => "active",
        }
    }
}

fn claim_path(claims_dir: &Path, claim_id: &str, kind: ClaimFile) -> PathBuf {
    claims_dir.join(format!("{claim_id}.{}", kind.suffix()))
}

fn lock_state(path: &Path) -> Result<File, ResourceAdmissionError> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| ResourceAdmissionError::io(path, error))?;
    lock.lock()
        .map_err(|error| ResourceAdmissionError::io(path, error))?;
    Ok(lock)
}

fn create_marker(path: &Path) -> Result<(), ResourceAdmissionError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|error| ResourceAdmissionError::io(path, error))
}

fn activate_claim(claims_dir: &Path, claim_id: &str) -> Result<(), ResourceAdmissionError> {
    let active_path = claim_path(claims_dir, claim_id, ClaimFile::Active);
    match create_marker(&active_path) {
        Ok(()) => {}
        Err(error) if error.io_kind() == Some(std::io::ErrorKind::AlreadyExists) => {}
        Err(error) => return Err(error),
    }
    remove_if_present(&claim_path(claims_dir, claim_id, ClaimFile::Pending))
}

fn scan_live_claims(claims_dir: &Path) -> Result<ClaimSnapshot, ResourceAdmissionError> {
    let entries = fs::read_dir(claims_dir)
        .map_err(|error| ResourceAdmissionError::io(claims_dir, error))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| ResourceAdmissionError::io(claims_dir, error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut claims = Vec::new();
    let mut live_ids = HashSet::<OsString>::new();
    let mut live_tickets = HashSet::new();
    let mut has_unknown_live_claim = false;

    for lease_path in entries
        .iter()
        .filter(|path| claim_file_stem(path, ClaimFile::Lease).is_some())
    {
        let Some(claim_id_os) = claim_file_stem(lease_path, ClaimFile::Lease) else {
            continue;
        };
        let lease = match OpenOptions::new().read(true).write(true).open(lease_path) {
            Ok(lease) => lease,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ResourceAdmissionError::io(lease_path, error)),
        };
        match lease.try_lock() {
            Ok(()) => {
                drop(lease);
                remove_claim_files_os(claims_dir, claim_id_os)?;
                continue;
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(ResourceAdmissionError::io(lease_path, error));
            }
        }

        let Some(claim_id) = claim_id_os.to_str() else {
            has_unknown_live_claim = true;
            live_ids.insert(claim_id_os.to_os_string());
            continue;
        };
        let metadata = match ClaimMetadata::parse(claim_id, lease_path) {
            Ok(metadata) => metadata,
            Err(_) => {
                has_unknown_live_claim = true;
                live_ids.insert(claim_id_os.to_os_string());
                continue;
            }
        };
        if !live_tickets.insert(metadata.ticket) {
            return Err(ResourceAdmissionError::invalid(
                lease_path.to_path_buf(),
                format!("duplicate live resource ticket {}", metadata.ticket),
            ));
        }
        let active_path = claim_path(claims_dir, claim_id, ClaimFile::Active);
        let pending_path = claim_path(claims_dir, claim_id, ClaimFile::Pending);
        let active = marker_exists(&active_path)?;
        let pending = marker_exists(&pending_path)?;
        let state = if active {
            if pending {
                remove_if_present(&pending_path)?;
            }
            ClaimState::Active
        } else if pending {
            ClaimState::Pending
        } else {
            return Err(ResourceAdmissionError::invalid(
                lease_path.to_path_buf(),
                "live resource lease has no state marker",
            ));
        };
        live_ids.insert(claim_id_os.to_os_string());
        claims.push(LiveClaim { metadata, state });
    }

    for marker_path in entries.iter() {
        let claim_id = claim_file_stem(marker_path, ClaimFile::Pending)
            .or_else(|| claim_file_stem(marker_path, ClaimFile::Active));
        let Some(claim_id) = claim_id else {
            continue;
        };
        if !live_ids.contains(claim_id) {
            remove_if_present(marker_path)?;
        }
    }

    Ok(ClaimSnapshot {
        claims,
        has_unknown_live_claim,
    })
}

fn claim_file_stem(path: &Path, kind: ClaimFile) -> Option<&OsStr> {
    let file_name = path.file_name()?;
    let suffix = kind.suffix();
    if file_name
        == OsStr::new(match kind {
            ClaimFile::Lease => ".lease",
            ClaimFile::Pending => ".pending",
            ClaimFile::Active => ".active",
        })
    {
        return Some(OsStr::new(""));
    }
    (path.extension() == Some(OsStr::new(suffix)))
        .then(|| path.file_stem())
        .flatten()
}

fn marker_exists(path: &Path) -> Result<bool, ResourceAdmissionError> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ResourceAdmissionError::io(path, error)),
    }
}

fn remove_if_present(path: &Path) -> Result<(), ResourceAdmissionError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResourceAdmissionError::io(path, error)),
    }
}

fn remove_claim_files(claims_dir: &Path, claim_id: &str) -> Result<(), ResourceAdmissionError> {
    remove_if_present(&claim_path(claims_dir, claim_id, ClaimFile::Pending))?;
    remove_if_present(&claim_path(claims_dir, claim_id, ClaimFile::Active))?;
    remove_if_present(&claim_path(claims_dir, claim_id, ClaimFile::Lease))
}

fn remove_claim_files_os(
    claims_dir: &Path,
    claim_id: &OsStr,
) -> Result<(), ResourceAdmissionError> {
    for kind in [ClaimFile::Pending, ClaimFile::Active, ClaimFile::Lease] {
        let mut file_name = claim_id.to_os_string();
        file_name.push(".");
        file_name.push(kind.suffix());
        remove_if_present(&claims_dir.join(file_name))?;
    }
    Ok(())
}

fn admission_decision(resource: FixedResource, snapshot: &ClaimSnapshot, ticket: u64) -> bool {
    if snapshot.has_unknown_live_claim() {
        return false;
    }
    let claims = snapshot.claims();
    let Some(contender) = claims.iter().find(|claim| claim.metadata.ticket == ticket) else {
        return false;
    };
    if contender.state != ClaimState::Pending {
        return false;
    }

    let active = snapshot.active_total();
    let capacity = snapshot
        .fixed_capacity_min()
        .unwrap_or_else(|| unreachable!("the contender is one live claim"));
    if active >= capacity {
        return false;
    }

    match resource {
        FixedResource::Cargo => snapshot.oldest_pending_ticket() == Some(ticket),
        FixedResource::Work => work_admission_decision(snapshot, contender),
    }
}

fn work_admission_decision(snapshot: &ClaimSnapshot, contender: &LiveClaim) -> bool {
    if snapshot.has_active_barrier() {
        return false;
    }
    let claims = snapshot.claims();

    match contender.metadata.kind {
        ClaimKind::Normal => {
            if snapshot.has_pending_barrier() {
                return false;
            }
            oldest_pending_ticket(claims, |claim| claim.metadata.kind == ClaimKind::Normal)
                == Some(contender.metadata.ticket)
        }
        ClaimKind::Barrier => {
            oldest_pending_ticket(claims, |claim| claim.metadata.kind == ClaimKind::Barrier)
                == Some(contender.metadata.ticket)
        }
        ClaimKind::Adaptive => false,
    }
}

fn oldest_pending_ticket(
    claims: &[LiveClaim],
    predicate: impl Fn(&LiveClaim) -> bool,
) -> Option<u64> {
    claims
        .iter()
        .filter(|claim| claim.state == ClaimState::Pending && predicate(claim))
        .map(|claim| claim.metadata.ticket)
        .min()
}

#[derive(Debug)]
pub struct ResourceAdmissionError {
    path: PathBuf,
    message: String,
    io_kind: Option<std::io::ErrorKind>,
}

impl ResourceAdmissionError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            message: source.to_string(),
            io_kind: Some(source.kind()),
        }
    }

    fn invalid(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
            io_kind: None,
        }
    }

    fn io_kind(&self) -> Option<std::io::ErrorKind> {
        self.io_kind
    }
}

impl std::fmt::Display for ResourceAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "resource admission at {}: {}",
            self.path.display(),
            self.message
        )
    }
}

impl std::error::Error for ResourceAdmissionError {}

#[cfg(test)]
mod tests {
    use super::{
        ClaimFile, ClaimKind, ClaimMetadata, ClaimSnapshot, ClaimState, ClaimStore, FixedResource,
        FixedResourceAdmission, LiveClaim, SchedulerResource, StoreDecision, StoreWaitReason,
        WorkMode, activate_claim, admission_decision, claim_path, lock_state, scan_live_claims,
    };
    use std::fs::{File, OpenOptions, TryLockError};
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::fd::AsRawFd;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::{ffi::OsStr, ffi::OsString};

    static ROOT_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn root(label: &str) -> PathBuf {
        let ordinal = ROOT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "kio-resource-admission-{label}-{}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn claim(ticket: u64, capacity: usize, kind: ClaimKind, state: ClaimState) -> LiveClaim {
        LiveClaim {
            metadata: ClaimMetadata {
                ticket,
                capacity: NonZeroUsize::new(capacity).unwrap(),
                kind,
            },
            state,
        }
    }

    fn permit_ticket(permit: &super::FixedResourcePermit) -> u64 {
        permit.lease.metadata.ticket
    }

    fn snapshot<const N: usize>(claims: [LiveClaim; N]) -> ClaimSnapshot {
        ClaimSnapshot {
            claims: Vec::from(claims),
            has_unknown_live_claim: false,
        }
    }

    fn resource_claims_dir(root: &Path, resource: FixedResource) -> PathBuf {
        root.join(resource.as_str()).join("claims")
    }

    fn wait_for_pending(root: &Path, resource: FixedResource, count: usize) {
        let claims_dir = resource_claims_dir(root, resource);
        for _ in 0..500 {
            let pending = std::fs::read_dir(&claims_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".pending"))
                })
                .count();
            if pending == count {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {count} pending claims");
    }

    fn create_zero_byte(path: &Path) {
        File::create(path).unwrap();
    }

    fn assert_lock_state(path: &Path, expected_locked: bool) {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap();
        match (lock.try_lock(), expected_locked) {
            (Err(TryLockError::WouldBlock), true) | (Ok(()), false) => {}
            (result, expected_locked) => {
                panic!("unexpected state-lock result {result:?}; locked={expected_locked}")
            }
        }
    }

    #[test]
    fn claim_identifiers_round_trip_without_lease_contents() {
        let path = Path::new("claim.lease");
        for metadata in [
            ClaimMetadata {
                ticket: 1,
                capacity: NonZeroUsize::new(1).unwrap(),
                kind: ClaimKind::Normal,
            },
            ClaimMetadata {
                ticket: u64::MAX,
                capacity: NonZeroUsize::new(usize::MAX).unwrap(),
                kind: ClaimKind::Barrier,
            },
            ClaimMetadata {
                ticket: 17,
                capacity: NonZeroUsize::new(2).unwrap(),
                kind: ClaimKind::Adaptive,
            },
        ] {
            assert_eq!(
                ClaimMetadata::parse(&metadata.claim_id(), path).unwrap(),
                metadata
            );
        }
    }

    #[test]
    fn malformed_claim_identifiers_are_rejected() {
        let path = Path::new("claim.lease");
        for claim_id in [
            "v0-00000000000000000001-n-00000000000000000001",
            "v1-1-n-00000000000000000001",
            "v1-00000000000000000000-n-00000000000000000001",
            "v1-00000000000000000001-x-00000000000000000001",
            "v1-00000000000000000001-n-00000000000000000000",
            "v1-00000000000000000001-n-1-extra",
        ] {
            assert!(ClaimMetadata::parse(claim_id, path).is_err(), "{claim_id}");
        }
    }

    #[test]
    fn ticket_derivation_uses_only_the_live_claim_set() {
        assert_eq!(snapshot([]).next_ticket(), Some(1));
        assert_eq!(
            snapshot([
                claim(4, 2, ClaimKind::Normal, ClaimState::Active),
                claim(9, 2, ClaimKind::Normal, ClaimState::Pending),
                claim(7, 2, ClaimKind::Barrier, ClaimState::Pending),
            ])
            .next_ticket(),
            Some(10)
        );
        assert_eq!(
            snapshot([claim(u64::MAX, 1, ClaimKind::Normal, ClaimState::Pending,)]).next_ticket(),
            None
        );
    }

    #[test]
    fn compiler_snapshot_exposes_only_revalidated_live_state() {
        let snapshot = snapshot([
            claim(4, 5, ClaimKind::Adaptive, ClaimState::Active),
            claim(5, 3, ClaimKind::Normal, ClaimState::Active),
            claim(8, 1, ClaimKind::Normal, ClaimState::Pending),
            claim(9, 4, ClaimKind::Barrier, ClaimState::Pending),
        ]);
        assert_eq!(snapshot.active_total(), 2);
        assert_eq!(snapshot.active_adaptive(), 1);
        assert_eq!(snapshot.fixed_capacity_min(), Some(1));
        assert_eq!(snapshot.oldest_pending_ticket(), Some(8));
        assert_eq!(snapshot.newest_live_ticket(), Some(9));
        assert!(!snapshot.has_active_barrier());
        assert!(snapshot.has_pending_barrier());
        assert!(!snapshot.has_unknown_live_claim());
    }

    #[test]
    fn reconsider_rescans_before_activation() {
        let root = root("reconsider-boundary");
        let store = ClaimStore::open(root.clone(), SchedulerResource::Compiler).unwrap();
        let lock_path = root.join("compiler/state.lock");
        let (lease, outcome) = store
            .create_pending(
                NonZeroUsize::new(2).unwrap(),
                ClaimKind::Adaptive,
                |snapshot, ticket| {
                    assert_lock_state(&lock_path, true);
                    assert_eq!(snapshot.oldest_pending_ticket(), Some(ticket));
                    StoreDecision::Wait
                },
            )
            .unwrap();
        assert_eq!(outcome.decision, StoreDecision::Wait);
        assert_eq!(lease.resource(), SchedulerResource::Compiler);
        assert_lock_state(&lock_path, false);

        let stale = ClaimMetadata {
            ticket: 99,
            capacity: NonZeroUsize::new(7).unwrap(),
            kind: ClaimKind::Normal,
        }
        .claim_id();
        let claims_dir = root.join("compiler/claims");
        create_zero_byte(&claim_path(&claims_dir, &stale, ClaimFile::Lease));
        create_zero_byte(&claim_path(&claims_dir, &stale, ClaimFile::Pending));

        let outcome = store
            .reconsider(&lease, |snapshot, ticket| {
                assert_lock_state(&lock_path, true);
                assert_eq!(snapshot.oldest_pending_ticket(), Some(ticket));
                assert!(!claim_path(&claims_dir, &stale, ClaimFile::Lease).exists());
                assert!(!claim_path(&claims_dir, &stale, ClaimFile::Pending).exists());
                StoreDecision::Admit
            })
            .unwrap();
        assert_eq!(outcome.decision, StoreDecision::Admit);
        assert_lock_state(&lock_path, false);
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_live_lease_is_a_store_level_fail_closed_condition() {
        let root = root("unknown-live");
        let store = ClaimStore::open(root.clone(), SchedulerResource::Work).unwrap();
        let claims_dir = root.join("work/claims");
        let unknown_path = claims_dir.join("unknown.lease");
        let unknown = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&unknown_path)
            .unwrap();
        unknown.lock().unwrap();

        let (lease, outcome) = store
            .create_pending(NonZeroUsize::new(2).unwrap(), ClaimKind::Normal, |_, _| {
                panic!("unknown live state must bypass caller policy")
            })
            .unwrap();
        assert_eq!(outcome.decision, StoreDecision::Wait);
        assert_eq!(outcome.wait_reason, Some(StoreWaitReason::UnknownLiveClaim));
        drop(lease);
        drop(unknown);
        let _ = std::fs::remove_file(unknown_path);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_live_lease_is_not_skipped_or_stripped_of_its_marker() {
        let root = root("non-unicode-live");
        let store = ClaimStore::open(root.clone(), SchedulerResource::Work).unwrap();
        let claims_dir = root.join("work/claims");
        let lease_name = OsString::from_vec(vec![0xff, b'.', b'l', b'e', b'a', b's', b'e']);
        let pending_name =
            OsString::from_vec(vec![0xff, b'.', b'p', b'e', b'n', b'd', b'i', b'n', b'g']);
        let lease_path = claims_dir.join(lease_name);
        let pending_path = claims_dir.join(pending_name);
        let unknown = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease_path);
        let unknown = match unknown {
            Ok(unknown) => unknown,
            Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => {
                // Some filesystems reject non-UTF-8 names before a claim can exist.
                assert!(!lease_path.exists());
                std::fs::remove_dir_all(root).unwrap();
                eprintln!("filesystem rejects the non-Unicode lease fixture: {error}");
                return;
            }
            Err(error) => panic!("cannot create non-Unicode lease fixture: {error}"),
        };
        unknown.lock().unwrap();
        create_zero_byte(&pending_path);

        let (lease, outcome) = store
            .create_pending(NonZeroUsize::new(2).unwrap(), ClaimKind::Normal, |_, _| {
                panic!("a non-Unicode live claim must bypass caller policy")
            })
            .unwrap();
        assert_eq!(outcome.decision, StoreDecision::Wait);
        assert_eq!(outcome.wait_reason, Some(StoreWaitReason::UnknownLiveClaim));
        assert!(pending_path.exists());

        drop(lease);
        drop(unknown);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_admission_is_fifo_and_capacity_bounded() {
        let claims = snapshot([
            claim(1, 2, ClaimKind::Normal, ClaimState::Active),
            claim(2, 2, ClaimKind::Normal, ClaimState::Pending),
            claim(3, 2, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(admission_decision(FixedResource::Cargo, &claims, 2));
        assert!(!admission_decision(FixedResource::Cargo, &claims, 3));

        let full = snapshot([
            claim(1, 1, ClaimKind::Normal, ClaimState::Active),
            claim(2, 2, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(!admission_decision(FixedResource::Cargo, &full, 2));
    }

    #[test]
    fn every_live_claim_contributes_to_the_effective_capacity() {
        let pending_reduction = snapshot([
            claim(1, 4, ClaimKind::Normal, ClaimState::Active),
            claim(2, 4, ClaimKind::Normal, ClaimState::Active),
            claim(3, 1, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(!admission_decision(
            FixedResource::Work,
            &pending_reduction,
            3
        ));

        let active_reduction = snapshot([
            claim(1, 1, ClaimKind::Normal, ClaimState::Active),
            claim(2, 4, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(!admission_decision(
            FixedResource::Work,
            &active_reduction,
            2
        ));
    }

    #[test]
    fn active_work_barrier_blocks_every_new_admission() {
        let claims = snapshot([
            claim(1, 3, ClaimKind::Normal, ClaimState::Active),
            claim(2, 3, ClaimKind::Barrier, ClaimState::Active),
            claim(3, 3, ClaimKind::Barrier, ClaimState::Pending),
            claim(4, 3, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(!admission_decision(FixedResource::Work, &claims, 3));
        assert!(!admission_decision(FixedResource::Work, &claims, 4));
    }

    #[test]
    fn pending_work_barrier_blocks_normals_and_has_barrier_fifo() {
        let claims = snapshot([
            claim(1, 3, ClaimKind::Normal, ClaimState::Pending),
            claim(2, 3, ClaimKind::Barrier, ClaimState::Pending),
            claim(3, 3, ClaimKind::Barrier, ClaimState::Pending),
        ]);
        assert!(!admission_decision(FixedResource::Work, &claims, 1));
        assert!(admission_decision(FixedResource::Work, &claims, 2));
        assert!(!admission_decision(FixedResource::Work, &claims, 3));
    }

    #[test]
    fn barrier_can_enter_beside_active_normals_when_a_slot_is_free() {
        let claims = snapshot([
            claim(1, 3, ClaimKind::Normal, ClaimState::Active),
            claim(2, 3, ClaimKind::Normal, ClaimState::Active),
            claim(3, 3, ClaimKind::Barrier, ClaimState::Pending),
        ]);
        assert!(admission_decision(FixedResource::Work, &claims, 3));
    }

    #[test]
    fn ordinary_work_is_fifo_without_a_barrier() {
        let claims = snapshot([
            claim(4, 3, ClaimKind::Normal, ClaimState::Pending),
            claim(5, 3, ClaimKind::Normal, ClaimState::Pending),
        ]);
        assert!(admission_decision(FixedResource::Work, &claims, 4));
        assert!(!admission_decision(FixedResource::Work, &claims, 5));
    }

    #[test]
    fn zero_capacity_is_rejected_before_state_creation() {
        let root = root("zero-capacity");
        assert!(FixedResourceAdmission::work(root.clone(), 0, WorkMode::Normal).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn schedule_root_is_canonical_and_permit_names_its_resource() {
        let root = root("canonical");
        std::fs::create_dir_all(root.join("child")).unwrap();
        let lexical = root.join("child/..");
        let admission = FixedResourceAdmission::work(lexical, 1, WorkMode::Normal).unwrap();
        let permit = admission.acquire().unwrap();
        assert_eq!(
            permit.schedule_root(),
            std::fs::canonicalize(&root).unwrap()
        );
        assert_eq!(permit.resource(), FixedResource::Work);
        assert_eq!(permit_ticket(&permit), 1);
        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lease_is_zero_byte_and_state_lives_in_markers() {
        let root = root("zero-byte-lease");
        let admission = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let permit = admission.acquire().unwrap();
        let claims_dir = resource_claims_dir(&root, FixedResource::Work);
        let claim_id = &permit.lease.claim_id;
        let lease_path = claim_path(&claims_dir, claim_id, ClaimFile::Lease);
        assert_eq!(std::fs::metadata(lease_path).unwrap().len(), 0);
        assert!(!claim_path(&claims_dir, claim_id, ClaimFile::Pending).exists());
        assert!(claim_path(&claims_dir, claim_id, ClaimFile::Active).exists());
        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_claims_and_orphan_markers_are_cleaned_before_ticket_allocation() {
        let root = root("stale-cleanup");
        let admission = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let claims_dir = resource_claims_dir(&root, FixedResource::Work);
        let stale = ClaimMetadata {
            ticket: 99,
            capacity: NonZeroUsize::new(4).unwrap(),
            kind: ClaimKind::Barrier,
        }
        .claim_id();
        for kind in [ClaimFile::Lease, ClaimFile::Pending, ClaimFile::Active] {
            create_zero_byte(&claim_path(&claims_dir, &stale, kind));
        }
        create_zero_byte(&claims_dir.join("orphan.pending"));
        create_zero_byte(&claims_dir.join("orphan.active"));

        let permit = admission.acquire().unwrap();
        assert_eq!(permit_ticket(&permit), 1);
        for kind in [ClaimFile::Lease, ClaimFile::Pending, ClaimFile::Active] {
            assert!(!claim_path(&claims_dir, &stale, kind).exists());
        }
        assert!(!claims_dir.join("orphan.pending").exists());
        assert!(!claims_dir.join("orphan.active").exists());
        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn active_marker_wins_an_interrupted_pending_to_active_transition() {
        let root = root("active-wins");
        let _admission = FixedResourceAdmission::work(root.clone(), 2, WorkMode::Normal).unwrap();
        let claims_dir = resource_claims_dir(&root, FixedResource::Work);
        let metadata = ClaimMetadata {
            ticket: 1,
            capacity: NonZeroUsize::new(2).unwrap(),
            kind: ClaimKind::Normal,
        };
        let claim_id = metadata.claim_id();
        let lease_path = claim_path(&claims_dir, &claim_id, ClaimFile::Lease);
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease_path)
            .unwrap();
        lease.lock().unwrap();
        create_zero_byte(&claim_path(&claims_dir, &claim_id, ClaimFile::Pending));
        create_zero_byte(&claim_path(&claims_dir, &claim_id, ClaimFile::Active));

        let state_lock = lock_state(&root.join("work/state.lock")).unwrap();
        let claims = scan_live_claims(&claims_dir).unwrap();
        drop(state_lock);
        assert_eq!(
            claims.claims(),
            &[claim(1, 2, ClaimKind::Normal, ClaimState::Active)]
        );
        assert!(!claim_path(&claims_dir, &claim_id, ClaimFile::Pending).exists());
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn activation_is_idempotent_after_both_markers_survive() {
        let root = root("activation-idempotent");
        let claims_dir = root.join("claims");
        std::fs::create_dir_all(&claims_dir).unwrap();
        let claim_id = "v1-00000000000000000001-n-00000000000000000001";
        create_zero_byte(&claim_path(&claims_dir, claim_id, ClaimFile::Pending));
        create_zero_byte(&claim_path(&claims_dir, claim_id, ClaimFile::Active));
        activate_claim(&claims_dir, claim_id).unwrap();
        assert!(!claim_path(&claims_dir, claim_id, ClaimFile::Pending).exists());
        assert!(claim_path(&claims_dir, claim_id, ClaimFile::Active).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_persistent_ticket_counter_is_created_and_empty_queues_reuse_one() {
        let root = root("no-counter");
        let admission = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let first = admission.acquire().unwrap();
        assert_eq!(permit_ticket(&first), 1);
        drop(first);
        let second = admission.acquire().unwrap();
        assert_eq!(permit_ticket(&second), 1);
        drop(second);
        assert!(!root.join("work/next-ticket").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fixed_capacity_blocks_until_an_active_permit_leaves() {
        let root = root("fixed-capacity");
        let admission = FixedResourceAdmission::work(root.clone(), 2, WorkMode::Normal).unwrap();
        let first = admission.acquire().unwrap();
        let second = admission.acquire().unwrap();
        let contender = admission.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let permit = contender.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        wait_for_pending(&root, FixedResource::Work, 1);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(second);
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_waiters_acquire_in_fifo_order() {
        let root = root("fifo");
        let admission = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let first = admission.acquire().unwrap();
        let (acquired_tx, acquired_rx) = mpsc::channel();

        let second_admission = admission.clone();
        let second_tx = acquired_tx.clone();
        let second = std::thread::spawn(move || {
            let permit = second_admission.acquire().unwrap();
            second_tx.send((2, permit)).unwrap();
        });
        wait_for_pending(&root, FixedResource::Work, 1);

        let third_admission = admission.clone();
        let third = std::thread::spawn(move || {
            let permit = third_admission.acquire().unwrap();
            acquired_tx.send((3, permit)).unwrap();
        });
        wait_for_pending(&root, FixedResource::Work, 2);

        drop(first);
        let (label, second_permit) = acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(label, 2);
        assert_eq!(permit_ticket(&second_permit), 2);
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        drop(second_permit);
        let (label, third_permit) = acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(label, 3);
        assert_eq!(permit_ticket(&third_permit), 3);
        drop(third_permit);
        second.join().unwrap();
        third.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pending_lower_capacity_applies_before_it_is_admitted() {
        let root = root("mixed-capacity");
        let wide = FixedResourceAdmission::work(root.clone(), 2, WorkMode::Normal).unwrap();
        let narrow = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let first = wide.acquire().unwrap();
        let second = wide.acquire().unwrap();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let permit = narrow.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        wait_for_pending(&root, FixedResource::Work, 1);
        drop(first);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(second);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn later_barrier_precedes_an_older_normal_waiter() {
        let root = root("barrier-priority");
        let active = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal)
            .unwrap()
            .acquire()
            .unwrap();
        let normal_admission =
            FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let barrier_admission =
            FixedResourceAdmission::work(root.clone(), 1, WorkMode::Barrier).unwrap();
        let (tx, rx) = mpsc::channel();

        let normal_tx = tx.clone();
        let normal = std::thread::spawn(move || {
            let permit = normal_admission.acquire().unwrap();
            normal_tx.send(("normal", permit)).unwrap();
        });
        wait_for_pending(&root, FixedResource::Work, 1);
        let barrier = std::thread::spawn(move || {
            let permit = barrier_admission.acquire().unwrap();
            tx.send(("barrier", permit)).unwrap();
        });
        wait_for_pending(&root, FixedResource::Work, 2);

        drop(active);
        let (label, barrier_permit) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(label, "barrier");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(barrier_permit);
        let (label, normal_permit) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(label, "normal");
        drop(normal_permit);
        normal.join().unwrap();
        barrier.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn barrier_enters_beside_existing_normal_work() {
        let root = root("barrier-beside-normal");
        let normal = FixedResourceAdmission::work(root.clone(), 2, WorkMode::Normal)
            .unwrap()
            .acquire()
            .unwrap();
        let barrier = FixedResourceAdmission::work(root.clone(), 2, WorkMode::Barrier)
            .unwrap()
            .acquire()
            .unwrap();
        assert_eq!(permit_ticket(&normal), 1);
        assert_eq!(permit_ticket(&barrier), 2);
        drop((normal, barrier));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_resource_is_independent_and_serial() {
        let root = root("cargo");
        let work = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal)
            .unwrap()
            .acquire()
            .unwrap();
        let cargo_admission = FixedResourceAdmission::cargo(root.clone()).unwrap();
        let first_cargo = cargo_admission.acquire().unwrap();
        assert_eq!(permit_ticket(&work), 1);
        assert_eq!(permit_ticket(&first_cargo), 1);

        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let permit = cargo_admission.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        wait_for_pending(&root, FixedResource::Cargo, 1);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first_cargo);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(thread.join().unwrap());
        drop(work);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inherited_lease_blocks_reuse_until_the_descendant_exits() {
        let root = root("inherited-lease");
        let admission = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal).unwrap();
        let permit = admission.acquire().unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 0.25"]);
        permit.prepare_inherited_lease(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        drop(permit);

        let (tx, rx) = mpsc::channel();
        let contender = admission.clone();
        let thread = std::thread::spawn(move || {
            let permit = contender.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(child.wait().unwrap().success());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inherited_lease_inventory_is_strict_ordered_and_idempotent() {
        use super::{LEASE_FDS_ENV, inherited_lease_fds_from_env, parse_lease_fds};

        let root = root("inherited-inventory");
        let permit = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal)
            .unwrap()
            .acquire()
            .unwrap();
        let fd = permit.lease.file.as_ref().unwrap().as_raw_fd();
        assert!(fd >= 3);
        let existing = fd.checked_add(100).unwrap();
        let mut command = Command::new("unused");
        command.env(LEASE_FDS_ENV, existing.to_string());

        let mut expected = inherited_lease_fds_from_env().unwrap();
        if !expected.contains(&existing) {
            expected.push(existing);
        }
        if !expected.contains(&fd) {
            expected.push(fd);
        }

        permit.prepare_inherited_lease(&mut command).unwrap();
        permit.prepare_inherited_lease(&mut command).unwrap();
        let encoded = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(LEASE_FDS_ENV))
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(parse_lease_fds(encoded).unwrap(), expected);
        assert!(inherited_lease_fds_from_env().is_ok());

        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn command_environment_removal_cannot_discard_ambient_lease_inventory() {
        use super::{LEASE_FDS_ENV, inherited_lease_fds_from_env, parse_lease_fds};

        let root = root("removed-inventory");
        let permit = FixedResourceAdmission::work(root.clone(), 1, WorkMode::Normal)
            .unwrap()
            .acquire()
            .unwrap();
        let fd = permit.lease.file.as_ref().unwrap().as_raw_fd();
        let mut expected = inherited_lease_fds_from_env().unwrap();
        if !expected.contains(&fd) {
            expected.push(fd);
        }
        let mut command = Command::new("unused");
        command.env_remove(LEASE_FDS_ENV);
        permit.prepare_inherited_lease(&mut command).unwrap();
        let encoded = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(LEASE_FDS_ENV))
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(parse_lease_fds(encoded).unwrap(), expected);

        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn command_environment_override_cannot_discard_ambient_lease_inventory() {
        use super::{LEASE_FDS_ENV, append_command_lease_fd_with_ambient, parse_lease_fds};

        let mut command = Command::new("unused");
        command.env(LEASE_FDS_ENV, "41,43");
        append_command_lease_fd_with_ambient(&mut command, 44, vec![40, 41, 42]).unwrap();
        let encoded = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(LEASE_FDS_ENV))
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(parse_lease_fds(encoded).unwrap(), [40, 41, 42, 43, 44]);
    }

    #[cfg(unix)]
    #[test]
    fn malformed_or_duplicate_lease_inventories_are_rejected() {
        use super::{append_command_lease_fd, parse_lease_fds};

        for value in [
            "-1",
            "0",
            "1",
            "2",
            "03",
            "+3",
            " 3",
            "3 ",
            "3,",
            ",3",
            "3,,4",
            "x",
            "3,3",
            "2147483648",
        ] {
            assert!(parse_lease_fds(OsStr::new(value)).is_err(), "{value:?}");
        }
        assert!(parse_lease_fds(&OsString::from_vec(vec![0xff])).is_err());
        assert_eq!(parse_lease_fds(OsStr::new("")).unwrap(), Vec::<i32>::new());
        assert_eq!(parse_lease_fds(OsStr::new("3,7,19")).unwrap(), [3, 7, 19]);

        let mut command = Command::new("unused");
        assert!(append_command_lease_fd(&mut command, 2).is_err());
    }
}
