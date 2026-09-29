#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, ExitStatus, Output};

    pub(crate) fn isolate_process_group(command: &mut Command) {
        configure_process_group(command, None, false);
    }

    pub(crate) fn isolate_process_group_with_foreground_tty(
        command: &mut Command,
        stdin_was_open: bool,
    ) -> io::Result<Option<ForegroundTty>> {
        let foreground = if stdin_was_open {
            foreground_process_group()?
        } else {
            None
        };
        configure_process_group(command, foreground, true);
        Ok(foreground.map(ForegroundTty::new))
    }

    fn configure_process_group(
        command: &mut Command,
        foreground: Option<libc::pid_t>,
        reset_abort_signals: bool,
    ) {
        // SAFETY: every operation reachable in this pre-exec hook is POSIX
        // async-signal-safe and touches only the child between fork and exec.
        // A distinct group gives the caller one stable handle for the complete
        // cooperative descendant tree.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if reset_abort_signals {
                    reset_child_abort_signals()?;
                }
                if let Some(original) = foreground {
                    let current = libc::tcgetpgrp(libc::STDIN_FILENO);
                    if current == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if current != original {
                        return Err(io::Error::from_raw_os_error(libc::EBUSY));
                    }
                    set_foreground_process_group(libc::STDIN_FILENO, libc::getpgrp())?;
                }
                Ok(())
            });
        }
    }

    fn foreground_process_group() -> io::Result<Option<libc::pid_t>> {
        // SAFETY: tcgetpgrp only inspects the open standard-input descriptor.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if foreground == -1 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOTTY) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: getpgrp has no preconditions.
        Ok((foreground == unsafe { libc::getpgrp() }).then_some(foreground))
    }

    fn reset_child_abort_signals() -> io::Result<()> {
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            // SAFETY: restoring the default disposition is async-signal-safe
            // and affects only the post-fork child before exec.
            if unsafe { libc::signal(signal, libc::SIG_DFL) } == libc::SIG_ERR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn set_foreground_process_group(fd: libc::c_int, group: libc::pid_t) -> io::Result<()> {
        // A process outside the terminal's foreground group would otherwise be
        // stopped by SIGTTOU while transferring or restoring ownership.
        // sigprocmask and tcsetpgrp are async-signal-safe, so this helper is
        // valid both in pre_exec and in the waiting parent.
        // SAFETY: sigemptyset initializes the set before sigaddset reads it.
        let mut blocked = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        let mut previous = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        if unsafe { libc::sigemptyset(&mut blocked) } == -1
            || unsafe { libc::sigaddset(&mut blocked, libc::SIGTTOU) } == -1
            || unsafe { libc::sigprocmask(libc::SIG_BLOCK, &blocked, &mut previous) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is the caller-validated controlling terminal and group is
        // in the same session. SIGTTOU is blocked for this operation.
        let changed = unsafe { libc::tcsetpgrp(fd, group) };
        let change_error = (changed == -1).then(io::Error::last_os_error);
        // SAFETY: previous was initialized by the successful SIG_BLOCK call.
        let restored =
            unsafe { libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
        if let Some(error) = change_error {
            Err(error)
        } else if restored == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) struct ForegroundTty {
        original_group: libc::pid_t,
        child_group: Option<libc::pid_t>,
        restored: bool,
    }

    impl ForegroundTty {
        fn new(original_group: libc::pid_t) -> Self {
            Self {
                original_group,
                child_group: None,
                restored: false,
            }
        }

        pub(crate) fn child_spawned(&mut self, pid: u32) {
            self.child_group = Some(pid as libc::pid_t);
        }

        pub(crate) fn restore(&mut self) -> io::Result<()> {
            if self.restored {
                return Ok(());
            }
            // SAFETY: tcgetpgrp only inspects the controlling terminal.
            let current = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
            if current == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ENOTTY) {
                    self.restored = true;
                    return Ok(());
                }
                return Err(error);
            }
            let still_ours = match self.child_group {
                Some(group) => current == group,
                None => current != self.original_group && !process_group_exists_raw(current)?,
            };
            if still_ours {
                set_foreground_process_group(libc::STDIN_FILENO, self.original_group)?;
            }
            self.restored = true;
            Ok(())
        }

        pub(crate) fn leave_with_unproven_child(&mut self) {
            self.restored = true;
        }
    }

    impl Drop for ForegroundTty {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }

    pub(crate) fn signal_process_group(pid: u32, signal: libc::c_int) -> io::Result<()> {
        let pid = i32::try_from(pid)
            .map_err(|_| io::Error::other("child pid does not fit in a process-group id"))?;
        // SAFETY: the negative id addresses the isolated child process group.
        if unsafe { libc::kill(-pid, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(crate) fn process_group_exists(pid: u32) -> io::Result<bool> {
        let pid = i32::try_from(pid)
            .map_err(|_| io::Error::other("child pid does not fit in a process-group id"))?;
        process_group_exists_raw(pid)
    }

    fn process_group_exists_raw(pid: libc::pid_t) -> io::Result<bool> {
        // SAFETY: signal zero probes existence without delivering a signal.
        if unsafe { libc::kill(-pid, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error),
        }
    }

    pub(crate) fn status(command: &mut Command, _stdin_was_open: bool) -> io::Result<ExitStatus> {
        command.status()
    }

    pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
        command.output()
    }

    pub(crate) fn readiness_status(command: &mut Command) -> io::Result<ExitStatus> {
        let lease_fds = crate::resource_admission::inherited_lease_fds_from_env()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        command.env_remove(crate::resource_admission::LEASE_FDS_ENV);
        // A readiness probe may start a daemon. It must not inherit any
        // scheduler lease or remain in the invocation process group, or that
        // daemon could keep admission or whole-tree drain occupied after the
        // admitted command has completed.
        isolate_process_group(command);
        unsafe {
            command.pre_exec(move || {
                for fd in &lease_fds {
                    if libc::close(*fd) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        command.status()
    }

    #[cfg(test)]
    mod tests {
        use super::{isolate_process_group, process_group_exists, readiness_status};
        use crate::resource_admission::{LEASE_FDS_ENV, inherited_lease_fds_from_env};
        use std::fs::{self, File, OpenOptions};
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Command;
        use std::thread;
        use std::time::{Duration, Instant};

        const MODE: &str = "KIO_TEST_UNIX_READINESS_MODE";
        const LEASE: &str = "KIO_TEST_UNIX_READINESS_LEASE";
        const STARTED: &str = "KIO_TEST_UNIX_READINESS_STARTED";
        const FINISHED: &str = "KIO_TEST_UNIX_READINESS_FINISHED";
        const RELEASE: &str = "KIO_TEST_UNIX_READINESS_RELEASE";

        #[test]
        #[ignore]
        fn readiness_fixture() {
            let mode = std::env::var(MODE).unwrap();
            let started = path_from_env(STARTED);
            let finished = path_from_env(FINISHED);
            let release = path_from_env(RELEASE);
            match mode.as_str() {
                "outer" => {
                    let mut adapter = fixture_command("adapter", &started, &finished, &release);
                    assert!(readiness_status(&mut adapter).unwrap().success());

                    for fd in inherited_lease_fds_from_env().unwrap() {
                        // SAFETY: the fixture owns every descriptor named in
                        // its inherited scheduler inventory.
                        assert_eq!(unsafe { libc::close(fd) }, 0);
                    }
                    let lease_path = path_from_env(LEASE);
                    let lease = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(lease_path)
                        .unwrap();
                    let acquired = lease.try_lock().is_ok();
                    fs::write(&release, b"release").unwrap();
                    wait_for_path(&finished);
                    assert!(
                        acquired,
                        "readiness descendant retained the lease descriptor"
                    );
                }
                "adapter" => {
                    assert!(std::env::var_os(LEASE_FDS_ENV).is_none());
                    // The adapter intentionally exits without waiting: this
                    // regression proves that its daemon-like descendant can
                    // outlive readiness without retaining a scheduler lease.
                    // The outer fixture releases it and observes completion.
                    #[allow(clippy::zombie_processes)]
                    fixture_command("descendant", &started, &finished, &release)
                        .spawn()
                        .unwrap();
                    wait_for_path(&started);
                }
                "group-outer" => {
                    let mut adapter = fixture_command("adapter", &started, &finished, &release);
                    assert!(readiness_status(&mut adapter).unwrap().success());
                }
                "descendant" => {
                    fs::write(&started, b"started").unwrap();
                    wait_for_path(&release);
                    fs::write(&finished, b"finished").unwrap();
                }
                "malformed-outer" => {
                    let marker = finished;
                    let mut child = fixture_command("must-not-start", &started, &marker, &release);
                    let error = readiness_status(&mut child).unwrap_err();
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                    assert!(!marker.exists());
                }
                "must-not-start" => {
                    fs::write(finished, b"started unexpectedly").unwrap();
                }
                _ => panic!("unknown readiness fixture mode"),
            }
        }

        #[test]
        fn readiness_descendants_do_not_retain_scheduler_leases() {
            let paths = FixturePaths::new("lease");
            let lease = File::create(&paths.lease).unwrap();
            lease.try_lock().unwrap();
            let lease_fd = lease.as_raw_fd();
            let mut fixture = paths.command("outer");
            let mut inherited_fds = inherited_lease_fds_from_env().unwrap();
            assert!(!inherited_fds.contains(&lease_fd));
            inherited_fds.push(lease_fd);
            fixture.env(
                LEASE_FDS_ENV,
                inherited_fds
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
            // SAFETY: the callback only clears CLOEXEC on the fixture-owned
            // duplicate of an already-open descriptor.
            unsafe {
                fixture.pre_exec(move || {
                    let flags = libc::fcntl(lease_fd, libc::F_GETFD);
                    if flags == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(lease_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut fixture = fixture.spawn().unwrap();
            drop(lease);
            assert!(fixture.wait().unwrap().success());
            paths.remove();
        }

        #[test]
        fn readiness_descendants_leave_the_enclosing_process_group() {
            let paths = FixturePaths::new("process-group");
            let mut fixture = paths.command("group-outer");
            isolate_process_group(&mut fixture);
            let mut fixture = fixture.spawn().unwrap();
            let outer_group = fixture.id();
            assert!(fixture.wait().unwrap().success());

            let outer_group_survived = process_group_exists(outer_group).unwrap();
            fs::write(&paths.release, b"release").unwrap();
            wait_for_path(&paths.finished);
            paths.remove();

            assert!(
                !outer_group_survived,
                "readiness descendant retained the enclosing process group"
            );
        }

        #[test]
        fn malformed_lease_inventory_fails_before_readiness_spawn() {
            let paths = FixturePaths::new("malformed");
            let status = paths
                .command("malformed-outer")
                .env(LEASE_FDS_ENV, "3,,4")
                .status()
                .unwrap();
            assert!(status.success());
            assert!(!paths.finished.exists());
            paths.remove();
        }

        struct FixturePaths {
            root: std::path::PathBuf,
            lease: std::path::PathBuf,
            started: std::path::PathBuf,
            finished: std::path::PathBuf,
            release: std::path::PathBuf,
        }

        impl FixturePaths {
            fn new(label: &str) -> Self {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "kio-scheduler-unix-readiness-{label}-{}-{nonce}",
                    std::process::id()
                ));
                fs::create_dir_all(&root).unwrap();
                Self {
                    lease: root.join("lease"),
                    started: root.join("started"),
                    finished: root.join("finished"),
                    release: root.join("release"),
                    root,
                }
            }

            fn command(&self, mode: &str) -> Command {
                let mut command =
                    fixture_command(mode, &self.started, &self.finished, &self.release);
                command.env(LEASE, &self.lease);
                command
            }

            fn remove(&self) {
                fs::remove_dir_all(&self.root).unwrap();
            }
        }

        fn fixture_command(
            mode: &str,
            started: &std::path::Path,
            finished: &std::path::Path,
            release: &std::path::Path,
        ) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("--exact")
                .arg("process_supervisor::unix::tests::readiness_fixture")
                .env(MODE, mode)
                .env(STARTED, started)
                .env(FINISHED, finished)
                .env(RELEASE, release);
            command
        }

        fn path_from_env(name: &str) -> std::path::PathBuf {
            std::env::var_os(name)
                .map(std::path::PathBuf::from)
                .unwrap()
        }

        fn wait_for_path(path: &std::path::Path) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !path.exists() {
                assert!(Instant::now() < deadline, "fixture did not make progress");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(all(not(unix), not(windows)))]
mod fallback {
    use std::io;
    use std::process::{Command, ExitStatus, Output};

    pub(crate) fn status(command: &mut Command, _stdin_was_open: bool) -> io::Result<ExitStatus> {
        command.status()
    }

    pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
        command.output()
    }

    pub(crate) fn readiness_status(command: &mut Command) -> io::Result<ExitStatus> {
        command.status()
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, ExitStatus, Output, Stdio};
    use std::thread;
    use std::time::Duration;

    use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_SUSPENDED, OpenThread, ResumeThread,
        THREAD_SUSPEND_RESUME,
    };
    use windows::core::HRESULT;

    const JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);

    pub(crate) fn status(command: &mut Command, stdin_was_open: bool) -> io::Result<ExitStatus> {
        if !stdin_was_open {
            command.stdin(Stdio::null());
        }
        spawn(command)?.wait()
    }

    pub(crate) fn status_cancellable(
        command: &mut Command,
        stdin_was_open: bool,
        cancellation: &std::path::Path,
    ) -> io::Result<(ExitStatus, bool)> {
        if !stdin_was_open {
            command.stdin(Stdio::null());
        }
        spawn(command)?.wait_cancellable(cancellation)
    }

    pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
        // Manual spawning bypasses `Command::output`'s default pipe selection.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        spawn(command)?.wait_with_output()
    }

    pub(crate) fn readiness_status(command: &mut Command) -> io::Result<ExitStatus> {
        // This is reserved for readiness probes that may start a long-lived
        // daemon. Letting that daemon inherit a Kio Job would prevent the
        // enclosing scheduled command from ever draining its process tree.
        command.creation_flags(CREATE_BREAKAWAY_FROM_JOB.0);
        command.status()
    }

    fn spawn(command: &mut Command) -> io::Result<JobChild> {
        let job = Job::new()?;
        // A running child could create a descendant before assignment. This
        // internal adapter owns the creation-flags field so assignment always
        // happens before any child code runs; retaining the console group also
        // preserves Windows' normal control-event delivery.
        command.creation_flags(CREATE_SUSPENDED.0);
        let mut child = command.spawn()?;
        let assigned = job
            .assign(&child)
            .and_then(|()| resume_suspended_process(&child));
        if let Err(error) = assigned {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(JobChild { child, job })
    }

    struct JobChild {
        child: Child,
        job: Job,
    }

    impl JobChild {
        fn wait(mut self) -> io::Result<ExitStatus> {
            let status = self.child.wait()?;
            self.job.wait_until_empty()?;
            Ok(status)
        }

        fn wait_with_output(self) -> io::Result<Output> {
            let Self { child, job } = self;
            let output = child.wait_with_output()?;
            job.wait_until_empty()?;
            Ok(output)
        }

        fn wait_cancellable(
            mut self,
            cancellation: &std::path::Path,
        ) -> io::Result<(ExitStatus, bool)> {
            let mut status = None;
            let mut cancelled = false;
            loop {
                if status.is_none() {
                    status = self.child.try_wait()?;
                }
                if !cancelled && cancellation.try_exists()? {
                    self.job.terminate()?;
                    cancelled = true;
                }
                if status.is_some() && self.job.is_empty()? {
                    return Ok((status.expect("leader status was observed"), cancelled));
                }
                thread::sleep(JOB_POLL_INTERVAL);
            }
        }
    }

    struct Job {
        handle: OwnedHandle,
    }

    impl Job {
        fn new() -> io::Result<Self> {
            let handle = OwnedHandle(
                // SAFETY: a null security descriptor and unnamed job carry no borrowed data.
                unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?,
            );
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // The unnamed handle is not inherited. If this supervisor dies,
            // closing its last handle terminates the tree instead of letting a
            // released scheduler permit overlap live compiler descendants.
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            // SAFETY: `limits` has the exact layout and lifetime required by this information class.
            unsafe {
                SetInformationJobObject(
                    handle.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of_val(&limits)
                        .try_into()
                        .expect("job limit information fits in a u32"),
                )
            }
            .map_err(io::Error::other)?;
            Ok(Self { handle })
        }

        fn assign(&self, child: &Child) -> io::Result<()> {
            let process = HANDLE(child.as_raw_handle());
            // Windows 8+ nests this job below any compatible host-runner job.
            // An incompatible UI-restricted host job fails here instead of
            // silently running a process tree outside scheduler ownership.
            // SAFETY: the child process handle stays live for the call and the job is owned by self.
            unsafe { AssignProcessToJobObject(self.handle.0, process) }.map_err(io::Error::other)
        }

        fn wait_until_empty(&self) -> io::Result<()> {
            // Leader exit does not release admission while a compiler
            // descendant remains. Query the authoritative active count because
            // Windows documents job completion-port notifications as lossy.
            loop {
                if self.is_empty()? {
                    return Ok(());
                }
                thread::sleep(JOB_POLL_INTERVAL);
            }
        }

        fn is_empty(&self) -> io::Result<bool> {
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            // SAFETY: `accounting` has the exact layout and lifetime required by this class.
            unsafe {
                QueryInformationJobObject(
                    Some(self.handle.0),
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut accounting).cast(),
                    std::mem::size_of_val(&accounting)
                        .try_into()
                        .expect("job accounting information fits in a u32"),
                    None,
                )
            }
            .map_err(io::Error::other)?;
            Ok(accounting.ActiveProcesses == 0)
        }

        fn terminate(&self) -> io::Result<()> {
            // SAFETY: this Job handle stays live and exclusively owns the
            // supervised process tree for the duration of the call.
            unsafe { TerminateJobObject(self.handle.0, 1) }.map_err(io::Error::other)
        }
    }

    fn resume_suspended_process(child: &Child) -> io::Result<()> {
        // `std::process::Child` exposes its process handle but not its primary
        // thread handle. The process has never run, so its sole thread can be
        // found and resumed without racing thread creation.
        let snapshot = OwnedHandle(
            // SAFETY: the snapshot has no borrowed inputs and is closed by OwnedHandle.
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(io::Error::other)?,
        );
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>()
                .try_into()
                .expect("thread entry size fits in a u32"),
            ..Default::default()
        };
        // SAFETY: `entry` is initialized with the required size and remains writable for the call.
        unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(io::Error::other)?;

        loop {
            if entry.th32OwnerProcessID == child.id() {
                let thread_handle = OwnedHandle(
                    // SAFETY: the enumerated thread id remains valid or OpenThread returns an error.
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                        .map_err(io::Error::other)?,
                );
                // SAFETY: `thread_handle` grants THREAD_SUSPEND_RESUME and stays live for the call.
                let previous_count = unsafe { ResumeThread(thread_handle.0) };
                return match previous_count {
                    1 => Ok(()),
                    u32::MAX => Err(io::Error::last_os_error()),
                    count => Err(io::Error::other(format!(
                        "new child had unexpected suspend count {count}"
                    ))),
                };
            }

            // SAFETY: `entry` remains initialized and writable across enumeration calls.
            match unsafe { Thread32Next(snapshot.0, &mut entry) } {
                Ok(()) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
                    return Err(io::Error::other(
                        "could not find the suspended child thread",
                    ));
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: this wrapper uniquely owns the live handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            Job, output, readiness_status, resume_suspended_process, status, status_cancellable,
        };
        use std::fs;
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        use std::thread;
        use std::time::{Duration, Instant};
        use windows::Win32::System::Threading::CREATE_SUSPENDED;

        const MODE: &str = "KIO_TEST_WINDOWS_SUPERVISOR_MODE";
        const STARTED: &str = "KIO_TEST_WINDOWS_SUPERVISOR_STARTED";
        const FINISHED: &str = "KIO_TEST_WINDOWS_SUPERVISOR_FINISHED";
        const RELEASE: &str = "KIO_TEST_WINDOWS_SUPERVISOR_RELEASE";

        #[test]
        fn exit_status_and_captured_output_are_preserved() {
            let mut exits = Command::new("cmd");
            exits.args(["/D", "/C", "exit /B 17"]);
            assert_eq!(status(&mut exits, true).unwrap().code(), Some(17));

            let mut prints = Command::new("cmd");
            prints.args(["/D", "/C", "echo scheduler-output"]);
            let captured = output(&mut prints).unwrap();
            assert!(captured.status.success());
            assert_eq!(captured.stdout, b"scheduler-output\r\n");
            assert!(captured.stderr.is_empty());
        }

        #[test]
        #[ignore]
        fn process_tree_fixture() {
            let mode = std::env::var(MODE).unwrap();
            let started = std::env::var_os(STARTED).map(std::path::PathBuf::from);
            let finished = std::env::var_os(FINISHED).map(std::path::PathBuf::from);
            let release = std::env::var_os(RELEASE).map(std::path::PathBuf::from);
            match mode.as_str() {
                "root" => {
                    fixture_process()
                        .env(MODE, "descendant")
                        .env(STARTED, started.unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.unwrap())
                        .spawn()
                        .unwrap();
                }
                "descendant" => {
                    fs::write(started.unwrap(), b"started").unwrap();
                    thread::sleep(Duration::from_millis(1_500));
                    fs::write(finished.unwrap(), b"finished").unwrap();
                }
                "kill-root" => {
                    fixture_process()
                        .env(MODE, "kill-descendant")
                        .env(STARTED, started.unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.unwrap())
                        .spawn()
                        .unwrap();
                }
                "kill-descendant" => {
                    fs::write(started.unwrap(), b"started").unwrap();
                    wait_for_path(release.as_ref().unwrap());
                    fs::write(finished.unwrap(), b"finished").unwrap();
                }
                "breakaway-root" => {
                    let mut adapter = fixture_process();
                    adapter
                        .env(MODE, "breakaway-adapter")
                        .env(STARTED, started.unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.unwrap());
                    assert!(readiness_status(&mut adapter).unwrap().success());
                }
                "breakaway-adapter" => {
                    fixture_process()
                        .env(MODE, "breakaway-descendant")
                        .env(STARTED, started.as_ref().unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.unwrap())
                        .spawn()
                        .unwrap();
                    wait_for_path(started.as_ref().unwrap());
                }
                "breakaway-descendant" => {
                    fs::write(started.unwrap(), b"started").unwrap();
                    wait_for_path(release.as_ref().unwrap());
                    fs::write(finished.unwrap(), b"finished").unwrap();
                }
                "breakaway-supervisor" => {
                    let mut root = fixture_process();
                    root.env(MODE, "breakaway-root")
                        .env(STARTED, started.unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.as_ref().unwrap());
                    assert!(status(&mut root, true).unwrap().success());
                    fs::write(release.unwrap(), b"release").unwrap();
                }
                "supervisor" => {
                    let mut root = fixture_process();
                    root.env(MODE, "kill-root")
                        .env(STARTED, started.unwrap())
                        .env(FINISHED, finished.unwrap())
                        .env(RELEASE, release.unwrap());
                    assert!(status(&mut root, true).unwrap().success());
                }
                "nested-supervisor" => {
                    let mut child = fixture_process();
                    child
                        .env(MODE, "nested-child")
                        .env(FINISHED, finished.unwrap());
                    assert!(status(&mut child, true).unwrap().success());
                }
                "nested-child" => {
                    fs::write(finished.unwrap(), b"finished").unwrap();
                }
                _ => panic!("unknown fixture mode"),
            }
        }

        #[test]
        fn waits_for_the_complete_process_tree() {
            let paths = FixturePaths::new("wait");
            let mut root = paths.command("root");
            let started_at = Instant::now();
            assert!(status(&mut root, true).unwrap().success());
            assert!(started_at.elapsed() >= Duration::from_millis(1_300));
            assert_eq!(fs::read(&paths.finished).unwrap(), b"finished");
            paths.remove();
        }

        #[test]
        fn cancellation_terminates_and_drains_the_complete_job() {
            let paths = FixturePaths::new("cancel");
            let started = paths.started.clone();
            let cancel = paths.cancel.clone();
            let canceller = thread::spawn(move || {
                wait_for_path(&started);
                fs::write(cancel, b"cancel").unwrap();
            });
            let mut root = paths.command("kill-root");
            let (status, cancelled) = status_cancellable(&mut root, true, &paths.cancel).unwrap();
            canceller.join().unwrap();
            assert!(status.success());
            assert!(cancelled);
            assert!(!paths.finished.exists());
            paths.remove();
        }

        #[test]
        fn supervisor_death_terminates_the_complete_process_tree() {
            let paths = FixturePaths::new("death");
            let mut supervisor = paths.command("supervisor").spawn().unwrap();
            wait_for_path(&paths.started);
            supervisor.kill().unwrap();
            supervisor.wait().unwrap();
            fs::write(&paths.release, b"release").unwrap();
            thread::sleep(Duration::from_millis(500));
            assert!(!paths.finished.exists());
            paths.remove();
        }

        #[test]
        fn readiness_descendants_break_away_from_the_owned_job() {
            let paths = FixturePaths::new("breakaway");
            let mut supervisor = paths.command("breakaway-supervisor").spawn().unwrap();
            if !path_appears(&paths.finished) {
                let _ = supervisor.kill();
                let _ = supervisor.wait();
                paths.remove();
                panic!("readiness descendant did not outlive the enclosing Job");
            }
            assert!(supervisor.wait().unwrap().success());
            assert!(paths.started.exists());
            assert!(paths.release.exists());
            paths.remove();
        }

        #[test]
        fn scheduler_job_nests_inside_an_outer_job() {
            let paths = FixturePaths::new("nested-job");
            let outer_job = Job::new().unwrap();
            let mut command = paths.command("nested-supervisor");
            // Assign the fixture supervisor before it can create its own child,
            // making the scheduler-owned Job in `nested-supervisor` genuinely
            // nested below this explicit kill-on-close outer Job.
            command.creation_flags(CREATE_SUSPENDED.0);
            let mut supervisor = command.spawn().unwrap();
            outer_job.assign(&supervisor).unwrap();
            resume_suspended_process(&supervisor).unwrap();

            assert!(supervisor.wait().unwrap().success());
            outer_job.wait_until_empty().unwrap();
            assert_eq!(fs::read(&paths.finished).unwrap(), b"finished");
            paths.remove();
        }

        struct FixturePaths {
            started: std::path::PathBuf,
            finished: std::path::PathBuf,
            release: std::path::PathBuf,
            cancel: std::path::PathBuf,
        }

        impl FixturePaths {
            fn new(label: &str) -> Self {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let unique = format!(
                    "kio-scheduler-windows-{label}-{}-{nonce}",
                    std::process::id()
                );
                let root = std::env::temp_dir().join(unique);
                fs::create_dir_all(&root).unwrap();
                Self {
                    started: root.join("started"),
                    finished: root.join("finished"),
                    release: root.join("release"),
                    cancel: root.join("cancel"),
                }
            }

            fn command(&self, mode: &str) -> Command {
                let mut command = fixture_process();
                command
                    .env(MODE, mode)
                    .env(STARTED, &self.started)
                    .env(FINISHED, &self.finished)
                    .env(RELEASE, &self.release);
                command
            }

            fn remove(&self) {
                fs::remove_dir_all(self.started.parent().unwrap()).unwrap();
            }
        }

        fn wait_for_path(path: &std::path::Path) {
            assert!(path_appears(path), "fixture did not start");
        }

        fn fixture_process() -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--ignored")
                .arg("--exact")
                .arg("process_supervisor::windows::tests::process_tree_fixture");
            command
        }

        fn path_appears(path: &std::path::Path) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if path.exists() {
                    return true;
                }
                thread::sleep(Duration::from_millis(10));
            }
            path.exists()
        }
    }
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) use fallback::{output, readiness_status, status};
#[cfg(unix)]
pub(crate) use unix::{
    isolate_process_group_with_foreground_tty, output, process_group_exists, readiness_status,
    signal_process_group, status,
};
#[cfg(windows)]
pub(crate) use windows::{output, readiness_status, status, status_cancellable};
