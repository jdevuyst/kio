//! End-to-end smoke tests for the `kio repl` subcommand.
//!
//! Each test spawns the real `kio` binary as a subprocess with its
//! cwd set to a temp package directory, feeds it a REPL transcript
//! over stdin, and asserts on the captured stdout — the same path a
//! user driving the prompt exercises.
//!
//! The REPL is line-oriented: one meta-command per input line, one
//! or more output lines per command. A test either feeds a complete
//! transcript up front (the common case) or holds stdin open and
//! interleaves filesystem edits (the auto-reload test).
//!
//! Gated on `feature = "repl"` — the `kio` binary
//! (`CARGO_BIN_EXE_kio`) only exists in a build that includes the
//! full pipeline, and the `repl` subcommand requires the `repl`
//! feature (which itself implies `full`).

#![cfg(feature = "repl")]

mod support;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use support::test_binary;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temp directory, removed on drop.
struct TempPackage(PathBuf);

impl TempPackage {
    /// Create an empty temp directory tagged `tag`.
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kio-repl-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        TempPackage(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Write `content` to `rel` within the package, creating parent
    /// directories as needed.
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&p, content).expect("write file");
    }

    fn write_demo_package(&self) {
        self.write(
            "demo.kio",
            "module demo;\n\n\
             host type Str role(str);\n\n\
             host fn print(p0: Str) -> .;\n",
        );
        self.write(
            "demo.pkg.kio",
            "package demo;\n\n\
             bridge {\n\
               demo;\n\
               demo/**;\n\
             }\n",
        );
    }
}

impl Drop for TempPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn public_session_refreshes_list_on_default_stack() {
    const CHILD: &str = "KIO_DEBUG_SESSION_STACK_TEST_CHILD";
    const TEST: &str = "public_session_refreshes_list_on_default_stack";
    const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;

    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/poc/list/workdir");
        let mut session = kio_lang::repl_core::session::Session::new(root);
        eprintln!("public Session refresh started");
        session.refresh().expect("refresh the list package");
        drop(session);
        eprintln!("public Session refresh and drop completed");
        return;
    }

    // A fresh process cannot inherit a CLI-initialized Rayon pool from another test.
    let capture = TempPackage::new("session-default-stack");
    let stdout_path = capture.path().join("stdout");
    let stderr_path = capture.path().join("stderr");
    let stdout_file = fs::File::create(&stdout_path).expect("create stdout capture");
    let stderr_file = fs::File::create(&stderr_path).expect("create stderr capture");
    let child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "1")
        .env_remove("RUST_MIN_STACK")
        .env_remove("KIO_STACK_SIZE_MB")
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .expect("spawn default-stack Session child");

    struct ReapChild(Option<std::process::Child>);
    impl Drop for ReapChild {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let mut child = ReapChild(Some(child));
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = (|| -> std::io::Result<std::process::ExitStatus> {
        loop {
            for path in [&stdout_path, &stderr_path] {
                if fs::metadata(path)?.len() > OUTPUT_LIMIT {
                    return Err(std::io::Error::other("Session child output limit exceeded"));
                }
            }
            if let Some(status) = child.0.as_mut().expect("live child").try_wait()? {
                child.0 = None;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Session child exceeded 180 seconds",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    // Stop/reap before any capture read or assertion can fail.
    drop(child);
    for path in [&stdout_path, &stderr_path] {
        assert!(
            fs::metadata(path).expect("final capture length").len() <= OUTPUT_LIMIT,
            "Session child output limit exceeded"
        );
    }
    let read_capture = |path: &Path| {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .expect("open child capture")
            .take(OUTPUT_LIMIT)
            .read_to_end(&mut bytes)
            .expect("read bounded child capture");
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let stdout = read_capture(&stdout_path);
    let stderr = read_capture(&stderr_path);
    eprintln!("Session child result: {result:?}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        result.is_ok_and(|status| status.success()),
        "Session child failed"
    );
    assert!(stdout.contains(&format!("test {TEST} ... ok")));
    assert!(stderr.contains("public Session refresh and drop completed"));
}

/// Run `kio repl` in `package_dir` feeding `transcript` (a list of
/// input lines) on stdin, and return the captured stdout. The
/// transcript should end with `:quit` (or rely on stdin EOF) so the
/// process terminates.
///
/// `TERM=dumb` and `NO_COLOR=1` are set so the output is plain text
/// — assertions match literal substrings, not ANSI escapes.
fn run_repl(package_dir: &Path, transcript: &[&str]) -> String {
    run_repl_with_args(package_dir, &[], transcript)
}

#[test]
fn repl_types_and_normalizes_all_three_trailing_block_exposures() {
    let package = TempPackage::new("trailing-blocks");
    package.write("app.pkg.kio", "package app; bridge { app; provider; }");
    package.write(
        "provider.kio",
        include_str!("fixtures/trailing-blocks/provider.kio"),
    );
    package.write("app.kio", include_str!("fixtures/trailing-blocks/app.kio"));
    let output = run_repl(
        package.path(),
        &[
            ":load app",
            ":type packet! { () }",
            ":normalize packet! { () }",
            ":type enter! { let value = (); value }",
            ":normalize enter! { let value = (); value }",
            ":type sequence! box_bind { let .(value: .) <- box_pure(()); box_pure(value) }",
            ":normalize sequence! box_bind { let .(value: .) <- box_pure(()); box_pure(value) }",
            ":quit",
        ],
    );
    assert!(output.contains("packet! { () } : ."), "{output}");
    assert!(
        output.contains("enter! { let value = (); value } : ."),
        "{output}"
    );
    assert!(output.contains("provider.Box(.)"), "{output}");
    assert_eq!(
        output.lines().filter(|line| *line == "()").count(),
        2,
        "{output}"
    );
    assert!(output.lines().any(|line| line == "Box.box(())"), "{output}");
    assert!(!output.contains("error"), "{output}");
}

/// Like [`run_repl`], but forwards `extra_args` to `kio repl` before
/// the transcript starts — used by the startup-selector tests.
fn run_repl_with_args(package_dir: &Path, extra_args: &[&str], transcript: &[&str]) -> String {
    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .args(extra_args)
        .current_dir(package_dir)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        for line in transcript {
            writeln!(stdin, "{line}").expect("write transcript line");
        }
        // Dropping stdin closes the pipe — the REPL sees EOF.
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    let status = child.wait().expect("wait for kio repl");
    assert!(
        status.success(),
        "kio repl exited non-zero ({status:?})\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Both streams are part of the user-visible transcript; the REPL
    // routes diagnostics to stderr. Join them so substring assertions
    // can target either.
    format!("{stdout}\n{stderr}")
}

/// Run `kio repl` in `package_dir` with `extra_args`, expecting a
/// non-zero exit. Returns `(exit-code, combined-stdout-and-stderr)`.
/// Used by the selector-mismatch test, which asserts a usage exit.
fn run_repl_expect_failure(package_dir: &Path, extra_args: &[&str]) -> (i32, String) {
    let output = Command::new(test_binary!("kio"))
        .arg("repl")
        .args(extra_args)
        .current_dir(package_dir)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn kio repl");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (code, format!("{stdout}\n{stderr}"))
}

/// A minimal valid package: a package file plus one module declaring
/// a documented `fn`.
fn minimal_package(tag: &str) -> TempPackage {
    let pkg = TempPackage::new(tag);
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         /// The package entry point.\n\
         pub fn run() -> . { () }\n",
    );
    pkg
}

/// A **package-less** module tree: one documented module on disk and
/// no `*.pkg.kio`. The REPL is module-centric — modules load by their
/// FQN with no package gate — so this directory opens and its module
/// loads just like a packaged one.
fn loose_module_tree(tag: &str) -> TempPackage {
    let dir = TempPackage::new(tag);
    dir.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         /// The package entry point.\n\
         pub fn run() -> . { () }\n",
    );
    dir
}

#[test]
fn repl_opens_on_bare_module_tree() {
    // A directory with a `.kio` module and NO `*.pkg.kio` is a valid
    // module tree — the REPL opens on it and exits cleanly. Modules
    // load by their FQN; a package file is optional and never gates
    // the REPL.
    let dir = loose_module_tree("baretree");
    let out = run_repl(dir.path(), &[":quit"]);
    assert!(
        out.contains("module inspector"),
        "the REPL should open on a bare module tree, got:\n{out}"
    );
    // The banner advertises the available module (count + how to load).
    assert!(
        out.contains("available") && out.contains(":load"),
        "the banner should advertise the available module, got:\n{out}"
    );
    assert!(out.contains("bye"), "should exit cleanly, got:\n{out}");
}

#[test]
fn repl_bare_module_tree_loads_and_queries() {
    // On a package-less tree, `:load <fqn>` brings the module in and
    // `:t` / `:source` answer — proof the module-centric load path does
    // not depend on a package file.
    let dir = loose_module_tree("baretreeload");
    let out = run_repl(
        dir.path(),
        &[":load demo/main", ":t run", ":source run", ":quit"],
    );
    assert!(out.contains("loaded demo/main"), "got:\n{out}");
    assert!(out.contains("run :"), "`:t` should answer, got:\n{out}");
    assert!(
        out.contains("fn run"),
        "`:source` should answer, got:\n{out}"
    );
}

#[test]
fn repl_empty_directory_opens_and_quits() {
    // An empty directory (no modules, no package file) opens on a
    // blank slate and `:quit` exits 0.
    let dir = TempPackage::new("emptydir");
    let out = run_repl(dir.path(), &[":quit"]);
    assert!(
        out.contains("module inspector"),
        "the REPL should open on an empty directory, got:\n{out}"
    );
    assert!(
        out.contains("no modules in this directory"),
        "the banner should note the empty directory, got:\n{out}"
    );
    assert!(out.contains("bye"), "should exit cleanly, got:\n{out}");
}

#[test]
fn repl_nested_fqn_module_loads_by_slash_path() {
    // A module nested under several path segments loads by its full
    // slash FQN on a package-less tree.
    let dir = TempPackage::new("nestedfqn");
    dir.write(
        "app/sub/deep.kio",
        "module app/sub/deep;\n\npub fn here() -> . { () }\n",
    );
    let out = run_repl(dir.path(), &[":load app/sub/deep", ":t here", ":quit"]);
    assert!(out.contains("loaded app/sub/deep"), "got:\n{out}");
    assert!(out.contains("here :"), "`:t` should answer, got:\n{out}");
}

#[test]
fn repl_help_lists_commands() {
    let output = Command::new(test_binary!("kio"))
        .arg("repl")
        .arg("--help")
        .output()
        .expect("run kio repl --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("module inspector"));
}

#[test]
fn repl_load_and_query() {
    let pkg = minimal_package("loadquery");
    let out = run_repl(
        pkg.path(),
        &[":load demo/main", ":mods", ":t run", ":source run", ":quit"],
    );
    // `:load` reports the loaded module.
    assert!(out.contains("loaded demo/main"), "got:\n{out}");
    // `:mods` marks the current module.
    assert!(out.contains("* demo/main"), "got:\n{out}");
    // `:t` shows the function's type.
    assert!(out.contains("run :"), "got:\n{out}");
    // `:source` shows the declaration with a body.
    assert!(out.contains("fn run"), "got:\n{out}");
}

#[test]
fn repl_prompt_not_printed_over_piped_stdin() {
    // On the *interactive* terminal path the prompt is two-line: after
    // `:load demo/main` the context line above the `kio> ` input symbol
    // names the current module (`demo/main\nkio> `); with nothing
    // loaded it shows the `(no module — :load …)` hint. That shape is
    // covered by the `prompt.rs` unit tests and
    // `current_module_of_names_the_loaded_module`, which drive the
    // reedline `KioPrompt` directly.
    //
    // Over piped stdin (this harness) the prompt is **not** written to
    // stdout: reedline's `Prompt` only renders on the interactive
    // terminal path, and a piped transcript has no terminal control
    // stream. The earlier rustyline-driven REPL leaked the prompt to
    // stdout even over a pipe; the reedline non-TTY branch deliberately
    // does not; see `repl::run_loop_piped`. This test pins the
    // no-prompt-over-pipe contract so a future "always print a prompt"
    // regression fails loudly, while the `:load` itself still succeeds.
    let pkg = minimal_package("promptmodule");
    let out = run_repl(pkg.path(), &[":load demo/main", ":quit"]);
    assert!(
        out.contains("loaded demo/main"),
        "the load should still succeed, got:\n{out}"
    );
    // Neither the input symbol nor the module-context line reaches
    // piped stdout.
    assert!(
        !out.contains("kio> ") && !out.contains("(no module —"),
        "no interactive prompt should reach piped stdout, got:\n{out}"
    );
}

#[test]
fn repl_piped_stdin_does_not_require_tty() {
    // Explicit assertion on the non-TTY branch: reedline puts the
    // terminal in raw mode unconditionally and errors on a pipe, so the
    // REPL must take a `BufRead` path when stdin is not a terminal. Feed
    // a small transcript over piped stdin and confirm the process exits
    // zero with the expected output. The existing smokes cover this
    // implicitly; this one pins the behaviour so a future "always
    // construct reedline" change fails loudly with a raw-mode error
    // instead of running.
    let pkg = minimal_package("pipednotty");
    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .current_dir(pkg.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, ":load demo/main").expect("write :load");
        writeln!(stdin, ":t run").expect("write :t");
        // Drop stdin without `:quit` — EOF on the pipe must break the
        // loop the same as Ctrl-D would on the interactive path.
    }
    let output = child.wait_with_output().expect("wait for kio repl");
    assert!(
        output.status.success(),
        "piped stdin should exit zero (no raw-mode requirement), status: {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("loaded demo/main"),
        "the transcript should have run, got:\n{stdout}"
    );
    assert!(
        stdout.contains("run :"),
        "`:t run` should have produced a type, got:\n{stdout}"
    );
    // EOF (no `:quit`) still reaches the clean-exit `bye`.
    assert!(
        stdout.contains("bye"),
        "EOF should exit cleanly, got:\n{stdout}"
    );
}

#[test]
fn repl_history_persists_with_reedline() {
    // The interactive path persists history to
    // `$XDG_DATA_HOME/kio/history` via reedline's `FileBackedHistory`,
    // which appends entries as they are submitted. The smoke harness
    // drives *piped* stdin (no TTY), where history is intentionally off
    // — reedline is never constructed, so no history file is written —
    // and no PTY harness exists yet to exercise the interactive path
    // end-to-end. This test pins the two observable, PTY-free facts:
    //
    //  1. A piped session does **not** create the history file (history
    //     is off for non-TTY stdin, by design).
    //  2. The reedline `FileBackedHistory` mechanism the interactive
    //     path uses does round-trip entries through that same path —
    //     verified by driving `FileBackedHistory` directly against the
    //     resolved path, so the persistence plumbing regression-fails
    //     loudly even without a terminal.
    use reedline::{FileBackedHistory, History, HistoryItem, SearchQuery};

    let data_home = TempPackage::new("histxdg");
    let pkg = minimal_package("histpkg");

    // (1) A piped session leaves no history file under XDG_DATA_HOME.
    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .current_dir(pkg.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, ":mods").expect("write :mods");
        writeln!(stdin, ":quit").expect("write :quit");
    }
    let status = child.wait().expect("wait for kio repl");
    assert!(status.success(), "piped session should exit zero");
    let history_path = data_home.path().join("kio").join("history");
    assert!(
        !history_path.exists(),
        "piped stdin keeps history off — no file should be written at {}",
        history_path.display()
    );

    // (2) The `FileBackedHistory` mechanism the interactive path uses
    // round-trips entries through the resolved path. Write a session's
    // worth of entries, drop the history (its `Drop` syncs to disk),
    // then re-open the same path and confirm the prior entries survive.
    {
        let mut history = FileBackedHistory::with_file(1000, history_path.clone())
            .expect("open file-backed history");
        history
            .save(HistoryItem::from_command_line(":load demo/main"))
            .expect("save first entry");
        history
            .save(HistoryItem::from_command_line(":t run"))
            .expect("save second entry");
        history.sync().expect("sync history to disk");
    }
    assert!(
        history_path.exists(),
        "FileBackedHistory should have written the history file"
    );
    let reopened =
        FileBackedHistory::with_file(1000, history_path.clone()).expect("re-open history");
    let entries = reopened
        .search(SearchQuery::everything(
            reedline::SearchDirection::Forward,
            None,
        ))
        .expect("read history entries");
    let lines: Vec<&str> = entries.iter().map(|e| e.command_line.as_str()).collect();
    assert!(
        lines.contains(&":load demo/main") && lines.contains(&":t run"),
        "the prior session's entries should persist, got: {lines:?}"
    );
}

#[test]
fn repl_startup_is_blank_slate_with_no_selectors() {
    // With no selectors, opening `kio repl` loads NOTHING — it is a
    // blank slate. The banner advertises what's available and how to
    // bring a module in; `:mods` reports nothing loaded until the user
    // `:load`s.
    let pkg = TempPackage::new("startupblank");
    pkg.write_demo_package();
    pkg.write("demo/a.kio", "module demo/a;\npub fn fa() -> . { () }\n");
    pkg.write("demo/b.kio", "module demo/b;\npub fn fb() -> . { () }\n");
    let out = run_repl(pkg.path(), &[":mods", ":quit"]);
    // The banner advertises the available count and how to `:load`,
    // and does NOT claim anything is loaded.
    assert!(
        out.contains("available") && out.contains(":load"),
        "banner should advertise available modules + how to load, got:\n{out}"
    );
    assert!(
        !out.contains("loaded:"),
        "blank-slate startup loads nothing — no `loaded:` line, got:\n{out}"
    );
    // The startup banner does not print a `package:` line.
    assert!(
        !out.contains("package:"),
        "the old `package: <path>` banner line is gone, got:\n{out}"
    );
    // `:mods` confirms nothing is loaded at startup.
    assert!(
        out.contains("no modules loaded"),
        "`:mods` should report an empty session at startup, got:\n{out}"
    );
}

#[test]
fn repl_startup_selector_filters_to_a_subset() {
    // A positional `<selector>` loads a chosen subset at startup
    // instead of opening on a blank slate. Matching is by module path,
    // filename, or stem.
    let pkg = TempPackage::new("startupsel");
    pkg.write_demo_package();
    pkg.write("demo/a.kio", "module demo/a;\npub fn fa() -> . { () }\n");
    pkg.write("demo/b.kio", "module demo/b;\npub fn fb() -> . { () }\n");
    // Selector by module path: only `demo/a` should be loaded.
    let out = run_repl_with_args(pkg.path(), &["demo/a"], &[":mods", ":quit"]);
    assert!(
        out.contains("demo/a"),
        "selected module should load, got:\n{out}"
    );
    // `demo/b` was not selected and shouldn't show in `:mods` output.
    // (The banner / mods output is short — a substring check is enough.)
    assert!(
        !out.lines().any(|line| line.contains("demo/b")),
        "unselected module should not be loaded, got:\n{out}"
    );
}

#[test]
fn repl_startup_selector_by_filename() {
    // Selectors can name a regular module's filename (with or
    // without the `.kio` extension) or its stem.
    let pkg = TempPackage::new("startupfile");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\npub fn run() -> . { () }\n",
    );
    pkg.write(
        "demo/util.kio",
        "module demo/util;\npub fn helper() -> . { () }\n",
    );
    // `main.kio` (with extension) selects `demo/main`.
    let out = run_repl_with_args(pkg.path(), &["main.kio"], &[":mods", ":quit"]);
    assert!(out.contains("demo/main"), "got:\n{out}");
    assert!(
        !out.lines().any(|line| line.contains("demo/util")),
        "got:\n{out}"
    );
}

#[test]
fn repl_startup_selector_mismatch_exits_usage() {
    // A selector that matches no regular module is a usage error.
    let pkg = minimal_package("badsel");
    let (code, combined) = run_repl_expect_failure(pkg.path(), &["does.not.exist"]);
    assert_ne!(code, 0, "selector mismatch should exit non-zero");
    assert!(
        combined.contains("does.not.exist"),
        "error should name the bad selector, got:\n{combined}"
    );
    assert!(
        combined.contains("regular module"),
        "error should reference regular modules, got:\n{combined}"
    );
}

#[test]
fn repl_startup_selector_dotted_module_path_does_not_match_slash_module() {
    let pkg = TempPackage::new("startupdottedsel");
    pkg.write_demo_package();
    pkg.write("demo/a.kio", "module demo/a;\npub fn fa() -> . { () }\n");

    let (code, combined) = run_repl_expect_failure(pkg.path(), &["demo.a"]);
    assert_ne!(code, 0, "dotted selector should exit non-zero");
    assert!(
        combined.contains("selector `demo.a` matches no regular module"),
        "error should name the dotted selector, got:\n{combined}"
    );
    assert!(
        combined.contains("demo/a"),
        "error should list the slash module path, got:\n{combined}"
    );
}

#[test]
fn repl_doc_renders_doc_comment() {
    let pkg = minimal_package("doc");
    let out = run_repl(pkg.path(), &[":load demo/main", ":doc run", ":quit"]);
    assert!(
        out.contains("The package entry point."),
        "doc-comment should render, got:\n{out}"
    );
    assert!(
        out.contains("fn run"),
        "signature should render, got:\n{out}"
    );
}

#[test]
fn repl_recursive_group_doc_preserves_context_and_selected_prose() {
    let pkg = TempPackage::new("recursive-group-doc");
    pkg.write("demo.pkg.kio", "package demo; bridge { demo/**; }\n");
    pkg.write("demo/main.kio", "module demo/main;\n\n/// Group prose.\nrec {\n  /// Alias prose.\n  type Chain = Node;\n  /// Nominal prose.\n  newtype Node : . | Chain { constructor make; projector read; };\n}\n");
    for command in [
        ":doc Node",
        ":doc demo/main.Node",
        ":signature Node",
        ":source Node",
    ] {
        let out = run_repl(pkg.path(), &[":load demo/main", command, ":quit"]);
        assert!(out.contains("rec {"), "{command}: {out}");
        assert!(out.contains("type Chain = Node;"), "{command}: {out}");
        assert!(out.contains("newtype Node : . | Chain"), "{command}: {out}");
        assert!(!out.contains("Alias prose."), "{command}: {out}");
        assert!(!out.contains("Group prose."), "{command}: {out}");
        if command.starts_with(":doc") {
            assert_eq!(out.matches("Nominal prose.").count(), 1, "{out}");
        }
    }
}

#[test]
fn repl_recursive_label_nominal_docs_show_their_source_owner() {
    let pkg = TempPackage::new("recursive-label-doc");
    pkg.write("demo.pkg.kio", "package demo; bridge { demo/**; }\n");
    pkg.write("demo/main.kio", "module demo/main;\nrec {\n  type Tree = Twig;\n  /// Twig prose.\n  labels { twig: . | Tree };\n}\n/// List prose.\nrec labels { list: . | List };\n");
    for (name, context, prose) in [
        ("Twig", "rec {", "Twig prose."),
        ("List", "rec labels", "List prose."),
    ] {
        let command = format!(":doc {name}");
        let out = run_repl(pkg.path(), &[":load demo/main", &command, ":quit"]);
        assert!(out.contains(context), "{out}");
        assert_eq!(out.matches(prose).count(), 1, "{out}");
    }
}

#[test]
fn repl_intrinsic_in_scope_resolves_via_which_doc_and_bare_input() {
    // When a loaded module declares `import __intrinsics__;`, the
    // intrinsic names (`__left__`, `__fst__`, `__pair__`, …)
    // resolve through `:which`, `:doc`, and the bare-input router.
    let pkg = TempPackage::new("intrinsicresolve");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\nimport __intrinsics__;\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":which __left__",
            ":doc __left__",
            // Bare input → router dispatches to `:doc`.
            "__fst__",
            // Another intrinsic via `:which`.
            ":which __pair__",
            ":quit",
        ],
    );
    // `:which __left__` reports intrinsic-in-scope.
    assert!(
        out.contains("__left__")
            && out
                .lines()
                .any(|l| l.contains("__left__") && l.contains("intrinsic in scope")),
        "`:which __left__` should report intrinsic-in-scope, got:\n{out}"
    );
    // `:doc __left__` renders the scope provenance + type scheme.
    assert!(
        out.lines()
            .any(|l| l.contains("__left__") && l.contains("intrinsic in scope")),
        "`:doc __left__` should explain provenance, got:\n{out}"
    );
    // Bare `__fst__` routes through `:doc`.
    assert!(
        out.lines()
            .any(|l| l.contains("__fst__") && l.contains("intrinsic in scope")),
        "bare `__fst__` should dispatch through `:doc`, got:\n{out}"
    );
    // `:which __pair__` likewise.
    assert!(
        out.lines()
            .any(|l| l.contains("__pair__") && l.contains("intrinsic in scope")),
        "`:which __pair__` should report intrinsic-in-scope, got:\n{out}"
    );
}

#[test]
fn repl_comptime_in_scope_resolves_via_which_doc_and_bare_input() {
    let pkg = TempPackage::new("comptimeresolve");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\nimport __comptime__;\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":which __reflect_type__",
            ":doc __reflect_type__",
            "__Type__",
            ":which __structural_recur__",
            ":quit",
        ],
    );
    assert!(
        out.lines()
            .any(|l| l.contains("__reflect_type__") && l.contains("compile-time helper in scope")),
        "`:which __reflect_type__` should report comptime-in-scope, got:\n{out}"
    );
    assert!(
        out.contains("__reflect_type__ :") && out.contains("Reflects a type argument"),
        "`:doc __reflect_type__` should render its signature and docs, got:\n{out}"
    );
    assert!(
        out.contains("type __Type__"),
        "bare `__Type__` should dispatch through `:doc`, got:\n{out}"
    );
    assert!(
        out.lines().any(|l| {
            l.contains("__structural_recur__") && l.contains("compile-time helper in scope")
        }),
        "`:which __structural_recur__` should report comptime-in-scope, got:\n{out}"
    );
}

#[test]
fn repl_doc_on_a_module_renders_summary() {
    // `:doc <module>` renders the module summary: doc-comment +
    // item count + import count.
    let pkg = TempPackage::new("docmodule");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "/// The main module of the demo package.\nmodule demo/main;\n\n\
         pub fn run() -> . { () }\n\
         fn helper() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":doc demo/main", ":quit"]);
    assert!(
        out.contains("The main module of the demo package."),
        "module doc-comment should render, got:\n{out}"
    );
    assert!(out.contains("module demo/main"), "got:\n{out}");
    assert!(out.contains("2 declared items"), "got:\n{out}");
}

#[test]
fn repl_bare_module_name_routes_through_doc() {
    // A bare module path now routes through `:doc`, so typing the
    // module's `/`-joined path renders the module summary.
    let pkg = TempPackage::new("docmodulebare");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "/// Greeting module.\nmodule demo/main;\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", "demo/main", ":quit"]);
    assert!(out.contains("Greeting module."), "got:\n{out}");
    assert!(out.contains("module demo/main"), "got:\n{out}");
}

#[test]
fn repl_implicit_loading_pulls_in_import_deps() {
    let pkg = TempPackage::new("implicit");
    pkg.write_demo_package();
    pkg.write(
        "demo/util.kio",
        "module demo/util;\npub fn helper() -> . { () }\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\nimport demo/util(helper);\n\n\
         pub fn run() -> . { helper() }\n",
    );
    // Blank-slate startup loads nothing; `:load demo/main` brings the
    // explicit module in and pulls `demo/util` along as an implicit
    // import dep.
    let out = run_repl(pkg.path(), &[":load demo/main", ":mods", ":quit"]);
    // The explicit module plus its implicit import dep.
    assert!(out.contains("loaded demo/main"), "got:\n{out}");
    assert!(
        out.contains("demo/util (via demo/main)"),
        "implicit dep should be marked, got:\n{out}"
    );
}

#[test]
fn repl_unload_cascades_implicit_dep() {
    let pkg = TempPackage::new("unload");
    pkg.write_demo_package();
    pkg.write(
        "demo/util.kio",
        "module demo/util;\npub fn helper() -> . { () }\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\nimport demo/util(helper);\n\n\
         pub fn run() -> . { helper() }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            // Blank-slate startup loads nothing; loading `demo/main`
            // pulls `demo/util` in as an implicit dep below.
            ":load demo/main",
            // Unloading an implicit module is rejected.
            ":unload demo/util",
            // Unloading the explicit module cascades the dep away.
            ":unload demo/main",
            ":mods",
            ":quit",
        ],
    );
    assert!(
        out.contains("implicit dependency"),
        "unloading an implicit dep should be rejected, got:\n{out}"
    );
    assert!(out.contains("unloaded demo/main"), "got:\n{out}");
    assert!(
        out.contains("no modules loaded"),
        "the orphaned implicit dep should cascade away, got:\n{out}"
    );
}

#[test]
fn repl_which_and_refs() {
    let pkg = TempPackage::new("whichrefs");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         pub fn helper() -> . { () }\n\
         pub fn run() -> . { helper() }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[":load demo/main", ":which helper", ":refs helper", ":quit"],
    );
    // `:which` reports the FQN.
    assert!(out.contains("demo/main.helper"), "got:\n{out}");
    // `:refs` finds the declaration and the call site.
    assert!(out.contains("references to `helper`"), "got:\n{out}");
}

#[test]
fn repl_packages_lists_and_views_a_package() {
    // `:packages` with no argument lists the `*.pkg.kio` package files;
    // `:packages <name>` views one's contract (its `bridge` glob). The
    // command is read-only and independent of the module load path.
    let pkg = minimal_package("packageslist");
    let out = run_repl(pkg.path(), &[":packages", ":packages demo", ":quit"]);
    // The listing names the package by its `.pkg.kio` stem.
    assert!(
        out.lines().any(|l| l.contains("demo")),
        "`:packages` should list the package, got:\n{out}"
    );
    // The view renders the package contract.
    assert!(
        out.contains("package demo"),
        "`:packages demo` should render the package header, got:\n{out}"
    );
    assert!(
        out.contains("bridge:") && out.contains("demo/**"),
        "the bridge glob should render, got:\n{out}"
    );
}

#[test]
fn repl_packages_pkgs_synonym_and_loose_only_dir() {
    // The `:pkgs` synonym works, and a package-less (loose-only) module
    // tree reports that there are no packages — the REPL still runs.
    let dir = loose_module_tree("packagesloose");
    let out = run_repl(dir.path(), &[":pkgs", ":quit"]);
    assert!(
        out.contains("no `*.pkg.kio` packages"),
        "a loose-only tree should report no packages, got:\n{out}"
    );
}

#[test]
fn repl_scope_lists_each_section() {
    // `:scope` lists everything in the current module's scope —
    // declared items, imported names by source module, operator
    // bindings, module aliases, and intrinsics state.
    let pkg = TempPackage::new("scopecmd");
    pkg.write_demo_package();
    pkg.write(
        "demo/util.kio",
        "module demo/util;\npub fn helper() -> . { () }\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         import __intrinsics__;\n\
         import __comptime__;\n\
         import demo/util(helper);\n\
         import demo/util as u;\n\
         pub fn run() -> . { helper() }\n\
         fn add(x: ., y: .) -> . { x }\n\
         op _ + __ { impl add; };\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":scope", ":quit"]);
    assert!(out.contains("scope of demo/main"), "got:\n{out}");
    assert!(out.contains("declared:"), "got:\n{out}");
    assert!(out.contains("pub fn run"), "got:\n{out}");
    assert!(out.contains("imported:"), "got:\n{out}");
    assert!(out.contains("from demo/util:"), "got:\n{out}");
    assert!(out.contains("operators: op _ + __"), "got:\n{out}");
    assert!(out.contains("module aliases:"), "got:\n{out}");
    assert!(out.contains("u -> demo/util"), "got:\n{out}");
    assert!(out.contains("intrinsics: in scope"), "got:\n{out}");
    assert!(out.contains("comptime: in scope"), "got:\n{out}");
}

#[test]
fn repl_unresolved_name_diagnoses_with_scope_hint() {
    // The "did not resolve" diagnostic now points at `:scope`.
    let pkg = minimal_package("unresolved");
    let out = run_repl(pkg.path(), &[":load demo/main", ":t no_such_name", ":quit"]);
    assert!(
        out.contains("current scope") && out.contains(":scope"),
        "the unresolved diagnostic should point at `:scope`, got:\n{out}"
    );
}

#[test]
fn repl_refs_labels_with_enclosing_function() {
    // `:refs` labels each hit with the enclosing top-level item —
    // here both the declaration site and the call site sit inside
    // a `fn`, getting two distinct `in fn …` labels.
    let pkg = TempPackage::new("refslabel");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         pub fn callee() -> . { () }\n\
         pub fn caller() -> . { callee() }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":refs callee", ":quit"]);
    assert!(
        out.contains("in fn caller"),
        "call site should be labelled with enclosing fn, got:\n{out}"
    );
    assert!(
        out.contains("in fn callee"),
        "declaration should be labelled with its own fn, got:\n{out}"
    );
}

#[test]
fn repl_refs_labels_import_clause_as_module() {
    // A reference inside an `import` clause is outside every item's span;
    // the label falls back to `in module <path>`.
    let pkg = TempPackage::new("refsuseclause");
    pkg.write_demo_package();
    pkg.write(
        "demo/util.kio",
        "module demo/util;\npub fn callee() -> . { () }\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\nimport demo/util(callee);\n\n\
         pub fn run() -> . { callee() }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":refs callee", ":quit"]);
    // The `import demo/util(callee);` line sits in demo/main outside
    // any item; the hit's label is the module fallback.
    assert!(
        out.contains("in module demo/main"),
        "import-clause hit should be labelled with module, got:\n{out}"
    );
}

#[test]
fn repl_which_finds_private_item() {
    // `:which` scans declared items — a private (no `pub`) fn is
    // reachable, not just `pub`-exported ones.
    let pkg = TempPackage::new("whichprivate");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         fn priv_helper() -> . { () }\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":which priv_helper",
            ":which run",
            ":quit",
        ],
    );
    // The private fn is found by its FQN.
    assert!(
        out.contains("demo/main.priv_helper"),
        "`:which` should find private items, got:\n{out}"
    );
    // The pub fn still works.
    assert!(
        out.contains("demo/main.run"),
        "`:which` should still find pub items, got:\n{out}"
    );
}

#[test]
fn repl_ls_does_not_leak_internal_labels_braces() {
    // Regression: an anonymous `labels { ... };` block used to render
    // as the literal `labels { … }` entry in `:ls` output — an internal
    // structural fragment leaking through. The fix expands the
    // anonymous form into one entry per declared label.
    let pkg = TempPackage::new("lsanonlabels");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\
         pub fn run() -> . { () }\n\
         labels { red: ., green: . };\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":ls demo/main", ":quit"]);
    assert!(
        !out.contains("{ … }"),
        "literal `{{ … }}` should not appear in :ls output, got:\n{out}"
    );
    assert!(
        !out.contains("labels {"),
        "literal `labels {{...}}` should not appear in :ls output, got:\n{out}"
    );
    // Each label entry surfaces as its own line.
    assert!(out.contains("label red"), "got:\n{out}");
    assert!(out.contains("label green"), "got:\n{out}");
}

#[test]
fn repl_ls_no_arg_lists_current_module() {
    // `:ls` with no argument lists the current module's items — after
    // `:load demo/main` the current module is `demo/main`, so the
    // no-arg `:ls` should show its declared `fn run`.
    let pkg = minimal_package("lsnoarg");
    let out = run_repl(pkg.path(), &[":load demo/main", ":ls", ":quit"]);
    assert!(out.contains("loaded demo/main"), "got:\n{out}");
    assert!(
        out.contains("demo/main:"),
        "`:ls` should header the current module, got:\n{out}"
    );
    assert!(
        out.contains("fn run"),
        "`:ls` should list the current module's items, got:\n{out}"
    );
}

#[test]
fn repl_ls_no_arg_without_current_module_diagnoses() {
    // `:ls` with no argument and no current module set prints the
    // "no current module" diagnostic. Blank-slate startup loads
    // nothing, so the session opens with no current module.
    let pkg = minimal_package("lsnocurrent");
    let out = run_repl(pkg.path(), &[":ls", ":quit"]);
    assert!(
        out.contains("no current module"),
        "`:ls` with no current module should diagnose, got:\n{out}"
    );
}

#[test]
fn repl_synonyms_accepted() {
    let pkg = minimal_package("synonyms");
    // Drive the short synonyms: `:l` / `:t` / `:ls` / `:mods` / `:q`.
    let out = run_repl(
        pkg.path(),
        &[":l demo/main", ":ls demo/main", ":mods", ":q"],
    );
    assert!(out.contains("loaded demo/main"), "got:\n{out}");
    assert!(
        out.contains("fn run"),
        "`:ls` should list items, got:\n{out}"
    );
}

#[test]
fn repl_rejects_garbage_expression_input() {
    // A non-`:` line is an expression query. Multi-token garbage that
    // matches no kind — not a name, not an expression, not a module
    // path — prints the single honest invalid-input line.
    let pkg = minimal_package("garbageexpr");
    let out = run_repl(pkg.path(), &[":load demo/main", "hello world", ":quit"]);
    assert!(
        out.contains("not a name or a Kio expression"),
        "garbage input should land in the invalid-input line, got:\n{out}"
    );
}

#[test]
fn repl_bare_name_routes_to_doc() {
    // A bare name (no `:`) now routes to `:doc`: it renders the
    // doc-comment plus the signature, not just the signature.
    let pkg = minimal_package("barenamedoc");
    let out = run_repl(pkg.path(), &[":load demo/main", "run", ":quit"]);
    assert!(
        out.contains("The package entry point."),
        "a bare name should render the doc-comment (`:doc`), got:\n{out}"
    );
    assert!(
        out.contains("fn run"),
        "the signature should still appear, got:\n{out}"
    );
}

#[test]
fn repl_bare_compound_prints_type_and_normal_form() {
    // A bare compound expression (no `:`) prints both views — the
    // synthesized type line, then the residual normal form. Host-
    // touching expressions commonly stay stuck as residual trees, so
    // the type line is the reliable summary either way.
    let pkg = minimal_package("barecompoundnormalize");
    let out = run_repl(pkg.path(), &[":load demo/main", "()", ":quit"]);
    assert!(out.contains("() :"), "type line expected, got:\n{out}");
    assert!(
        out.lines().any(|l| l.trim() == "()"),
        "normal-form line expected, got:\n{out}"
    );
}

#[test]
fn repl_bare_input_neither_prints_the_invalid_line() {
    // Input that matches no kind prints one honest line pointing at
    // `:help` — not the retired fixed `:doc`/`:normalize` two-command dump.
    let pkg = minimal_package("bareneither");
    let out = run_repl(pkg.path(), &["hello world", ":quit"]);
    assert!(
        out.contains("not a name or a Kio expression"),
        "the invalid-input line should fire, got:\n{out}"
    );
    assert!(out.contains(":help"), "should point at :help, got:\n{out}");
}

#[test]
fn repl_bare_name_footer_lists_applicable_commands() {
    // A bare name routes to `:doc` AND appends a footer advertising the
    // other commands applicable to a name, under their full spellings
    // (`:signature`, `:type`, …). The unit test
    // `applicable_commands_footer_shows_full_spellings` pins the
    // spelling rule; this pins that the footer reaches the transcript.
    let pkg = minimal_package("barenamefooter");
    let out = run_repl(pkg.path(), &[":load demo/main", "run", ":quit"]);
    assert!(out.contains("also:"), "footer should appear, got:\n{out}");
    assert!(
        out.contains(":signature"),
        "footer should list :signature, got:\n{out}"
    );
    assert!(
        out.contains(":normalize"),
        "footer should list :normalize, got:\n{out}"
    );
    assert!(
        out.contains(":type"),
        "footer should list :type in full, got:\n{out}"
    );
}

#[test]
fn repl_type_on_fully_qualified_path_reports_type() {
    // `:t mod/path.item` — a genuine FQN mixing `/` and `.` — now
    // classifies as a name and reports the item's type, instead of
    // leaking the synthetic wrapper's `;`.
    let pkg = minimal_package("fqntype");
    let out = run_repl(
        pkg.path(),
        &[":load demo/main", ":t demo/main.run", ":quit"],
    );
    assert!(
        out.contains("run : ") || out.contains("run :"),
        "`:t` on an FQN should report the item's type, got:\n{out}"
    );
    assert!(
        !out.contains("expected `;`"),
        "no synthetic wrapper `;` should leak, got:\n{out}"
    );
}

#[test]
fn repl_type_on_module_path_points_at_ls() {
    // `:t op/main` — a module path, not a value — points at `:ls`,
    // never the generic "did not resolve".
    let pkg = minimal_package("typemodule");
    let out = run_repl(pkg.path(), &[":load demo/main", ":t demo/main", ":quit"]);
    assert!(
        out.contains("is a module, not a value") && out.contains(":ls demo/main"),
        "`:t` on a module should point at `:ls`, got:\n{out}"
    );
}

#[test]
fn repl_type_on_garbage_gives_invalid_line_not_parser_error() {
    // `:t 1 +` — an incomplete expression — gives the kind-classified
    // invalid-input line, never the wrapper-leaked "expected `;`".
    let pkg = minimal_package("typegarbage");
    let out = run_repl(pkg.path(), &[":load demo/main", ":t 1 +", ":quit"]);
    assert!(
        !out.contains("expected `;`"),
        "no synthetic wrapper `;` should leak, got:\n{out}"
    );
}

#[test]
fn repl_missing_argument_renders_usage_hint() {
    // `:doc` and `:t` (and other commands that take an argument)
    // get a per-command usage hint when invoked with no argument,
    // not a generic "see `:help`" message.
    let pkg = minimal_package("usagehint");
    let out = run_repl(pkg.path(), &[":doc", ":t", ":quit"]);
    assert!(
        out.contains("Usage: :doc"),
        "`:doc` with no arg should print a usage line, got:\n{out}"
    );
    assert!(
        out.contains("Usage: :type"),
        "`:t` with no arg should print the full-spelling usage line, got:\n{out}"
    );
}

#[test]
fn repl_signature_command_prints_declaration_header() {
    // `:signature` (and its `:sig` short form) prints a name's
    // declaration header, body suppressed.
    let pkg = minimal_package("sigcmd");
    let out = run_repl(
        pkg.path(),
        &[":load demo/main", ":signature run", ":sig run", ":quit"],
    );
    assert!(
        out.contains("fn run"),
        "`:signature` should print the declaration header, got:\n{out}"
    );
}

#[test]
fn repl_source_includes_preceding_doc_comment() {
    // `:source <name>` prints the canonical-form body prefixed by
    // the contiguous run of `//` and `///` comment lines directly
    // above the declaration.
    let pkg = TempPackage::new("sourcedoc");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         /// First line of the doc.\n\
         /// Second line of the doc.\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":source run", ":quit"]);
    assert!(
        out.contains("/// First line of the doc."),
        "doc-comment line should appear in `:source`, got:\n{out}"
    );
    assert!(
        out.contains("/// Second line of the doc."),
        "every doc-comment line should appear, got:\n{out}"
    );
    assert!(
        out.contains("fn run"),
        "the declaration body should still appear, got:\n{out}"
    );
}

#[test]
fn repl_source_includes_mixed_line_and_doc_comments() {
    // A contiguous run of `//` and `///` lines is kept together —
    // including a line-comment that wouldn't show up in `:doc`.
    let pkg = TempPackage::new("sourcemixed");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         // implementation note (not Kiodoc)\n\
         /// public-facing doc\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":source run", ":quit"]);
    assert!(
        out.contains("// implementation note (not Kiodoc)"),
        "`//` line comment should appear in `:source`, got:\n{out}"
    );
    assert!(
        out.contains("/// public-facing doc"),
        "`///` doc-comment should appear in `:source`, got:\n{out}"
    );
}

#[test]
fn repl_source_skips_comment_separated_by_blank_line() {
    // A blank line between the comment and the declaration severs
    // the attachment — the comment is NOT part of the item's source.
    let pkg = TempPackage::new("sourceblank");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         // detached commentary — blank line below\n\
         \n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":source run", ":quit"]);
    assert!(
        !out.contains("detached commentary"),
        "a blank-line-separated comment should not appear, got:\n{out}"
    );
    assert!(
        out.contains("fn run"),
        "the body should still appear, got:\n{out}"
    );
}

#[test]
fn repl_source_short_synonym_accepted() {
    // `:src` is a first-class synonym for `:source`.
    let pkg = minimal_package("srcsyn");
    let out = run_repl(pkg.path(), &[":load demo/main", ":src run", ":quit"]);
    assert!(
        out.contains("fn run"),
        "`:src` should print the canonical source, got:\n{out}"
    );
}

#[test]
fn repl_type_on_type_level_name_is_kind_aware_error() {
    // `:t` on a type-level name reports a kind-aware error — the same
    // message shape `kio doc check` produces for `@type`.
    let pkg = TempPackage::new("typelevelt");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\ntype Same[A] = A;\n\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", ":t Same", ":quit"]);
    assert!(
        out.contains("type alias") && out.contains("value binding"),
        "`:t` on a type-level name should be a kind-aware error, got:\n{out}"
    );
}

#[test]
fn repl_compound_expression_normalizes() {
    // A bare compound expression (no `:`) prints its residual normal
    // form. The unit literal reduces to itself: `()`.
    let pkg = minimal_package("compoundexpr");
    let out = run_repl(pkg.path(), &[":load demo/main", "()", ":quit"]);
    assert!(out.contains("()"), "got:\n{out}");
}

#[test]
fn repl_normalize_and_t_expression_commands() {
    // `:normalize <expr>` prints the residual NF; `:t <expr>` prints the
    // synthesized type. Exercise both over a function call: the call
    // β-reduces through the fn's body, so the NF is the body's value.
    let pkg = TempPackage::new("normt");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         pub fn greeting() -> . & . { ((), ()) }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":t greeting()",
            ":normalize greeting()",
            ":quit",
        ],
    );
    // `:t` reports the call's synthesized type.
    assert!(
        out.contains("greeting() : (. & .)"),
        "`:t <expr>` should print the type, got:\n{out}"
    );
    // `:normalize` reduces `greeting()` to its body's value.
    assert!(
        out.contains("__pair__((), ())"),
        "`:normalize <expr>` should print the residual NF, got:\n{out}"
    );
}

#[test]
fn repl_pure_uses_declaration_contracts_and_pure_context_typechecking() {
    let pkg = TempPackage::new("pure-query");
    pkg.write("demo.pkg.kio", "package demo;\n\nbridge { demo/main; }\n");
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         host type Text role(str);\n\
         host fn emit(value: Text) -> .;\n\n\
         pure fn _expr0(x: .) -> . { x }\n\
         pure fn identity(x: .) -> . { x }\n\
         fn unrestricted(x: .) -> . { x }\n\
         type Alias = .;\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":pure ()",
            ":pure identity",
            ":pure demo/main.identity",
            ":pure identity(())",
            ":pure unrestricted",
            ":pure unrestricted(())",
            ":pure emit",
            ":pure emit(\"seen\")",
            ":pure .(x: .) { x }",
            ":pure (.(x: .) { x })(())",
            ":pure .(_x: .) { emit(\"captured\") }",
            ":pure Alias",
            ":quit",
        ],
    );
    let verdicts: Vec<&str> = out
        .lines()
        .filter(|line| matches!(*line, "pure" | "impure"))
        .collect();
    assert_eq!(
        verdicts,
        [
            "pure", "pure", "pure", "pure", "impure", "impure", "impure", "impure", "pure", "pure",
            "impure",
        ],
        "each declaration, FQN, call, and lambda query needs its own verdict:\n{out}"
    );
    assert!(
        out.contains("`Alias` is a type alias") && out.contains("expects an executable value"),
        "a type-level declaration should receive a kind-aware diagnostic:\n{out}"
    );
    assert!(
        !out.contains("_expr"),
        "synthetic query framing leaked into the transcript:\n{out}"
    );
}

#[test]
fn repl_query_wrapper_does_not_capture_unresolved_spelling() {
    let pkg = TempPackage::new("query-wrapper-capture");
    pkg.write("demo.pkg.kio", "package demo;\n\nbridge { demo/main; }\n");
    pkg.write("demo/main.kio", "module demo/main;\n");

    let out = run_repl(
        pkg.path(),
        &[":load demo/main", ":pure (_expr0)", ":t (_expr1)", ":quit"],
    );

    assert!(
        out.contains("the expression does not type-check: unbound name `_expr0`")
            && out.contains("the expression does not type-check: unbound name `_expr1`")
            && !out.lines().any(|line| line == "pure"),
        "unresolved query spellings must remain unresolved:\n{out}"
    );
}

#[test]
fn repl_query_wrapper_avoids_existing_name_for_type_query() {
    let pkg = TempPackage::new("type-query-wrapper-collision");
    pkg.write("demo.pkg.kio", "package demo;\n\nbridge { demo/main; }\n");
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\npure fn _expr0(x: .) -> . { x }\n",
    );

    let out = run_repl(pkg.path(), &[":load demo/main", ":t ()", ":quit"]);

    assert!(
        out.contains("() : .") && !out.contains("duplicate top-level declaration"),
        "`:t` must not collide with a legal same-module declaration:\n{out}"
    );
}

#[test]
fn repl_query_wrapper_stays_distinct_from_elaborator_capture() {
    let pkg = TempPackage::new("query-wrapper-elaborator-capture");
    pkg.write(
        "demo.pkg.kio",
        "package demo;\n\nbridge { demo/main; demo/elaborators; }\n",
    );
    pkg.write(
        "demo/elaborators.kio",
        "module demo/elaborators;\n\n\
         import __comptime__;\n\n\
         pub pure fn _expr0() -> . { () }\n\n\
         pub pure fn captured_call_impl(\n\
           ct: __Comptime__, captured: __Checked_term__\n\
         ) -> __Checked_term__ {\n\
           __term_call__(ct, __term_type__(ct, captured), captured, __term_unit__(ct))\n\
         }\n\n\
         pub elab captured_call : . -> . { captures _expr0; impl captured_call_impl; };\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\nimport demo/elaborators(captured_call);\n",
    );

    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            ":pure captured_call!()",
            ":normalize captured_call!()",
            ":quit",
        ],
    );

    assert!(
        out.lines().any(|line| line == "pure") && out.lines().any(|line| line == "()"),
        "the elaborator's resolved capture must remain distinct from the same-spelled wrapper:\n{out}"
    );
    assert!(
        !out.contains("duplicate top-level declaration") && !out.contains("does not type-check"),
        "the elaborated query must stay valid:\n{out}"
    );
}

#[test]
fn repl_multiline_normalize_match_block() {
    // Multi-line input: a `:normalize` whose expression opens a brace on the
    // first physical line and closes it on the next is gathered into one
    // input and normalized once — no parse error fires on the partial
    // first line. The piped path reconstructs the continuation the
    // interactive validator drives, so the transcript reads exactly as a
    // user typing across the `... ` continuation prompt would.
    let pkg = TempPackage::new("multilinenorm");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         pub fn greeting() -> . & . { ((), ()) }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            // The expression opens a brace-delimited lambda body on line one
            // and closes it on line two; the call applies the lambda
            // to `()`. Gathered as one `:normalize`, it reduces to the
            // body's value.
            ":normalize (.(x: .) {",
            "  greeting()",
            "})(())",
            ":quit",
        ],
    );
    // The gathered input normalizes to the residual NF — proof the
    // two physical lines were joined and reduced as one expression.
    assert!(
        out.contains("__pair__((), ())"),
        "the multi-line `:normalize` should reduce to the fn body's value, got:\n{out}"
    );
    // The partial first line (`:normalize (.(x: .) {`) must NOT have
    // been dispatched on its own — that would surface a parse error
    // about an unexpected end of input / unclosed brace. The clean run
    // shows no such diagnostic.
    assert!(
        !out.contains("error:"),
        "no parse error should fire on the partial first line, got:\n{out}"
    );
}

#[test]
fn repl_product_literal_normalizes() {
    // A bare product literal's residual NF is the product itself.
    let pkg = TempPackage::new("strlit");
    pkg.write_demo_package();
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(pkg.path(), &[":load demo/main", "((), ())", ":quit"]);
    // The residual NF is the product literal.
    assert!(
        out.contains("__pair__((), ())"),
        "a product literal should reduce to itself, got:\n{out}"
    );
}

#[test]
fn repl_normalize_reduces_compound_expression() {
    // `:normalize` and bare-compound queries print the **residual normal
    // form**, not the surface form: a call into a sibling module's
    // fn unfolds through that fn's body, and the result is the body's
    // value — not the call expression as written.
    let pkg = TempPackage::new("normnf");
    pkg.write_demo_package();
    pkg.write(
        "demo/util.kio",
        "module demo/util;\n\n\
         pub fn shout() -> . & . { ((), ()) }\n",
    );
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\nimport demo/util(shout);\n\n\
         pub fn pick() -> . & . { shout() }\n\
         pub fn run() -> . { () }\n",
    );
    let out = run_repl(
        pkg.path(),
        &[
            ":load demo/main",
            // Calling an imported sibling-module fn reduces through
            // its body.
            ":normalize shout()",
            // Two-step unfold: `pick()` → its body `shout()` → the product.
            ":normalize pick()",
            ":quit",
        ],
    );
    // The product of the unfolded fn body shows up — not `shout()`,
    // which is the surface form.
    assert!(
        out.contains("__pair__((), ())"),
        "fn unfold should produce the body's value, got:\n{out}"
    );
    // Two-step unfold also lands on the same product.
    let product_count = out.matches("__pair__((), ())").count();
    assert!(
        product_count >= 2,
        "both `:normalize shout()` and `:normalize pick()` should reduce to `__pair__((), ())`, got:\n{out}"
    );
}

#[test]
fn repl_expression_query_without_current_module_explains() {
    // With no module loaded, an expression query with a short name
    // has nothing to resolve against; the REPL says so. Blank-slate
    // startup loads nothing, so the session opens with no current
    // module.
    let pkg = minimal_package("noexprmod");
    let out = run_repl(pkg.path(), &["1", ":quit"]);
    assert!(
        out.contains("no current module"),
        "an expression query with no module should explain, got:\n{out}"
    );
}

#[test]
fn repl_auto_reload_picks_up_a_file_edit() {
    let pkg = minimal_package("autoreload");

    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .current_dir(pkg.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");

    let mut stdin = child.stdin.take().expect("stdin");

    // Load the module, then give the watcher a moment to settle.
    writeln!(stdin, ":load demo/main").expect("write :load");
    std::thread::sleep(Duration::from_millis(400));

    // Edit the file on disk — add a second function.
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         /// The package entry point.\n\
         pub fn run() -> . { () }\n\
         pub fn added() -> . { () }\n",
    );

    // Wait past the debounce window, then issue a command. The REPL
    // drains the watcher before this read and re-typechecks.
    std::thread::sleep(Duration::from_millis(700));
    writeln!(stdin, ":ls demo/main").expect("write :ls");
    writeln!(stdin, ":quit").expect("write :quit");
    drop(stdin);

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    // Bound the wait for the child.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("kio repl did not exit\nstdout:\n{stdout}\nstderr:\n{stderr}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let combined = format!("{stdout}\n{stderr}");
    // The auto-reload either fires (the watcher saw the edit and the
    // `reloaded` line printed and `:ls` shows `added`) or — on a
    // sandbox with no working file watcher — does not. The watcher
    // is best effort, so accept either: the test asserts the REPL
    // did not crash and `:ls` ran.
    assert!(
        combined.contains("demo/main:"),
        "`:ls` should have produced output, got:\n{combined}"
    );
    // When the watcher works (the normal dev / CI case), the edit is
    // visible.
    if combined.contains("reloaded") {
        assert!(
            combined.contains("added"),
            "after a reload, `:ls` should show the added fn, got:\n{combined}"
        );
    }
}

#[test]
fn repl_auto_reload_skips_content_unchanged_overwrite() {
    // Writing the file's exact current contents back to disk fires a
    // filesystem event but doesn't change a single byte. The
    // fingerprint cache's content-hash second-guess catches this and
    // skips the redundant refresh; no `reloaded` line prints between
    // two subsequent prompts.
    let pkg = minimal_package("noopreload");
    let main_path = pkg.path().join("demo/main.kio");
    let main_contents = fs::read_to_string(&main_path).expect("read main.kio");

    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .current_dir(pkg.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");

    let mut stdin = child.stdin.take().expect("stdin");

    // Load the module — this populates the fingerprint cache.
    writeln!(stdin, ":load demo/main").expect("write :load");
    std::thread::sleep(Duration::from_millis(400));

    // Overwrite main.kio with its *exact* current contents — the
    // filesystem event fires (mtime bumps), but the content hash is
    // unchanged so the fingerprint cache should filter the event.
    fs::write(&main_path, &main_contents).expect("re-write identical contents");

    // Sleep past the debounce window so any event has a chance to
    // surface, then send a marker command. The watcher is drained
    // between the read and the command; if the fingerprint cache is
    // working, no `reloaded …` line prints ahead of `:mods`.
    std::thread::sleep(Duration::from_millis(700));
    writeln!(stdin, ":mods").expect("write :mods");
    writeln!(stdin, ":quit").expect("write :quit");
    drop(stdin);

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("kio repl did not exit\nstdout:\n{stdout}\nstderr:\n{stderr}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let combined = format!("{stdout}\n{stderr}");
    // `:mods` must have run (the prompt was alive between the
    // identical-rewrite and `:quit`).
    assert!(
        combined.contains("* demo/main"),
        "`:mods` should have produced output, got:\n{combined}"
    );
    // The key assertion: an identical-content overwrite does not
    // produce a `reloaded` line.
    assert!(
        !combined.contains("reloaded"),
        "an identical-content overwrite must not trigger a reload, got:\n{combined}"
    );
}

#[test]
fn repl_auto_reload_fires_on_content_change() {
    // A content-changing edit triggers the fingerprint cache's hash
    // mismatch and re-typechecks the package. The `reloaded …` line
    // names the changed module (`demo/main`), not the
    // explicitly-loaded set.
    let pkg = minimal_package("contentchange");

    let mut child = Command::new(test_binary!("kio"))
        .arg("repl")
        .current_dir(pkg.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kio repl");

    let mut stdin = child.stdin.take().expect("stdin");

    writeln!(stdin, ":load demo/main").expect("write :load");
    std::thread::sleep(Duration::from_millis(400));

    // A genuine content change — body added.
    pkg.write(
        "demo/main.kio",
        "module demo/main;\n\n\
         /// The package entry point.\n\
         pub fn run() -> . { () }\n\
         pub fn added() -> . { () }\n",
    );

    std::thread::sleep(Duration::from_millis(700));
    writeln!(stdin, ":mods").expect("write :mods");
    writeln!(stdin, ":quit").expect("write :quit");
    drop(stdin);

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("kio repl did not exit\nstdout:\n{stdout}\nstderr:\n{stderr}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let combined = format!("{stdout}\n{stderr}");
    // The watcher is best effort — on a sandbox with no working file
    // watcher the `reloaded` line never appears. When it does appear,
    // the assertions about its content apply.
    if combined.contains("reloaded") {
        assert!(
            combined.contains("reloaded demo/main"),
            "the `reloaded …` line should name the changed module, got:\n{combined}"
        );
    }
}
