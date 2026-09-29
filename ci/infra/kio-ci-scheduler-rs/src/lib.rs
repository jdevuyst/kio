//! Shared scheduling authority for Kio's local and CI compiler producers.

pub mod available_parallelism;
mod compiler_admission;
mod compiler_feedback;
mod compiler_trace;
pub mod held_resources;
mod process_supervisor;
mod resource_admission;
pub mod self_test;

pub use compiler_admission::{AdmittedCommand, CompilerAdmission, CompilerPermit, Error};

use held_resources::{HeldResource, HeldResources, ResourceRequest};
use resource_admission::{FixedResourceAdmission, FixedResourcePermit, WorkMode};
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

pub const BUILD_ID: &str = match option_env!("KIO_CI_SCHEDULER_BUILD_ID") {
    Some(id) => id,
    None => "dev",
};

pub fn main_entry<I>(arguments: I) -> Result<i32, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let Some(action) = arguments.next() else {
        return Err(usage());
    };
    if action == "--build-id" {
        if arguments.next().is_some() {
            return Err(usage());
        }
        println!("{BUILD_ID}");
        return Ok(0);
    }
    if action == "available-parallelism" {
        if arguments.next().is_some() {
            return Err(usage());
        }
        let parallelism = available_parallelism::get()
            .map_err(|error| format!("cannot determine available parallelism: {error}"))?;
        println!("{parallelism}");
        return Ok(0);
    }
    if action == "self-test" {
        if arguments.next().is_some() {
            return Err(usage());
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("cannot create a self-test nonce: {error}"))?
            .as_nanos();
        let state_root = env::temp_dir().join(format!(
            "kio-ci-scheduler-self-test-{}-{nonce}",
            std::process::id()
        ));
        self_test::run(&state_root).map_err(|error| error.to_string())?;
        println!("kio-ci-scheduler self-test: ok");
        return Ok(0);
    }
    if action == "supervise" {
        let mut stdin_closed = false;
        let mut drained_marker = None;
        let mut cancel_file = None;
        loop {
            let argument = arguments.next().ok_or_else(usage)?;
            if argument == "--" {
                break;
            }
            if argument == "--stdin-closed" && !stdin_closed {
                stdin_closed = true;
                continue;
            }
            if argument == "--drained-marker" && drained_marker.is_none() {
                drained_marker = Some(
                    arguments
                        .next()
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .ok_or_else(usage)?,
                );
                continue;
            }
            if argument == "--cancel-file" && cancel_file.is_none() {
                cancel_file = Some(
                    arguments
                        .next()
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .ok_or_else(usage)?,
                );
                continue;
            }
            return Err(usage());
        }
        let command: Vec<_> = arguments.collect();
        if command.is_empty() {
            return Err(usage());
        }
        let drained_marker = drained_marker.ok_or_else(usage)?;
        let cancel_file = cancel_file.ok_or_else(usage)?;
        initialize_supervision_paths(&drained_marker, &cancel_file)?;
        if cancel_file.try_exists().map_err(|error| {
            format!(
                "cannot inspect supervision cancellation file {}: {error}",
                cancel_file.display()
            )
        })? {
            publish_drained_marker(&drained_marker)?;
            return Ok(143);
        }
        let stdin_was_open = if stdin_closed {
            false
        } else {
            stdin_is_open().map_err(|error| {
                format!("cannot inspect standard input before process-tree supervision: {error}")
            })?
        };
        let mut child = Command::new(&command[0]);
        child.args(&command[1..]);
        let status =
            run_supervised_process_tree(child, stdin_was_open, &cancel_file).map_err(|error| {
                format!(
                    "cannot supervise command {}: {error}",
                    command[0].to_string_lossy()
                )
            })?;
        publish_drained_marker(&drained_marker)?;
        return Ok(status);
    }
    if action == "readiness" {
        if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
            return Err(usage());
        }
        let command: Vec<_> = arguments.collect();
        if command.is_empty() {
            return Err(usage());
        }
        let mut readiness = Command::new(&command[0]);
        readiness
            .args(&command[1..])
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .env_remove(resource_admission::LEASE_FDS_ENV);
        let status = process_supervisor::readiness_status(&mut readiness)
            .map_err(|error| format!("cannot run isolated readiness command: {error}"))?;
        return Ok(status_code(status));
    }
    if action != "run" {
        return Err(usage());
    }

    let mut resource = None;
    let mut barrier = false;
    let mut work_jobs = None;
    let mut readiness_hook_program = None;
    let mut readiness_hook_args = Vec::new();
    let mut stdin_closed = false;
    loop {
        let Some(argument) = arguments.next() else {
            return Err(usage());
        };
        if argument == "--" {
            break;
        }
        if argument == "--resource" && resource.is_none() {
            resource = Some(match arguments.next().as_deref() {
                Some(value) if value == "work" => HeldResource::Work,
                Some(value) if value == "cargo" => HeldResource::Cargo,
                Some(value) if value == "compiler" => HeldResource::Compiler,
                _ => return Err(usage()),
            });
            continue;
        }
        if argument == "--barrier" && !barrier {
            barrier = true;
            continue;
        }
        if argument == "--jobs" && work_jobs.is_none() {
            let value = arguments.next().ok_or_else(usage)?;
            work_jobs = Some(parse_positive(&value, "--jobs")?);
            continue;
        }
        if argument == "--readiness-hook" && readiness_hook_program.is_none() {
            readiness_hook_program = Some(
                arguments
                    .next()
                    .filter(|program| !program.is_empty())
                    .ok_or_else(usage)?,
            );
            continue;
        }
        if argument == "--readiness-hook-arg" {
            readiness_hook_args.push(arguments.next().ok_or_else(usage)?);
            continue;
        }
        if argument == "--stdin-closed" && !stdin_closed {
            stdin_closed = true;
            continue;
        }
        return Err(usage());
    }
    let command: Vec<_> = arguments.collect();
    if command.is_empty() {
        return Err(usage());
    }
    let resource = resource.ok_or_else(usage)?;
    if barrier && resource != HeldResource::Work {
        return Err("--barrier is valid only for the work resource".into());
    }
    if work_jobs.is_some() && resource != HeldResource::Work {
        return Err("--jobs is valid only for the work resource".into());
    }
    if resource != HeldResource::Compiler
        && (readiness_hook_program.is_some() || !readiness_hook_args.is_empty())
    {
        return Err("a readiness hook is valid only for the compiler resource".into());
    }
    let stdin_was_open = if stdin_closed {
        false
    } else {
        stdin_is_open()
            .map_err(|error| format!("cannot inspect standard input before admission: {error}"))?
    };

    let mut child_command = Command::new(&command[0]);
    child_command.args(&command[1..]);
    let held = held_resources_from_env()?;

    if schedule_disabled()? {
        if resource == HeldResource::Compiler && !held.contains(HeldResource::Compiler) {
            let mut admission =
                CompilerAdmission::from_scheduler_env().map_err(|error| error.to_string())?;
            configure_readiness_hook(&mut admission, readiness_hook_program, readiness_hook_args)?;
            if let Some(status) = admission
                .run_readiness_hook(&child_command)
                .map_err(|error| error.to_string())?
                && !status.success()
            {
                return Ok(status_code(status));
            }
        }
        return run_supervised(child_command, stdin_was_open).map_err(|error| {
            format!(
                "cannot run command {} with scheduling disabled: {error}",
                command[0].to_string_lossy()
            )
        });
    }

    let request = held.request(resource).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    if request == ResourceRequest::Reuse {
        resource_admission::inherited_lease_fds_from_env().map_err(|error| error.to_string())?;
    }
    let inherited = held
        .with_requested(resource)
        .map_err(|error| error.to_string())?;

    if resource != HeldResource::Compiler {
        let permit = acquire_fixed_resource(resource, request, barrier, work_jobs)?;
        if let Some(permit) = &permit {
            prepare_fixed_permit(permit, &mut child_command)?;
            child_command.env("KIO_CI_SCHEDULE_DIR", permit.schedule_root());
        }
        child_command.env("KIO_CI_SCHEDULE_HELD", inherited.encode());
        let result = run_supervised(child_command, stdin_was_open).map_err(|error| {
            format!(
                "cannot run admitted {} command {}: {error}",
                resource,
                command[0].to_string_lossy()
            )
        });
        drop(permit);
        return result;
    }

    let mut admission =
        CompilerAdmission::from_scheduler_env().map_err(|error| error.to_string())?;
    configure_readiness_hook(&mut admission, readiness_hook_program, readiness_hook_args)?;
    let permit = admission
        .acquire_for_program(child_command.get_program())
        .map_err(|error| error.to_string())?;
    if request != ResourceRequest::Reuse
        && let Some(status) = admission
            .run_readiness_hook(&child_command)
            .map_err(|error| error.to_string())?
        && !status.success()
    {
        return Ok(status_code(status));
    }

    permit
        .prepare_for(&mut child_command)
        .map_err(|error| error.to_string())?;
    child_command.env("KIO_CI_SCHEDULE_HELD", inherited.encode());
    run_supervised(child_command, stdin_was_open).map_err(|error| {
        format!(
            "cannot run admitted command {}: {error}",
            command[0].to_string_lossy()
        )
    })
}

fn usage() -> String {
    "usage: kio-ci-scheduler --build-id\n       kio-ci-scheduler available-parallelism\n       kio-ci-scheduler self-test\n       kio-ci-scheduler supervise --drained-marker PATH --cancel-file PATH [--stdin-closed] -- command [args...]\n       kio-ci-scheduler readiness -- command [args...]\n       kio-ci-scheduler run --resource work [--barrier] [--jobs N] [--stdin-closed] -- command [args...]\n       kio-ci-scheduler run --resource cargo [--stdin-closed] -- command [args...]\n       kio-ci-scheduler run --resource compiler [--readiness-hook PROGRAM] [--readiness-hook-arg ARG]... [--stdin-closed] -- command [args...]".into()
}

fn parse_positive(value: &OsString, option: &str) -> Result<usize, String> {
    value
        .to_str()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{option} must be a positive integer"))
}

fn publish_drained_marker(path: &std::path::Path) -> Result<(), String> {
    // The empty file's atomic create is the publication event; there is no
    // payload that an observer could see partially written.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|error| format!("cannot publish drained marker {}: {error}", path.display()))
}

fn initialize_supervision_paths(
    drained_marker: &std::path::Path,
    cancel_file: &std::path::Path,
) -> Result<(), String> {
    if drained_marker == cancel_file {
        return Err("drained marker and cancellation file must be distinct".into());
    }
    control_path_parent(drained_marker, "drained marker")?;
    control_path_parent(cancel_file, "cancellation file")?;
    match std::fs::remove_file(drained_marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot initialize drained marker {}: {error}",
            drained_marker.display()
        )),
    }
}

fn control_path_parent<'a>(
    path: &'a std::path::Path,
    label: &str,
) -> Result<&'a std::path::Path, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{label} must have a parent: {}", path.display()))?;
    if path.file_name().is_none() || !parent.is_dir() {
        return Err(format!(
            "{label} parent is not a directory: {}",
            parent.display()
        ));
    }
    Ok(parent)
}

fn schedule_disabled() -> Result<bool, String> {
    match env::var("KIO_CI_SCHEDULE") {
        Ok(value) if value == "DISABLE" => Ok(true),
        Ok(value) if value.is_empty() => Ok(false),
        Ok(value) => Err(format!(
            "KIO_CI_SCHEDULE must be DISABLE or unset (got {value})"
        )),
        Err(env::VarError::NotPresent) => Ok(false),
        Err(env::VarError::NotUnicode(_)) => Err("KIO_CI_SCHEDULE must be valid Unicode".into()),
    }
}

fn held_resources_from_env() -> Result<HeldResources, String> {
    match env::var("KIO_CI_SCHEDULE_HELD") {
        Ok(value) => HeldResources::parse(Some(&value)).map_err(|error| error.to_string()),
        Err(env::VarError::NotPresent) => Ok(HeldResources::default()),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_CI_SCHEDULE_HELD must be valid Unicode".into())
        }
    }
}

fn schedule_root_from_env() -> Result<PathBuf, String> {
    let value = env::var_os("KIO_CI_SCHEDULE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "KIO_CI_SCHEDULE_DIR must not be empty or unset".to_owned())?;
    Ok(PathBuf::from(value))
}

fn work_capacity(explicit: Option<usize>) -> Result<usize, String> {
    if let Some(capacity) = explicit {
        return Ok(capacity);
    }
    match env::var("KIO_CI_SCHEDULE_JOBS") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                format!("KIO_CI_SCHEDULE_JOBS must be a positive integer (got {value})")
            }),
        Err(env::VarError::NotPresent) => available_parallelism::get()
            .map_err(|error| format!("cannot determine work capacity: {error}")),
        Err(env::VarError::NotUnicode(_)) => {
            Err("KIO_CI_SCHEDULE_JOBS must be valid Unicode".into())
        }
    }
}

fn acquire_fixed_resource(
    resource: HeldResource,
    request: ResourceRequest,
    barrier: bool,
    jobs: Option<usize>,
) -> Result<Option<FixedResourcePermit>, String> {
    if request == ResourceRequest::Reuse {
        return Ok(None);
    }
    let schedule_root = schedule_root_from_env()?;
    let admission = match resource {
        HeldResource::Work => FixedResourceAdmission::work(
            schedule_root,
            work_capacity(jobs)?,
            if barrier {
                WorkMode::Barrier
            } else {
                WorkMode::Normal
            },
        ),
        HeldResource::Cargo => FixedResourceAdmission::cargo(schedule_root),
        HeldResource::Compiler => {
            unreachable!("compiler admission does not use FixedResourceAdmission")
        }
    }
    .map_err(|error| error.to_string())?;
    admission
        .acquire()
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn prepare_fixed_permit(permit: &FixedResourcePermit, command: &mut Command) -> Result<(), String> {
    permit
        .prepare_inherited_lease(command)
        .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn prepare_fixed_permit(
    _permit: &FixedResourcePermit,
    _command: &mut Command,
) -> Result<(), String> {
    Ok(())
}

fn configure_readiness_hook(
    admission: &mut CompilerAdmission,
    program: Option<OsString>,
    prefix_args: Vec<OsString>,
) -> Result<(), String> {
    if let Some(program) = program {
        admission
            .configure_readiness_hook(program, prefix_args)
            .map_err(|error| error.to_string())
    } else if prefix_args.is_empty() {
        Ok(())
    } else {
        Err(usage())
    }
}

fn status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(1)
    }
    #[cfg(not(unix))]
    1
}

#[cfg(unix)]
fn stdin_is_open() -> std::io::Result<bool> {
    // SAFETY: `fcntl` only queries the process's standard-input descriptor.
    let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFD) };
    if flags != -1 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn stdin_is_open() -> std::io::Result<bool> {
    Ok(true)
}

#[cfg(all(test, unix))]
static TEST_PRE_PUBLICATION_SIGNAL_FD: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(-1);

#[cfg(all(test, unix))]
static TEST_FORWARDED_CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(all(test, unix))]
static TEST_REAPED_CHILD_SIGNAL: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
fn run_supervised(command: Command, stdin_was_open: bool) -> std::io::Result<i32> {
    run_supervised_unix(command, stdin_was_open, None)
}

#[cfg(unix)]
fn run_supervised_process_tree(
    command: Command,
    stdin_was_open: bool,
    cancellation: &std::path::Path,
) -> std::io::Result<i32> {
    run_supervised_unix(command, stdin_was_open, Some(cancellation))
}

#[cfg(unix)]
fn run_supervised_unix(
    mut command: Command,
    stdin_was_open: bool,
    cancellation: Option<&std::path::Path>,
) -> std::io::Result<i32> {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    static CHILD_PID: AtomicI32 = AtomicI32::new(0);
    static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);
    static PROCESS_GROUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    extern "C" fn forward(signal: libc::c_int) {
        let _ = PENDING_SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
        let owns_process_group = PROCESS_GROUP.load(Ordering::SeqCst);
        let child = CHILD_PID.load(Ordering::SeqCst);
        #[cfg(test)]
        if child == 0 {
            let signal_fd = TEST_PRE_PUBLICATION_SIGNAL_FD.swap(-1, Ordering::SeqCst);
            if signal_fd >= 0 {
                let observed = [signal as u8];
                // SAFETY: the causal test installs a live pipe descriptor, and
                // `write` is async-signal-safe. The one-byte write cannot block
                // because the otherwise-empty pipe has at least one byte free.
                unsafe {
                    libc::write(signal_fd, observed.as_ptr().cast(), observed.len());
                }
            }
        }
        if child > 0 && !owns_process_group {
            #[cfg(test)]
            TEST_FORWARDED_CHILD.store(child, Ordering::SeqCst);
            let child_signal = if signal == libc::SIGINT {
                libc::SIGTERM
            } else {
                signal
            };
            // SAFETY: `kill` is async-signal-safe. The stored positive PID is
            // the direct child. Process-group delivery belongs to the monitor
            // loop, so its final extinction/clear transition cannot target a
            // newly reused group identifier from this asynchronous handler.
            unsafe {
                libc::kill(child, child_signal);
            }
        }
    }

    // The scheduler CLI is a one-shot supervisor, but unit tests exercise this
    // function repeatedly in one process. Never let a completed invocation's
    // child or pending signal become authority for the next invocation.
    CHILD_PID.store(0, Ordering::SeqCst);
    PENDING_SIGNAL.store(0, Ordering::SeqCst);
    #[cfg(test)]
    TEST_FORWARDED_CHILD.store(0, Ordering::SeqCst);
    #[cfg(test)]
    TEST_REAPED_CHILD_SIGNAL.store(0, Ordering::SeqCst);
    let own_process_group = cancellation.is_some();
    PROCESS_GROUP.store(own_process_group, Ordering::SeqCst);

    let mut foreground_tty = if own_process_group {
        process_supervisor::isolate_process_group_with_foreground_tty(&mut command, stdin_was_open)?
    } else {
        None
    };

    let _stdin_reservation = if stdin_was_open {
        None
    } else {
        Some(reserve_closed_stdin()?)
    };
    if !stdin_was_open {
        close_stdin_for_child(&mut command);
    }

    // SAFETY: this one-shot CLI owns its signal dispositions until exit, and
    // the handler performs only atomic operations and async-signal-safe calls.
    let handler = forward as *const () as libc::sighandler_t;
    let tty_was_handed_off = foreground_tty.is_some();
    let mut dispositions = SignalDispositions::install(handler)?;
    let mut child_obtained = false;
    let mut tree_drained = false;
    let status = command.spawn().and_then(|mut child| {
        child_obtained = true;
        if let Some(tty) = foreground_tty.as_mut() {
            tty.child_spawned(child.id());
        }
        CHILD_PID.store(child.id() as i32, Ordering::SeqCst);
        #[cfg(test)]
        let publication_error = std::env::var_os("KIO_TEST_UNIX_CHILD_PID_PUBLISHED")
            .and_then(|path| std::fs::write(path, b"published").err());
        #[cfg(not(test))]
        let publication_error = None;
        let pending = PENDING_SIGNAL.load(Ordering::SeqCst);
        if pending != 0 {
            forward(pending);
        }
        if !own_process_group {
            return child.wait().map(|status| {
                #[cfg(test)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    TEST_REAPED_CHILD_SIGNAL.store(status.signal().unwrap_or(0), Ordering::SeqCst);
                }
                (status, false)
            });
        }

        const POLL: Duration = Duration::from_millis(10);
        const GRACE: Duration = Duration::from_millis(250);
        const KILL_DRAIN_LIMIT: Duration = Duration::from_secs(5);
        let child_pid = child.id();
        let mut leader_status = None;
        let mut abort_deadline = None;
        let mut kill_drain_deadline = None;
        let mut killed = false;
        let mut cancelled = false;
        let mut leader_aborted = false;
        let mut primary_error = publication_error;
        let mut cleanup_error = None;
        #[cfg(test)]
        let mut injected_monitor_error = false;
        loop {
            if leader_status.is_none() {
                match child.try_wait() {
                    Ok(status) => leader_status = status,
                    Err(error) if primary_error.is_none() => primary_error = Some(error),
                    Err(error) if cleanup_error.is_none() => cleanup_error = Some(error),
                    Err(_) => {}
                }
            }
            #[cfg(test)]
            if !injected_monitor_error
                && std::env::var_os("KIO_TEST_SUPERVISE_MONITOR_ERROR_AFTER")
                    .is_some_and(|path| std::path::Path::new(&path).exists())
            {
                primary_error.get_or_insert_with(|| {
                    std::io::Error::other("injected post-spawn supervision monitor error")
                });
                injected_monitor_error = true;
            }
            if !cancelled && primary_error.is_none() {
                match cancellation
                    .expect("process-tree supervision has a cancellation path")
                    .try_exists()
                {
                    Ok(true) => cancelled = true,
                    Ok(false) => {}
                    Err(error) => primary_error = Some(error),
                }
            }
            let pending = PENDING_SIGNAL.load(Ordering::SeqCst);
            if !leader_aborted
                && tty_was_handed_off
                && pending == 0
                && !cancelled
                && leader_status
                    .as_ref()
                    .is_some_and(|status| matches!(status_code(*status), 129 | 130 | 143))
            {
                leader_aborted = true;
            }
            let cleaning_up =
                pending != 0 || cancelled || leader_aborted || primary_error.is_some();
            if cleaning_up && abort_deadline.is_none() {
                let cleanup_signal = if pending == libc::SIGINT || pending == 0 {
                    libc::SIGTERM
                } else {
                    pending
                };
                if let Err(error) =
                    process_supervisor::signal_process_group(child_pid, cleanup_signal)
                {
                    if primary_error.is_none() {
                        primary_error = Some(error);
                    } else if cleanup_error.is_none() {
                        cleanup_error = Some(error);
                    }
                }
                abort_deadline = Some(Instant::now() + GRACE);
            }
            if let Some(deadline) = abort_deadline
                && !killed
                && Instant::now() >= deadline
            {
                if let Err(error) =
                    process_supervisor::signal_process_group(child_pid, libc::SIGKILL)
                {
                    if primary_error.is_none() {
                        primary_error = Some(error);
                    } else if cleanup_error.is_none() {
                        cleanup_error = Some(error);
                    }
                }
                killed = true;
                kill_drain_deadline = Some(Instant::now() + KILL_DRAIN_LIMIT);
            }
            match process_supervisor::process_group_exists(child_pid) {
                Ok(false) => {
                    if leader_status.is_none() {
                        match child.wait() {
                            Ok(status) => leader_status = Some(status),
                            Err(error) if primary_error.is_none() => primary_error = Some(error),
                            Err(error) if cleanup_error.is_none() => cleanup_error = Some(error),
                            Err(_) => {}
                        }
                    }
                    tree_drained = true;
                    break match primary_error {
                        Some(error) => Err(combine_supervision_errors(error, cleanup_error, false)),
                        None => Ok((
                            leader_status.expect("extinct child group has a leader status"),
                            cancelled,
                        )),
                    };
                }
                Ok(true) => {}
                Err(error) if primary_error.is_none() => primary_error = Some(error),
                Err(error) if cleanup_error.is_none() => cleanup_error = Some(error),
                Err(_) => {}
            }
            if kill_drain_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let error = primary_error.take().unwrap_or_else(|| {
                    std::io::Error::other("supervised process group did not drain after SIGKILL")
                });
                break Err(combine_supervision_errors(error, cleanup_error, true));
            }
            std::thread::sleep(POLL);
        }
    });
    #[cfg(test)]
    if let Some(ready) = std::env::var_os("KIO_TEST_SUPERVISE_FINAL_TRANSITION_READY") {
        std::fs::write(ready, b"ready")?;
        if let Some(signal_seen) =
            std::env::var_os("KIO_TEST_SUPERVISE_FINAL_TRANSITION_SIGNAL_SEEN")
        {
            // Keep the causal fixture parked until the asynchronous handler,
            // not merely kill(2), has published the signal being tested.
            while PENDING_SIGNAL.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            std::fs::write(signal_seen, b"seen")?;
        }
        let release = std::env::var_os("KIO_TEST_SUPERVISE_FINAL_TRANSITION_RELEASE")
            .expect("final-transition test hook has a release path");
        while !std::path::Path::new(&release).exists() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // The direct-child route reaps before returning. The process-tree route
    // additionally observes process-group extinction before clearing the id.
    CHILD_PID.store(0, Ordering::SeqCst);
    PROCESS_GROUP.store(false, Ordering::SeqCst);
    let tty_restored = if !child_obtained || tree_drained {
        match foreground_tty.as_mut() {
            Some(tty) => tty.restore(),
            None => Ok(()),
        }
    } else {
        // Extinction is unproven, so restoring the old foreground owner could
        // take the terminal away from a still-live child. Disarm Drop's retry;
        // the returned error and absent drained marker leave both terminal and
        // state with that possible survivor.
        if let Some(tty) = foreground_tty.as_mut() {
            tty.leave_with_unproven_child();
        }
        Ok(())
    };
    let restored = dispositions.restore();
    let pending = PENDING_SIGNAL.load(Ordering::SeqCst);
    let mut epilogue_error = tty_restored.err().map(|error| {
        std::io::Error::new(
            error.kind(),
            format!("foreground-terminal restoration failed: {error}"),
        )
    });
    if let Err(error) = restored {
        epilogue_error = Some(match epilogue_error {
            Some(primary) => append_supervision_error(
                primary,
                "signal-disposition restoration also failed",
                error,
            ),
            None => std::io::Error::new(
                error.kind(),
                format!("signal-disposition restoration failed: {error}"),
            ),
        });
    }
    let (status, cancelled) = match status {
        Ok(status) => status,
        Err(primary) => {
            return Err(match epilogue_error {
                Some(error) => {
                    append_supervision_error(primary, "supervisor epilogue also failed", error)
                }
                None => primary,
            });
        }
    };
    if let Some(error) = epilogue_error {
        return Err(error);
    }
    Ok(if pending != 0 {
        128 + pending
    } else if cancelled {
        143
    } else {
        status_code(status)
    })
}

#[cfg(unix)]
fn combine_supervision_errors(
    primary: std::io::Error,
    cleanup: Option<std::io::Error>,
    extinction_unproven: bool,
) -> std::io::Error {
    let kind = primary.kind();
    let mut message = primary.to_string();
    if let Some(cleanup) = cleanup {
        message.push_str("; process-tree cleanup also failed: ");
        message.push_str(&cleanup.to_string());
    }
    if extinction_unproven {
        message.push_str(
            "; process-tree extinction could not be established; foreground-terminal restoration was withheld from the possibly live child group",
        );
    }
    std::io::Error::new(kind, message)
}

#[cfg(unix)]
fn append_supervision_error(
    primary: std::io::Error,
    label: &str,
    secondary: std::io::Error,
) -> std::io::Error {
    let kind = primary.kind();
    std::io::Error::new(kind, format!("{primary}; {label}: {secondary}"))
}

#[cfg(unix)]
struct SignalDispositions {
    previous: Vec<(libc::c_int, libc::sighandler_t)>,
    restored: bool,
}

#[cfg(unix)]
impl SignalDispositions {
    fn install(handler: libc::sighandler_t) -> std::io::Result<Self> {
        let mut previous = Vec::with_capacity(3);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: `handler` has C signal-handler ABI and remains valid for
            // the lifetime of this one-shot supervisor invocation.
            let disposition = unsafe { libc::signal(signal, handler) };
            if disposition == libc::SIG_ERR {
                let error = std::io::Error::last_os_error();
                for &(installed_signal, installed_disposition) in previous.iter().rev() {
                    // SAFETY: each pair was returned by a successful `signal`
                    // call above and is restored before this function returns.
                    let _ = unsafe { libc::signal(installed_signal, installed_disposition) };
                }
                return Err(error);
            }
            previous.push((signal, disposition));
        }
        Ok(Self {
            previous,
            restored: false,
        })
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let mut first_error = None;
        for &(signal, disposition) in self.previous.iter().rev() {
            // SAFETY: `disposition` was returned when this guard installed the
            // forwarding handler for the same signal.
            if unsafe { libc::signal(signal, disposition) } == libc::SIG_ERR
                && first_error.is_none()
            {
                first_error = Some(std::io::Error::last_os_error());
            }
        }
        self.restored = true;
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(unix)]
impl Drop for SignalDispositions {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(unix)]
fn reserve_closed_stdin() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;

    // SAFETY: the facade reported that caller stdin was closed, so fd 0 is
    // occupied only accidentally by supervisor setup and may be reclaimed.
    if unsafe { libc::close(libc::STDIN_FILENO) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EBADF) {
            return Err(error);
        }
    }
    // Reserve fd 0 so Command's internal exec-error pipe cannot claim it;
    // the child pre-exec hook closes only its inherited copy.
    // SAFETY: the nul-terminated path is valid and the returned descriptor is
    // immediately checked before ownership moves into `File`.
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if fd != libc::STDIN_FILENO {
        // SAFETY: `fd` was returned live by `open` above.
        unsafe {
            libc::close(fd);
        }
        return Err(std::io::Error::other(
            "could not reserve the closed standard-input descriptor",
        ));
    }
    // SAFETY: this function uniquely owns the live descriptor from `open`.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn close_stdin_for_child(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: after fork and before exec this closure invokes only the
    // async-signal-safe `close` operation on the standard-input descriptor.
    unsafe {
        command.pre_exec(|| {
            if libc::close(libc::STDIN_FILENO) == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn run_supervised(mut command: Command, stdin_was_open: bool) -> std::io::Result<i32> {
    process_supervisor::status(&mut command, stdin_was_open).map(status_code)
}

#[cfg(windows)]
fn run_supervised_process_tree(
    mut command: Command,
    stdin_was_open: bool,
    cancellation: &std::path::Path,
) -> std::io::Result<i32> {
    let (status, cancelled) =
        process_supervisor::status_cancellable(&mut command, stdin_was_open, cancellation)?;
    Ok(if cancelled { 143 } else { status_code(status) })
}

#[cfg(all(not(unix), not(windows)))]
fn run_supervised(mut command: Command, _stdin_was_open: bool) -> std::io::Result<i32> {
    command.status().map(status_code)
}

#[cfg(all(not(unix), not(windows)))]
fn run_supervised_process_tree(
    _command: Command,
    _stdin_was_open: bool,
    _cancellation: &std::path::Path,
) -> std::io::Result<i32> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process-tree supervision is supported only on Unix and Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::{BUILD_ID, main_entry};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use std::fs::{File, OpenOptions, TryLockError};
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    const SIGNAL_MODE: &str = "KIO_TEST_UNIX_SIGNAL_MODE";
    #[cfg(unix)]
    const SIGNAL_READY: &str = "KIO_TEST_UNIX_SIGNAL_READY";
    #[cfg(unix)]
    const SIGNAL_DELIVERED: &str = "KIO_TEST_UNIX_SIGNAL_DELIVERED";
    #[cfg(unix)]
    const SIGNAL_RELEASE: &str = "KIO_TEST_UNIX_SIGNAL_RELEASE";
    #[cfg(unix)]
    const SIGNAL_HOLD: &str = "KIO_TEST_UNIX_SIGNAL_HOLD";
    #[cfg(unix)]
    const CHILD_PID_PUBLISHED: &str = "KIO_TEST_UNIX_CHILD_PID_PUBLISHED";
    #[cfg(unix)]
    const TREE_ROOT: &str = "KIO_TEST_SUPERVISE_TREE_ROOT";
    #[cfg(unix)]
    const TREE_MODE: &str = "KIO_TEST_SUPERVISE_TREE_MODE";
    #[cfg(unix)]
    const FINAL_TRANSITION_READY: &str = "KIO_TEST_SUPERVISE_FINAL_TRANSITION_READY";
    #[cfg(unix)]
    const FINAL_TRANSITION_SIGNAL_SEEN: &str = "KIO_TEST_SUPERVISE_FINAL_TRANSITION_SIGNAL_SEEN";
    #[cfg(unix)]
    const FINAL_TRANSITION_RELEASE: &str = "KIO_TEST_SUPERVISE_FINAL_TRANSITION_RELEASE";
    #[cfg(unix)]
    const FINAL_TRANSITION_FORWARDED: &str = "KIO_TEST_SUPERVISE_FINAL_TRANSITION_FORWARDED";

    #[cfg(unix)]
    static CHILD_TERM_RECEIVED: AtomicBool = AtomicBool::new(false);
    #[cfg(unix)]
    static RESTORED_HUP_RECEIVED: AtomicBool = AtomicBool::new(false);

    #[cfg(unix)]
    extern "C" fn record_child_term(_signal: libc::c_int) {
        CHILD_TERM_RECEIVED.store(true, Ordering::SeqCst);
    }

    #[cfg(unix)]
    extern "C" fn record_restored_hup(_signal: libc::c_int) {
        RESTORED_HUP_RECEIVED.store(true, Ordering::SeqCst);
    }

    #[test]
    fn build_id_has_a_safe_development_fallback() {
        assert!(!BUILD_ID.is_empty());
    }

    #[test]
    fn preexisting_supervision_cancellation_skips_launch_and_publishes_drain() {
        let root = fixture_root("supervise-pre-cancel");
        let drained = root.join("drained");
        let cancel = root.join("cancel");
        std::fs::write(&drained, b"stale").unwrap();
        std::fs::write(&cancel, b"cancel").unwrap();
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("supervise"),
            OsString::from("--drained-marker"),
            drained.clone().into_os_string(),
            OsString::from("--cancel-file"),
            cancel.into_os_string(),
            OsString::from("--"),
            OsString::from("kio-supervise-target-must-not-launch"),
        ];

        assert_eq!(main_entry(arguments).unwrap(), 143);
        assert!(std::fs::read(&drained).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_supervision_control_paths_fail_before_launch() {
        let root = fixture_root("supervise-invalid-control");
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("supervise"),
            OsString::from("--drained-marker"),
            root.join("missing/drained").into_os_string(),
            OsString::from("--cancel-file"),
            root.join("cancel").into_os_string(),
            OsString::from("--"),
            OsString::from("kio-supervise-target-must-not-launch"),
        ];

        assert!(
            main_entry(arguments)
                .unwrap_err()
                .contains("drained marker parent is not a directory")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore]
    fn supervise_exit_cli_fixture() {
        let root = env_path_any("KIO_TEST_SUPERVISE_ROOT");
        let mode = std::env::var("KIO_TEST_SUPERVISE_EXIT").unwrap();
        let target = match mode.as_str() {
            "0" => "tests::supervise_target_exit_zero_fixture",
            "17" => "tests::supervise_target_exit_seventeen_fixture",
            #[cfg(unix)]
            "closed" => "tests::supervise_target_exit_zero_fixture",
            value => panic!("unknown supervise exit fixture status {value}"),
        };
        let mut arguments = supervise_test_arguments(&root, target);
        if mode == "closed" {
            arguments.insert(6, OsString::from("--stdin-closed"));
            arguments.truncate(8);
            arguments.extend([
                OsString::from("sh"),
                OsString::from("-c"),
                OsString::from("if ( exec 7<&0 ) 2>/dev/null; then exit 98; fi"),
            ]);
        }
        let result = main_entry(arguments).unwrap();
        std::process::exit(result);
    }

    #[test]
    #[ignore]
    fn supervise_target_exit_zero_fixture() {}

    #[test]
    #[ignore]
    fn supervise_target_exit_seventeen_fixture() {
        std::process::exit(17);
    }

    #[test]
    fn supervision_preserves_normal_and_nonzero_leader_statuses() {
        for expected in [0, 17] {
            let root = fixture_root(&format!("supervise-exit-{expected}"));
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .arg("--exact")
                .arg("tests::supervise_exit_cli_fixture")
                .env("KIO_TEST_SUPERVISE_ROOT", &root)
                .env("KIO_TEST_SUPERVISE_EXIT", expected.to_string())
                .env("KIO_CI_SCHEDULE", "DISABLE")
                .env("KIO_CI_SCHEDULE_HELD", "not-a-resource")
                .env("KIO_CI_SCHEDULE_LEASE_FDS", "not-a-descriptor")
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(expected));
            assert!(std::fs::read(root.join("drained")).unwrap().is_empty());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_tree_supervision_preserves_closed_stdin() {
        let root = fixture_root("supervise-closed-stdin");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("tests::supervise_exit_cli_fixture")
            .env("KIO_TEST_SUPERVISE_ROOT", &root)
            .env("KIO_TEST_SUPERVISE_EXIT", "closed");
        super::close_stdin_for_child(&mut command);
        assert!(command.status().unwrap().success());
        assert!(std::fs::read(root.join("drained")).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_cli_is_rejected_before_admission() {
        let arguments = ["scheduler", "run", "command"]
            .into_iter()
            .map(OsString::from);
        assert!(main_entry(arguments).unwrap_err().starts_with("usage:"));
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn malformed_reused_lease_cli_fixture() {
        let executable = std::env::current_exe().unwrap();
        let resource = std::env::var("KIO_TEST_RESOURCE").unwrap();
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("run"),
            OsString::from("--resource"),
            OsString::from(&resource),
            OsString::from("--"),
            executable.into_os_string(),
            OsString::from("--ignored"),
            OsString::from("malformed_reused_lease_target_must_not_run"),
        ];
        let error = main_entry(arguments).unwrap_err();
        assert!(
            error.contains("KIO_CI_SCHEDULE_LEASE_FDS contains an invalid descriptor"),
            "unexpected {resource} reuse error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn malformed_reused_lease_target_must_not_run() {
        panic!("malformed reused lease inventory launched its target");
    }

    #[cfg(unix)]
    #[test]
    fn reused_fixed_resources_reject_malformed_lease_inventory() {
        let executable = std::env::current_exe().unwrap();
        for resource in ["work", "cargo"] {
            let status = std::process::Command::new(&executable)
                .arg("--ignored")
                .arg("malformed_reused_lease_cli_fixture")
                .env("KIO_TEST_RESOURCE", resource)
                .env("KIO_CI_SCHEDULE_HELD", resource)
                .env("KIO_CI_SCHEDULE_LEASE_FDS", "3,,4")
                .env_remove("KIO_CI_SCHEDULE")
                .status()
                .unwrap();
            assert!(status.success(), "malformed reused {resource} was accepted");
        }
    }

    #[test]
    #[ignore]
    fn readiness_action_fixture() {
        let executable = std::env::current_exe().unwrap();
        let arguments = [
            OsString::from("scheduler"),
            OsString::from("readiness"),
            OsString::from("--"),
            executable.into_os_string(),
            OsString::from("--ignored"),
            OsString::from("readiness_target_fixture"),
        ];
        assert_eq!(main_entry(arguments).unwrap(), 0);
    }

    #[test]
    #[ignore]
    fn readiness_target_fixture() {
        assert!(std::env::var_os("KIO_CI_SCHEDULE_HELD").is_none());
        assert!(std::env::var_os("KIO_CI_SCHEDULE_LEASE_FDS").is_none());
    }

    #[test]
    fn readiness_action_isolates_the_adapter_environment() {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--ignored")
            .arg("readiness_action_fixture")
            .env("KIO_CI_SCHEDULE_HELD", "work")
            .env_remove("KIO_CI_SCHEDULE_LEASE_FDS");
        // Model the owned work context, including its breakaway-permitting Job
        // on Windows; Cargo's enclosing Job can itself forbid breakaway.
        let status = super::process_supervisor::status(&mut command, true).unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore]
    fn reused_compiler_readiness_cli_fixture() {
        let executable = std::env::current_exe().unwrap();
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("run"),
            OsString::from("--resource"),
            OsString::from("compiler"),
            OsString::from("--readiness-hook"),
            executable.clone().into_os_string(),
            OsString::from("--readiness-hook-arg"),
            OsString::from("--ignored"),
            OsString::from("--readiness-hook-arg"),
            OsString::from("readiness_hook_must_not_run_fixture"),
            OsString::from("--"),
            executable.into_os_string(),
            OsString::from("--ignored"),
            OsString::from("reused_compiler_target_fixture"),
        ];
        assert_eq!(main_entry(arguments).unwrap(), 0);
    }

    #[test]
    #[ignore]
    fn readiness_hook_must_not_run_fixture() {
        panic!("a reused compiler lease repeated its readiness hook");
    }

    #[test]
    #[ignore]
    fn reused_compiler_target_fixture() {
        assert_eq!(std::env::var("KIO_CI_SCHEDULE_HELD").unwrap(), "compiler");
    }

    #[test]
    fn reused_compiler_cli_skips_readiness_with_or_without_global_bypass() {
        for disabled in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("reused_compiler_readiness_cli_fixture")
                .env("KIO_CI_SCHEDULE_HELD", "compiler")
                .env_remove("KIO_CI_SCHEDULE_DIR")
                .env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS")
                .env_remove("KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM")
                .env_remove("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT")
                .env_remove("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0");
            if disabled {
                command.env("KIO_CI_SCHEDULE", "DISABLE");
            } else {
                command.env_remove("KIO_CI_SCHEDULE");
            }
            assert!(command.status().unwrap().success());
        }
    }

    #[test]
    #[ignore]
    fn cli_ignores_library_readiness_environment_fixture() {
        let executable = std::env::current_exe().unwrap();
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("run"),
            OsString::from("--resource"),
            OsString::from("compiler"),
            OsString::from("--"),
            executable.into_os_string(),
            OsString::from("--ignored"),
            OsString::from("readiness_target_fixture"),
        ];
        assert_eq!(main_entry(arguments).unwrap(), 0);
    }

    #[test]
    fn cli_uses_only_its_explicit_readiness_hook() {
        let executable = std::env::current_exe().unwrap();
        let status = std::process::Command::new(&executable)
            .arg("--ignored")
            .arg("cli_ignores_library_readiness_environment_fixture")
            .env("KIO_CI_SCHEDULE", "DISABLE")
            .env_remove("KIO_CI_SCHEDULE_HELD")
            .env_remove("KIO_CI_SCHEDULE_LEASE_FDS")
            .env("KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM", &executable)
            .env("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT", "2")
            .env("KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0", "--ignored")
            .env(
                "KIO_CI_SCHEDULE_READINESS_HOOK_ARG_1",
                "readiness_hook_must_not_run_fixture",
            )
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn supervised_child_exit_and_signal_statuses_are_preserved() {
        let mut exits = std::process::Command::new("sh");
        exits.arg("-c").arg("exit 17");
        assert_eq!(super::run_supervised(exits, true).unwrap(), 17);

        let mut signals = std::process::Command::new("sh");
        signals.arg("-c").arg("kill -TERM $$");
        assert_eq!(super::run_supervised(signals, true).unwrap(), 143);
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_process_tree_supervise_cli_fixture() {
        if std::env::var_os("KIO_TEST_SUPERVISE_IGNORE_INT").is_some() {
            // SAFETY: this fixture is an isolated process and intentionally
            // models the SIGINT disposition inherited by an async shell child.
            assert_ne!(
                unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) },
                libc::SIG_ERR
            );
        }
        let root = env_path_any(TREE_ROOT);
        let script = match std::env::var(TREE_MODE).unwrap().as_str() {
            "natural" => {
                r#"
printf '%s\n' "$$" >"$KIO_TEST_TREE_LEADER"
(
  while [ ! -f "$KIO_TEST_TREE_RELEASE" ]; do sleep 0.02; done
  if [ -f "$KIO_TEST_TREE_STATE" ]; then
    printf 'state-present\n' >"$KIO_TEST_TREE_RESULT"
  else
    printf 'state-missing\n' >"$KIO_TEST_TREE_RESULT"
  fi
  : >"$KIO_TEST_TREE_DONE"
) &
printf '%s\n' "$!" >"$KIO_TEST_TREE_DESCENDANT"
: >"$KIO_TEST_TREE_READY"
exit "$KIO_TEST_TREE_EXIT"
"#
            }
            "signal" => {
                r#"
printf '%s\n' "$$" >"$KIO_TEST_TREE_LEADER"
"$KIO_TEST_TREE_EXECUTABLE" --ignored --exact tests::unix_process_tree_signal_child_fixture &
wait
"#
            }
            "ignore" => {
                r#"
printf '%s\n' "$$" >"$KIO_TEST_TREE_LEADER"
sh -c '
  trap "" HUP INT TERM
  printf "%s\n" "$$" >"$KIO_TEST_TREE_DESCENDANT"
  : >"$KIO_TEST_TREE_READY"
  while :; do sleep 1; done
' &
wait
"#
            }
            "tty-read" => {
                r#"
printf '%s\n' "$$" >"$KIO_TEST_TREE_LEADER"
printf '%s\n' "$$" >"$KIO_TEST_TREE_DESCENDANT"
IFS= read -r line
printf '%s\n' "$line" >"$KIO_TEST_TREE_RESULT"
"#
            }
            "tty-int" => {
                r#"
printf '%s\n' "$$" >"$KIO_TEST_TREE_LEADER"
trap 'exit 130' INT
sh -c '
  trap "" HUP INT TERM
  printf "%s\n" "$$" >"$KIO_TEST_TREE_DESCENDANT"
  : >"$KIO_TEST_TREE_READY"
  while :; do sleep 1; done
' &
wait
"#
            }
            mode => panic!("unknown process-tree supervision fixture mode {mode}"),
        };
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("supervise"),
            OsString::from("--drained-marker"),
            root.join("drained").into_os_string(),
            OsString::from("--cancel-file"),
            root.join("cancel").into_os_string(),
            OsString::from("--"),
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from(script),
        ];
        let result = match main_entry(arguments) {
            Ok(result) => result,
            Err(error) if std::env::var_os("KIO_TEST_SUPERVISE_MONITOR_ERROR_AFTER").is_some() => {
                std::fs::write(root.join("monitor-error"), error.to_string().as_bytes()).unwrap();
                0
            }
            Err(error) => panic!("unexpected process-tree supervisor error: {error}"),
        };
        if let Some(path) = std::env::var_os(FINAL_TRANSITION_FORWARDED) {
            std::fs::write(
                path,
                super::TEST_FORWARDED_CHILD
                    .load(Ordering::SeqCst)
                    .to_string(),
            )
            .unwrap();
        }
        if matches!(
            std::env::var(TREE_MODE).unwrap().as_str(),
            "tty-read" | "tty-int"
        ) || std::env::var_os("KIO_TEST_SUPERVISE_MONITOR_ERROR_AFTER").is_some()
        {
            // SAFETY: stdin is the fixture's controlling terminal.
            let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
            // SAFETY: getpgrp has no preconditions.
            if foreground == unsafe { libc::getpgrp() } {
                std::fs::write(root.join("tty-restored"), b"restored").unwrap();
            }
        }
        std::process::exit(result);
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_process_tree_signal_child_fixture() {
        CHILD_TERM_RECEIVED.store(false, Ordering::SeqCst);
        let handler = record_child_term as *const () as libc::sighandler_t;
        // SAFETY: the handler performs one lock-free atomic store and remains
        // valid until the prior dispositions are restored below.
        let previous_hup = unsafe { libc::signal(libc::SIGHUP, handler) };
        assert_ne!(previous_hup, libc::SIG_ERR);
        let previous_term = unsafe { libc::signal(libc::SIGTERM, handler) };
        assert_ne!(previous_term, libc::SIG_ERR);

        let root = env_path_any(TREE_ROOT);
        let leader: u32 = std::fs::read_to_string(root.join("leader-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_ne!(std::process::id(), leader);
        // SAFETY: getpgrp has no preconditions.
        assert_eq!(unsafe { libc::getpgrp() }, leader as i32);
        std::fs::write(root.join("descendant-pid"), std::process::id().to_string()).unwrap();
        std::fs::write(root.join("ready"), b"ready").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !CHILD_TERM_RECEIVED.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(CHILD_TERM_RECEIVED.load(Ordering::SeqCst));
        let result = if root.join("state").is_file() {
            "state-present\n"
        } else {
            "state-missing\n"
        };
        std::fs::write(root.join("result"), result).unwrap();
        std::fs::write(root.join("done"), b"done").unwrap();

        for (signal, previous) in [(libc::SIGHUP, previous_hup), (libc::SIGTERM, previous_term)] {
            // SAFETY: each previous disposition came from its installation above.
            assert_ne!(unsafe { libc::signal(signal, previous) }, libc::SIG_ERR);
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn supervise_tty_spawn_error_cli_fixture() {
        let root = env_path_any(TREE_ROOT);
        let arguments = vec![
            OsString::from("scheduler"),
            OsString::from("supervise"),
            OsString::from("--drained-marker"),
            root.join("drained").into_os_string(),
            OsString::from("--cancel-file"),
            root.join("cancel").into_os_string(),
            OsString::from("--"),
            OsString::from("kio-supervise-tty-target-must-not-exist"),
        ];
        assert!(
            main_entry(arguments)
                .unwrap_err()
                .contains("cannot supervise")
        );
        // SAFETY: stdin is the fixture's controlling terminal.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        // SAFETY: getpgrp has no preconditions.
        if foreground == unsafe { libc::getpgrp() } {
            std::fs::write(root.join("tty-restored"), b"restored").unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn supervision_waits_for_natural_descendants_and_preserves_leader_status() {
        for expected in [0, 17, 130] {
            let root = fixture_root(&format!("supervise-tree-{expected}"));
            std::fs::write(root.join("state"), b"live").unwrap();
            let mut supervisor = unix_tree_supervisor(&root, "natural", expected, false);
            let mut supervisor = supervisor.spawn().unwrap();
            if !path_appears(&root.join("ready")) {
                kill_tree_fixture(&mut supervisor, &root);
                panic!("natural descendant did not start");
            }
            assert!(!root.join("drained").exists());
            std::thread::sleep(Duration::from_millis(50));
            assert!(supervisor.try_wait().unwrap().is_none());
            std::fs::write(root.join("release"), b"release").unwrap();
            let status = wait_for_child_exit(&mut supervisor).unwrap_or_else(|| {
                kill_tree_fixture(&mut supervisor, &root);
                panic!("supervisor did not observe natural descendant completion")
            });
            assert_eq!(status.code(), Some(expected));
            assert_eq!(
                std::fs::read(root.join("result")).unwrap(),
                b"state-present\n"
            );
            assert!(root.join("done").exists());
            assert_tree_drained(&root);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_an_ignoring_tree_after_grace_and_then_publishes_drain() {
        let root = fixture_root("supervise-tree-ignore");
        let mut supervisor = unix_tree_supervisor(&root, "ignore", 0, false);
        let mut supervisor = supervisor.spawn().unwrap();
        if !path_appears(&root.join("ready")) {
            kill_tree_fixture(&mut supervisor, &root);
            panic!("signal-ignoring descendant did not start");
        }
        let started = Instant::now();
        std::fs::write(root.join("cancel"), b"cancel").unwrap();
        let status = wait_for_child_exit(&mut supervisor).unwrap_or_else(|| {
            kill_tree_fixture(&mut supervisor, &root);
            panic!("cancelled supervisor did not kill its ignoring descendant")
        });
        assert_eq!(status.code(), Some(143));
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_tree_drained(&root);
        assert!(!root.join("done").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn supervise_signals_drain_the_tree_and_preserve_exact_statuses() {
        for (label, signal, expected, ignore_int) in [
            ("hup", libc::SIGHUP, 129, false),
            ("inherited-ignored-int", libc::SIGINT, 130, true),
            ("term", libc::SIGTERM, 143, false),
        ] {
            let root = fixture_root(&format!("supervise-tree-{label}"));
            std::fs::write(root.join("state"), b"live").unwrap();
            let mut supervisor = unix_tree_supervisor(&root, "signal", 0, ignore_int);
            let mut supervisor = supervisor.spawn().unwrap();
            if !path_appears(&root.join("ready")) {
                kill_tree_fixture(&mut supervisor, &root);
                panic!("{label} descendant did not start");
            }
            assert!(!root.join("drained").exists());
            // SAFETY: this PID is the live isolated fixture supervisor.
            assert_eq!(unsafe { libc::kill(supervisor.id() as i32, signal) }, 0);
            let status = wait_for_child_exit(&mut supervisor).unwrap_or_else(|| {
                kill_tree_fixture(&mut supervisor, &root);
                panic!("{label} supervisor did not finish draining")
            });
            assert_eq!(status.code(), Some(expected));
            assert_eq!(
                std::fs::read(root.join("result")).unwrap(),
                b"state-present\n"
            );
            assert!(root.join("done").exists());
            assert_tree_drained(&root);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_tree_supervision_preserves_foreground_tty_input() {
        let root = fixture_root("supervise-tty-read");
        eprintln!("PTY input: spawning isolated supervisor");
        let (mut supervisor, mut terminal) = pty_tree_supervisor(&root, "tty-read");
        eprintln!("PTY input: writing to supervisor {}", supervisor.id());
        terminal.write_all(b"hello from tty\n").unwrap();
        eprintln!("PTY input: waiting for complete drain");
        let status = match wait_for_pty_exit(&mut supervisor, &mut terminal) {
            Ok(Some(status)) => status,
            outcome => {
                eprintln!("PTY input: drain failed; killing owned fixture processes: {outcome:?}");
                drop(terminal);
                kill_tree_fixture(&mut supervisor, &root);
                panic!("supervised foreground-TTY reader stopped or hung")
            }
        };
        assert!(status.success());
        assert_eq!(
            std::fs::read(root.join("result")).unwrap(),
            b"hello from tty\n"
        );
        assert!(root.join("tty-restored").exists());
        assert_tree_drained(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn terminal_interrupt_drains_ignoring_descendants_and_preserves_status() {
        let root = fixture_root("supervise-tty-int");
        eprintln!("PTY interrupt: spawning isolated supervisor");
        let (mut supervisor, mut terminal) = pty_tree_supervisor(&root, "tty-int");
        eprintln!(
            "PTY interrupt: waiting for supervisor {} readiness",
            supervisor.id()
        );
        if !path_appears(&root.join("ready")) {
            eprintln!("PTY interrupt: startup timed out; killing owned fixture processes");
            drop(terminal);
            kill_tree_fixture(&mut supervisor, &root);
            panic!("supervised foreground-TTY interrupt fixture did not start")
        }
        let started = Instant::now();
        eprintln!("PTY interrupt: reading terminal interrupt character");
        let interrupt = tty_interrupt_byte(terminal.as_raw_fd());
        eprintln!("PTY interrupt: writing terminal interrupt character");
        terminal.write_all(&[interrupt]).unwrap();
        eprintln!("PTY interrupt: waiting for complete drain");
        let status = match wait_for_pty_exit(&mut supervisor, &mut terminal) {
            Ok(Some(status)) => status,
            outcome => {
                eprintln!(
                    "PTY interrupt: drain failed; killing owned fixture processes: {outcome:?}"
                );
                drop(terminal);
                kill_tree_fixture(&mut supervisor, &root);
                panic!("terminal interrupt did not drain its ignoring descendant")
            }
        };
        assert_eq!(status.code(), Some(130));
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(root.join("tty-restored").exists());
        assert_tree_drained(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn foreground_tty_is_restored_when_target_exec_fails() {
        let root = fixture_root("supervise-tty-spawn-error");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("tests::supervise_tty_spawn_error_cli_fixture")
            .env(TREE_ROOT, &root);
        let (mut supervisor, _terminal) = spawn_in_pty_session(command);
        assert!(supervisor.wait().unwrap().success());
        assert!(root.join("tty-restored").exists());
        assert!(!root.join("drained").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn post_spawn_monitor_error_still_drains_before_restoring_the_tty() {
        let root = fixture_root("supervise-tty-monitor-error");
        let mut command = unix_tree_supervisor(&root, "ignore", 0, false);
        command.env("KIO_TEST_SUPERVISE_MONITOR_ERROR_AFTER", root.join("ready"));
        let started = Instant::now();
        let (mut supervisor, _terminal) = spawn_in_pty_session(command);
        assert!(supervisor.wait().unwrap().success());
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(
            std::fs::read_to_string(root.join("monitor-error"))
                .unwrap()
                .contains("injected post-spawn supervision monitor error")
        );
        assert!(!root.join("drained").exists());
        assert!(root.join("tty-restored").exists());
        assert_descendant_extinct(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn final_extinction_transition_defers_group_delivery_until_publication_is_cleared() {
        let root = fixture_root("supervise-final-transition");
        std::fs::write(root.join("state"), b"state").unwrap();
        std::fs::write(root.join("release"), b"release").unwrap();
        let mut command = unix_tree_supervisor(&root, "natural", 0, false);
        command
            .env(FINAL_TRANSITION_READY, root.join("transition-ready"))
            .env(
                FINAL_TRANSITION_SIGNAL_SEEN,
                root.join("transition-signal-seen"),
            )
            .env(FINAL_TRANSITION_RELEASE, root.join("transition-release"))
            .env(FINAL_TRANSITION_FORWARDED, root.join("forwarded-child"));
        let mut supervisor = command.spawn().unwrap();
        assert!(path_appears(&root.join("transition-ready")));
        // SAFETY: the supervisor is live until the transition fixture is released.
        assert_eq!(
            unsafe { libc::kill(supervisor.id() as i32, libc::SIGTERM) },
            0
        );
        if !path_appears(&root.join("transition-signal-seen")) {
            kill_tree_fixture(&mut supervisor, &root);
            panic!("final-transition supervisor did not observe SIGTERM");
        }
        std::fs::write(root.join("transition-release"), b"release").unwrap();
        let status = wait_for_child_exit(&mut supervisor)
            .expect("signal-safe final transition did not complete");
        assert_eq!(status.code(), Some(143));
        assert_eq!(
            std::fs::read_to_string(root.join("forwarded-child")).unwrap(),
            "0"
        );
        assert!(root.join("drained").exists());
        assert_tree_drained(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn supervision_errors_keep_primary_then_cleanup_then_epilogue_context() {
        let primary = std::io::Error::new(std::io::ErrorKind::TimedOut, "monitor failed");
        let cleanup = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "kill failed");
        let combined = super::combine_supervision_errors(primary, Some(cleanup), true);
        let result = super::append_supervision_error(
            combined,
            "supervisor epilogue also failed",
            std::io::Error::other("TTY restore failed"),
        );

        assert_eq!(result.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(
            result.to_string(),
            "monitor failed; process-tree cleanup also failed: kill failed; process-tree extinction could not be established; foreground-terminal restoration was withheld from the possibly live child group; supervisor epilogue also failed: TTY restore failed"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_signal_child_fixture() {
        CHILD_TERM_RECEIVED.store(false, Ordering::SeqCst);
        let handler = record_child_term as *const () as libc::sighandler_t;
        // SAFETY: the handler performs one lock-free atomic store and remains
        // valid until the prior disposition is restored below.
        let previous = unsafe { libc::signal(libc::SIGTERM, handler) };
        assert_ne!(previous, libc::SIG_ERR);

        let ready = env_path(SIGNAL_READY);
        std::fs::write(&ready, b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !CHILD_TERM_RECEIVED.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(CHILD_TERM_RECEIVED.load(Ordering::SeqCst));
        std::fs::write(env_path(SIGNAL_DELIVERED), b"delivered").unwrap();
        if std::env::var_os(SIGNAL_HOLD).is_some() {
            assert!(path_appears(&env_path(SIGNAL_RELEASE)));
        }

        // SAFETY: `previous` came from the successful installation above.
        assert_ne!(
            unsafe { libc::signal(libc::SIGTERM, previous) },
            libc::SIG_ERR
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_default_term_child_fixture() {
        std::thread::sleep(Duration::from_secs(1));
        panic!("publication-window TERM was not delivered to the child");
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_signal_supervisor_fixture() {
        RESTORED_HUP_RECEIVED.store(false, Ordering::SeqCst);
        let observer = record_restored_hup as *const () as libc::sighandler_t;
        // SAFETY: the observer performs one lock-free atomic store and remains
        // valid for this isolated fixture process.
        let previous_hup = unsafe { libc::signal(libc::SIGHUP, observer) };
        assert_ne!(previous_hup, libc::SIG_ERR);

        let mode = std::env::var(SIGNAL_MODE).unwrap();
        let result = if mode == "publication" {
            let (signal_observed, signal_observer) = signal_observer_pipe();
            let signal_observed_fd = signal_observed.as_raw_fd();
            let signal_observer_fd = signal_observer.as_raw_fd();
            super::TEST_PRE_PUBLICATION_SIGNAL_FD.store(signal_observer_fd, Ordering::SeqCst);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("--ignored")
                .arg("--exact")
                .arg("tests::unix_default_term_child_fixture");
            // Reparenting must not select a different signal recipient.
            let supervisor_pid = std::process::id() as libc::pid_t;
            // Command::spawn cannot publish the child PID before exec. The
            // acknowledgement proves the parent's handler ran in that window.
            // After exec, default TERM termination avoids routing a blocked
            // signal between the test harness's initial and worker threads.
            unsafe {
                child.pre_exec(move || {
                    libc::close(signal_observer_fd);
                    if libc::getppid() != supervisor_pid {
                        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    if libc::kill(supervisor_pid, libc::SIGTERM) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let mut observed = 0_u8;
                    loop {
                        let read =
                            libc::read(signal_observed_fd, (&mut observed as *mut u8).cast(), 1);
                        if read == 1 && observed == libc::SIGTERM as u8 {
                            break;
                        }
                        if read == -1
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                        {
                            continue;
                        }
                        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
                    }
                    libc::close(signal_observed_fd);
                    Ok(())
                });
            }
            let result = super::run_supervised(child, true).unwrap();
            super::TEST_PRE_PUBLICATION_SIGNAL_FD.store(-1, Ordering::SeqCst);
            drop(signal_observed);
            drop(signal_observer);
            let child_signal = super::TEST_REAPED_CHILD_SIGNAL.load(Ordering::SeqCst);
            eprintln!("publication-window child termination signal: {child_signal}");
            assert_eq!(child_signal, libc::SIGTERM);
            std::fs::write(env_path(SIGNAL_DELIVERED), b"delivered").unwrap();
            result
        } else {
            assert_eq!(mode, "external");
            let executable = std::env::current_exe().unwrap().into_os_string();
            let arguments = vec![
                OsString::from("scheduler"),
                OsString::from("run"),
                OsString::from("--resource"),
                OsString::from("work"),
                OsString::from("--jobs"),
                OsString::from("1"),
                OsString::from("--"),
                executable,
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("tests::unix_signal_child_fixture"),
            ];
            main_entry(arguments).unwrap()
        };
        assert_eq!(result, 143);

        if mode == "publication" {
            // The first invocation left a pending TERM. A second invocation in
            // this fixture proves that entry resets both signal statics.
            let mut exits = std::process::Command::new("sh");
            exits.arg("-c").arg("exit 17");
            assert_eq!(super::run_supervised(exits, true).unwrap(), 17);
        }

        // run_supervised must restore the observer rather than leave its own
        // zero-child forwarding handler swallowing signals after completion.
        // SAFETY: raising HUP synchronously invokes the installed disposition.
        assert_eq!(unsafe { libc::raise(libc::SIGHUP) }, 0);
        assert!(RESTORED_HUP_RECEIVED.load(Ordering::SeqCst));
        // SAFETY: `previous_hup` came from the successful installation above.
        assert_ne!(
            unsafe { libc::signal(libc::SIGHUP, previous_hup) },
            libc::SIG_ERR
        );
        std::process::exit(result);
    }

    #[cfg(unix)]
    #[test]
    fn signal_between_spawn_and_pid_publication_is_forwarded() {
        let paths = UnixSignalPaths::new("publication");
        let mut supervisor = paths.supervisor_command("publication");
        let mut supervisor = supervisor.spawn().unwrap();
        let status = wait_for_child_exit(&mut supervisor).unwrap_or_else(|| {
            kill_fixture_group(&mut supervisor);
            panic!("publication-window supervisor did not exit")
        });
        assert_eq!(status.code(), Some(143));
        assert!(paths.published.exists());
        assert_eq!(std::fs::read(&paths.delivered).unwrap(), b"delivered");
        paths.remove();
    }

    #[cfg(unix)]
    #[test]
    fn external_term_is_forwarded_without_releasing_the_live_child_lease() {
        let paths = UnixSignalPaths::new("external");
        let mut command = paths.supervisor_command("external");
        command.env(SIGNAL_HOLD, "1");
        let mut supervisor = command.spawn().unwrap();
        if !path_appears(&paths.ready) || !path_appears(&paths.published) {
            kill_fixture_group(&mut supervisor);
            paths.remove();
            panic!("external-signal supervisor did not publish its live child")
        }

        // SAFETY: the fixture supervisor PID is live and belongs to this test.
        assert_eq!(
            unsafe { libc::kill(supervisor.id() as i32, libc::SIGTERM) },
            0
        );
        if !path_appears(&paths.delivered) {
            kill_fixture_group(&mut supervisor);
            paths.remove();
            panic!("external TERM was not delivered to the supervised child")
        }
        assert!(supervisor.try_wait().unwrap().is_none());

        let lease_path = wait_for_active_lease(&paths.state).unwrap_or_else(|| {
            kill_fixture_group(&mut supervisor);
            paths.remove();
            panic!("supervisor published no active work lease")
        });
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lease_path)
            .unwrap();
        assert!(matches!(lease.try_lock(), Err(TryLockError::WouldBlock)));

        std::fs::write(&paths.release, b"release").unwrap();
        let status = wait_for_child_exit(&mut supervisor).unwrap_or_else(|| {
            kill_fixture_group(&mut supervisor);
            paths.remove();
            panic!("externally signaled supervisor did not exit after child release")
        });
        assert_eq!(status.code(), Some(143));
        lease.try_lock().unwrap();
        drop(lease);
        paths.remove();
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn closed_stdin_supervisor_fixture() {
        let mode = std::env::var("KIO_TEST_CLOSED_STDIN_MODE").unwrap();
        if mode == "command" {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("if ( exec 7<&0 ) 2>/dev/null; then exit 98; fi");
            assert_eq!(super::run_supervised(command, false).unwrap(), 0);
        } else {
            let command = std::process::Command::new(
                "/kio-ci-scheduler-selftest-command-that-does-not-exist",
            );
            assert_eq!(
                super::run_supervised(command, false).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn closed_stdin_is_preserved_without_hiding_spawn_errors() {
        for mode in ["command", "missing"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("closed_stdin_supervisor_fixture")
                .env("KIO_TEST_CLOSED_STDIN_MODE", mode);
            super::close_stdin_for_child(&mut command);
            assert!(command.status().unwrap().success());
        }
    }

    fn fixture_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kio-scheduler-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn env_path_any(name: &str) -> PathBuf {
        std::env::var_os(name).map(PathBuf::from).unwrap()
    }

    fn supervise_test_arguments(root: &Path, target: &str) -> Vec<OsString> {
        vec![
            OsString::from("scheduler"),
            OsString::from("supervise"),
            OsString::from("--drained-marker"),
            root.join("drained").into_os_string(),
            OsString::from("--cancel-file"),
            root.join("cancel").into_os_string(),
            OsString::from("--"),
            std::env::current_exe().unwrap().into_os_string(),
            OsString::from("--ignored"),
            OsString::from("--exact"),
            OsString::from(target),
        ]
    }

    #[cfg(unix)]
    fn unix_tree_supervisor(
        root: &Path,
        mode: &str,
        exit_status: i32,
        ignore_int: bool,
    ) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("tests::unix_process_tree_supervise_cli_fixture")
            .env(TREE_ROOT, root)
            .env(TREE_MODE, mode)
            .env("KIO_TEST_TREE_EXECUTABLE", std::env::current_exe().unwrap())
            .env("KIO_TEST_TREE_LEADER", root.join("leader-pid"))
            .env("KIO_TEST_TREE_DESCENDANT", root.join("descendant-pid"))
            .env("KIO_TEST_TREE_READY", root.join("ready"))
            .env("KIO_TEST_TREE_RELEASE", root.join("release"))
            .env("KIO_TEST_TREE_STATE", root.join("state"))
            .env("KIO_TEST_TREE_RESULT", root.join("result"))
            .env("KIO_TEST_TREE_DONE", root.join("done"))
            .env("KIO_TEST_TREE_EXIT", exit_status.to_string());
        if ignore_int {
            command.env("KIO_TEST_SUPERVISE_IGNORE_INT", "1");
        } else {
            command.env_remove("KIO_TEST_SUPERVISE_IGNORE_INT");
        }
        command
    }

    #[cfg(unix)]
    fn pty_tree_supervisor(root: &Path, mode: &str) -> (std::process::Child, File) {
        let command = unix_tree_supervisor(root, mode, 0, false);
        spawn_in_pty_session(command)
    }

    #[cfg(unix)]
    fn spawn_in_pty_session(mut command: std::process::Command) -> (std::process::Child, File) {
        let (terminal, slave) = open_pty();
        command.stdin(std::process::Stdio::from(slave));
        // SAFETY: the child becomes an isolated session leader before claiming
        // its already-open slave as the controlling terminal. These operations
        // are async-signal-safe and touch only the post-fork child.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        (command.spawn().unwrap(), terminal)
    }

    #[cfg(unix)]
    fn open_pty() -> (File, File) {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: openpty initializes both descriptors on success, and each is
        // transferred exactly once into its returned File owner.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // The terminal host owns the master. Inheriting it into the session
        // would prevent closing the host side from ending a failed fixture.
        for descriptor in [master, slave] {
            // SAFETY: openpty returned these live descriptors. Command copies
            // the slave to stdin without CLOEXEC when launching the fixture.
            assert_eq!(
                unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        // SAFETY: successful openpty returned two distinct owned descriptors.
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }

    #[cfg(unix)]
    fn wait_for_pty_exit(
        child: &mut std::process::Child,
        terminal: &mut File,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        // Darwin drains terminal output during session-leader exit. Consume
        // input echo as a terminal host would, without changing tty semantics.
        let descriptor = terminal.as_raw_fd();
        // SAFETY: descriptor names the live master owned by this fixture.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut bytes_read = 0;
        let mut buffer = [0; 1024];
        loop {
            match terminal.read(&mut buffer) {
                Ok(count) => bytes_read += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) || error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => return Err(error),
            }
            let status = child.try_wait()?;
            if status.is_some() || Instant::now() >= deadline {
                eprintln!("PTY host consumed {bytes_read} output bytes");
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn tty_interrupt_byte(fd: i32) -> u8 {
        // SAFETY: fd names the live PTY master owned by the fixture.
        let mut attributes = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut attributes) }, 0);
        attributes.c_cc[libc::VINTR]
    }

    #[cfg(unix)]
    fn kill_tree_fixture(supervisor: &mut std::process::Child, root: &Path) {
        if let Ok(pid) = std::fs::read_to_string(root.join("leader-pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
            && pid > 1
        {
            // SAFETY: the native supervisor isolates this target leader as
            // the id of the fixture-owned process group.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        let _ = supervisor.kill();
        assert!(
            wait_for_child_exit(supervisor).is_some(),
            "fixture supervisor {} did not exit after SIGKILL",
            supervisor.id()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn assert_tree_drained(root: &Path) {
        assert!(std::fs::read(root.join("drained")).unwrap().is_empty());
        assert_descendant_extinct(root);
    }

    #[cfg(unix)]
    fn assert_descendant_extinct(root: &Path) {
        let descendant = std::fs::read_to_string(root.join("descendant-pid"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        // SAFETY: signal zero only probes the fixture-recorded process id.
        assert_eq!(unsafe { libc::kill(descendant, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    struct UnixSignalPaths {
        root: PathBuf,
        state: PathBuf,
        ready: PathBuf,
        delivered: PathBuf,
        release: PathBuf,
        published: PathBuf,
    }

    #[cfg(unix)]
    impl UnixSignalPaths {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "kio-scheduler-unix-signal-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self {
                state: root.join("state"),
                ready: root.join("ready"),
                delivered: root.join("delivered"),
                release: root.join("release"),
                published: root.join("published"),
                root,
            }
        }

        fn supervisor_command(&self, mode: &str) -> std::process::Command {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("--exact")
                .arg("tests::unix_signal_supervisor_fixture")
                .arg("--nocapture")
                .env(SIGNAL_MODE, mode)
                .env(SIGNAL_READY, &self.ready)
                .env(SIGNAL_DELIVERED, &self.delivered)
                .env(SIGNAL_RELEASE, &self.release)
                .env(CHILD_PID_PUBLISHED, &self.published)
                .env("KIO_CI_SCHEDULE_DIR", &self.state)
                .env_remove("KIO_CI_SCHEDULE")
                .env_remove("KIO_CI_SCHEDULE_HELD")
                .env_remove("KIO_CI_SCHEDULE_JOBS")
                .env_remove("KIO_CI_SCHEDULE_COMPILER_JOBS")
                .process_group(0);
            command
        }

        fn remove(&self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[cfg(unix)]
    fn env_path(name: &str) -> PathBuf {
        std::env::var_os(name).map(PathBuf::from).unwrap()
    }

    #[cfg(unix)]
    fn signal_observer_pipe() -> (OwnedFd, OwnedFd) {
        let mut descriptors = [-1; 2];
        // SAFETY: `pipe` initializes both descriptors on success, and ownership
        // of each is transferred exactly once to the returned `OwnedFd`.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        unsafe {
            (
                OwnedFd::from_raw_fd(descriptors[0]),
                OwnedFd::from_raw_fd(descriptors[1]),
            )
        }
    }

    #[cfg(unix)]
    fn path_appears(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        path.exists()
    }

    #[cfg(unix)]
    fn wait_for_child_exit(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.try_wait().unwrap()
    }

    #[cfg(unix)]
    fn kill_fixture_group(supervisor: &mut std::process::Child) {
        // SAFETY: the fixture was made the leader of a dedicated process group.
        let _ = unsafe { libc::kill(-(supervisor.id() as i32), libc::SIGKILL) };
        let _ = supervisor.wait();
    }

    #[cfg(unix)]
    fn wait_for_active_lease(state: &Path) -> Option<PathBuf> {
        let claims = state.join("work").join("claims");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(entries) = std::fs::read_dir(&claims) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension() == Some(std::ffi::OsStr::new("active")) {
                        return Some(path.with_extension("lease"));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}
