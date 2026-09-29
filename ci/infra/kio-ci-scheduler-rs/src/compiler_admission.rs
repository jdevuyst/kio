//! Shared compiler-process admission for Kio's native compiler producers.
//!
//! Every client shares the scheduler's cross-platform claim store:
//!
//! ```text
//! <KIO_CI_SCHEDULE_DIR>/compiler/
//!   state.lock
//!   claims/<immutable-id>.lease
//!   claims/<immutable-id>.pending|active
//! ```
//!
//! Explicit capacities remain hard ceilings. Omitted capacity uses paced,
//! best-effort aggregate feedback, not per-command memory reservations.
//! The common store owns crash cleanup,
//! live-only FIFO tickets, and process-tree lease lifetime.

use std::env;
use std::ffi::{OsStr, OsString};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::time::Duration;

use crate::compiler_trace::{CompilerTrace, CompilerTraceConfig};
use crate::held_resources::{HeldResource, HeldResources};
use crate::resource_admission::{
    ClaimKind, ClaimLease, ClaimSnapshot, ClaimStore, ResourceAdmissionError, SchedulerResource,
    StoreDecision, StoreOutcome, StoreWaitReason,
};

#[cfg(test)]
const TEST_FEEDBACK_TARGET: usize = 2;
const READINESS_HOOK_PROGRAM_ENV: &str = "KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM";
const READINESS_HOOK_ARG_COUNT_ENV: &str = "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT";
const READINESS_HOOK_ARG_ENV_PREFIX: &str = "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionMode {
    Fixed(usize),
    Adaptive,
}

impl AdmissionMode {
    fn claim_capacity(self) -> NonZeroUsize {
        let capacity = match self {
            Self::Fixed(jobs) => jobs,
            Self::Adaptive => crate::available_parallelism::get().unwrap_or(1),
        };
        NonZeroUsize::new(capacity)
            .unwrap_or_else(|| unreachable!("compiler admission validates positive capacities"))
    }

    const fn claim_kind(self) -> ClaimKind {
        match self {
            Self::Fixed(_) => ClaimKind::Normal,
            Self::Adaptive => ClaimKind::Adaptive,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Fixed(_) => "fixed",
            Self::Adaptive => "adaptive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompilerAdmission {
    context: AdmissionContext,
    readiness_hook: Option<ReadinessHook>,
    trace: Option<CompilerTraceConfig>,
}

#[derive(Clone, Debug)]
enum AdmissionContext {
    Disabled,
    Shared {
        store: ClaimStore,
        mode: AdmissionMode,
    },
    Inherited(InheritedAdmission),
}

#[derive(Clone, Debug)]
struct InheritedAdmission {
    held: HeldResources,
    #[cfg(unix)]
    lease_fds: Vec<std::os::fd::RawFd>,
}

#[derive(Clone, Debug)]
struct ReadinessHook {
    program: OsString,
    prefix_args: Vec<OsString>,
}

impl CompilerAdmission {
    pub fn disabled() -> Self {
        Self {
            context: AdmissionContext::Disabled,
            readiness_hook: None,
            trace: None,
        }
    }

    pub fn shared(schedule_dir: PathBuf, jobs: usize) -> Result<Self, Error> {
        if jobs == 0 {
            return Err(Error::invalid(
                schedule_dir,
                "compiler admission capacity must be at least one",
            ));
        }
        Self::with_mode(schedule_dir, AdmissionMode::Fixed(jobs))
    }

    pub fn adaptive(schedule_dir: PathBuf) -> Result<Self, Error> {
        Self::with_mode(schedule_dir, AdmissionMode::Adaptive)
    }

    fn with_mode(schedule_dir: PathBuf, mode: AdmissionMode) -> Result<Self, Error> {
        let store = ClaimStore::open(schedule_dir, SchedulerResource::Compiler)?;
        let trace = CompilerTraceConfig::from_env(store.schedule_root()).map_err(Error::trace)?;
        Ok(Self {
            context: AdmissionContext::Shared { store, mode },
            readiness_hook: None,
            trace,
        })
    }

    /// Resolve the scheduler context inherited from `ci/run-tests.sh`.
    ///
    /// Environment parsing lives here, at runner setup. The build cache
    /// receives the resulting capability and never reads scheduler variables.
    pub fn from_env() -> Result<Self, Error> {
        let readiness_hook = ReadinessHook::from_env()?;
        let held = held_resources_from_env()?;
        let mut admission = Self::from_schedule_env(&held)?;
        if !held.contains(HeldResource::Compiler) {
            admission.readiness_hook = readiness_hook;
        }
        Ok(admission)
    }

    /// Resolve scheduler capacity and inherited resources for the native CLI.
    ///
    /// Readiness-hook environment variables belong to in-process library
    /// clients such as the test runners. The CLI receives its hook explicitly
    /// through arguments and must not accidentally inherit that library hook.
    pub(crate) fn from_scheduler_env() -> Result<Self, Error> {
        let held = held_resources_from_env()?;
        Self::from_schedule_env(&held)
    }

    fn from_schedule_env(held: &HeldResources) -> Result<Self, Error> {
        match env::var("KIO_CI_SCHEDULE") {
            Ok(value) if value == "DISABLE" => return Ok(Self::disabled()),
            Ok(value) if !value.is_empty() => {
                return Err(Error::invalid(
                    PathBuf::from("KIO_CI_SCHEDULE"),
                    format!("KIO_CI_SCHEDULE must be DISABLE or unset (got {value})"),
                ));
            }
            Ok(_) | Err(env::VarError::NotPresent) => {}
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::invalid(
                    PathBuf::from("KIO_CI_SCHEDULE"),
                    "KIO_CI_SCHEDULE must be valid Unicode",
                ));
            }
        }

        if held.contains(HeldResource::Compiler) {
            return Self::reusing(*held);
        }

        let Some(raw_dir) = env::var_os("KIO_CI_SCHEDULE_DIR") else {
            return Ok(Self::disabled());
        };
        if raw_dir.is_empty() {
            return Err(Error::invalid(
                PathBuf::from("KIO_CI_SCHEDULE_DIR"),
                "KIO_CI_SCHEDULE_DIR must not be empty",
            ));
        }
        let schedule_dir = PathBuf::from(raw_dir);
        match env::var("KIO_CI_SCHEDULE_COMPILER_JOBS") {
            Ok(value) => {
                let jobs = value
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    Error::invalid(
                        PathBuf::from("KIO_CI_SCHEDULE_COMPILER_JOBS"),
                        format!(
                            "KIO_CI_SCHEDULE_COMPILER_JOBS must be a positive integer (got {value})"
                        ),
                    )
                })?;
                Self::shared(schedule_dir, jobs)
            }
            Err(env::VarError::NotPresent) => Self::adaptive(schedule_dir),
            Err(env::VarError::NotUnicode(_)) => Err(Error::invalid(
                PathBuf::from("KIO_CI_SCHEDULE_COMPILER_JOBS"),
                "KIO_CI_SCHEDULE_COMPILER_JOBS must be valid Unicode",
            )),
        }
    }

    fn reusing(held: HeldResources) -> Result<Self, Error> {
        Ok(Self {
            context: AdmissionContext::Inherited(InheritedAdmission {
                held,
                #[cfg(unix)]
                lease_fds: crate::resource_admission::inherited_lease_fds_from_env()?,
            }),
            readiness_hook: None,
            trace: None,
        })
    }

    pub(crate) fn configure_readiness_hook(
        &mut self,
        program: OsString,
        prefix_args: Vec<OsString>,
    ) -> Result<(), Error> {
        if program.is_empty() {
            return Err(Error::invalid(
                PathBuf::from("readiness hook"),
                "readiness hook program must not be empty",
            ));
        }
        self.readiness_hook = Some(ReadinessHook {
            program,
            prefix_args,
        });
        Ok(())
    }

    pub fn acquire(&self) -> Result<CompilerPermit, Error> {
        self.acquire_for_program_inner(None)
    }

    pub(crate) fn acquire_for_program(&self, program: &OsStr) -> Result<CompilerPermit, Error> {
        self.acquire_for_program_inner(Some(program))
    }

    fn acquire_for_program_inner(&self, program: Option<&OsStr>) -> Result<CompilerPermit, Error> {
        self.acquire_with_program(program)
    }

    fn acquire_with_program(&self, program: Option<&OsStr>) -> Result<CompilerPermit, Error> {
        let (store, mode) = match &self.context {
            AdmissionContext::Disabled => {
                return Ok(CompilerPermit {
                    context: PermitContext::Disabled,
                });
            }
            AdmissionContext::Inherited(inherited) => {
                return Ok(CompilerPermit {
                    context: PermitContext::Inherited(inherited.clone()),
                });
            }
            AdmissionContext::Shared { store, mode } => (store, *mode),
        };
        let capacity = mode.claim_capacity();
        let mut evaluation = None;
        let (lease, mut outcome) =
            store.create_pending(capacity, mode.claim_kind(), |snapshot, ticket| {
                let current = admission_evaluation_with_feedback(store, snapshot, ticket);
                evaluation = Some(current);
                store_decision(current.decision)
            })?;
        let current = evaluation.unwrap_or_else(|| store_wait_evaluation(outcome));
        debug_assert_eq!(outcome.decision, store_decision(current.decision));
        let mut trace = self
            .trace
            .as_ref()
            .map(|config| config.start(program, mode.label(), capacity.get(), outcome.ticket))
            .transpose()
            .map_err(Error::trace)?;
        record_trace(&mut trace, current)?;

        loop {
            match outcome.decision {
                StoreDecision::Admit => {
                    return Ok(CompilerPermit {
                        context: PermitContext::Acquired(lease),
                    });
                }
                StoreDecision::Wait => std::thread::sleep(Duration::from_millis(50)),
            }
            evaluation = None;
            outcome = store.reconsider(&lease, |snapshot, ticket| {
                let current = admission_evaluation_with_feedback(store, snapshot, ticket);
                evaluation = Some(current);
                store_decision(current.decision)
            })?;
            let current = evaluation.unwrap_or_else(|| store_wait_evaluation(outcome));
            debug_assert_eq!(outcome.decision, store_decision(current.decision));
            record_trace(&mut trace, current)?;
        }
    }

    /// Acquire a permit and arrange for the spawned compiler process and its
    /// descendants to retain the lease if this supervising runner dies.
    pub fn acquire_for<'command>(
        &self,
        command: &'command mut Command,
    ) -> Result<AdmittedCommand<'command>, Error> {
        let permit = self.acquire_for_program(command.get_program())?;
        if let Some(status) = self.run_readiness_hook(command)?
            && !status.success()
        {
            let hook = self.readiness_hook.as_ref().unwrap_or_else(|| {
                unreachable!("a readiness status is returned only for a configured hook")
            });
            return Err(Error::invalid(
                PathBuf::from(&hook.program),
                format!("post-admission readiness hook exited with {status}"),
            ));
        }
        permit.prepare_for(command)?;
        Ok(AdmittedCommand {
            _permit: permit,
            command,
        })
    }

    pub(crate) fn run_readiness_hook(&self, target: &Command) -> Result<Option<ExitStatus>, Error> {
        let Some(hook) = &self.readiness_hook else {
            return Ok(None);
        };
        let mut command = Command::new(&hook.program);
        command
            .args(&hook.prefix_args)
            .arg("--compiler-readiness")
            .arg("--")
            .arg(target.get_program())
            .args(target.get_args())
            .env_remove("KIO_CI_SCHEDULE_HELD");
        crate::process_supervisor::readiness_status(&mut command)
            .map(Some)
            .map_err(|error| Error::io(Path::new(&hook.program), error))
    }
}

impl ReadinessHook {
    fn from_env() -> Result<Option<Self>, Error> {
        let program = env::var_os(READINESS_HOOK_PROGRAM_ENV);
        let count = match env::var(READINESS_HOOK_ARG_COUNT_ENV) {
            Ok(value) => Some(value.parse::<usize>().map_err(|_| {
                Error::invalid(
                    PathBuf::from(READINESS_HOOK_ARG_COUNT_ENV),
                    format!("{READINESS_HOOK_ARG_COUNT_ENV} must be a non-negative integer"),
                )
            })?),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::invalid(
                    PathBuf::from(READINESS_HOOK_ARG_COUNT_ENV),
                    format!("{READINESS_HOOK_ARG_COUNT_ENV} must be valid Unicode"),
                ));
            }
        };

        let mut configured_indices = std::collections::BTreeSet::new();
        for (name, _) in env::vars_os() {
            if name == OsStr::new(READINESS_HOOK_ARG_COUNT_ENV) {
                continue;
            }
            if !name
                .as_encoded_bytes()
                .starts_with(READINESS_HOOK_ARG_ENV_PREFIX.as_bytes())
            {
                continue;
            }
            let name = name.to_str().ok_or_else(|| {
                Error::invalid(
                    PathBuf::from(READINESS_HOOK_ARG_COUNT_ENV),
                    "readiness-hook argument variable names must be valid Unicode",
                )
            })?;
            let suffix = name
                .strip_prefix(READINESS_HOOK_ARG_ENV_PREFIX)
                .unwrap_or_else(|| unreachable!("the byte prefix was already checked"));
            if suffix.is_empty()
                || !suffix.bytes().all(|byte| byte.is_ascii_digit())
                || (suffix.len() > 1 && suffix.starts_with('0'))
            {
                return Err(Error::invalid(
                    PathBuf::from(name),
                    format!(
                        "{name} must use one canonical decimal argument index without leading zeroes"
                    ),
                ));
            }
            let index = suffix.parse::<usize>().map_err(|_| {
                Error::invalid(
                    PathBuf::from(name),
                    format!("{name} argument index is too large"),
                )
            })?;
            configured_indices.insert(index);
        }

        let Some(program) = program else {
            if count.is_some() || !configured_indices.is_empty() {
                return Err(Error::invalid(
                    PathBuf::from(READINESS_HOOK_PROGRAM_ENV),
                    format!(
                        "{READINESS_HOOK_PROGRAM_ENV} is required when readiness-hook arguments are configured"
                    ),
                ));
            }
            return Ok(None);
        };
        if program.is_empty() {
            return Err(Error::invalid(
                PathBuf::from(READINESS_HOOK_PROGRAM_ENV),
                format!("{READINESS_HOOK_PROGRAM_ENV} must not be empty"),
            ));
        }

        let count = count.unwrap_or(0);
        if configured_indices.len() != count || !configured_indices.iter().copied().eq(0..count) {
            let expected = if count == 0 {
                "no readiness-hook argument variables".to_owned()
            } else {
                format!(
                    "exactly {READINESS_HOOK_ARG_ENV_PREFIX}0 through {READINESS_HOOK_ARG_ENV_PREFIX}{}",
                    count - 1
                )
            };
            return Err(Error::invalid(
                PathBuf::from(READINESS_HOOK_ARG_COUNT_ENV),
                format!("{READINESS_HOOK_ARG_COUNT_ENV}={count} requires {expected}"),
            ));
        }
        let prefix_args = (0..count)
            .map(|index| {
                env::var_os(format!("{READINESS_HOOK_ARG_ENV_PREFIX}{index}")).unwrap_or_else(
                    || unreachable!("the exact readiness-hook argument inventory was validated"),
                )
            })
            .collect();
        Ok(Some(Self {
            program,
            prefix_args,
        }))
    }
}

fn held_resources_from_env() -> Result<HeldResources, Error> {
    match env::var("KIO_CI_SCHEDULE_HELD") {
        Ok(value) => HeldResources::parse(Some(&value)).map_err(|error| {
            Error::invalid(PathBuf::from("KIO_CI_SCHEDULE_HELD"), error.to_string())
        }),
        Err(env::VarError::NotPresent) => Ok(HeldResources::default()),
        Err(env::VarError::NotUnicode(_)) => Err(Error::invalid(
            PathBuf::from("KIO_CI_SCHEDULE_HELD"),
            "KIO_CI_SCHEDULE_HELD must be valid Unicode",
        )),
    }
}

fn append_command_held(command: &mut Command, resource: HeldResource) -> Result<(), Error> {
    let ambient = held_resources_from_env()?;
    append_command_held_with_ambient(command, resource, ambient)
}

fn append_command_held_with_ambient(
    command: &mut Command,
    resource: HeldResource,
    ambient: HeldResources,
) -> Result<(), Error> {
    let key = OsStr::new("KIO_CI_SCHEDULE_HELD");
    let configured = command
        .get_envs()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.map(OsStr::to_os_string));
    let configured = match configured {
        Some(Some(raw)) => {
            let value = raw.to_str().ok_or_else(|| {
                Error::invalid(
                    PathBuf::from("KIO_CI_SCHEDULE_HELD"),
                    "KIO_CI_SCHEDULE_HELD must be valid Unicode",
                )
            })?;
            HeldResources::parse(Some(value)).map_err(|error| {
                Error::invalid(PathBuf::from("KIO_CI_SCHEDULE_HELD"), error.to_string())
            })?
        }
        Some(None) | None => HeldResources::default(),
    };
    let inherited = ambient
        .union(configured)
        .with_requested(resource)
        .map_err(|error| {
            Error::invalid(PathBuf::from("KIO_CI_SCHEDULE_HELD"), error.to_string())
        })?;
    command.env(key, inherited.encode());
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionDecision {
    Admit,
    Wait,
}

impl AdmissionDecision {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Wait => "wait",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecisionBlocker {
    UnknownLiveClaim,
    NotFifoHead,
    AdaptiveCapacity,
    FixedCapacity,
}

impl DecisionBlocker {
    pub(crate) const ALL: [Self; 4] = [
        Self::UnknownLiveClaim,
        Self::NotFifoHead,
        Self::AdaptiveCapacity,
        Self::FixedCapacity,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::UnknownLiveClaim => "unknown-live",
            Self::NotFifoHead => "not-fifo",
            Self::AdaptiveCapacity => "adaptive-cap",
            Self::FixedCapacity => "fixed-cap",
        }
    }

    const fn mask(self) -> u16 {
        1 << self as u16
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DecisionBlockers(u16);

impl DecisionBlockers {
    pub(crate) const fn one(blocker: DecisionBlocker) -> Self {
        Self(blocker.mask())
    }

    pub(crate) const fn contains(self, blocker: DecisionBlocker) -> bool {
        self.0 & blocker.mask() != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AdmissionEvaluation {
    pub(crate) decision: AdmissionDecision,
    pub(crate) blockers: DecisionBlockers,
    pub(crate) ticket: Option<u64>,
    pub(crate) active_total: Option<usize>,
    pub(crate) active_adaptive: Option<usize>,
    pub(crate) pending_total: Option<usize>,
    pub(crate) oldest_pending_ticket: Option<u64>,
    pub(crate) fixed_capacity_min: Option<usize>,
    pub(crate) effective_limit: Option<usize>,
    pub(crate) candidate_slot: Option<usize>,
    pub(crate) adaptive_capacity: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct ClaimEvaluationContext {
    active_total: usize,
    active_adaptive: usize,
    pending_total: usize,
    oldest_pending_ticket: Option<u64>,
    fixed_capacity_min: Option<usize>,
    has_adaptive_claim: bool,
    adaptive_capacity_min: usize,
}

impl ClaimEvaluationContext {
    fn collect(claims: &ClaimSnapshot) -> Self {
        let mut context = Self {
            active_total: 0,
            active_adaptive: 0,
            pending_total: 0,
            oldest_pending_ticket: None,
            fixed_capacity_min: None,
            has_adaptive_claim: false,
            adaptive_capacity_min: usize::MAX,
        };
        for claim in claims.claims() {
            if claim.state == crate::resource_admission::ClaimState::Active {
                context.active_total += 1;
                if claim.metadata.kind == ClaimKind::Adaptive {
                    context.active_adaptive += 1;
                }
            } else {
                context.pending_total += 1;
                context.oldest_pending_ticket = Some(
                    context
                        .oldest_pending_ticket
                        .map_or(claim.metadata.ticket, |oldest| {
                            oldest.min(claim.metadata.ticket)
                        }),
                );
            }
            if claim.metadata.kind == ClaimKind::Adaptive {
                context.has_adaptive_claim = true;
                context.adaptive_capacity_min = context
                    .adaptive_capacity_min
                    .min(claim.metadata.capacity.get());
            } else {
                context.fixed_capacity_min = Some(
                    context
                        .fixed_capacity_min
                        .map_or(claim.metadata.capacity.get(), |capacity| {
                            capacity.min(claim.metadata.capacity.get())
                        }),
                );
            }
        }
        context
    }

    fn effective_limit(self, adaptive_limit: usize) -> usize {
        let fixed = self.fixed_capacity_min.unwrap_or(usize::MAX);
        if self.has_adaptive_claim {
            fixed.min(adaptive_limit).min(self.adaptive_capacity_min)
        } else {
            fixed
        }
    }
}

impl AdmissionEvaluation {
    fn from_context(
        context: ClaimEvaluationContext,
        ticket: u64,
        adaptive_limit: Option<usize>,
    ) -> Self {
        Self {
            decision: AdmissionDecision::Wait,
            blockers: DecisionBlockers::default(),
            ticket: Some(ticket),
            active_total: Some(context.active_total),
            active_adaptive: Some(context.active_adaptive),
            pending_total: Some(context.pending_total),
            oldest_pending_ticket: context.oldest_pending_ticket,
            fixed_capacity_min: context.fixed_capacity_min,
            effective_limit: if context.has_adaptive_claim {
                adaptive_limit.map(|limit| context.effective_limit(limit))
            } else {
                Some(context.effective_limit(usize::MAX))
            },
            candidate_slot: context.active_total.checked_add(1),
            adaptive_capacity: adaptive_limit,
        }
    }

    fn decide(mut self, decision: AdmissionDecision) -> Self {
        self.decision = decision;
        self
    }

    fn wait(mut self, blocker: DecisionBlocker) -> Self {
        self.decision = AdmissionDecision::Wait;
        self.blockers = DecisionBlockers::one(blocker);
        self
    }
}

fn admission_evaluation_with_feedback(
    store: &ClaimStore,
    claims: &ClaimSnapshot,
    ticket: u64,
) -> AdmissionEvaluation {
    admission_evaluation_with_target(claims, ticket, |ceiling, idle, backlog| {
        crate::compiler_feedback::target(&store.compiler_feedback_path(), ceiling, idle, backlog)
    })
}

fn admission_evaluation_with_target(
    claims: &ClaimSnapshot,
    ticket: u64,
    target: impl FnOnce(usize, bool, bool) -> usize,
) -> AdmissionEvaluation {
    let context = ClaimEvaluationContext::collect(claims);
    let target = if context.has_adaptive_claim
        && !claims.has_unknown_live_claim()
        && context.oldest_pending_ticket == Some(ticket)
    {
        Some(target(
            context.effective_limit(usize::MAX),
            context.active_total == 0 && context.pending_total == 1,
            context.active_total > 0 || context.pending_total > 1,
        ))
    } else {
        None
    };
    admission_evaluation_at_capacity(claims, ticket, target)
}

#[cfg(test)]
fn admission_evaluation(claims: &ClaimSnapshot, ticket: u64) -> AdmissionEvaluation {
    admission_evaluation_at_capacity(claims, ticket, Some(TEST_FEEDBACK_TARGET))
}

fn admission_evaluation_at_capacity(
    claims: &ClaimSnapshot,
    ticket: u64,
    adaptive_limit: Option<usize>,
) -> AdmissionEvaluation {
    let context = ClaimEvaluationContext::collect(claims);
    let evaluation = AdmissionEvaluation::from_context(context, ticket, adaptive_limit);
    if claims.has_unknown_live_claim() {
        return evaluation.wait(DecisionBlocker::UnknownLiveClaim);
    }
    if context.oldest_pending_ticket != Some(ticket) {
        return evaluation.wait(DecisionBlocker::NotFifoHead);
    }
    let active_total = context.active_total;
    let effective_limit = context.effective_limit(adaptive_limit.unwrap_or(1));
    if active_total < effective_limit {
        return evaluation.decide(AdmissionDecision::Admit);
    }
    if context.fixed_capacity_min.is_some_and(|capacity| {
        capacity
            <= adaptive_limit
                .unwrap_or(usize::MAX)
                .min(context.adaptive_capacity_min)
    }) {
        return evaluation.wait(DecisionBlocker::FixedCapacity);
    }
    if context.has_adaptive_claim {
        return evaluation.wait(DecisionBlocker::AdaptiveCapacity);
    }
    evaluation.wait(DecisionBlocker::FixedCapacity)
}

#[cfg(test)]
fn admission_decision(claims: &ClaimSnapshot, ticket: u64) -> AdmissionDecision {
    admission_evaluation(claims, ticket).decision
}

fn store_wait_evaluation(outcome: StoreOutcome) -> AdmissionEvaluation {
    match outcome.wait_reason {
        Some(StoreWaitReason::UnknownLiveClaim) => AdmissionEvaluation {
            decision: AdmissionDecision::Wait,
            blockers: DecisionBlockers::one(DecisionBlocker::UnknownLiveClaim),
            ticket: Some(outcome.ticket),
            active_total: None,
            active_adaptive: None,
            pending_total: None,
            oldest_pending_ticket: None,
            fixed_capacity_min: None,
            effective_limit: None,
            candidate_slot: None,
            adaptive_capacity: None,
        },
        None => unreachable!("a suppressed store decision names its wait reason"),
    }
}

fn record_trace(
    trace: &mut Option<CompilerTrace>,
    evaluation: AdmissionEvaluation,
) -> Result<(), Error> {
    trace
        .as_mut()
        .map(|trace| trace.record(evaluation).map_err(Error::trace))
        .transpose()
        .map(|_| ())
}

const fn store_decision(decision: AdmissionDecision) -> StoreDecision {
    match decision {
        AdmissionDecision::Admit => StoreDecision::Admit,
        AdmissionDecision::Wait => StoreDecision::Wait,
    }
}

#[cfg(test)]
fn effective_limit(claims: &ClaimSnapshot) -> usize {
    ClaimEvaluationContext::collect(claims).effective_limit(TEST_FEEDBACK_TARGET)
}

pub struct CompilerPermit {
    context: PermitContext,
}

enum PermitContext {
    Disabled,
    Acquired(ClaimLease),
    Inherited(InheritedAdmission),
}

/// A compiler command whose permit and process-tree supervision cannot be
/// separated from its spawn.
///
/// This wrapper is load-bearing on Windows: the child must be created
/// suspended and assigned to its Job Object before it may execute. Returning
/// a bare permit alongside `&mut Command` would let an otherwise-correct
/// caller bypass that ordering by invoking `Command::status` directly.
pub struct AdmittedCommand<'command> {
    _permit: CompilerPermit,
    command: &'command mut Command,
}

impl AdmittedCommand<'_> {
    pub fn status(self) -> std::io::Result<ExitStatus> {
        crate::process_supervisor::status(self.command, true)
    }

    pub fn output(self) -> std::io::Result<Output> {
        crate::process_supervisor::output(self.command)
    }
}

impl CompilerPermit {
    pub(crate) fn prepare_for(&self, command: &mut Command) -> Result<(), Error> {
        self.inherit_into(command)?;
        match &self.context {
            PermitContext::Disabled => {}
            PermitContext::Acquired(lease) => {
                command.env("KIO_CI_SCHEDULE_DIR", lease.schedule_root());
                append_command_held(command, HeldResource::Compiler)?;
            }
            PermitContext::Inherited(inherited) => {
                append_command_held_with_ambient(command, HeldResource::Compiler, inherited.held)?;
                #[cfg(unix)]
                crate::resource_admission::propagate_inherited_lease_fds(
                    command,
                    &inherited.lease_fds,
                )?;
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn inherit_into(&self, command: &mut Command) -> Result<(), Error> {
        match &self.context {
            PermitContext::Acquired(lease) => {
                lease.prepare_inherited_lease(command).map_err(Error::from)
            }
            PermitContext::Disabled | PermitContext::Inherited(_) => Ok(()),
        }
    }

    #[cfg(windows)]
    fn inherit_into(&self, _command: &mut Command) -> Result<(), Error> {
        // The scheduler-owned Windows spawn assigns the suspended child to a
        // kill-on-close Job before resuming it, then retains this permit until
        // the complete Job drains. No inheritable lease handle is needed.
        Ok(())
    }

    #[cfg(all(not(unix), not(windows)))]
    fn inherit_into(&self, _command: &mut Command) -> Result<(), Error> {
        match &self.context {
            PermitContext::Acquired(lease) => Err(Error::invalid(
                lease.schedule_root().join(lease.resource().as_str()),
                "active compiler admission requires process-tree lease inheritance",
            )),
            PermitContext::Disabled | PermitContext::Inherited(_) => Ok(()),
        }
    }
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
}

#[derive(Debug)]
enum ErrorKind {
    Local { path: PathBuf, message: String },
    Resource(ResourceAdmissionError),
}

impl Error {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self {
            kind: ErrorKind::Local {
                path: path.to_path_buf(),
                message: source.to_string(),
            },
        }
    }

    fn invalid(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Local {
                path,
                message: message.into(),
            },
        }
    }

    fn trace(error: crate::compiler_trace::Error) -> Self {
        let (path, message) = error.into_parts();
        Self::invalid(path, message)
    }
}

impl From<ResourceAdmissionError> for Error {
    fn from(error: ResourceAdmissionError) -> Self {
        Self {
            kind: ErrorKind::Resource(error),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (path, message) = match &self.kind {
            ErrorKind::Local { path, message } => (path, message),
            ErrorKind::Resource(error) => return error.fmt(f),
        };
        if matches!(
            path.to_str(),
            Some(
                "KIO_CI_SCHEDULE"
                    | "KIO_CI_SCHEDULE_DIR"
                    | "KIO_CI_SCHEDULE_COMPILER_JOBS"
                    | "KIO_CI_SCHEDULE_HELD"
                    | crate::compiler_trace::TRACE_ENV
                    | READINESS_HOOK_PROGRAM_ENV
                    | READINESS_HOOK_ARG_COUNT_ENV
            )
        ) {
            return f.write_str(message);
        }
        write!(f, "compiler admission at {}: {}", path.display(), message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Local { .. } => None,
            ErrorKind::Resource(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::READINESS_HOOK_ARG_ENV_PREFIX;
    use super::{
        AdmissionContext, AdmissionDecision, AdmissionMode, CompilerAdmission, DecisionBlocker,
        DecisionBlockers, PermitContext, TEST_FEEDBACK_TARGET, admission_decision,
        admission_evaluation, append_command_held, append_command_held_with_ambient,
        effective_limit, store_decision,
    };
    use super::{READINESS_HOOK_ARG_COUNT_ENV, READINESS_HOOK_PROGRAM_ENV};
    use crate::compiler_trace::CompilerTraceConfig;
    use crate::held_resources::{HeldResource, HeldResources};
    use crate::resource_admission::{
        ClaimKind, ClaimMetadata, ClaimSnapshot, ClaimState, ClaimStore, LiveClaim,
        SchedulerResource, StoreDecision, StoreOutcome, StoreWaitReason,
    };
    use std::ffi::OsStr;
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kio-compiler-admission-{label}-{}",
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

    fn claims<const N: usize>(claims: [LiveClaim; N]) -> ClaimSnapshot {
        ClaimSnapshot::from_claims(Vec::from(claims))
    }

    fn traced_admission(
        root: &Path,
        mode: AdmissionMode,
        token: &str,
        lock_path: &Path,
    ) -> CompilerAdmission {
        let store = ClaimStore::open(root.to_path_buf(), SchedulerResource::Compiler).unwrap();
        let trace = CompilerTraceConfig::for_test(store.schedule_root(), token)
            .assert_io_after_publication(lock_path.to_path_buf());
        CompilerAdmission {
            context: AdmissionContext::Shared { store, mode },
            readiness_hook: None,
            trace: Some(trace),
        }
    }

    #[cfg(unix)]
    fn wait_for(path: &Path) {
        for _ in 0..500 {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {}", path.display());
    }

    #[cfg(unix)]
    fn schedule_script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .join("ci/schedule.sh")
    }

    #[cfg(unix)]
    fn write_script(root: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn two_slots_bound_a_third_producer() {
        let root = root("two-slots");
        let admission = CompilerAdmission::shared(root.clone(), 2).unwrap();
        let first = admission.acquire().unwrap();
        let second = admission.acquire().unwrap();
        let (tx, rx) = mpsc::channel();
        let contender = admission.clone();
        let thread = std::thread::spawn(move || {
            let permit = contender.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(second);
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shared_canonicalizes_schedule_root() {
        let root = root("canonical-root");
        std::fs::create_dir_all(&root).unwrap();
        let lexical = root.join("child").join("..");
        std::fs::create_dir_all(root.join("child")).unwrap();
        let admission = CompilerAdmission::shared(lexical, 1).unwrap();
        let permit = admission.acquire().unwrap();
        let PermitContext::Acquired(lease) = &permit.context else {
            panic!("shared compiler admission did not acquire a lease");
        };
        assert_eq!(lease.schedule_root(), std::fs::canonicalize(&root).unwrap());
        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn live_cpu_ceiling_constrains_feedback_and_fixed_capacity() {
        for (cpu, feedback, fixed, expected) in [(3, 8, 6, 3), (8, 5, 6, 5), (8, 7, 4, 4)] {
            let input = claims([
                claim(1, cpu, ClaimKind::Adaptive, ClaimState::Pending),
                claim(2, fixed, ClaimKind::Normal, ClaimState::Pending),
            ]);
            assert_eq!(
                super::ClaimEvaluationContext::collect(&input).effective_limit(feedback),
                expected
            );
        }
    }

    #[test]
    fn lower_live_cpu_ceiling_blocks_a_fourth_producer() {
        let claims = claims([
            claim(5, 3, ClaimKind::Adaptive, ClaimState::Active),
            claim(6, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(7, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(8, 8, ClaimKind::Adaptive, ClaimState::Pending),
        ]);
        let evaluation = super::admission_evaluation_at_capacity(&claims, 8, Some(8));
        assert_eq!(evaluation.active_total, Some(3));
        assert_eq!(evaluation.oldest_pending_ticket, Some(8));
        assert_eq!(evaluation.fixed_capacity_min, None);
        assert_eq!(evaluation.effective_limit, Some(3));
        assert_eq!(evaluation.candidate_slot, Some(4));
        assert_eq!(evaluation.decision, AdmissionDecision::Wait);
        assert_eq!(
            evaluation.blockers,
            DecisionBlockers::one(DecisionBlocker::AdaptiveCapacity)
        );
    }

    #[test]
    fn fixed_four_still_admits_four_producers() {
        let root = root("fixed-four");
        let admission = CompilerAdmission::shared(root.clone(), 4).unwrap();
        let first = admission.acquire().unwrap();
        let second = admission.acquire().unwrap();
        let third = admission.acquire().unwrap();
        let (tx, rx) = mpsc::channel();
        let contender = admission.clone();
        let thread = std::thread::spawn(move || {
            let permit = contender.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });

        rx.recv_timeout(Duration::from_secs(2))
            .expect("an explicit fixed capacity of four did not admit the fourth producer");

        drop((first, second, third));
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn feedback_target_and_advertised_cpu_ceiling_gate_actual_admission() {
        let input = claims([
            claim(1, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(2, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(3, 8, ClaimKind::Adaptive, ClaimState::Pending),
        ]);
        let evaluation =
            super::admission_evaluation_with_target(&input, 3, |ceiling, idle, backlog| {
                assert_eq!(ceiling, 8);
                assert!(!idle && backlog);
                3
            });
        assert_eq!(evaluation.decision, AdmissionDecision::Admit);
        assert_eq!(evaluation.adaptive_capacity, Some(3));
        assert_eq!(evaluation.effective_limit, Some(3));
        assert_eq!(
            super::admission_evaluation_at_capacity(&input, 3, Some(2)).decision,
            AdmissionDecision::Wait
        );
    }

    #[test]
    fn fixed_only_skips_feedback_and_non_head_reports_no_measured_target() {
        let root = root("fixed-no-feedback");
        let store = ClaimStore::open(root.clone(), SchedulerResource::Compiler).unwrap();
        let fixed = claims([claim(1, 4, ClaimKind::Normal, ClaimState::Pending)]);
        assert_eq!(
            super::admission_evaluation_with_feedback(&store, &fixed, 1).decision,
            AdmissionDecision::Admit
        );
        assert!(!store.compiler_feedback_path().exists());
        let queued = claims([
            claim(1, 8, ClaimKind::Adaptive, ClaimState::Pending),
            claim(2, 8, ClaimKind::Adaptive, ClaimState::Pending),
        ]);
        let evaluation = super::admission_evaluation_with_feedback(&store, &queued, 2);
        assert_eq!(evaluation.adaptive_capacity, None);
        assert_eq!(evaluation.effective_limit, None);
        assert!(!store.compiler_feedback_path().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn feedback_target_one_stops_new_admission() {
        let input = claims([
            claim(1, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(2, 8, ClaimKind::Adaptive, ClaimState::Pending),
        ]);
        assert_eq!(
            super::admission_evaluation_at_capacity(&input, 2, Some(1)).decision,
            AdmissionDecision::Wait
        );
    }

    #[test]
    fn adaptive_cpu_ceiling_is_not_reported_as_a_larger_fixed_blocker() {
        let input = claims([
            claim(1, 2, ClaimKind::Adaptive, ClaimState::Active),
            claim(2, 8, ClaimKind::Adaptive, ClaimState::Active),
            claim(3, 4, ClaimKind::Normal, ClaimState::Pending),
        ]);
        let evaluation = super::admission_evaluation_at_capacity(&input, 3, Some(8));
        assert_eq!(evaluation.effective_limit, Some(2));
        assert_eq!(evaluation.decision, AdmissionDecision::Wait);
        assert_eq!(
            evaluation.blockers,
            DecisionBlockers::one(DecisionBlocker::AdaptiveCapacity)
        );
    }

    #[test]
    fn explicit_and_adaptive_caps_remain_hard_limits() {
        let fixed = claims([claim(1, 5, ClaimKind::Normal, ClaimState::Pending)]);
        assert_eq!(effective_limit(&fixed), 5);

        let mixed = claims([
            claim(1, 1, ClaimKind::Normal, ClaimState::Pending),
            claim(
                2,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Pending,
            ),
        ]);
        assert_eq!(effective_limit(&mixed), 1);
    }

    #[test]
    fn compiler_claims_are_fifo() {
        let claims = claims([
            claim(7, 2, ClaimKind::Normal, ClaimState::Pending),
            claim(
                8,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Pending,
            ),
        ]);
        assert_eq!(admission_decision(&claims, 7), AdmissionDecision::Admit);
        assert_eq!(admission_decision(&claims, 8), AdmissionDecision::Wait);
    }

    #[test]
    fn typed_evaluation_drives_literal_store_outcomes() {
        let third = claims([
            claim(
                5,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Active,
            ),
            claim(
                6,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Active,
            ),
            claim(
                7,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Pending,
            ),
        ]);
        let second = claims([
            claim(
                5,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Active,
            ),
            claim(
                6,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Pending,
            ),
        ]);
        let cases = [
            (
                admission_evaluation(&third, 8),
                AdmissionDecision::Wait,
                StoreDecision::Wait,
                DecisionBlockers::one(DecisionBlocker::NotFifoHead),
            ),
            (
                admission_evaluation(&third, 7),
                AdmissionDecision::Wait,
                StoreDecision::Wait,
                DecisionBlockers::one(DecisionBlocker::AdaptiveCapacity),
            ),
            (
                admission_evaluation(&second, 6),
                AdmissionDecision::Admit,
                StoreDecision::Admit,
                DecisionBlockers::default(),
            ),
        ];
        for (evaluation, expected_decision, expected_store, expected_blockers) in cases {
            assert_eq!(evaluation.decision, expected_decision);
            assert_eq!(store_decision(evaluation.decision), expected_store);
            assert_eq!(evaluation.blockers, expected_blockers);
        }
    }

    #[test]
    fn typed_policy_blockers_cover_every_reachable_queue_wait() {
        let cases = [
            (
                claims([
                    claim(
                        5,
                        TEST_FEEDBACK_TARGET,
                        ClaimKind::Adaptive,
                        ClaimState::Active,
                    ),
                    claim(
                        6,
                        TEST_FEEDBACK_TARGET,
                        ClaimKind::Adaptive,
                        ClaimState::Pending,
                    ),
                ]),
                7,
                DecisionBlocker::NotFifoHead,
            ),
            (
                claims([
                    claim(
                        5,
                        TEST_FEEDBACK_TARGET,
                        ClaimKind::Adaptive,
                        ClaimState::Active,
                    ),
                    claim(
                        6,
                        TEST_FEEDBACK_TARGET,
                        ClaimKind::Adaptive,
                        ClaimState::Active,
                    ),
                    claim(
                        7,
                        TEST_FEEDBACK_TARGET,
                        ClaimKind::Adaptive,
                        ClaimState::Pending,
                    ),
                ]),
                7,
                DecisionBlocker::AdaptiveCapacity,
            ),
            (
                claims([
                    claim(5, 1, ClaimKind::Normal, ClaimState::Active),
                    claim(6, 1, ClaimKind::Normal, ClaimState::Pending),
                ]),
                6,
                DecisionBlocker::FixedCapacity,
            ),
        ];

        for (claims, ticket, blocker) in cases {
            let evaluation = admission_evaluation(&claims, ticket);
            assert_eq!(evaluation.decision, AdmissionDecision::Wait);
            assert!(evaluation.blockers.contains(blocker), "{blocker:?}");
        }
        assert_eq!(
            DecisionBlocker::ALL.map(DecisionBlocker::label),
            ["unknown-live", "not-fifo", "adaptive-cap", "fixed-cap",]
        );
    }

    #[test]
    fn smaller_fixed_cap_precedes_adaptive_feedback() {
        let claims = claims([
            claim(5, 1, ClaimKind::Normal, ClaimState::Active),
            claim(
                6,
                TEST_FEEDBACK_TARGET,
                ClaimKind::Adaptive,
                ClaimState::Pending,
            ),
        ]);

        let evaluation = admission_evaluation(&claims, 6);
        assert_eq!(evaluation.decision, AdmissionDecision::Wait);
        assert_eq!(evaluation.candidate_slot, Some(2));
        assert_eq!(evaluation.effective_limit, Some(1));
        assert!(evaluation.blockers.contains(DecisionBlocker::FixedCapacity));
        assert!(
            !evaluation
                .blockers
                .contains(DecisionBlocker::AdaptiveCapacity)
        );
    }

    #[test]
    fn store_level_unknown_live_wait_maps_to_the_typed_blocker() {
        let evaluation = super::store_wait_evaluation(StoreOutcome {
            decision: StoreDecision::Wait,
            wait_reason: Some(StoreWaitReason::UnknownLiveClaim),
            ticket: 27,
        });

        assert_eq!(evaluation.decision, AdmissionDecision::Wait);
        assert_eq!(evaluation.ticket, Some(27));
        assert!(
            evaluation
                .blockers
                .contains(DecisionBlocker::UnknownLiveClaim)
        );
        assert_eq!(evaluation.active_total, None);
    }

    #[test]
    fn traced_wait_then_admission_records_the_same_transition() {
        let root = root("trace-reject-admit");
        let lock_path = root.join("compiler/state.lock");
        let admission =
            traced_admission(&root, AdmissionMode::Fixed(1), "reject-admit", &lock_path);
        let first = admission
            .acquire_for_program(OsStr::new("first-compiler"))
            .unwrap();
        let contender = admission.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let permit = contender
                .acquire_for_program(OsStr::new("second-compiler"))
                .unwrap();
            tx.send(()).unwrap();
            permit
        });
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(thread.join().unwrap());

        let directory = root.join("debug/compiler-admission/reject-admit");
        let second_program_hex = "7365636f6e642d636f6d70696c6572";
        let trace = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .find(|trace| trace.contains(second_program_hex))
            .expect("missing contender trace");
        let wait = trace.find("\"decision\":\"wait\"").unwrap();
        let admit = trace.rfind("\"decision\":\"admit\"").unwrap();
        assert!(wait < admit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn trace_setup_follows_ticket_publication_and_unlock() {
        let root = root("trace-start-boundary");
        let lock_path = root.join("compiler/state.lock");
        let admission =
            traced_admission(&root, AdmissionMode::Fixed(1), "start-boundary", &lock_path);
        let directory = root.join("debug/compiler-admission/start-boundary");
        assert!(!directory.exists());

        let permit = admission
            .acquire_for_program(OsStr::new("generic-compiler"))
            .unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);

        drop(permit);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn disabled_admission_creates_no_trace_file() {
        let root = root("trace-disabled");
        std::fs::create_dir_all(&root).unwrap();
        let trace = CompilerTraceConfig::for_test(&root, "disabled");
        let admission = CompilerAdmission {
            context: AdmissionContext::Disabled,
            readiness_hook: None,
            trace: Some(trace),
        };
        let permit = admission
            .acquire_for_program(OsStr::new("unused-compiler"))
            .unwrap();
        drop(permit);
        assert!(!root.join("debug/compiler-admission/disabled").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore]
    fn malformed_trace_token_fixture() {
        let expected = std::env::var("KIO_TEST_EXPECT_ERROR").unwrap();
        assert_eq!(
            CompilerAdmission::from_env().unwrap_err().to_string(),
            expected
        );
    }

    #[test]
    fn malformed_trace_token_fails_before_admission() {
        let root = root("malformed-trace-token");
        std::fs::create_dir_all(&root).unwrap();
        let expected = format!(
            "{} must be 1 to 64 ASCII letters, digits, '-' or '_', and not a reserved portable filename",
            crate::compiler_trace::TRACE_ENV
        );
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("malformed_trace_token_fixture")
            .env("KIO_CI_SCHEDULE_DIR", &root)
            .env(crate::compiler_trace::TRACE_ENV, "../escape")
            .env("KIO_TEST_EXPECT_ERROR", expected)
            .env_remove("KIO_CI_SCHEDULE")
            .env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS")
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .env_remove(READINESS_HOOK_PROGRAM_ENV)
            .env_remove(READINESS_HOOK_ARG_COUNT_ENV)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!root.join("debug").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore]
    fn disabled_from_env_fixture() {
        let admission = CompilerAdmission::from_env().unwrap();
        let mut command = Command::new("unused");
        drop(admission.acquire_for(&mut command).unwrap());
    }

    #[test]
    fn explicit_disable_is_portable_and_needs_no_process_tree_lease() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("disabled_from_env_fixture")
            .env("KIO_CI_SCHEDULE", "DISABLE")
            .env_remove("KIO_CI_SCHEDULE_DIR")
            .env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS")
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore]
    fn from_env_mode_fixture() {
        let admission = CompilerAdmission::from_env().unwrap();
        let expected = std::env::var("KIO_TEST_EXPECT_MODE").unwrap();
        let AdmissionContext::Shared { mode, .. } = admission.context else {
            panic!("scheduler environment did not create shared admission");
        };
        match (expected.as_str(), mode) {
            ("adaptive", AdmissionMode::Adaptive) => {
                assert_eq!(
                    mode.claim_capacity().get(),
                    crate::available_parallelism::get().unwrap_or(1)
                );
            }
            ("fixed-4", AdmissionMode::Fixed(4)) => {
                assert_eq!(mode.claim_capacity().get(), 4);
            }
            pair => panic!("unexpected admission mode {pair:?}"),
        }
    }

    #[test]
    fn omitted_capacity_is_adaptive_and_explicit_capacity_is_fixed() {
        let root = root("from-env-mode");
        std::fs::create_dir_all(&root).unwrap();
        for (expected, jobs) in [("adaptive", None), ("fixed-4", Some("4"))] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("from_env_mode_fixture")
                .env("KIO_CI_SCHEDULE_DIR", &root)
                .env("KIO_TEST_EXPECT_MODE", expected)
                .env_remove("KIO_CI_SCHEDULE")
                .env_remove("KIO_CI_SCHEDULE_HELD");
            match jobs {
                Some(jobs) => {
                    command.env("KIO_CI_SCHEDULE_COMPILER_JOBS", jobs);
                }
                None => {
                    command.env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS");
                }
            }
            assert!(command.status().unwrap().success());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore]
    fn malformed_held_from_env_fixture() {
        let expected = std::env::var("KIO_TEST_EXPECT_ERROR").unwrap();
        let error = CompilerAdmission::from_env().unwrap_err().to_string();
        assert_eq!(error, expected);
    }

    #[test]
    fn malformed_or_noncanonical_inherited_resources_fail_closed() {
        let root = root("malformed-held-from-env");
        std::fs::create_dir_all(&root).unwrap();
        for (value, expected) in [
            (
                "compiler,work",
                "KIO_CI_SCHEDULE_HELD must follow work,cargo,compiler order (work follows compiler)",
            ),
            (
                "compiler,compiler",
                "KIO_CI_SCHEDULE_HELD contains duplicate resource compiler",
            ),
            (
                "unknown,compiler",
                "KIO_CI_SCHEDULE_HELD contains unknown resource \"unknown\"",
            ),
        ] {
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .arg("malformed_held_from_env_fixture")
                .env("KIO_CI_SCHEDULE_DIR", &root)
                .env("KIO_CI_SCHEDULE_HELD", value)
                .env("KIO_TEST_EXPECT_ERROR", expected)
                .env_remove("KIO_CI_SCHEDULE")
                .env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS")
                .status()
                .unwrap();
            assert!(status.success(), "malformed inherited value {value:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn command_specific_inherited_resources_use_the_same_strict_parser() {
        for value in ["compiler,work", "work,,compiler", "work,unknown"] {
            let mut command = Command::new("unused");
            command.env("KIO_CI_SCHEDULE_HELD", value);
            let error = append_command_held(&mut command, HeldResource::Compiler).unwrap_err();
            assert!(
                error.to_string().starts_with("KIO_CI_SCHEDULE_HELD "),
                "unexpected error for {value:?}: {error}"
            );
        }
    }

    #[test]
    fn command_environment_cannot_discard_ambient_held_resources() {
        let ambient = HeldResources::parse(Some("work")).unwrap();

        let mut overridden = Command::new("unused");
        overridden.env("KIO_CI_SCHEDULE_HELD", "cargo");
        append_command_held_with_ambient(&mut overridden, HeldResource::Compiler, ambient).unwrap();
        let held = overridden
            .get_envs()
            .find(|(name, _)| *name == "KIO_CI_SCHEDULE_HELD")
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(held, "work,cargo,compiler");

        let mut removed = Command::new("unused");
        removed.env_remove("KIO_CI_SCHEDULE_HELD");
        append_command_held_with_ambient(&mut removed, HeldResource::Compiler, ambient).unwrap();
        let held = removed
            .get_envs()
            .find(|(name, _)| *name == "KIO_CI_SCHEDULE_HELD")
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(held, "work,compiler");
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn reused_from_env_propagation_fixture() {
        use std::os::fd::AsRawFd;

        let mode = std::env::var("KIO_TEST_REUSED_PROPAGATION_MODE").unwrap();
        let ambient_held = super::held_resources_from_env().unwrap();
        let ambient_fds = crate::resource_admission::inherited_lease_fds_from_env().unwrap();
        assert!(ambient_held.contains(HeldResource::Compiler));
        assert!(!ambient_fds.is_empty());
        for fd in &ambient_fds {
            // SAFETY: this only queries a descriptor named by the inherited
            // scheduler inventory; the outer fixture keeps its lease alive.
            assert_ne!(unsafe { libc::fcntl(*fd, libc::F_GETFD) }, -1);
        }

        let admission = CompilerAdmission::from_env().unwrap();
        let mut command = Command::new("unused");
        let mut expected_fds = ambient_fds.clone();
        let expected_held;
        let _extra = match mode.as_str() {
            "remove" => {
                command
                    .env_remove("KIO_CI_SCHEDULE_HELD")
                    .env_remove(crate::resource_admission::LEASE_FDS_ENV);
                expected_held = ambient_held;
                None
            }
            "override" => {
                let extra = std::fs::File::open("/dev/null").unwrap();
                let fd = extra.as_raw_fd();
                command
                    .env("KIO_CI_SCHEDULE_HELD", "work")
                    .env(crate::resource_admission::LEASE_FDS_ENV, fd.to_string());
                expected_held = ambient_held.union(HeldResources::parse(Some("work")).unwrap());
                if !expected_fds.contains(&fd) {
                    expected_fds.push(fd);
                }
                Some(extra)
            }
            _ => panic!("unknown reused-propagation mode {mode:?}"),
        };

        drop(admission.acquire_for(&mut command).unwrap());
        let held = command
            .get_envs()
            .find(|(name, _)| *name == "KIO_CI_SCHEDULE_HELD")
            .and_then(|(_, value)| value)
            .unwrap();
        let expected_held = expected_held.encode();
        assert_eq!(held, std::ffi::OsStr::new(&expected_held));
        let lease_fds = command
            .get_envs()
            .find(|(name, _)| *name == crate::resource_admission::LEASE_FDS_ENV)
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(
            crate::resource_admission::parse_lease_fds(lease_fds).unwrap(),
            expected_fds
        );
    }

    #[cfg(unix)]
    #[test]
    fn reused_from_env_context_survives_command_override_and_removal() {
        let root = root("reused-context-propagation");
        let admission = CompilerAdmission::shared(root.clone(), 1).unwrap();
        for mode in ["remove", "override"] {
            let permit = admission.acquire().unwrap();
            let mut fixture = Command::new(std::env::current_exe().unwrap());
            fixture
                .arg("--ignored")
                .arg("reused_from_env_propagation_fixture")
                .env("KIO_TEST_REUSED_PROPAGATION_MODE", mode)
                .env_remove("KIO_CI_SCHEDULE")
                .env_remove("KIO_CI_SCHEDULE_HELD")
                .env_remove(crate::resource_admission::LEASE_FDS_ENV)
                .env_remove(READINESS_HOOK_PROGRAM_ENV)
                .env_remove(READINESS_HOOK_ARG_COUNT_ENV)
                .env_remove(format!("{READINESS_HOOK_ARG_ENV_PREFIX}0"));
            permit.prepare_for(&mut fixture).unwrap();
            assert!(fixture.status().unwrap().success(), "mode {mode}");
            drop(permit);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore]
    fn malformed_readiness_inventory_fixture() {
        assert!(CompilerAdmission::from_env().is_err());
    }

    #[test]
    fn readiness_argument_inventory_rejects_aliases_and_unknown_suffixes() {
        for variable in [
            "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_00",
            "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_name",
            "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_",
        ] {
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .arg("malformed_readiness_inventory_fixture")
                .env(READINESS_HOOK_PROGRAM_ENV, "unused")
                .env(READINESS_HOOK_ARG_COUNT_ENV, "1")
                .env(variable, "alias")
                .env_remove("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0")
                .env("KIO_CI_SCHEDULE", "DISABLE")
                .status()
                .unwrap();
            assert!(status.success(), "readiness variable {variable:?}");
        }

        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("malformed_readiness_inventory_fixture")
            .env(READINESS_HOOK_PROGRAM_ENV, "unused")
            .env(READINESS_HOOK_ARG_COUNT_ENV, usize::MAX.to_string())
            .env_remove("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0")
            .env("KIO_CI_SCHEDULE", "DISABLE")
            .status()
            .unwrap();
        assert!(status.success(), "oversized readiness argument count");
    }

    #[cfg(unix)]
    #[test]
    fn acquire_for_propagates_the_held_resource_to_the_child() {
        let root = root("child-held-resource");
        let admission = CompilerAdmission::shared(root.clone(), 1).unwrap();
        let mut command = Command::new("unused");
        command.env("KIO_CI_SCHEDULE_HELD", "work,cargo");
        let admitted = admission.acquire_for(&mut command).unwrap();
        drop(admitted);
        let held = command
            .get_envs()
            .find(|(name, _)| *name == "KIO_CI_SCHEDULE_HELD")
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(held, "work,cargo,compiler");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn acquire_for_propagates_the_canonical_schedule_root() {
        let relative = PathBuf::from(format!(
            ".kio-compiler-admission-relative-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&relative).unwrap();
        let expected = std::fs::canonicalize(&relative).unwrap();
        let admission = CompilerAdmission::shared(relative.clone(), 1).unwrap();
        let mut command = Command::new("unused");
        let admitted = admission.acquire_for(&mut command).unwrap();
        drop(admitted);
        let inherited = command
            .get_envs()
            .find(|(name, _)| *name == "KIO_CI_SCHEDULE_DIR")
            .and_then(|(_, value)| value)
            .unwrap();
        assert_eq!(Path::new(inherited), expected);
        let _ = std::fs::remove_dir_all(relative);
    }

    #[cfg(unix)]
    #[test]
    fn acquire_for_child_reenters_the_scheduler_facade_without_deadlock() {
        let root = root("child-scheduler-reentry");
        std::fs::create_dir_all(&root).unwrap();
        let admitted = root.join("admitted");
        let mark = write_script(&root, "mark.sh", "#!/bin/sh\nset -eu\n: >\"$ADMITTED\"\n");
        let admission = CompilerAdmission::shared(root.clone(), 1).unwrap();
        let mut command = Command::new("sh");
        command
            .arg(schedule_script())
            .arg("--resource")
            .arg("compiler")
            .arg("--")
            .arg(&mark)
            .env("KIO_CI_SCHEDULE_DIR", &root)
            .env("KIO_CI_SCHEDULE_COMPILER_JOBS", "1")
            .env("ADMITTED", &admitted)
            .env_remove("KIO_CI_SCHEDULE")
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
        let permit = admission.acquire().unwrap();
        permit.prepare_for(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        let mut completed = false;
        for _ in 0..200 {
            if admitted.exists() {
                completed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !completed {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        drop(permit);
        assert!(
            completed && status.success(),
            "nested scheduler admission did not reuse the Rust-held permit"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pending_smaller_limit_stops_new_admission() {
        let root = root("limit-shrink");
        let wide = CompilerAdmission::shared(root.clone(), 2).unwrap();
        let first = wide.acquire().unwrap();
        let second = wide.acquire().unwrap();
        let narrow = CompilerAdmission::shared(root.clone(), 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let permit = narrow.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        drop(first);
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        drop(second);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(thread.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_publication_and_release_never_exceed_capacity() {
        let root = root("linearizable");
        let admission = CompilerAdmission::shared(root.clone(), 1).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let admission = admission.clone();
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            threads.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    let permit = admission.acquire().unwrap();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    std::thread::yield_now();
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn readiness_hook_fixture() {
        let admission = CompilerAdmission::from_env().unwrap();
        let target = std::env::var_os("KIO_TEST_READINESS_TARGET").unwrap();
        let mut command = Command::new(target);
        drop(admission.acquire_for(&mut command).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn configured_readiness_hook_runs_before_the_lease_is_inherited() {
        let root = root("readiness-hook");
        std::fs::create_dir_all(&root).unwrap();
        let started = root.join("started");
        let hook = write_script(
            &root,
            "readiness-hook",
            "#!/bin/sh\nset -eu\n[ \"$1\" = probe ]\n[ \"$2\" = --compiler-readiness ]\n[ \"$3\" = -- ]\n[ \"$4\" = unused-target ]\n[ -z \"${KIO_CI_SCHEDULE_HELD:-}\" ]\n[ -z \"${KIO_CI_SCHEDULE_LEASE_FDS:-}\" ]\n: >\"$KIO_TEST_READINESS_STARTED\"\n",
        );
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("readiness_hook_fixture")
            .env("KIO_CI_SCHEDULE_DIR", &root)
            .env("KIO_CI_SCHEDULE_COMPILER_JOBS", "1")
            .env(READINESS_HOOK_PROGRAM_ENV, &hook)
            .env(READINESS_HOOK_ARG_COUNT_ENV, "1")
            .env(format!("{READINESS_HOOK_ARG_ENV_PREFIX}0"), "probe")
            .env("KIO_TEST_READINESS_TARGET", "unused-target")
            .env("KIO_TEST_READINESS_STARTED", &started)
            .env_remove("KIO_CI_SCHEDULE")
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(started.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inherited_compiler_lease_reuses_established_readiness() {
        let root = root("inherited-readiness");
        std::fs::create_dir_all(&root).unwrap();
        let started = root.join("started");
        let hook = write_script(
            &root,
            "must-not-run-readiness-hook",
            "#!/bin/sh\nset -eu\n: >\"$KIO_TEST_READINESS_STARTED\"\nexit 97\n",
        );
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("readiness_hook_fixture")
            .env("KIO_CI_SCHEDULE_DIR", &root)
            .env("KIO_CI_SCHEDULE_COMPILER_JOBS", "1")
            .env("KIO_CI_SCHEDULE_HELD", "compiler")
            .env(READINESS_HOOK_PROGRAM_ENV, &hook)
            .env(READINESS_HOOK_ARG_COUNT_ENV, "0")
            .env("KIO_TEST_READINESS_TARGET", "unused-target")
            .env("KIO_TEST_READINESS_STARTED", &started)
            .env_remove("KIO_CI_SCHEDULE")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            !started.exists(),
            "a reused compiler lease must not repeat the readiness probe"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn inherited_lease_supervisor_fixture() {
        let root = PathBuf::from(std::env::var_os("KIO_TEST_SCHEDULE_ROOT").unwrap());
        let ready = PathBuf::from(std::env::var_os("KIO_TEST_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("KIO_TEST_RELEASE").unwrap());
        let compiler_pid = PathBuf::from(std::env::var_os("KIO_TEST_COMPILER_PID").unwrap());
        let descendant_pid = PathBuf::from(std::env::var_os("KIO_TEST_DESCENDANT_PID").unwrap());
        let done = PathBuf::from(std::env::var_os("KIO_TEST_DONE").unwrap());
        let script = format!(
            "set -eu\n( : >'{}'; while [ ! -e '{}' ]; do sleep 0.01; done; : >'{}' ) &\nprintf '%s\\n' \"$!\" >'{}'\nwait\n",
            ready.display(),
            release.display(),
            done.display(),
            descendant_pid.display(),
        );
        let admission = CompilerAdmission::shared(root, 1).unwrap();
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        let permit = admission.acquire().unwrap();
        permit.prepare_for(&mut command).unwrap();
        let child = command.spawn().unwrap();
        std::fs::write(compiler_pid, child.id().to_string()).unwrap();
        wait_for(&ready);
        std::mem::forget(child);
        std::mem::forget(permit);
    }

    #[cfg(unix)]
    #[test]
    fn compiler_descendant_retains_lease_after_supervisor_and_parent_die() {
        let root = root("process-tree");
        let ready = root.join("ready");
        let release = root.join("release");
        let done = root.join("done");
        let compiler_pid = root.join("compiler-pid");
        let descendant_pid = root.join("descendant-pid");
        let mut supervisor = Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("inherited_lease_supervisor_fixture")
            .env("KIO_TEST_SCHEDULE_ROOT", &root)
            .env("KIO_TEST_READY", &ready)
            .env("KIO_TEST_RELEASE", &release)
            .env("KIO_TEST_COMPILER_PID", &compiler_pid)
            .env("KIO_TEST_DESCENDANT_PID", &descendant_pid)
            .env("KIO_TEST_DONE", &done)
            .spawn()
            .unwrap();
        assert!(supervisor.wait().unwrap().success());
        wait_for(&ready);

        let admission = CompilerAdmission::shared(root.clone(), 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            let permit = admission.acquire().unwrap();
            tx.send(()).unwrap();
            permit
        });
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

        let pid: i32 = std::fs::read_to_string(&compiler_pid)
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: the fixture recorded this live child and the signal has no
        // memory-safety contract.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

        std::fs::write(&release, b"").unwrap();
        wait_for(&done);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(contender.join().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}
