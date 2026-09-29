//! End-to-end smoke tests for the `kio lsp` subcommand.
//!
//! Each test spawns the real `kio` binary as a subprocess, drives it
//! through an LSP transcript over stdin/stdout, and asserts on the
//! server's responses and notifications. The framing is bare
//! `Content-Length: N\r\n\r\n<body>` — the LSP wire format — so we
//! exercise the same stdio path a real editor uses.
//!
//! Gated on `feature = "full,lsp"` — the `kio` binary
//! (`CARGO_BIN_EXE_kio`) only exists in builds that include the full
//! pipeline, and the `lsp` subcommand requires the `lsp` feature.

#![cfg(all(feature = "surface", feature = "lsp"))]

mod support;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use support::test_binary;

use serde_json::{Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kio-lsp-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&p, content).expect("write file");
        p
    }

    fn write_pkg_root_package(&self) {
        self.write("pkg.kio", "module pkg;\n");
        self.write(
            "pkg.pkg.kio",
            "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
        );
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A `kio lsp` subprocess with framed stdin / stdout helpers.
///
/// A dedicated background thread drains the child's stdout into an
/// in-memory mpsc channel, decoding LSP frames as they arrive. Tests
/// then `recv` (blocking with a deadline) or `try_recv_for` (bounded
/// wait, returns `None` on quiet stream) against the channel. This
/// keeps debounce / coalescing tests honest — a "stream is quiet"
/// assertion can't be expressed with a synchronous byte-by-byte
/// `read` call.
struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    /// Messages framed off the child's stdout.
    rx: Receiver<Value>,
    /// Counter for client-side request ids.
    next_id: i64,
}

impl LspProcess {
    fn spawn() -> Self {
        let mut command = Command::new(test_binary!("kio"));
        command.stderr(Stdio::inherit());
        Self::spawn_command(&mut command).0
    }

    fn spawn_with_env_and_captured_stderr(envs: &[(&str, &str)]) -> (Self, Receiver<String>) {
        let mut command = Command::new(test_binary!("kio"));
        command.stderr(Stdio::piped());
        for (key, value) in envs {
            command.env(key, value);
        }
        Self::spawn_command(&mut command)
    }

    fn spawn_command(command: &mut Command) -> (Self, Receiver<String>) {
        let mut child = command
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn kio lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel::<Value>();
        thread::spawn(move || drain_stdout(stdout, tx));

        let (stderr_tx, stderr_rx) = mpsc::channel::<String>();
        if let Some(mut stderr) = child.stderr.take() {
            thread::spawn(move || {
                let mut out = String::new();
                let _ = stderr.read_to_string(&mut out);
                let _ = stderr_tx.send(out);
            });
        }

        (
            Self {
                child,
                stdin,
                rx,
                next_id: 1,
            },
            stderr_rx,
        )
    }

    /// Send a framed JSON-RPC message: `Content-Length: N\r\n\r\n<body>`.
    fn send(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).expect("serialize");
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.stdin.write_all(frame.as_bytes()).expect("write");
        self.stdin.flush().expect("flush");
    }

    /// Send a request and return the allocated id.
    fn send_request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        id
    }

    /// Send a notification (no id).
    fn send_notification(&mut self, method: &str, params: Value) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// Block until one framed message arrives on stdout or 15s
    /// elapses. Returns `None` if the drain thread reported the
    /// stdout pipe closed (clean shutdown).
    fn recv(&mut self) -> Option<Value> {
        match self.rx.recv_timeout(Duration::from_secs(15)) {
            Ok(v) => Some(v),
            Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => {
                panic!("LSP recv timed out after 15s with no message");
            }
        }
    }

    /// Wait up to `timeout` for the next message; return `None` if
    /// nothing arrives. Used to assert "the stream is quiet" without
    /// hanging.
    fn try_recv_for(&mut self, timeout: Duration) -> Option<Value> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Read messages until one whose `method` (or `id` for a
    /// response) matches the predicate. Other messages are
    /// discarded.
    fn recv_matching<F>(&mut self, mut want: F) -> Value
    where
        F: FnMut(&Value) -> bool,
    {
        loop {
            let msg = self.recv().expect("LSP server closed stdout unexpectedly");
            if want(&msg) {
                return msg;
            }
        }
    }

    /// Initialize handshake with the given workspace root URI.
    fn initialize(&mut self, root_uri: &str) {
        self.initialize_with_capabilities(root_uri, json!({}));
    }

    fn initialize_with_capabilities(&mut self, root_uri: &str, capabilities: Value) -> Value {
        let id = self.send_request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": capabilities,
            }),
        );
        let resp = self.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
        let result = resp
            .get("result")
            .expect("initialize must return a result, got {resp:?}");
        // Sanity: capabilities.textDocumentSync should be present.
        result
            .get("capabilities")
            .and_then(|c| c.get("textDocumentSync"))
            .expect("server must advertise textDocumentSync");
        let capabilities = result["capabilities"].clone();
        self.send_notification("initialized", json!({}));
        capabilities
    }

    /// `shutdown` + `exit`. Blocks until the child exits.
    fn shutdown(mut self) -> i32 {
        let id = self.send_request("shutdown", json!(null));
        let _ = self.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
        self.send_notification("exit", json!(null));
        // Drop stdin so the server's main_loop sees EOF if it
        // doesn't react to `exit` immediately.
        drop(self.stdin);
        // Wait for the child with a deadline.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return status.code().unwrap_or(-1),
                None => {
                    if Instant::now() > deadline {
                        let _ = self.child.kill();
                        panic!("LSP server did not exit after shutdown/exit within 10s");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

/// Background-thread loop: read LSP frames from `stdout`, parse them
/// as JSON, and send each into `tx`. Exits when the pipe closes
/// (the child exits) or when the receiver is dropped. Panics on
/// malformed frames so test failures point at the wire bug rather
/// than swallowing it.
fn drain_stdout(mut stdout: ChildStdout, tx: std::sync::mpsc::Sender<Value>) {
    let mut buf = [0u8; 1];
    loop {
        // Read the headers byte-by-byte until \r\n\r\n.
        let mut header_buf: Vec<u8> = Vec::new();
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => return, // EOF
                Ok(_) => header_buf.push(buf[0]),
                Err(_) => return,
            }
            if header_buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let headers = String::from_utf8(header_buf).expect("utf-8 headers");
        let mut content_length: Option<usize> = None;
        for line in headers.lines() {
            if let Some(v) = line.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse().expect("Content-Length integer"));
            }
        }
        let n = content_length.expect("Content-Length header");
        let mut body = vec![0u8; n];
        if stdout.read_exact(&mut body).is_err() {
            return;
        }
        let v: Value = serde_json::from_slice(&body).expect("parse JSON-RPC body");
        if tx.send(v).is_err() {
            return;
        }
    }
}

/// Build a `file://` URI for a path.
fn path_to_file_uri(p: &Path) -> String {
    // Canonicalize so the URI matches what the server's
    // `package_collection::walk` produces — on macOS, `/tmp` is a symlink to
    // `/private/tmp`, and the walker resolves it. The fallback to
    // the input path preserves behavior on paths that don't exist
    // yet (some tests build the URI before writing the file).
    let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canonical.to_str().expect("UTF-8 path");
    let mut uri = String::from("file://");
    let normalized;
    let path = if cfg!(windows) {
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(s);
        normalized = stripped.replace('\\', "/");
        uri.push('/');
        normalized.as_str()
    } else {
        s
    };
    for ch in path.chars() {
        match ch {
            ' ' => uri.push_str("%20"),
            _ => uri.push(ch),
        }
    }
    uri
}

/// Pull all diagnostics from a message stream until a
/// `publishDiagnostics` notification for `uri` lands. Returns the
/// `diagnostics` field as JSON.
fn wait_for_diagnostics(lsp: &mut LspProcess, uri: &str) -> Value {
    wait_for_publish(lsp, uri)
        .get("diagnostics")
        .cloned()
        .expect("diagnostics field")
}

/// As [`wait_for_diagnostics`], but returns the full
/// `PublishDiagnosticsParams` JSON (so callers can inspect `version`
/// alongside the diagnostic list).
fn wait_for_publish(lsp: &mut LspProcess, uri: &str) -> Value {
    let msg = lsp.recv_matching(|v| {
        v.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && v.get("params")
                .and_then(|p| p.get("uri"))
                .and_then(Value::as_str)
                == Some(uri)
    });
    msg.get("params").cloned().expect("params field")
}

fn wait_for_publish_version(lsp: &mut LspProcess, uri: &str, version: i64) -> Value {
    loop {
        let publish = wait_for_publish(lsp, uri);
        if publish.get("version").and_then(Value::as_i64) == Some(version) {
            return publish;
        }
    }
}

#[test]
fn lsp_initialize_and_shutdown_cleanly() {
    let dir = TempDir::new("init");
    let mut lsp = LspProcess::spawn();
    let capabilities = lsp.initialize_with_capabilities(&path_to_file_uri(dir.path()), json!({}));
    let exit = lsp.shutdown();
    assert_eq!(exit, 0, "kio lsp must exit 0 on shutdown+exit");
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ",", "{"])
    );
}

#[test]
fn lsp_timing_telemetry_emits_request_analysis_and_publish_lines() {
    let dir = TempDir::new("timing");
    let source = "module pkg/main;\n\npub fn run() -> . { 1 }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let (mut lsp, stderr_rx) =
        LspProcess::spawn_with_env_and_captured_stderr(&[("KIO_DEBUG_TIMING", "lsp")]);
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    assert_eq!(publish.get("version").and_then(Value::as_i64), Some(1));

    let _ = send_hover(&mut lsp, &uri, 2, 21);
    assert_eq!(lsp.shutdown(), 0);

    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("captured LSP stderr");
    assert!(
        stderr.contains("lsp-timing: analysis"),
        "missing analysis timing in stderr: {stderr}"
    );
    assert!(
        stderr.contains("lsp-timing: publish"),
        "missing publish timing in stderr: {stderr}"
    );
    assert!(
        stderr.contains("lsp-timing: request method=textDocument/hover"),
        "missing hover request timing in stderr: {stderr}"
    );
}

#[test]
fn lsp_hover_before_background_analysis_schedules_foreground_work() {
    let dir = TempDir::new("foreground-timing");
    let source = "module pkg/main;\n\npub fn run() -> . { 1 }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let (mut lsp, stderr_rx) =
        LspProcess::spawn_with_env_and_captured_stderr(&[("KIO_DEBUG_TIMING", "lsp")]);
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let _ = send_hover(&mut lsp, &uri, 2, 21);
    let publish = wait_for_publish(&mut lsp, &uri);
    assert_eq!(publish.get("version").and_then(Value::as_i64), Some(1));
    assert_eq!(lsp.shutdown(), 0);

    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("captured LSP stderr");
    assert!(
        stderr.contains("lsp-timing: schedule") && stderr.contains("priority=foreground"),
        "missing foreground schedule timing in stderr: {stderr}"
    );
    assert!(
        stderr.contains("lsp-timing: analysis") && stderr.contains("priority=foreground"),
        "missing foreground analysis timing in stderr: {stderr}"
    );
}

#[test]
fn lsp_reports_type_error_for_open_document() {
    let dir = TempDir::new("type-err");
    // Package with a guaranteed type error: returning an I32
    // literal where the signature says `.`. `kio check` exits 14
    // on this; the LSP must surface the diagnostic.
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { 1 }\n",
    );
    // The package needs a package file so `kio check`'s walker
    // treats `dir` as a package root.
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    // didOpen on the type-error file. The server should respond
    // with a publishDiagnostics carrying a single Error-severity
    // diagnostic.
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": std::fs::read_to_string(&main_path).unwrap(),
            }
        }),
    );

    let diags = wait_for_diagnostics(&mut lsp, &uri);
    let arr = diags.as_array().expect("diagnostics array");
    assert_eq!(arr.len(), 1, "expected one diagnostic, got {diags:?}");
    let d = &arr[0];
    assert_eq!(d.get("source").and_then(Value::as_str), Some("kio"));
    assert_eq!(d.get("severity").and_then(Value::as_i64), Some(1)); // 1 = Error
    // The error spans the `1` literal at byte 36 in the source.
    let range = d.get("range").expect("range");
    let start = range.get("start").expect("start");
    let end = range.get("end").expect("end");
    assert_eq!(start.get("line").and_then(Value::as_i64), Some(2));
    assert_eq!(end.get("line").and_then(Value::as_i64), Some(2));
    // The error message names a type mismatch.
    let msg = d
        .get("message")
        .and_then(Value::as_str)
        .expect("message string");
    assert!(
        msg.contains("()") || msg.to_lowercase().contains("type"),
        "diagnostic message should reference the type mismatch, got: {msg}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_label_payload_context_clears_a_conflicting_factory_argument() {
    let dir = TempDir::new("label-payload-context");
    let source = concat!(
        "module pkg/main;\n",
        "newtype Box[A] : . { pub constructor make; pub projector unpack }\n",
        "fn empty[A]() -> Box(A) { Box.make(()) }\n",
        "labels { first: Box(. -> .) };\n",
        "fn run() -> First { {first = empty(., ())} }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri, "languageId": "kio", "version": 1, "text": source
            }
        }),
    );
    let initial = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = initial["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert_eq!(diagnostics.len(), 1, "{initial:?}");
    assert_eq!(diagnostics[0]["severity"], 1);
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("type mismatch"),
        "{initial:?}"
    );

    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": source.replace("empty(., ())", "empty()") }]
        }),
    );
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared:?}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_fills_computed_input_diagnostic_clears_with_an_outside_annotation() {
    let dir = TempDir::new("fills-computed-input");
    let source = include_str!(
        "../../test-data/goldens/14_type_error/fills_computed_input_requires_determined_type/workdir/main.kio"
    )
    .replace("module main;", "module pkg/main;")
    .replace("fn run()", "pub fn run()");
    let main_path = dir.write("pkg/main.kio", &source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri, "languageId": "kio", "version": 1, "text": source
            }
        }),
    );
    let initial = wait_for_publish_version(&mut lsp, &uri, 1);
    let diagnostics = initial["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert_eq!(diagnostics.len(), 1, "{initial:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic["severity"], 1);
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("value source needs a complete type")
    );
    let start = source.find("boxed()").expect("computed argument");
    let (line, character) = source_position(&source, start);
    let (end_line, end_character) = source_position(&source, start + "boxed()".len());
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": { "line": line, "character": character },
            "end": { "line": end_line, "character": end_character }
        })
    );

    let fixed = source.replace("let result =", "let .(result: .) =");
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": fixed }]
        }),
    );
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(cleared["diagnostics"], json!([]), "{cleared:?}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_publish_diagnostics_groups_multiple_errors_for_one_file() {
    let dir = TempDir::new("multi-diag");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn a() -> . { 1 }\n",
        "pub fn b() -> . { 2 }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    assert_eq!(publish.get("version").and_then(Value::as_i64), Some(1));
    let diagnostics = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("diagnostics array");
    assert_eq!(
        diagnostics.len(),
        2,
        "expected both independent type errors, got {publish:?}"
    );
    for diagnostic in diagnostics {
        assert_eq!(diagnostic.get("severity").and_then(Value::as_i64), Some(1));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(14));
    }
    assert_ne!(
        diagnostics[0].get("range"),
        diagnostics[1].get("range"),
        "independent errors should have distinct source ranges"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_diagnostic_suggestion_produces_quickfix_code_action() {
    let dir = TempDir::new("quickfix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn helper() -> . { () }\n",
        "pub fn run() -> . { hepler() }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostics = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("diagnostics array");
    assert_eq!(diagnostics.len(), 1, "expected one name error: {publish:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    let data = diagnostic.get("data").expect("diagnostic data");
    assert_eq!(
        data.get("fixes")
            .and_then(Value::as_array)
            .and_then(|fixes| fixes.first())
            .and_then(|fix| fix.get("edits"))
            .and_then(Value::as_array)
            .and_then(|edits| edits.first())
            .and_then(|edit| edit.get("replacement"))
            .and_then(Value::as_str),
        Some("helper")
    );

    let action_resp = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        action_resp.get("error").is_none(),
        "codeAction must not error: {action_resp:?}"
    );
    let actions = action_resp
        .get("result")
        .and_then(Value::as_array)
        .expect("codeAction result array");
    let action = actions
        .iter()
        .find(|action| {
            action
                .get("edit")
                .and_then(|edit| workspace_edit_edits_for_uri(edit, &uri))
                .map(|(edits, _)| edits)
                .and_then(|changes| changes.first())
                .and_then(|change| change.get("newText"))
                .and_then(Value::as_str)
                == Some("helper")
        })
        .expect("expected eager replacement quickfix");
    assert_eq!(action.get("kind").and_then(Value::as_str), Some("quickfix"));
    let (changes, version) = action_edits_for_uri(action, &uri);
    assert_eq!(
        version,
        Some(1),
        "quickfix must target the analyzed version"
    );
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].get("newText").and_then(Value::as_str),
        Some("helper")
    );

    assert_eq!(lsp.shutdown(), 0);
}

fn assert_auto_import_reanalysis(lsp: &mut LspProcess, uri: &str, source: &str, edits: &[Value]) {
    let repaired = apply_lsp_text_edits(source, edits);
    send_full_text_change(lsp, uri, 3, &repaired);
    let publish = wait_for_publish_version(lsp, uri, 3);
    let diagnostics = publish["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["severity"].as_i64() != Some(1)),
        "resolved auto-import must parse and resolve after application: {publish:?}\n{repaired}"
    );
}

#[test]
fn lsp_code_action_resolve_auto_import_inserts_use_and_qualifies_call() {
    let dir = TempDir::new("auto-import");
    let clean = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let broken = "module pkg/main;\n\npub fn run() -> . { helper() }\n";
    let main_path = dir.write("pkg/main.kio", clean);
    dir.write(
        "pkg/util.kio",
        "module pkg/util;\n\npub fn helper() -> . { () }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, clean);
    let hover = wait_for_hover(&mut lsp, &uri, 2, 21);
    assert!(
        !hover.is_null(),
        "clean analysis should be available before edit"
    );

    send_full_text_change(&mut lsp, &uri, 2, broken);
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .expect("name diagnostic");
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));

    let action_resp = send_code_action(&mut lsp, &uri, diagnostic);
    let actions = action_resp
        .get("result")
        .and_then(Value::as_array)
        .expect("code actions");
    assert!(
        actions.iter().all(|action| {
            action.get("title").and_then(Value::as_str) != Some("Add `helper` declaration")
        }),
        "auto-import candidate should suppress add-stub action: {action_resp:?}"
    );
    let action = actions
        .iter()
        .find(|action| {
            action.get("title").and_then(Value::as_str) == Some("Import `pkg/util` as `util`")
        })
        .expect("auto-import action");
    assert_eq!(
        action.get("isPreferred").and_then(Value::as_bool),
        Some(true)
    );
    let resolved = send_code_action_resolve(&mut lsp, action);
    let edits = resolved_edits_for_uri(&resolved, &uri);
    assert!(
        edits
            .iter()
            .any(|edit| edit.get("newText").and_then(Value::as_str)
                == Some("\nimport pkg/util as util;")),
        "resolved auto-import must insert a qualified import: {resolved:?}"
    );
    assert!(
        edits
            .iter()
            .any(|edit| edit.get("newText").and_then(Value::as_str) == Some("util.helper")),
        "resolved auto-import must qualify the use site: {resolved:?}"
    );

    assert_auto_import_reanalysis(&mut lsp, &uri, broken, edits);

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_auto_import_finds_public_recursive_type_group_members() {
    let dir = TempDir::new("auto-import-recursive-type-member");
    let clean = "module pkg/main;\n\npub fn run(value: .) -> . { () }\n";
    let broken = "module pkg/main;\n\npub fn run(value: Node) -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", clean);
    dir.write(
        "pkg/types.kio",
        concat!(
            "module pkg/types;\n",
            "rec {\n",
            "  pub type Chain = . | Node;\n",
            "  pub newtype Node : Chain { pub constructor make_node; pub projector read_node; };\n",
            "}\n",
        ),
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, clean);
    assert!(!wait_for_hover(&mut lsp, &uri, 2, 8).is_null());

    send_full_text_change(&mut lsp, &uri, 2, broken);
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .expect("unresolved recursive group member diagnostic");
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Import `pkg/types` as `types`")
            })
        })
        .unwrap_or_else(|| panic!("recursive group member auto-import missing: {response:?}"));
    let resolved = send_code_action_resolve(&mut lsp, action);
    let edits = resolved_edits_for_uri(&resolved, &uri);
    assert!(edits.iter().any(|edit| {
        edit.get("newText").and_then(Value::as_str) == Some("\nimport pkg/types as types;")
    }));
    assert!(
        edits
            .iter()
            .any(|edit| { edit.get("newText").and_then(Value::as_str) == Some("types.Node") })
    );

    assert_auto_import_reanalysis(&mut lsp, &uri, broken, edits);

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_code_action_resolve_auto_import_alias_collision_uses_selective_import() {
    let dir = TempDir::new("auto-import-collision");
    let clean = concat!(
        "module pkg/main;\n",
        "import pkg/other as util;\n",
        "\n",
        "pub fn run() -> . { () }\n",
    );
    let broken = concat!(
        "module pkg/main;\n",
        "import pkg/other as util;\n",
        "\n",
        "pub fn run() -> . { helper() }\n",
    );
    let main_path = dir.write("pkg/main.kio", clean);
    dir.write(
        "pkg/other.kio",
        "module pkg/other;\n\npub fn noop() -> . { () }\n",
    );
    dir.write(
        "pkg/util.kio",
        "module pkg/util;\n\npub fn helper() -> . { () }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, clean);
    let hover = wait_for_hover(&mut lsp, &uri, 3, 21);
    assert!(
        !hover.is_null(),
        "clean analysis should be available before edit"
    );

    send_full_text_change(&mut lsp, &uri, 2, broken);
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .expect("name diagnostic");
    let action_resp = send_code_action(&mut lsp, &uri, diagnostic);
    let action = action_resp
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Import `helper` from `pkg/util`")
            })
        })
        .expect("selective auto-import action");
    assert_eq!(
        action.get("isPreferred").and_then(Value::as_bool),
        Some(true)
    );
    let resolved = send_code_action_resolve(&mut lsp, action);
    let edits = resolved_edits_for_uri(&resolved, &uri);
    assert!(
        edits
            .iter()
            .any(|edit| edit.get("newText").and_then(Value::as_str)
                == Some("\nimport pkg/util(helper);")),
        "alias collision should insert a selective import: {resolved:?}"
    );
    assert_eq!(
        edits.len(),
        1,
        "selective import fallback must not rewrite the use site: {resolved:?}"
    );

    assert_auto_import_reanalysis(&mut lsp, &uri, broken, edits);

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_code_action_resolve_add_stub_inserts_eof_stub() {
    let dir = TempDir::new("add-stub");
    let source = "module pkg/main;\n\npub fn run() -> . { missing(()) }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .expect("name diagnostic");
    let action_resp = send_code_action(&mut lsp, &uri, diagnostic);
    let action = action_resp
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Add `missing` declaration")
            })
        })
        .expect("add-stub action");
    let resolved = send_code_action_resolve(&mut lsp, action);
    let edits = resolved_edits_for_uri(&resolved, &uri);
    assert_eq!(edits.len(), 1, "{resolved:?}");
    assert_eq!(
        edits[0].get("newText").and_then(Value::as_str),
        Some("\n\nfn missing(arg0: .) -> . { () }\n")
    );
    assert_ne!(
        edits[0]
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("line"))
            .and_then(Value::as_u64),
        Some(u32::MAX as u64),
        "add-stub must use a real EOF range"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_inlay_hint_returns_let_type_and_inferred_type_args() {
    let dir = TempDir::new("inlay");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn id[A](x: A) -> A { x }\n",
        "\n",
        "pub fn run() -> . {\n",
        "  let x = id(());\n",
        "  x\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let hover = wait_for_hover(&mut lsp, &uri, 5, 10);
    assert!(
        !hover.is_null(),
        "clean analysis should be available before inlay request"
    );

    let result = send_inlay_hints(&mut lsp, &uri);
    let hints = result.as_array().expect("inlayHint result array");
    assert!(
        hints
            .iter()
            .any(|hint| hint.get("label").and_then(Value::as_str) == Some(": .")),
        "expected inferred let-type hint: {result:?}"
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.get("label").and_then(Value::as_str) == Some("[.]")),
        "expected inferred type-argument hint: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_symmetric_common_results_refresh_hover_and_inlay_hints() {
    let dir = TempDir::new("symmetric-common-results");
    let common_source = concat!(
        "module pkg/common;\n",
        "import __comptime__;\n",
        "\n",
        "pure fn common_impl(\n",
        "  ct: __Comptime__,\n",
        "  fills: __Fill_ctx__,\n",
        "  _first_type: __Type__,\n",
        "  first: __Checked_term__,\n",
        "  _second_type: __Type__,\n",
        "  second: __Checked_term__,\n",
        "  target: __Type__\n",
        ") -> (__Checked_term__ & __Fill_ctx__) {\n",
        "  (\n",
        "    first,\n",
        "    __fill__(\n",
        "      ct,\n",
        "      __fill__(ct, fills, target, __term_type__(ct, first)),\n",
        "      target,\n",
        "      __term_type__(ct, second)\n",
        "    )\n",
        "  )\n",
        "}\n",
        "\n",
        "pub elab common :\n",
        "  [First] First ->\n",
        "  [Second] Second ->\n",
        "  [Target] Target\n",
        "{\n",
        "  impl(fills) common_impl;\n",
        "};\n",
    );
    let initial_source = concat!(
        "module pkg/main;\n",
        "import pkg/common(common);\n",
        "import control(if);\n",
        "\n",
        "host type Bool role(bool);\n",
        "host type I32 role(i32);\n",
        "host type String role(str);\n",
        "host fn make[A]() -> A;\n",
        "host fn witness() -> I32;\n",
        "\n",
        "pub fn run(condition: Bool) -> . {\n",
        "  let _if_result =\n",
        "    if! condition {\n",
        "      make()\n",
        "    } else {\n",
        "      witness()\n",
        "    };\n",
        "  let fill_thunk =\n",
        "    common!(\n",
        "      .() { make() },\n",
        "      .() { witness() }\n",
        "    );\n",
        "  let _fill_result = fill_thunk();\n",
        "  let readiness = ();\n",
        "  ()\n",
        "}\n",
    );
    dir.write("pkg/common.kio", common_source);
    dir.write(
        "control.kio",
        include_str!("../../test-data/poc/elab/workdir/control.kio"),
    );
    let main_path = dir.write("pkg/main.kio", initial_source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    let assert_ready = |publish: &Value, expected_version: i64| {
        assert_eq!(
            publish.get("version").and_then(Value::as_i64),
            Some(expected_version),
            "analysis publish must match the current document version: {publish:?}"
        );
        let diagnostics = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .expect("diagnostics array");
        assert_eq!(diagnostics.len(), 1, "expected one readiness warning");
        assert_eq!(
            diagnostics[0].get("severity").and_then(Value::as_i64),
            Some(2),
            "readiness diagnostic must be a warning: {publish:?}"
        );
        assert_eq!(
            diagnostics[0].get("message").and_then(Value::as_str),
            Some("unused binding `readiness`")
        );
    };
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": initial_source,
            }
        }),
    );
    let initial_publish = wait_for_publish(&mut lsp, &uri);
    assert_ready(&initial_publish, 1);

    let initial_make_offsets = initial_source
        .match_indices("make()")
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(initial_make_offsets.len(), 2, "test source shape changed");
    for (subject, offset) in ["conditional arm", "fills source"]
        .into_iter()
        .zip(initial_make_offsets)
    {
        let position = source_position(initial_source, offset + "make(".len());
        let hover = send_hover(&mut lsp, &uri, position.0, position.1);
        assert_eq!(
            hover_type(&hover),
            "I32",
            "{subject} should inherit the concrete later result: {hover:?}"
        );
    }
    let initial_hints = send_inlay_hints(&mut lsp, &uri);
    assert!(
        symmetric_common_result_hints_match(initial_source, &initial_hints, "pkg.main.I32"),
        "initial symmetric-result hints should resolve to I32: {initial_hints:?}"
    );

    let incomplete_source = initial_source.replace("  ()\n}\n", "  let unfinished =\n}\n");
    assert_ne!(
        incomplete_source, initial_source,
        "incomplete edit marker must match"
    );
    let final_source = initial_source.replace("witness() -> I32", "witness() -> String");
    assert_ne!(
        final_source, initial_source,
        "concrete witness edit marker must match"
    );
    send_full_text_change(&mut lsp, &uri, 2, &incomplete_source);
    send_full_text_change(&mut lsp, &uri, 3, &final_source);
    let final_publish = loop {
        let publish = wait_for_publish(&mut lsp, &uri);
        match publish.get("version").and_then(Value::as_i64) {
            Some(3) => break publish,
            Some(2) => {}
            version => panic!("unexpected analysis version {version:?}: {publish:?}"),
        }
    };
    assert_ready(&final_publish, 3);

    let final_make_offsets = final_source
        .match_indices("make()")
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(final_make_offsets.len(), 2, "test source shape changed");
    let first_position = source_position(&final_source, final_make_offsets[0] + "make(".len());
    let final_hover = send_hover(&mut lsp, &uri, first_position.0, first_position.1);
    assert_eq!(
        hover_type(&final_hover),
        "String",
        "conditional hover must come from the final document version: {final_hover:?}"
    );

    let second_position = source_position(&final_source, final_make_offsets[1] + "make(".len());
    let fill_hover = send_hover(&mut lsp, &uri, second_position.0, second_position.1);
    assert_eq!(
        hover_type(&fill_hover),
        "String",
        "fills-source hover must come from the final document version: {fill_hover:?}"
    );

    let final_hints = send_inlay_hints(&mut lsp, &uri);
    assert!(
        symmetric_common_result_hints_match(&final_source, &final_hints, "pkg.main.String"),
        "symmetric-result hints must come from the final document version: {final_hints:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

fn symmetric_common_result_hints_match(source: &str, result: &Value, ty: &str) -> bool {
    let Some(hints) = result.as_array() else {
        return false;
    };
    let has_hint = |expected_position: (u64, u64), label: &str| {
        hints.iter().any(|hint| {
            hint.get("position").and_then(|json_position| {
                Some((
                    json_position.get("line")?.as_u64()?,
                    json_position.get("character")?.as_u64()?,
                ))
            }) == Some(expected_position)
                && hint.get("label").and_then(Value::as_str) == Some(label)
        })
    };
    let after = |marker: &str| {
        source
            .find(marker)
            .map(|offset| source_position(source, offset + marker.len()))
    };
    let make_positions = source
        .match_indices("make()")
        .map(|(offset, _)| source_position(source, offset + "make".len()))
        .collect::<Vec<_>>();
    let type_arg_label = format!("[{ty}]");

    make_positions.len() == 2
        && make_positions
            .into_iter()
            .all(|position| has_hint(position, &type_arg_label))
        && after("_if_result").is_some_and(|position| has_hint(position, &format!(": {ty}")))
        && after("fill_thunk").is_some_and(|position| has_hint(position, &format!(": . -> {ty}")))
        && after("_fill_result").is_some_and(|position| has_hint(position, &format!(": {ty}")))
}

#[test]
fn lsp_signature_help_uses_value_argument_active_parameter() {
    let dir = TempDir::new("signature-help");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn choose[A](x: A, y: A) -> A { y }\n",
        "\n",
        "pub fn run() -> . { choose((), ()) }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let hover = wait_for_hover(&mut lsp, &uri, 4, 21);
    assert!(
        !hover.is_null(),
        "clean analysis should be available before signature request"
    );

    let result = wait_for_signature_help(&mut lsp, &uri, 4, 32);
    let signature = result
        .get("signatures")
        .and_then(Value::as_array)
        .and_then(|signatures| signatures.first())
        .expect("signature result");
    assert_eq!(
        signature.get("label").and_then(Value::as_str),
        Some("fn choose[A](x: A, y: A) -> A"),
        "signature label should render the declaration: {result:?}"
    );
    assert_eq!(
        result.get("activeParameter").and_then(Value::as_u64),
        Some(1),
        "cursor in second value argument should select active parameter 1: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_signature_help_preserves_shadowed_source_binder_spelling() {
    let dir = TempDir::new("signature-help-shadowed-binder");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn choose[A](x: A, f: [A] A -> A) -> A { x }\n",
        "\n",
        "pub fn run() -> . { choose((), .[B](x: B) -> B { x }) }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let hover = wait_for_hover(&mut lsp, &uri, 4, 24);
    assert!(
        !hover.is_null(),
        "clean analysis should be available before signature request"
    );

    let result = wait_for_signature_help(&mut lsp, &uri, 4, 42);
    let signature = result
        .get("signatures")
        .and_then(Value::as_array)
        .and_then(|signatures| signatures.first())
        .expect("signature result");
    assert_eq!(
        signature.get("label").and_then(Value::as_str),
        Some("fn choose[A](x: A, f: [A] A -> A) -> A"),
        "signature help must preserve the declaration's source spelling: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_uses_the_source_shadowed_binder_spelling() {
    let dir = TempDir::new("hover-shadowed-binder");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn inspect[A](outer: A) -> A {\n",
        "  let inner = .[A](value: A) -> A { value };\n",
        "  outer\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let hover = wait_for_hover(&mut lsp, &uri, 3, 38);
    assert_eq!(
        hover
            .get("contents")
            .and_then(|contents| contents.get("value"))
            .and_then(Value::as_str),
        Some("```kio\nA\n```"),
        "hover must present the occurrence's source binder without changing its checked identity: {hover:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_retains_the_occurrences_polymorphic_binder_spelling() {
    let dir = TempDir::new("hover-polymorphic-occurrence");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn witness(first: [A] A -> A, second: [B] B -> B) -> . {\n",
        "  let chosen = second;\n",
        "  ()\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let hover = wait_for_hover(&mut lsp, &uri, 3, 17);
    assert_eq!(
        hover_type(&hover),
        "[B] B -> B",
        "hover must use the selected occurrence's binder spelling: {hover:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_unused_warning_publishes_and_offers_underscore_quickfix() {
    let dir = TempDir::new("unused-warning");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn run() -> . {\n",
        "  let x = ();\n",
        "  ()\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostics = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("diagnostics array");
    assert_eq!(diagnostics.len(), 1, "expected one warning: {publish:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(
        diagnostic.get("severity").and_then(Value::as_i64),
        Some(2),
        "unused binding should publish as Warning severity"
    );
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("unused binding `x`")
    );
    let action_resp = send_code_action(&mut lsp, &uri, diagnostic);
    let actions = action_resp
        .get("result")
        .and_then(Value::as_array)
        .expect("codeAction result array");
    let underscore = actions
        .iter()
        .find(|action| {
            action
                .get("edit")
                .and_then(|edit| workspace_edit_edits_for_uri(edit, &uri))
                .map(|(edits, _)| edits)
                .and_then(|edits| edits.first())
                .and_then(|edit| edit.get("newText"))
                .and_then(Value::as_str)
                == Some("_x")
        })
        .expect("underscore quickfix");
    assert_eq!(
        underscore.get("isPreferred").and_then(Value::as_bool),
        Some(true)
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_source_fix_all_applies_every_coexisting_warning_in_a_recursive_module() {
    // Compile errors follow the pipeline's deterministic earliest-error
    // contract, so clean-analysis warnings are the real same-file channel in
    // which the server can receive more than one action at once. Recursive
    // compile-error repairs are exercised sequentially by the dedicated
    // marker/group application-and-retraction tests below.
    let dir = TempDir::new("source-fix-all-warnings");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec newtype Tree : . | Tree { constructor mk_tree; projector un_tree; };\n",
        "\n",
        "pub fn run(first: ., second: .) -> . { () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 3,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostics = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("diagnostics array");
    assert_eq!(diagnostics.len(), 2, "expected two warnings: {publish:?}");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| { diagnostic.get("severity").and_then(Value::as_i64) == Some(2) })
    );

    let response = send_code_actions_for_kind(&mut lsp, &uri, diagnostics, "source.fixAll");
    let actions = response
        .get("result")
        .and_then(Value::as_array)
        .expect("fix-all actions");
    assert_eq!(
        actions.len(),
        1,
        "a source.fixAll request must contain only its combined action: {response:?}"
    );
    let action = &actions[0];
    assert_eq!(
        action.get("title").and_then(Value::as_str),
        Some("Fix all Kio diagnostics")
    );
    assert_eq!(
        action.get("kind").and_then(Value::as_str),
        Some("source.fixAll")
    );
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(3));
    assert_eq!(edits.len(), 2);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(
        repaired.contains("run(_first: ., _second: .)"),
        "{repaired}"
    );
    assert!(repaired.contains("rec newtype Tree"), "{repaired}");

    send_full_text_change(&mut lsp, &uri, 4, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 4);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the combined repair must recheck cleanly: {cleared:?}\n{repaired}"
    );

    let stale = send_code_actions_for_kind(&mut lsp, &uri, diagnostics, "source.fixAll");
    assert!(
        stale
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "version-3 fix-all must disappear at version 4: {stale:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_reports_declared_dependency_that_is_not_materialized() {
    let dir = TempDir::new("missing-dep");
    let source = concat!(
        "module pkg/main;\n",
        "import foo/lib as lib;\n",
        "\n",
        "pub fn run() -> . { () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    let dep_path = dir.write(
        "foo.dep.kio",
        "dependency foo;\nsource { path \"../foo/foo.pkg.kio\"; }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let main_uri = path_to_file_uri(&main_path);
    let dep_uri = path_to_file_uri(&dep_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &dep_uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .expect("dependency diagnostic");
    assert_eq!(diagnostic.get("severity").and_then(Value::as_i64), Some(1));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(30));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("dependency `foo` is declared but not materialized")
    );
    assert_eq!(
        diagnostic
            .get("data")
            .and_then(|data| data.get("help"))
            .and_then(Value::as_str),
        Some("run `kio dep fetch foo` to materialize the dependency")
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_reports_parse_error() {
    let dir = TempDir::new("parse-err");
    // Unterminated paren list — a guaranteed parse error.
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { (\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": std::fs::read_to_string(&main_path).unwrap(),
            }
        }),
    );

    let diags = wait_for_diagnostics(&mut lsp, &uri);
    let arr = diags.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected one parse diagnostic: {diags:?}");
    let d = &arr[0];
    assert_eq!(d.get("severity").and_then(Value::as_i64), Some(1));

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_publishes_empty_diagnostics_for_clean_package() {
    let dir = TempDir::new("clean");
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": std::fs::read_to_string(&main_path).unwrap(),
            }
        }),
    );

    // A clean package shouldn't produce a publishDiagnostics
    // notification (there's nothing to publish AND nothing
    // previously published to clear). To verify the server is
    // alive and quiescent we issue a shutdown handshake — its
    // success means the server finished the open-handler without
    // panic.
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_clears_diagnostics_when_file_becomes_clean() {
    let dir = TempDir::new("clear");
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { 1 }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": std::fs::read_to_string(&main_path).unwrap(),
            }
        }),
    );
    let initial = wait_for_diagnostics(&mut lsp, &uri);
    assert_eq!(
        initial.as_array().map(Vec::len),
        Some(1),
        "initial diagnostics: {initial:?}"
    );

    // Fix the file: send didChange to update the overlay, then
    // didSave to mirror an editor's save-after-edit flow. The
    // overlay is authoritative now — disk-only changes don't affect
    // analysis. Send a full-document replacement (range omitted)
    // to keep the test edit-position-independent.
    let fixed = "module pkg/main;\n\npub fn run() -> . { () }\n";
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [ { "text": fixed } ],
        }),
    );
    fs::write(&main_path, fixed).unwrap();
    lsp.send_notification(
        "textDocument/didSave",
        json!({
            "textDocument": { "uri": uri }
        }),
    );

    // We should get a publishDiagnostics with an empty list to
    // clear the prior diagnostic.
    let cleared = wait_for_diagnostics(&mut lsp, &uri);
    assert_eq!(
        cleared.as_array().map(Vec::len),
        Some(0),
        "expected empty diagnostics to clear stale set: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// Live-editing tests added in the document-overlay session.
// Each test exercises an aspect of the keystroke-driven loop:
// incremental didChange, debounce coalescing, overlay-vs-disk
// precedence, and version-tagged diagnostics.
// ============================================================

#[test]
fn lsp_didchange_incremental_edit_publishes_fresh_diagnostics() {
    // Start with a clean package; send a didChange that breaks the
    // file's only function body; assert a fresh type-error
    // diagnostic appears.
    let dir = TempDir::new("didchange-incr");
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    let initial = std::fs::read_to_string(&main_path).unwrap();
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": initial,
            }
        }),
    );

    // Replace the `()` body with `1` — the type error from
    // lsp_reports_type_error_for_open_document. Range: line 2,
    // characters 21..23 (the `()` between the braces).
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 2, "character": 21 },
                    "end":   { "line": 2, "character": 23 },
                },
                "text": "1",
            }],
        }),
    );

    let diags = wait_for_diagnostics(&mut lsp, &uri);
    let arr = diags.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "expected fresh type-error diagnostic after didChange: {diags:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_didchange_overlay_overrides_disk() {
    // Open a *broken* file: didOpen ships the broken text as the
    // overlay; the on-disk text matches it; the server publishes a
    // type-error diagnostic. Then mutate the *disk* to a clean
    // version without sending a didChange — a real editor user
    // doing this out-of-band should not affect diagnostics.
    // Trigger a fresh analysis by sending a didChange that re-states
    // the same broken text (full-replace; the overlay stays broken).
    // The new publish should still carry the diagnostic — the
    // overlay overrides disk.
    let dir = TempDir::new("overlay-overrides");
    let broken = "module pkg/main;\n\npub fn run() -> . { 1 }\n";
    let main_path = dir.write("pkg/main.kio", broken);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": broken,
            }
        }),
    );
    // didOpen analysis: broken → 1 diagnostic at version=1.
    let pub_v1 = wait_for_publish(&mut lsp, &uri);
    assert_eq!(pub_v1.get("version").and_then(Value::as_i64), Some(1));
    assert_eq!(
        pub_v1
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "initial broken overlay should produce a diagnostic"
    );

    // Mutate the disk to a clean version. The LSP doesn't know about
    // this change; the overlay still says "broken".
    let clean = "module pkg/main;\n\npub fn run() -> . { () }\n";
    std::fs::write(&main_path, clean).unwrap();

    // Send a didChange that *re-states* the broken text in full
    // (range = None means full-document replacement). The overlay
    // ends up identical to its pre-edit state, just at version=2.
    // The server reanalyzes; the result should still be the
    // diagnostic — proving the overlay wins over disk.
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": broken }],
        }),
    );

    let pub_v2 = wait_for_publish(&mut lsp, &uri);
    assert_eq!(pub_v2.get("version").and_then(Value::as_i64), Some(2));
    let diags = pub_v2.get("diagnostics").expect("diagnostics field");
    let arr = diags.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "overlay overrides disk: broken overlay yields diagnostic even when disk is clean; got: {diags:?}",
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_publish_diagnostics_carries_overlay_version() {
    // Open a broken file at version=1, then send a didChange that
    // bumps to version=2. The next publishDiagnostics for this URI
    // must carry `version: 2` to let the client correlate the
    // diagnostic with its local state.
    let dir = TempDir::new("version-tag");
    let main_path = dir.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { 1 }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    let initial = std::fs::read_to_string(&main_path).unwrap();
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": initial,
            }
        }),
    );
    let publish_v1 = wait_for_publish(&mut lsp, &uri);
    assert_eq!(
        publish_v1.get("version").and_then(Value::as_i64),
        Some(1),
        "first publishDiagnostics should carry version=1 (the didOpen version)"
    );

    // Change `1` to `2` — still a type error, still version-tagged.
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 2, "character": 21 },
                    "end":   { "line": 2, "character": 22 },
                },
                "text": "2",
            }],
        }),
    );
    // Skip any leftover v1 publishes and assert on the v2 one.
    let mut publish_v2 = wait_for_publish(&mut lsp, &uri);
    while publish_v2
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        < 2
    {
        publish_v2 = wait_for_publish(&mut lsp, &uri);
    }
    assert_eq!(
        publish_v2.get("version").and_then(Value::as_i64),
        Some(2),
        "post-didChange publishDiagnostics should carry the bumped version"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_didchange_burst_coalesces_to_single_publish() {
    // Send a rapid burst of didChange notifications. The debounce
    // scheduler should collapse them into one analysis: after the
    // burst, exactly one publishDiagnostics with the final-version
    // tag should arrive. Subsequent reads should find the stream
    // quiet (no additional publishes for this URI).
    //
    // Use a broken-by-construction body so every reanalysis produces
    // a diagnostic — keeps the publish from being skipped by the
    // "clean → clean, nothing to clear" short-circuit.
    let dir = TempDir::new("debounce");
    let broken = "module pkg/main;\n\npub fn run() -> . { 1 }\n";
    let main_path = dir.write("pkg/main.kio", broken);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": broken,
            }
        }),
    );
    // Consume the didOpen's publish so it doesn't appear in the
    // burst's count. The didOpen analysis carries version=1.
    let v1 = wait_for_publish(&mut lsp, &uri);
    assert_eq!(v1.get("version").and_then(Value::as_i64), Some(1));
    assert_eq!(
        v1.get("diagnostics")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
    );

    // Send 5 rapid full-text-replace didChange events, all carrying
    // the same broken text — each one just bumps the version, but
    // every reanalysis still produces a diagnostic. The debounce
    // window is 200ms; the loop runs much faster than that, so the
    // coalescer collapses them into one analysis tagged with the
    // final version.
    let final_version = 6_i64;
    for v in 2..=final_version {
        lsp.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": v },
                "contentChanges": [{ "text": broken }],
            }),
        );
    }

    // Wait for one publishDiagnostics — should be the post-debounce
    // one carrying the final version.
    let publish = wait_for_publish(&mut lsp, &uri);
    assert_eq!(
        publish.get("version").and_then(Value::as_i64),
        Some(final_version),
        "debounced publish should carry the final version, got {publish:?}",
    );

    // The stream should now be quiet for this URI. Wait a debounce
    // window plus a generous grace period, then drain non-blockingly
    // and assert no further publishes arrive.
    let extra = lsp.try_recv_for(Duration::from_millis(500));
    assert!(
        extra.is_none()
            || extra
                .as_ref()
                .and_then(|v| v.get("method"))
                .and_then(Value::as_str)
                != Some("textDocument/publishDiagnostics"),
        "burst should debounce to one publish, but got an extra: {extra:?}",
    );

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// Hover / goto-definition / find-references — added in lsp/05.
// ============================================================

/// Send a `textDocument/hover` request and wait for the response.
/// Returns the JSON `result` field (`null` for positions with no type
/// entry, or a Hover object).
fn send_hover(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let id = lsp.send_request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

fn wait_for_hover(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let mut result = Value::Null;
    for _ in 0..20 {
        result = send_hover(lsp, uri, line, character);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    result
}

fn send_inlay_hints(lsp: &mut LspProcess, uri: &str) -> Value {
    let id = lsp.send_request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 999, "character": 0 },
            },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

fn send_signature_help(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let id = lsp.send_request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

fn wait_for_signature_help(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let mut result = Value::Null;
    for _ in 0..20 {
        result = send_signature_help(lsp, uri, line, character);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    result
}

/// Send a `textDocument/definition` request and wait for the response.
/// Retries until a non-null result arrives or the attempts run out —
/// the analysis may still be in flight right after didOpen. Returns the
/// JSON `result` field (`null` if no resolvable binder at the cursor).
fn send_definition(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let mut result = Value::Null;
    for _ in 0..20 {
        let id = lsp.send_request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        );
        let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
        assert!(
            resp.get("error").is_none(),
            "goto-definition must not return a JSON-RPC error; got: {resp:?}"
        );
        result = resp.get("result").cloned().unwrap_or(Value::Null);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    result
}

/// Open a file and wait briefly for any analysis notification (e.g.
/// `publishDiagnostics`). For clean packages the server may not emit
/// one; the bounded wait lets the debounce timer fire and lets the
/// worker store the position index before the test queries it.
fn open_and_wait_for_analysis(lsp: &mut LspProcess, uri: &str, text: &str) {
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": text,
            }
        }),
    );
    // Drain any publishDiagnostics that arrives, then proceed.
    // For clean packages, no notification is sent; the try_recv_for
    // timeout gives the debounce + analysis time to complete.
    let _ = lsp.try_recv_for(Duration::from_millis(600));
}

#[test]
fn lsp_capabilities_advertise_hover_definition_references() {
    // The initialize response must advertise hoverProvider,
    // definitionProvider, and referencesProvider.
    let dir = TempDir::new("caps");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    assert!(
        caps.get("hoverProvider").is_some(),
        "server must advertise hoverProvider; caps = {caps:?}"
    );
    assert!(
        caps.get("definitionProvider").is_some(),
        "server must advertise definitionProvider; caps = {caps:?}"
    );
    assert!(
        caps.get("referencesProvider").is_some(),
        "server must advertise referencesProvider; caps = {caps:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_returns_type_on_expression() {
    // Open a clean package and hover on the `()` expression in the
    // return position. The position index records the unit type there
    // — the hover should return a Hover with `contents.value`
    // containing `.`.
    //
    // Source layout:
    //   line 0: "module pkg/main;\n"   (16 chars + \n)
    //   line 1: "\n"
    //   line 2: "pub fn run() -> . { () }\n"
    //   Inner `()` starts at col 21 on line 2.
    let dir = TempDir::new("hover");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Retry hover a few times — the analysis may still be in flight.
    let mut result = Value::Null;
    for _ in 0..5 {
        result = send_hover(&mut lsp, &uri, 2, 21);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    if !result.is_null() {
        // contents.value should mention the unit type `.`.
        let value = result
            .get("contents")
            .and_then(|c| c.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(
            value.contains("."),
            "hover on the unit value should show `.`; got: {result:?}"
        );
    }
    // Null is also acceptable — the position index may not yet have
    // an entry at the exact byte offset for the `()` literal.

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_documentation_group_hover_and_completion_preserve_doc_ownership() {
    let dir = TempDir::new("recursive-group-documentation");
    dir.write_pkg_root_package();
    let source = "module pkg/main;\n\n/// Group prose.\nrec {\n  /// Alias prose.\n  type Chain = Node;\n  /// Nominal prose.\n  newtype Node : . | Chain { constructor make; projector read; };\n}\nfn use_site(value: Node) -> . { () }\n";
    let path = dir.write("pkg/main.kio", source);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    for (needle, selected, excluded) in [
        ("Chain =", "Alias prose.", "Nominal prose."),
        ("Node :", "Nominal prose.", "Alias prose."),
    ] {
        let (line, character) = source_position(source, source.find(needle).unwrap());
        let hover = wait_for_hover(&mut lsp, &uri, line, character);
        let text = hover_type(&hover);
        assert!(text.contains("rec {"), "{needle}: {hover}");
        assert!(text.contains("type Chain = Node;"), "{hover}");
        assert!(text.contains("newtype Node : . | Chain"), "{hover}");
        assert_eq!(text.matches(selected).count(), 1, "{hover}");
        assert!(!text.contains(excluded), "{hover}");
    }
    let (line, character) = source_position(
        source,
        source.find("value: Node").unwrap() + "value: ".len(),
    );
    let id = lsp.send_request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }
        }),
    );
    let response = lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id));
    let items = response["result"]
        .as_array()
        .or_else(|| response["result"]["items"].as_array())
        .expect("completion items");
    let node = items
        .iter()
        .find(|item| item["label"] == "Node")
        .expect("Node completion");
    assert_eq!(node["documentation"]["value"], "Nominal prose.");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_documentation_outer_group_hover_is_not_a_member() {
    let dir = TempDir::new("recursive-group-outer-documentation");
    dir.write_pkg_root_package();
    let source = "module pkg/main;\n/// Group prose.\nrec {\n  /// Alias prose.\n  type Chain = Node;\n  /// Nominal prose.\n  newtype Node : . | Chain { constructor make; projector read; };\n}\n";
    let path = dir.write("pkg/main.kio", source);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let offset = source.find("rec {").unwrap();
    let (line, character) = source_position(source, offset);
    let hover = send_hover(&mut lsp, &uri, line, character);
    assert!(!hover.is_null(), "outer recursive group hover is absent");
    let text = hover_type(&hover);
    assert!(text.contains("rec {"), "{hover}");
    assert!(text.contains("type Chain = Node;"), "{hover}");
    assert!(text.contains("newtype Node : . | Chain"), "{hover}");
    assert_eq!(text.matches("Group prose.").count(), 1, "{hover}");
    assert!(!text.contains("Alias prose."), "{hover}");
    assert!(!text.contains("Nominal prose."), "{hover}");
    assert_eq!(
        json_range(&hover["range"]),
        source_range(source, offset, "rec".len())
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_documentation_labels_preserve_their_written_owner() {
    let dir = TempDir::new("recursive-label-documentation");
    dir.write_pkg_root_package();
    let source = "module pkg/main;\nrec {\n  type Tree = Twig;\n  /// Twig prose.\n  labels { twig: . | Tree };\n}\n/// List prose.\nrec labels { list: . | List };\nfn use_site(value: Twig) -> . { () }\n";
    let path = dir.write("pkg/main.kio", source);
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    for (needle, context, prose, excluded) in [
        ("twig:", "rec {", "Twig prose.", "List prose."),
        ("list:", "rec labels", "List prose.", "Twig prose."),
    ] {
        let (line, character) = source_position(source, source.find(needle).unwrap());
        let hover = wait_for_hover(&mut lsp, &uri, line, character);
        let text = hover_type(&hover);
        assert!(text.contains(context), "{hover}");
        assert_eq!(text.matches(prose).count(), 1, "{hover}");
        assert!(!text.contains(excluded), "{hover}");
    }
    let (line, character) = source_position(
        source,
        source.find("value: Twig").unwrap() + "value: ".len(),
    );
    let id = lsp.send_request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri }, "position": { "line": line, "character": character }
        }),
    );
    let response = lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id));
    let items = response["result"]
        .as_array()
        .or_else(|| response["result"]["items"].as_array())
        .expect("completion items");
    for (name, prose) in [("Twig", "Twig prose."), ("List", "List prose.")] {
        let item = items
            .iter()
            .find(|item| item["label"] == name)
            .expect("recursive nominal completion");
        assert_eq!(item["documentation"]["value"], prose);
    }
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_returns_type_on_elaborator_implementation_path() {
    let dir = TempDir::new("hover-elaborator-implementation");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "import pkg/provider as helper;\n",
        "\n",
        "elab unit : . -> . { impl helper.unit_impl; };\n",
        "\n",
        "pub fn run() -> . { unit!() }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write(
        "pkg/provider.kio",
        concat!(
            "module pkg/provider;\n",
            "\n",
            "import __comptime__;\n",
            "\n",
            "pub(pkg) pure fn unit_impl(ct: __Comptime__) -> __Checked_term__ {\n",
            "  __term_unit__(ct)\n",
            "}\n",
        ),
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let implementation = "helper.unit_impl";
    let start = source.find(implementation).expect("implementation path");
    let cursor = start + "helper.".len();
    let hover = wait_for_hover(
        &mut lsp,
        &uri,
        source_position(source, cursor).0,
        source_position(source, cursor).1,
    );

    assert!(
        !hover.is_null(),
        "the declaration implementation path must retain its synthesized type"
    );
    let leaf_start = start + "helper.".len();
    assert_eq!(
        hover_range(&hover),
        source_range(source, leaf_start, "unit_impl".len()),
        "hover must cover the exact callable leaf: {hover:?}"
    );
    let ty = hover_type(&hover);
    assert!(
        ty.contains("__Comptime__") && ty.contains("__Checked_term__"),
        "hover must show the implementation function type: {hover:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_callable_declaration_targets_use_exact_semantic_leaf_positions() {
    let dir = TempDir::new("callable-declaration-target-positions");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "import pkg/provider as helper;\n",
        "\n",
        "op _ + _ { impl helper.combine; };\n",
        "varop [* *] {\n",
        "  foldr helper.push helper.empty;\n",
        "  finalize helper.finish;\n",
        "};\n",
        "elab unit : . -> . { impl helper.elab_impl; };\n",
        "\n",
        "fn direct() -> . { helper.combine((), ()) }\n",
        "fn use_operator() -> . { () + () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    let provider = concat!(
        "module pkg/provider;\n",
        "\n",
        "import __comptime__;\n",
        "\n",
        "pub fn combine(left: ., right: .) -> . { left }\n",
        "pub fn empty() -> . { () }\n",
        "pub fn push(left: ., right: .) -> . { left }\n",
        "pub fn finish(value: .) -> . { value }\n",
        "pub pure fn elab_impl(ct: __Comptime__) -> __Checked_term__ {\n",
        "  __term_unit__(ct)\n",
        "}\n",
    );
    let provider_path = dir.write("pkg/provider.kio", provider);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    let provider_uri = path_to_file_uri(&provider_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    for target in ["combine", "empty", "push", "finish"] {
        let written = format!("helper.{target}");
        let path_start = source.find(&written).expect("declaration target");
        let leaf_start = path_start + "helper.".len();
        let position = source_position(source, leaf_start + 1);
        let hover = wait_for_hover(&mut lsp, &uri, position.0, position.1);
        assert!(!hover.is_null(), "`{written}` must have a hover type");
        assert_eq!(
            hover_range(&hover),
            source_range(source, leaf_start, target.len()),
            "`{written}` hover must cover only the exact callable leaf"
        );

        let definition = send_definition(&mut lsp, &uri, position.0, position.1);
        assert_eq!(
            definition_uri(&definition),
            provider_uri,
            "`{written}` must resolve to its provider module"
        );
        assert_eq!(
            definition_start(&definition),
            source_position(
                provider,
                provider.find(target).expect("provider declaration name")
            ),
            "`{written}` must resolve to the exact provider declaration token"
        );
    }

    let declaration_start = source
        .find("helper.combine")
        .expect("operator declaration target")
        + "helper.".len();
    let direct_start = source
        .rfind("helper.combine")
        .expect("ordinary qualified call")
        + "helper.".len();
    let declaration_position = source_position(source, declaration_start + 1);
    let expected_ranges =
        [declaration_start, direct_start].map(|start| source_range(source, start, "combine".len()));

    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": {
                "line": declaration_position.0,
                "character": declaration_position.1,
            },
            "context": { "includeDeclaration": true },
        }),
    );
    let references =
        lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(references_id));
    assert!(references.get("error").is_none(), "{references:?}");
    let mut reference_ranges = references
        .get("result")
        .and_then(Value::as_array)
        .expect("callable references")
        .iter()
        .filter(|location| location.get("uri").and_then(Value::as_str) == Some(uri.as_str()))
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    reference_ranges.sort_unstable();
    let mut expected_ranges = expected_ranges.to_vec();
    expected_ranges.sort_unstable();
    assert_eq!(reference_ranges, expected_ranges);

    let rename = send_rename(
        &mut lsp,
        &uri,
        declaration_position.0,
        declaration_position.1,
        "merge",
    );
    assert!(rename.get("error").is_none(), "{rename:?}");
    let mut rename_sites = rename
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .expect("rename documentChanges")
        .iter()
        .flat_map(|change| {
            let changed_uri = change
                .get("textDocument")
                .and_then(|document| document.get("uri"))
                .and_then(Value::as_str)
                .expect("rename document URI")
                .to_owned();
            change
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |edit| {
                    (
                        changed_uri.clone(),
                        json_range(edit.get("range").expect("rename range")),
                    )
                })
        })
        .collect::<Vec<_>>();
    rename_sites.sort_unstable();
    let provider_declaration = provider.find("combine").expect("provider declaration");
    let mut expected_rename_sites = vec![
        (
            provider_uri.clone(),
            source_range(provider, provider_declaration, "combine".len()),
        ),
        (uri.clone(), expected_ranges[0]),
        (uri.clone(), expected_ranges[1]),
    ];
    expected_rename_sites.sort_unstable();
    assert_eq!(rename_sites, expected_rename_sites);

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_identity_alias_callable_paths_keep_head_and_member_identities_separate() {
    let dir = TempDir::new("identity-alias-callable-dual-identity");
    let origin = concat!(
        "module pkg/origin;\n",
        "\n",
        "pub type Alias = .;\n",
        "pub newtype Terminal : . {\n",
        "  pub constructor make;\n",
        "  pub projector open;\n",
        "};\n",
    );
    let relay = concat!(
        "module pkg/relay;\n",
        "\n",
        "import pkg/origin as source;\n",
        "\n",
        "pub type Alias = source.Terminal;\n",
        "pub fn make() -> . { () }\n",
    );
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "import pkg/relay as forwarded;\n",
        "\n",
        "op _ + _ { impl forwarded.Alias.make; };\n",
        "fn direct() -> forwarded.Alias { forwarded.Alias.make(()) }\n",
    );
    let origin_path = dir.write("pkg/origin.kio", origin);
    let relay_path = dir.write("pkg/relay.kio", relay);
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let origin_uri = path_to_file_uri(&origin_path);
    let relay_uri = path_to_file_uri(&relay_path);
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let callable_start = source
        .find("forwarded.Alias.make")
        .expect("operator callable");
    let alias_start = callable_start + "forwarded.".len();
    let member_start = alias_start + "Alias.".len();
    let alias_position = source_position(source, alias_start + 1);
    let member_position = source_position(source, member_start + 1);

    let alias_definition = send_definition(&mut lsp, &uri, alias_position.0, alias_position.1);
    assert_eq!(definition_uri(&alias_definition), relay_uri);
    assert_eq!(
        definition_start(&alias_definition),
        source_position(
            relay,
            relay.find("type Alias").expect("relay alias") + "type ".len()
        ),
        "the written head must retain the source alias identity"
    );

    let member_definition = send_definition(&mut lsp, &uri, member_position.0, member_position.1);
    assert_eq!(definition_uri(&member_definition), origin_uri);
    assert_eq!(
        definition_start(&member_definition),
        source_position(
            origin,
            origin.find("constructor make").expect("terminal member") + "constructor ".len()
        ),
        "the callable leaf must retain the terminal member identity"
    );

    let references_at =
        |lsp: &mut LspProcess, position: (u64, u64)| -> Vec<(String, (u64, u64, u64, u64))> {
            let id = lsp.send_request(
                "textDocument/references",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": position.0, "character": position.1 },
                    "context": { "includeDeclaration": true },
                }),
            );
            let response =
                lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id));
            assert!(response.get("error").is_none(), "{response:?}");
            let mut locations = response
                .get("result")
                .and_then(Value::as_array)
                .expect("identity references")
                .iter()
                .map(|location| {
                    (
                        location
                            .get("uri")
                            .and_then(Value::as_str)
                            .expect("reference URI")
                            .to_owned(),
                        json_range(location.get("range").expect("reference range")),
                    )
                })
                .collect::<Vec<_>>();
            locations.sort_unstable();
            locations
        };

    let direct_return =
        source.find("-> forwarded.Alias").expect("alias return") + "-> forwarded.".len();
    let direct_call = source.rfind("forwarded.Alias.make").expect("direct call");
    let direct_alias = direct_call + "forwarded.".len();
    let direct_member = direct_alias + "Alias.".len();
    let relay_alias = relay.find("type Alias").expect("relay alias") + "type ".len();
    let origin_member =
        origin.find("constructor make").expect("origin member") + "constructor ".len();

    let mut expected_alias_references = vec![
        (
            relay_uri.clone(),
            source_range(relay, relay_alias, "Alias".len()),
        ),
        (
            uri.clone(),
            source_range(source, alias_start, "Alias".len()),
        ),
        (
            uri.clone(),
            source_range(source, direct_return, "Alias".len()),
        ),
        (
            uri.clone(),
            source_range(source, direct_alias, "Alias".len()),
        ),
    ];
    expected_alias_references.sort_unstable();
    assert_eq!(
        references_at(&mut lsp, alias_position),
        expected_alias_references,
        "the origin's same-spelling type must remain a different identity"
    );

    let mut expected_member_references = vec![
        (
            origin_uri.clone(),
            source_range(origin, origin_member, "make".len()),
        ),
        (
            uri.clone(),
            source_range(source, member_start, "make".len()),
        ),
        (
            uri.clone(),
            source_range(source, direct_member, "make".len()),
        ),
    ];
    expected_member_references.sort_unstable();
    assert_eq!(
        references_at(&mut lsp, member_position),
        expected_member_references,
        "the relay's same-spelling function must remain a different identity"
    );

    for (position, new_name, expected_sites) in [
        (alias_position, "Renamed", expected_alias_references),
        (member_position, "create", expected_member_references),
    ] {
        let rename = send_rename(&mut lsp, &uri, position.0, position.1, new_name);
        assert!(rename.get("error").is_none(), "{rename:?}");
        let mut sites = rename
            .get("result")
            .and_then(|result| result.get("documentChanges"))
            .and_then(Value::as_array)
            .expect("rename documentChanges")
            .iter()
            .flat_map(|change| {
                let changed_uri = change
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(Value::as_str)
                    .expect("rename URI")
                    .to_owned();
                change
                    .get("edits")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(move |edit| {
                        (
                            changed_uri.clone(),
                            json_range(edit.get("range").expect("rename range")),
                        )
                    })
            })
            .collect::<Vec<_>>();
        sites.sort_unstable();
        assert_eq!(sites, expected_sites);
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_newtype_callable_path_uses_packed_declaration_identity() {
    let dir = TempDir::new("recursive-newtype-callable-definition");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  newtype A : (. | B) { constructor mk_a; projector un_a; };\n",
        "  newtype B : (. | A) { constructor mk_b; projector un_b; };\n",
        "}\n",
        "op _ + _ { impl A.mk_a; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let callable = source.find("A.mk_a").expect("recursive callable");
    let head_position = source_position(source, callable);
    let head_definition = send_definition(&mut lsp, &uri, head_position.0, head_position.1);
    assert_eq!(definition_uri(&head_definition), uri);
    assert_eq!(
        definition_start(&head_definition),
        source_position(source, source.find("newtype A").expect("A declaration") + 8)
    );

    let member_position = source_position(source, callable + "A.".len());
    let member_definition = send_definition(&mut lsp, &uri, member_position.0, member_position.1);
    assert_eq!(definition_uri(&member_definition), uri);
    assert_eq!(
        definition_start(&member_definition),
        source_position(
            source,
            source.find("constructor mk_a").expect("member declaration") + "constructor ".len()
        )
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_uses_focused_module_when_sibling_body_fails() {
    let dir = TempDir::new("hover-focused");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write(
        "pkg/bad.kio",
        "module pkg/bad;\n\npub fn bad() -> . { 1 }\n",
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let mut result = Value::Null;
    for _ in 0..20 {
        result = send_hover(&mut lsp, &uri, 2, 21);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        value.contains("."),
        "focused hover should type the clean module despite a bad sibling body; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_hover_returns_null_for_whitespace() {
    // Hover on the blank line — no typeable node, must return null.
    //   line 1: "\n"  (blank between module decl and fn)
    let dir = TempDir::new("hover-ws");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    open_and_wait_for_analysis(&mut lsp, &uri, source);
    // Give the analysis extra time so a non-null hover on line 1
    // would have a chance to appear if we're wrong.
    std::thread::sleep(Duration::from_millis(400));

    let result = send_hover(&mut lsp, &uri, 1, 0);
    assert!(
        result.is_null(),
        "hover on blank line should return null; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_definition_returns_null_or_location() {
    // Open a clean package and send a goto-definition request.
    // The result must be either null or a valid Location / array —
    // never an error response.
    let dir = TempDir::new("def");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 7 },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    // An error field means the server returned a JSON-RPC error —
    // that's a bug; null or a location are both valid LSP responses.
    assert!(
        resp.get("error").is_none(),
        "goto-definition must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_null() || result.is_object() || result.is_array(),
        "goto-definition must return null or a location; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_references_returns_null_or_list() {
    // Send a textDocument/references request and verify the server
    // responds with either null (no binder at cursor) or a list.
    let dir = TempDir::new("refs");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 7 },
            "context": { "includeDeclaration": true },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "references must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_null() || result.is_array(),
        "references must return null or an array; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_document_highlight_returns_occurrences_in_current_file() {
    // A recursive fn: the name `run` appears at its declaration and in
    // the body call. documentHighlight on the declaration name returns
    // both occurrences, each with kind = Text (1).
    let dir = TempDir::new("highlight");
    let source = "module pkg/main;\n\npub fn run() -> . { run() }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/documentHighlight",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 7 },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "documentHighlight must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_null() || result.is_array(),
        "documentHighlight must return null or an array; got: {result:?}"
    );
    if let Some(arr) = result.as_array() {
        for hl in arr {
            assert!(
                hl.get("range").is_some(),
                "each highlight carries a range; got: {hl:?}"
            );
            // kind, when present, is Text (1) — never a guessed read/write.
            if let Some(kind) = hl.get("kind") {
                assert_eq!(kind.as_u64(), Some(1), "highlight kind must be Text");
            }
        }
    }

    assert_eq!(lsp.shutdown(), 0);
}

/// Extract the `(line, character)` start of a goto-definition result.
/// The server returns a scalar `Location` (a JSON object with `uri` and
/// `range`); panics if the result is not a single location object.
fn definition_start(result: &Value) -> (u64, u64) {
    let range = result
        .get("range")
        .unwrap_or_else(|| panic!("definition result must carry a range; got {result:?}"));
    let start = range.get("start").expect("range.start");
    (
        start.get("line").and_then(Value::as_u64).expect("line"),
        start
            .get("character")
            .and_then(Value::as_u64)
            .expect("character"),
    )
}

fn definition_uri(result: &Value) -> String {
    result
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("definition result must carry a uri; got {result:?}"))
        .to_owned()
}

#[test]
fn lsp_definition_resolves_local_to_let_declaration() {
    // Go-to-definition on a use of a `let`-bound local resolves to the
    // `let` declaration on a different line.
    //   line 0: "module pkg/main;"
    //   line 1: "pub fn run() -> . {"
    //   line 2: "  let payload = ();"
    //   line 3: "  payload"
    //   line 4: "}"
    let dir = TempDir::new("def-local");
    let source = "module pkg/main;\npub fn run() -> . {\n  let payload = ();\n  payload\n}\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Cursor on the `payload` use (line 3, char 2..9).
    let result = send_definition(&mut lsp, &uri, 3, 4);
    assert!(
        !result.is_null(),
        "go-to-def on a local use must resolve to its declaration; got null"
    );
    let (line, _col) = definition_start(&result);
    assert_eq!(
        line, 2,
        "local use must resolve to the `let` declaration on line 2; got line {line}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_definition_resolves_fn_param_to_declaration() {
    // Go-to-definition on a use of a `fn` value parameter resolves to
    // the parameter binder in the signature.
    //   line 0: "module pkg/main;"
    //   line 1: "pub fn echo(x: .) -> . {"
    //   line 2: "  x"
    //   line 3: "}"
    let dir = TempDir::new("def-param");
    let source = "module pkg/main;\npub fn echo(x: .) -> . {\n  x\n}\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Cursor on the `x` use (line 2, char 2).
    let result = send_definition(&mut lsp, &uri, 2, 2);
    assert!(
        !result.is_null(),
        "go-to-def on a value-param use must resolve to its declaration; got null"
    );
    let (line, _col) = definition_start(&result);
    assert_eq!(
        line, 1,
        "value-param use must resolve to the signature binder on line 1; got line {line}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_definition_resolves_type_param_to_binder() {
    // Go-to-definition on a use of a type parameter in a type position
    // resolves to its `[A]` binder in the signature.
    //   line 1: "pub fn id[A](x: A) -> A {"
    // The `[A]` binder is on line 1; the `A` use in `x: A` is also on
    // line 1 but the binder's own span is what we resolve to.
    let dir = TempDir::new("def-tyvar");
    let source = "module pkg/main;\npub fn id[A](x: A) -> A {\n  x\n}\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Cursor on the `A` in `-> A` (line 1, char 22).
    let result = send_definition(&mut lsp, &uri, 1, 22);
    assert!(
        !result.is_null(),
        "go-to-def on a type-param use must resolve to its binder; got null"
    );
    let (line, col) = definition_start(&result);
    // The exact `A` name token starts on line 1 at char 10.
    assert_eq!(line, 1, "type-param binder is on line 1; got line {line}");
    assert_eq!(
        col, 10,
        "type-param binder name `A` starts at char 10; got {col}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_definition_does_not_cross_module_for_same_named_local() {
    // Two modules each declare a local `payload`. Go-to-definition on
    // `pkg/main`'s use must resolve within `pkg/main`, never hop to the
    // same-named local in `pkg/other` — locals are module-scoped.
    let dir = TempDir::new("def-xmod");
    let main_src = "module pkg/main;\npub fn run() -> . {\n  let payload = ();\n  payload\n}\n";
    let other_src = "module pkg/other;\npub fn go() -> . {\n  let payload = ();\n  payload\n}\n";
    let main_path = dir.write("pkg/main.kio", main_src);
    dir.write("pkg/other.kio", other_src);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let main_uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &main_uri, main_src);

    let result = send_definition(&mut lsp, &main_uri, 3, 4);
    assert!(
        !result.is_null(),
        "go-to-def on a local use must resolve within its own module; got null"
    );
    let resolved_uri = definition_uri(&result);
    assert!(
        resolved_uri.ends_with("pkg/main.kio"),
        "same-named local must resolve in pkg/main, never pkg/other; got {resolved_uri}"
    );
    let (line, _col) = definition_start(&result);
    assert_eq!(line, 2, "must resolve to pkg/main's `let` on line 2");

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// documentSymbol and foldingRange — added in lsp/06.
// ============================================================

#[test]
fn lsp_capabilities_advertise_document_symbol_and_folding_range() {
    // The initialize response must advertise documentSymbolProvider
    // and foldingRangeProvider.
    let dir = TempDir::new("caps-06");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    assert!(
        caps.get("documentSymbolProvider").is_some(),
        "server must advertise documentSymbolProvider; caps = {caps:?}"
    );
    assert!(
        caps.get("foldingRangeProvider").is_some(),
        "server must advertise foldingRangeProvider; caps = {caps:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_document_symbol_returns_symbols_for_multi_item_file() {
    // Open a file with mixed top-level items and assert that
    // documentSymbol returns one entry per item in source order.
    let dir = TempDir::new("sym");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn run() -> . { () }\n",
        "\n",
        "type Same[a] = a;\n",
        "\n",
        "newtype Wrap[a] : a { pub constructor mk_wrap; pub projector un_wrap; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "documentSymbol must not return a JSON-RPC error; got: {resp:?}"
    );

    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    // Result is either null (document not yet analyzed) or an array
    // of DocumentSymbol objects.
    if !result.is_null() {
        let arr = result
            .as_array()
            .expect("documentSymbol result must be an array");
        assert_eq!(arr.len(), 3, "expected 3 symbols; got: {result:?}");
        // Source order: run, Same, Wrap.
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            ["run", "Same", "Wrap"],
            "unexpected symbol names: {names:?}"
        );
        // Each entry must have name, kind, range, selectionRange.
        for sym in arr {
            assert!(sym.get("name").is_some(), "symbol missing name: {sym:?}");
            assert!(sym.get("kind").is_some(), "symbol missing kind: {sym:?}");
            assert!(sym.get("range").is_some(), "symbol missing range: {sym:?}");
            assert!(
                sym.get("selectionRange").is_some(),
                "symbol missing selectionRange: {sym:?}"
            );
        }
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_document_symbol_stale_snapshot_serves_from_overlay() {
    // Send a didOpen but no didChange — the server may not have
    // finished analysis yet. documentSymbol must still return a
    // correct result derived from the overlay text (not from a
    // possibly-absent analysis snapshot).
    let dir = TempDir::new("sym-stale");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    // Send didOpen but don't wait for analysis — fire documentSymbol
    // immediately to test the stale / overlay path.
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "documentSymbol immediately after didOpen must not error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let symbols = result
        .as_array()
        .expect("documentSymbol should use overlay text immediately");
    assert_eq!(symbols.len(), 1, "expected the `run` symbol: {result:?}");

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_document_symbol_uses_lazy_parse_when_body_is_broken() {
    let dir = TempDir::new("sym-lazy-body");
    let source = "module pkg/main;\n\npub fn run() -> . { let x = ; () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "documentSymbol should not error on a broken body; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let symbols = result
        .as_array()
        .expect("documentSymbol should return header symbols from lazy parse");
    assert_eq!(symbols.len(), 1, "expected the `run` symbol: {result:?}");
    assert_eq!(symbols[0].get("name").and_then(Value::as_str), Some("run"));

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_document_symbol_selects_scoped_rec_member_name_from_lazy_overlay() {
    let dir = TempDir::new("sym-lazy-scoped-rec");
    let disk_source = "module pkg/main;\n\nfn disk_only() -> . { () }\n";
    let overlay_source = concat!(
        "module pkg/main;\n",
        "rec(loop) {\n",
        "  pub(pkg) fn pkg(value: .) -> . { let broken = ; rec pkg(value) }\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", disk_source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": overlay_source,
            }
        }),
    );

    let id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let response = lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id));
    assert!(response.get("error").is_none(), "{response:?}");
    let symbols = response
        .get("result")
        .and_then(Value::as_array)
        .expect("documentSymbol should use the lazy overlay");
    assert_eq!(symbols.len(), 1, "{response:?}");
    assert_eq!(symbols[0].get("name").and_then(Value::as_str), Some("pkg"));
    assert_eq!(
        json_range(symbols[0].get("selectionRange").expect("selectionRange")),
        (2, 14, 2, 17)
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_folding_range_returns_ranges_for_multi_brace_file() {
    // A file with a function body (outer brace) and a nested match
    // expression (inner brace). foldingRange must return at least
    // two ranges.
    let dir = TempDir::new("fold");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn run(x: Bool) -> . {\n",
        "    let r = match x {\n",
        "        | true -> .\n",
        "        | false -> .\n",
        "    };\n",
        "    r\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "foldingRange must not return a JSON-RPC error; got: {resp:?}"
    );

    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    if !result.is_null() {
        let arr = result
            .as_array()
            .expect("foldingRange result must be an array");
        assert!(
            arr.len() >= 2,
            "expected at least 2 folding ranges for nested braces; got: {result:?}"
        );
        // Each range must have startLine and endLine.
        for r in arr {
            assert!(
                r.get("startLine").is_some(),
                "folding range missing startLine: {r:?}"
            );
            assert!(
                r.get("endLine").is_some(),
                "folding range missing endLine: {r:?}"
            );
        }
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_folding_range_on_single_line_body_returns_empty() {
    // A file where every brace group is on one line. The server must
    // return an empty array (no collapse points).
    let dir = TempDir::new("fold-empty");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let id = lsp.send_request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "foldingRange must not return a JSON-RPC error; got: {resp:?}"
    );

    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    if !result.is_null() {
        let arr = result
            .as_array()
            .expect("foldingRange result must be an array");
        assert!(
            arr.is_empty(),
            "single-line brace groups must not produce folding ranges; got: {result:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// textDocument/formatting — added in lsp/08.
// ============================================================

/// Send a `textDocument/formatting` request and wait for the response.
/// Returns the JSON `result` field (`null` or a `TextEdit[]`).
fn send_formatting(lsp: &mut LspProcess, uri: &str) -> Value {
    let id = lsp.send_request(
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

#[test]
fn lsp_capabilities_advertise_document_formatting_provider() {
    // The initialize response must advertise documentFormattingProvider.
    let dir = TempDir::new("caps-08");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    assert!(
        caps.get("documentFormattingProvider").is_some(),
        "server must advertise documentFormattingProvider; caps = {caps:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_formatting_non_canonical_returns_single_edit() {
    // Open a file whose body is not in canonical form (the formatter
    // would add a blank line between the module declaration and the
    // first fn). Formatting must return exactly one full-file
    // replacement edit whose new_text equals `kio fmt`'s output.
    let dir = TempDir::new("fmt-noncanonical");
    // Non-canonical: no blank line between module header and fn.
    let non_canonical = "module pkg/main;\nfn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", non_canonical);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": non_canonical,
            }
        }),
    );
    // Give the server time to register the overlay.
    let _ = lsp.try_recv_for(Duration::from_millis(200));

    let result = send_formatting(&mut lsp, &uri);
    assert!(
        !result.is_null(),
        "formatting must return a TextEdit array, not null"
    );
    let edits = result.as_array().expect("result must be an array");
    assert_eq!(
        edits.len(),
        1,
        "non-canonical source must produce exactly one edit; got: {result:?}"
    );
    let edit = &edits[0];
    // The range must start at (0, 0).
    assert_eq!(
        edit.get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(Value::as_u64),
        Some(0),
        "edit range must start at line 0"
    );
    assert_eq!(
        edit.get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("character"))
            .and_then(Value::as_u64),
        Some(0),
        "edit range must start at character 0"
    );
    // The new text must be non-empty and differ from the input.
    let new_text = edit
        .get("newText")
        .and_then(Value::as_str)
        .expect("newText");
    assert_ne!(
        new_text, non_canonical,
        "the edit must change the buffer (non-canonical → canonical)"
    );
    assert!(
        !new_text.is_empty(),
        "newText must not be empty for a non-canonical file"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_formatting_already_canonical_returns_empty() {
    // A file that is already in canonical form. The server must return
    // an empty edit list — applying it leaves the buffer unchanged.
    let dir = TempDir::new("fmt-canonical");
    // Canonical form: blank line between module header and fn, fn body
    // on its own line if it fits inline, trailing newline.
    let canonical = "module pkg/main;\n\nfn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", canonical);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": canonical,
            }
        }),
    );
    let _ = lsp.try_recv_for(Duration::from_millis(200));

    let result = send_formatting(&mut lsp, &uri);
    assert!(
        !result.is_null(),
        "formatting must return a (possibly empty) TextEdit array, not null"
    );
    let edits = result.as_array().expect("result must be an array");
    assert!(
        edits.is_empty(),
        "already-canonical source must produce no edits; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_formatting_parse_broken_returns_empty() {
    // A file with a parse error. The server must return an empty edit
    // list — no JSON-RPC error, no MethodNotFound, just silence.
    let dir = TempDir::new("fmt-broken");
    let broken = "module pkg/main;\n\nfn run() -> . { (\n";
    let main_path = dir.write("pkg/main.kio", broken);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": broken,
            }
        }),
    );
    // The server will publish a parse-error diagnostic; consume it
    // so the channel is clear before we send the formatting request.
    let _ = lsp.try_recv_for(Duration::from_millis(600));

    let id = lsp.send_request(
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    // Must not return a JSON-RPC error.
    assert!(
        resp.get("error").is_none(),
        "formatting on parse-broken source must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    // Either null (document not found in overlay — unlikely since we
    // sent didOpen) or an empty array.
    if !result.is_null() {
        let edits = result.as_array().expect("result must be an array");
        assert!(
            edits.is_empty(),
            "parse-broken source must produce no edits; got: {result:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// textDocument/completion — added in lsp/07.
// ============================================================

/// Send a `textDocument/completion` request and wait for the response.
/// Returns the JSON `result` field (`null` or a CompletionList).
fn send_completion(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let id = lsp.send_request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "triggerKind": 1 },
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

fn completion_labels(result: &Value) -> Vec<&str> {
    result
        .get("items")
        .and_then(Value::as_array)
        .expect("completion result must have an items array")
        .iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str))
        .collect()
}

#[path = "lsp_smoke/completion_context.rs"]
mod completion_context;

#[path = "lsp_smoke/block_calls.rs"]
mod block_calls;

#[path = "lsp_smoke/completion_closed.rs"]
mod completion_closed;

#[path = "lsp_smoke/completion_calls.rs"]
mod completion_calls;

#[path = "lsp_smoke/identifier_names.rs"]
mod identifier_names;

#[test]
fn lsp_capabilities_advertise_completion_provider() {
    // The initialize response must advertise completionProvider with
    // resolveProvider = false and no trigger characters.
    let dir = TempDir::new("caps-07");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    let cp = caps
        .get("completionProvider")
        .expect("server must advertise completionProvider; caps = {caps:?}");
    assert_eq!(
        cp.get("resolveProvider").and_then(Value::as_bool),
        Some(false),
        "completionProvider.resolveProvider must be false"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_exact_scope_and_shadow_identity() {
    let dir = TempDir::new("completion-exact-scope");
    let source = concat!(
        "module pkg/main;\n",
        "/// Documentation belonging only to the function.\n",
        "fn value() -> . { () }\n",
        "fn consumer(value: .) -> . { value }\n",
        "fn later() -> . { () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let result = send_completion(&mut lsp, &uri, 3, 30);
    let items = result["items"].as_array().expect("completion items");
    let local = items
        .iter()
        .find(|item| item["label"] == "value")
        .expect("local value");
    assert!(local.get("documentation").is_none(), "{local}");
    assert_eq!(local["detail"], ".", "{local}");
    assert!(
        !items
            .iter()
            .any(|item| item["label"] == "consumer" || item["label"] == "later"),
        "{items:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_returns_module_level_fn_names() {
    // Ordinary module declarations follow source order, including the
    // exclusion of a nonrecursive function from its own body.
    let dir = TempDir::new("completion-fns");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub fn run() -> . { () }\n",
        "fn helper() -> . { () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Position inside `run`'s body: line 2, character 21 (after `{`).
    let result = send_completion(&mut lsp, &uri, 2, 21);

    assert!(
        !result.is_null(),
        "completion inside fn body must return a CompletionList, not null"
    );
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .expect("completion result must have an items array");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(Value::as_str))
        .collect();
    assert!(
        !labels.contains(&"run"),
        "self fn `run` must not appear in completion; labels = {labels:?}"
    );
    assert!(
        !labels.contains(&"helper"),
        "later fn `helper` must not appear in completion; labels = {labels:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_inside_nested_let() {
    // A fn body with a nested `let y = …`. Completion inside the body
    // continuation must list both `x` (fn param) and `y` (let-bound).
    let dir = TempDir::new("completion-let");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn f(x: .) -> . {\n",
        "    let y = x;\n",
        "    y\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Position on the `y` expression line (line 4, char 4).
    let result = send_completion(&mut lsp, &uri, 4, 4);

    assert!(
        !result.is_null(),
        "completion inside let-body must return a CompletionList"
    );
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .expect("items array");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(Value::as_str))
        .collect();
    assert!(
        labels.contains(&"x"),
        "fn param `x` must appear in completion; labels = {labels:?}"
    );
    assert!(
        labels.contains(&"y"),
        "let-bound `y` must appear in completion; labels = {labels:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_shadowing_deduplication() {
    // `fn f(x: .) -> . { let x = (); x }` — the let-bound `x`
    // shadows the parameter `x`. The completion list must contain
    // exactly one `x` entry.
    let dir = TempDir::new("completion-shadow");
    let source = "module pkg/main;\n\nfn f(x: .) -> . { let x = (); x }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Position on the trailing `x` expression (line 2, char 31).
    let result = send_completion(&mut lsp, &uri, 2, 31);

    assert!(
        !result.is_null(),
        "completion inside shadowed-let body must return a list"
    );
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .expect("items array");
    let x_count = items
        .iter()
        .filter(|i| i.get("label").and_then(Value::as_str) == Some("x"))
        .count();
    assert_eq!(
        x_count, 1,
        "shadowed `x` must appear exactly once; items = {items:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_use_import_visible() {
    // An imported name is available at an expression-reference position,
    // even before its provider can be resolved.
    let dir = TempDir::new("completion-use");
    let source = concat!(
        "module pkg/main;\n",
        "import pkg/helper(h);\n",
        "\n",
        "fn f() -> . { () }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let offset = source.find("{ ()").unwrap() + 1;
    let (line, character) = source_position(source, offset);
    let result = send_completion(&mut lsp, &uri, line, character);

    // The file has an `import pkg/helper(h);` that won't typecheck
    // (no dep), but the parser succeeds, so completion should work.
    assert!(
        !result.is_null(),
        "completion must return a list even when the package has unresolved `import`; got null"
    );
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .expect("items array");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(Value::as_str))
        .collect();
    assert!(
        labels.contains(&"h"),
        "`h` imported via `import` must appear in completion; labels = {labels:?}"
    );

    // After the completed unit expression, another name would require a
    // separator. This is not an expression-reference insertion position.
    let offset = source.find("() }").unwrap() + 2;
    let (line, character) = source_position(source, offset);
    let result = send_completion(&mut lsp, &uri, line, character);
    assert!(completion_labels(&result).is_empty());

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_completion_at_top_level_no_fn_locals() {
    // Completion at the module header line must not include fn-local
    // names (parameters, let-bound variables).
    let dir = TempDir::new("completion-toplevel");
    let source = "module pkg/main;\n\nfn f(x: .) -> . { x }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Cursor on the module header (line 0, char 5).
    let result = send_completion(&mut lsp, &uri, 0, 5);

    // Result may be null (no parse-accessible items at module header)
    // or a list without fn-local `x`.
    if !result.is_null() {
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .expect("items array");
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|i| i.get("label").and_then(Value::as_str))
            .collect();
        assert!(
            !labels.contains(&"x"),
            "fn-local `x` must not appear at module-header position; labels = {labels:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_completion_obeys_explicit_recursive_scopes_and_source_order() {
    let dir = TempDir::new("completion-recursive-type-scopes");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type Earlier = .;\n",
        "newtype Unmarked : Later { constructor mk_unmarked; projector un_unmarked; };\n",
        "rec newtype Singleton : . | Singleton { constructor mk_singleton; projector un_singleton; };\n",
        "rec labels Label_tree = { leaf: . } | { branch: Label_tree };\n",
        "rec {\n",
        "  type Group_alias = Group_node;\n",
        "  newtype Group_node[A] : Group_alias & A { constructor mk_group; projector un_group; };\n",
        "}\n",
        "type Later = .;\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let _ = wait_for_publish(&mut lsp, &uri);

    let labels_after = |lsp: &mut LspProcess, marker: &str| {
        let offset = source.find(marker).expect("completion marker") + marker.len();
        let position = source_position(source, offset);
        completion_labels(&send_completion(lsp, &uri, position.0, position.1))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    let unmarked = labels_after(&mut lsp, "Unmarked : ");
    assert!(unmarked.iter().any(|name| name == "Earlier"));
    for excluded in [
        "Unmarked",
        "Singleton",
        "Label_tree",
        "Group_alias",
        "Later",
    ] {
        assert!(
            !unmarked.iter().any(|name| name == excluded),
            "unmarked/later head {excluded} escaped source order: {unmarked:?}"
        );
    }

    let singleton = labels_after(&mut lsp, "Singleton : ");
    assert!(singleton.iter().any(|name| name == "Singleton"));
    assert!(!singleton.iter().any(|name| name == "Later"));

    let labels_scope = labels_after(&mut lsp, "branch: ");
    for expected in ["Label_tree", "Leaf", "Branch"] {
        assert!(
            labels_scope.iter().any(|name| name == expected),
            "recursive labels completion omitted {expected}: {labels_scope:?}"
        );
    }

    let group = labels_after(&mut lsp, "Group_node[A] : ");
    for expected in ["Group_alias", "Group_node", "A"] {
        assert!(
            group.iter().any(|name| name == expected),
            "mutual-group completion omitted {expected}: {group:?}"
        );
    }
    assert_eq!(group.iter().filter(|name| name.as_str() == "A").count(), 1);
    assert!(!group.iter().any(|name| name == "Later"));

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_completion_recovery_uses_declarations_not_capitalized_text() {
    for (case, source, marker, expected) in [
        (
            "lazy-body",
            concat!(
                "module pkg/main;\n",
                "type Earlier = .;\n",
                "fn broken() -> . { let .(value: ); \"StringLeak\" } // CommentLeak\n",
                "type Later = .;\n",
            ),
            "value: ",
            vec!["Earlier"],
        ),
        (
            "incomplete-group",
            concat!(
                "module pkg/main;\n",
                "type Earlier = .;\n",
                "rec {\n",
                "  type A = B;\n",
                "  newtype B : \n",
                "  // CommentLeak\n",
                "  \"StringLeak\"\n",
            ),
            "B : ",
            vec!["Earlier", "A", "B"],
        ),
    ] {
        let dir = TempDir::new(&format!("completion-recovery-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );
        let _ = wait_for_publish(&mut lsp, &uri);

        let offset = source.find(marker).expect("completion marker") + marker.len();
        let position = source_position(source, offset);
        let response = send_completion(&mut lsp, &uri, position.0, position.1);
        let labels = completion_labels(&response);
        for expected in expected {
            assert!(
                labels.contains(&expected),
                "{case}: missing declaration candidate {expected}: {labels:?}"
            );
        }
        for leaked in ["Later", "CommentLeak", "StringLeak"] {
            assert!(
                !labels.contains(&leaked),
                "{case}: non-scope capitalized text leaked into completion: {labels:?}"
            );
        }
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_incomplete_generic_recursive_labels_complete_their_generated_heads() {
    for (case, source, expected, excluded) in [
        (
            "anonymous",
            "module pkg/main;\nimport pkg/dep(Imported);\nrec labels { list[A] <U> : Pair(A, bogus[B]: ",
            vec!["A", "U", "List", "Imported"],
            vec!["B", "Bogus"],
        ),
        (
            "named",
            "module pkg/main;\nimport pkg/dep(Imported);\nrec labels Tree[A] = { list[B] <U> : Pair(B, bogus[C]: ",
            vec!["A", "B", "U", "Tree", "List", "Imported"],
            vec!["C", "Bogus"],
        ),
    ] {
        let dir = TempDir::new(&format!("completion-incomplete-generic-rec-labels-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write(
            "pkg/dep.kio",
            "module pkg/dep;\npub type Imported[A] = A;\n",
        );
        dir.write_pkg_root_package();
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );
        let _ = wait_for_publish(&mut lsp, &uri);

        let position = source_position(source, source.len());
        let response = send_completion(&mut lsp, &uri, position.0, position.1);
        let labels = completion_labels(&response);
        for expected in expected {
            assert!(
                labels.contains(&expected),
                "{case}: missing incomplete recursive-label scope name {expected}: {labels:?}"
            );
        }
        for excluded in excluded {
            assert!(
                !labels.contains(&excluded),
                "{case}: payload text {excluded} leaked into completion: {labels:?}"
            );
        }
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_incomplete_recursive_label_reuse_does_not_create_a_generated_head() {
    let dir = TempDir::new("completion-incomplete-rec-label-reuse");
    let source = "module pkg/main;\ntype Earlier = .;\nrec labels { first: _";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let _ = wait_for_publish(&mut lsp, &uri);

    let offset = source.find("first: ").expect("reuse marker") + "first: ".len();
    let position = source_position(source, offset);
    let response = send_completion(&mut lsp, &uri, position.0, position.1);
    let labels = completion_labels(&response);
    assert!(
        labels.contains(&"Earlier"),
        "recovery omitted an earlier valid type: {labels:?}"
    );
    assert!(
        !labels.contains(&"First"),
        "an active reuse marker became a fresh generated head: {labels:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_incomplete_unmarked_type_declarations_keep_only_active_binders() {
    for (case, source, expected, excluded) in [
        (
            "newtype",
            "module pkg/main;\nimport pkg/dep(Imported);\ntype Earlier = .;\nnewtype Box[A] <E> : ",
            vec!["A", "E", "Earlier", "Imported"],
            vec!["Box"],
        ),
        (
            "labels",
            "module pkg/main;\nimport pkg/dep(Imported);\nlabels Tree[A] = { first[X]: ., list[B] <E> : ",
            vec!["A", "B", "E", "First", "Imported"],
            vec!["Tree", "List", "X"],
        ),
    ] {
        let dir = TempDir::new(&format!("completion-incomplete-unmarked-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write(
            "pkg/dep.kio",
            "module pkg/dep;\npub type Imported[A] = A;\n",
        );
        dir.write_pkg_root_package();
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );
        let _ = wait_for_publish(&mut lsp, &uri);

        let position = source_position(source, source.len());
        let response = send_completion(&mut lsp, &uri, position.0, position.1);
        let labels = completion_labels(&response);
        for expected in expected {
            assert!(
                labels.contains(&expected),
                "{case}: missing in-scope type name {expected}: {labels:?}"
            );
        }
        for excluded in excluded {
            assert!(
                !labels.contains(&excluded),
                "{case}: unfinished declaration leaked {excluded}: {labels:?}"
            );
        }
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_valid_recursive_groups_retract_every_diagnostic_after_reanalysis() {
    for (case, valid) in [
        (
            "mutual-newtypes",
            concat!(
                "module pkg/main;\n",
                "rec {\n",
                "  newtype A : B { constructor mk_a; projector un_a; };\n",
                "  newtype B : A { constructor mk_b; projector un_b; };\n",
                "}\n",
            ),
        ),
        (
            "multiple-label-knots",
            concat!(
                "module pkg/main;\n",
                "rec labels Pair = { first: . | First, second: . | Second };\n",
            ),
        ),
        (
            "mutual-named-labels",
            concat!(
                "module pkg/main;\n",
                "rec {\n",
                "  labels A = { to_b: B };\n",
                "  labels B = { to_a: A };\n",
                "}\n",
            ),
        ),
    ] {
        let dir = TempDir::new(&format!("valid-recursive-reanalysis-{case}"));
        let invalid = format!("{valid}fn broken() -> Missing {{ () }}\n");
        let main_path = dir.write("pkg/main.kio", &invalid);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": invalid,
                }
            }),
        );
        let initial = wait_for_publish_version(&mut lsp, &uri, 1);
        assert!(
            initial
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|diagnostics| !diagnostics.is_empty()),
            "{case}: the sentinel error must force a diagnostic publication: {initial:?}"
        );

        send_full_text_change(&mut lsp, &uri, 2, valid);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "{case}: the valid recursive construct must retract every diagnostic: {cleared:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_mutual_type_references_navigate_to_exact_group_member_heads() {
    let dir = TempDir::new("definition-recursive-type-group-members");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  type Forest = Tree;\n",
        "  newtype Tree : Forest { constructor mk; projector un; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let tree = send_definition(&mut lsp, &uri, 3, 17);
    assert_eq!(
        tree.pointer("/range/start/line").and_then(Value::as_u64),
        Some(4),
        "the alias-body reference must navigate to the exact grouped newtype head: {tree:?}"
    );
    assert_eq!(
        tree.pointer("/range/start/character")
            .and_then(Value::as_u64),
        Some(10),
        "the grouped newtype definition range must begin at `Tree`: {tree:?}"
    );

    let forest = send_definition(&mut lsp, &uri, 4, 18);
    assert_eq!(
        forest.pointer("/range/start/line").and_then(Value::as_u64),
        Some(3),
        "the newtype payload reference must navigate to the exact grouped alias head: {forest:?}"
    );
    assert_eq!(
        forest
            .pointer("/range/start/character")
            .and_then(Value::as_u64),
        Some(7),
        "the grouped alias definition range must begin at `Forest`: {forest:?}"
    );

    let alias_hover = wait_for_hover(&mut lsp, &uri, 4, 18);
    assert!(
        hover_type(&alias_hover).contains("type Forest"),
        "a grouped alias reference must have a finite identity-preserving hover: {alias_hover:?}"
    );

    let symbols_id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(symbols_id));
    let symbol_names = symbols
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("recursive type members must remain document symbols: {symbols:?}")
        })
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(symbol_names, vec!["Forest", "Tree"]);

    let folding_id = lsp.send_request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let folding =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(folding_id));
    assert!(
        folding
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(|ranges| ranges.iter().any(|range| {
                range.get("startLine").and_then(Value::as_u64) == Some(2)
                    && range.get("endLine").and_then(Value::as_u64) == Some(5)
            })),
        "the recursive group braces must remain foldable: {folding:?}"
    );

    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 18 },
            "context": { "includeDeclaration": true },
        }),
    );
    let references = lsp
        .recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(references_id));
    let mut ranges = references
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("grouped alias references must resolve: {references:?}"))
        .iter()
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    assert_eq!(ranges, vec![(3, 7, 3, 13), (4, 17, 4, 23)]);

    let renamed = send_rename(&mut lsp, &uri, 4, 18, "Woods");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let edits = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("edits"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("grouped alias rename must publish edits: {renamed:?}"));
    assert_eq!(edits.len(), 2, "{renamed:?}");
    let renamed_source = apply_lsp_text_edits(source, edits);
    assert!(renamed_source.contains("type Woods = Tree"));
    assert!(renamed_source.contains("newtype Tree : Woods"));
    send_full_text_change(&mut lsp, &uri, 2, &renamed_source);
    let renamed_definition = send_definition(&mut lsp, &uri, 4, 18);
    assert_eq!(
        renamed_definition
            .pointer("/range/start/line")
            .and_then(Value::as_u64),
        Some(3),
        "the complete renamed group must reanalyse with the same exact identity: {renamed_definition:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_identity_alias_members_keep_alias_heads_and_terminal_member_identity() {
    let dir = TempDir::new("identity-alias-newtype-member-navigation");
    let origin_source = concat!(
        "module pkg/origin;\n",
        "pub newtype Tag : . { pub constructor make; pub projector open; };\n",
        "fn local() -> Tag { Tag.make(()) }\n",
        "fn inspect(value: Tag) -> . { Tag.open(value) }\n",
    );
    let relay_source = concat!(
        "module pkg/relay;\n",
        "import pkg/origin as source;\n",
        "pub type Tag = source.Tag;\n",
        "fn make() -> . { () }\n",
        "fn open(value: .) -> . { value }\n",
    );
    let consumer_source = concat!(
        "module pkg/consumer;\n",
        "import pkg/relay as forwarded;\n",
        "fn first() -> forwarded.Tag { forwarded.Tag.make(()) }\n",
        "fn second(value: forwarded.Tag) -> . { forwarded.Tag.open(value) }\n",
        "fn third() -> forwarded.Tag { forwarded.Tag.make(()) }\n",
        "fn fourth(value: forwarded.Tag) -> . { forwarded.Tag.open(value) }\n",
    );
    let origin_path = dir.write("pkg/origin.kio", origin_source);
    let relay_path = dir.write("pkg/relay.kio", relay_source);
    let consumer_path = dir.write("pkg/consumer.kio", consumer_source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let origin_uri = path_to_file_uri(&origin_path);
    let relay_uri = path_to_file_uri(&relay_path);
    let consumer_uri = path_to_file_uri(&consumer_path);
    open_and_wait_for_analysis(&mut lsp, &consumer_uri, consumer_source);

    let alias_head_offset =
        consumer_source.find("forwarded.Tag {").expect("alias head") + "forwarded.".len();
    let alias_head_position = source_position(consumer_source, alias_head_offset);
    let alias_definition = send_definition(
        &mut lsp,
        &consumer_uri,
        alias_head_position.0,
        alias_head_position.1,
    );
    assert_eq!(
        definition_uri(&alias_definition),
        relay_uri,
        "the written type head must navigate to the source alias: {alias_definition:?}"
    );
    let relay_alias_offset =
        relay_source.find("type Tag").expect("alias declaration") + "type ".len();
    let relay_alias_position = source_position(relay_source, relay_alias_offset);
    assert_eq!(
        json_range(
            alias_definition
                .get("range")
                .expect("alias definition range")
        ),
        (
            relay_alias_position.0,
            relay_alias_position.1,
            relay_alias_position.0,
            relay_alias_position.1 + 3,
        )
    );

    let references_at = |lsp: &mut LspProcess,
                         position: (u64, u64),
                         include_declaration: bool|
     -> Vec<(String, (u64, u64, u64, u64))> {
        let id = lsp.send_request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": consumer_uri },
                "position": { "line": position.0, "character": position.1 },
                "context": { "includeDeclaration": include_declaration },
            }),
        );
        let response =
            lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(id));
        let mut locations = response
            .get("result")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("identity-alias member references: {response:?}"))
            .iter()
            .map(|location| {
                (
                    location
                        .get("uri")
                        .and_then(Value::as_str)
                        .expect("reference URI")
                        .to_owned(),
                    json_range(location.get("range").expect("reference range")),
                )
            })
            .collect::<Vec<_>>();
        locations.sort_unstable();
        locations
    };

    let member_case = |declaration_needle: &str,
                       direct_needle: &str,
                       consumer_needle: &str,
                       member_name: &str,
                       rename_to: &str,
                       lsp: &mut LspProcess| {
        let declaration = origin_source
            .find(declaration_needle)
            .expect("member declaration")
            + declaration_needle.len()
            - member_name.len();
        let direct = origin_source
            .find(direct_needle)
            .expect("direct member use")
            + direct_needle.len()
            - member_name.len();
        let first_consumer = consumer_source
            .find(consumer_needle)
            .expect("first alias member use")
            + consumer_needle.len()
            - member_name.len();
        let second_consumer = consumer_source
            .rfind(consumer_needle)
            .expect("second alias member use")
            + consumer_needle.len()
            - member_name.len();
        assert_ne!(first_consumer, second_consumer);
        let position = source_position(consumer_source, first_consumer);

        let mut uses = vec![
            (
                origin_uri.clone(),
                source_range(origin_source, direct, member_name.len()),
            ),
            (
                consumer_uri.clone(),
                source_range(consumer_source, first_consumer, member_name.len()),
            ),
            (
                consumer_uri.clone(),
                source_range(consumer_source, second_consumer, member_name.len()),
            ),
        ];
        uses.sort_unstable();
        let without_declaration = references_at(lsp, position, false);

        let mut with_declaration = uses.clone();
        with_declaration.push((
            origin_uri.clone(),
            source_range(origin_source, declaration, member_name.len()),
        ));
        with_declaration.sort_unstable();
        let actual_with_declaration = references_at(lsp, position, true);

        let renamed = send_rename(lsp, &consumer_uri, position.0, position.1, rename_to);
        assert!(renamed.get("error").is_none(), "{renamed:?}");
        let edit = renamed
            .get("result")
            .unwrap_or_else(|| panic!("member rename result: {renamed:?}"));
        let mut edits = edit
            .get("documentChanges")
            .and_then(Value::as_array)
            .expect("rename documentChanges")
            .iter()
            .flat_map(|change| {
                let uri = change
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(Value::as_str)
                    .expect("rename URI")
                    .to_owned();
                change
                    .get("edits")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(move |edit| {
                        assert_eq!(edit.get("newText").and_then(Value::as_str), Some(rename_to));
                        (
                            uri.clone(),
                            json_range(edit.get("range").expect("rename range")),
                        )
                    })
            })
            .collect::<Vec<_>>();
        edits.sort_unstable();
        assert!(
            workspace_edit_edits_for_uri(edit, &relay_uri).is_none(),
            "same-spelling ordinary declarations must retain distinct identities: {renamed:?}"
        );
        assert_eq!(
            (without_declaration, actual_with_declaration, edits),
            (uses, with_declaration.clone(), with_declaration,),
            "references and rename must apply the explicit member declaration policy together"
        );
    };

    member_case(
        "projector open",
        "Tag.open",
        "forwarded.Tag.open",
        "open",
        "unpack",
        &mut lsp,
    );
    member_case(
        "constructor make",
        "Tag.make",
        "forwarded.Tag.make",
        "make",
        "build",
        &mut lsp,
    );

    let member_offset = consumer_source.find("make(())").expect("member use");
    let member_position = source_position(consumer_source, member_offset);
    let member_definition = send_definition(
        &mut lsp,
        &consumer_uri,
        member_position.0,
        member_position.1,
    );
    assert_eq!(
        definition_uri(&member_definition),
        origin_uri,
        "the member must navigate to the terminal nominal declaration: {member_definition:?}"
    );
    let origin_member_offset = origin_source.find("make").expect("member declaration");
    let origin_member_position = source_position(origin_source, origin_member_offset);
    assert_eq!(
        json_range(
            member_definition
                .get("range")
                .expect("member definition range")
        ),
        (
            origin_member_position.0,
            origin_member_position.1,
            origin_member_position.0,
            origin_member_position.1 + 4,
        )
    );

    let hover = wait_for_hover(
        &mut lsp,
        &consumer_uri,
        member_position.0,
        member_position.1,
    );
    assert!(!hover.is_null(), "identity-alias member hover: {hover:?}");
    assert!(
        hover_type(&hover).contains("->"),
        "the terminal member scheme must reach hover: {hover:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_binders_shadow_same_named_recursive_group_heads() {
    let dir = TempDir::new("recursive-type-binder-shadowing");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  type Universal = Ubox(.);\n",
        "  newtype Ubox[Universal] : Universal & Utail { constructor mk_u; projector un_u; };\n",
        "  type Utail = Universal;\n",
        "}\n",
        "rec {\n",
        "  type Existential = Ebox;\n",
        "  newtype Ebox <Existential> : Existential & Etail { constructor mk_e; projector un_e; };\n",
        "  type Etail = Existential;\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    for (binder_marker, use_marker, name) in [
        ("Ubox[Universal]", "] : Universal", "Universal"),
        ("Ebox <Existential>", "> : Existential", "Existential"),
    ] {
        let binder = source.find(binder_marker).unwrap() + binder_marker.find(name).unwrap();
        let use_offset = source.find(use_marker).unwrap() + use_marker.len() - name.len();
        let binder_position = source_position(source, binder);
        let use_position = source_position(source, use_offset);
        let definition = send_definition(&mut lsp, &uri, use_position.0, use_position.1);
        assert_eq!(
            json_range(definition.get("range").expect("binder definition range")),
            (
                binder_position.0,
                binder_position.1,
                binder_position.0,
                binder_position.1 + name.len() as u64,
            ),
            "a local type binder must shadow the same-named group head: {definition:?}"
        );

        let completion_offset = source.find(use_marker).unwrap() + 4;
        let completion_position = source_position(source, completion_offset);
        let completion_response =
            send_completion(&mut lsp, &uri, completion_position.0, completion_position.1);
        let completion = completion_labels(&completion_response);
        assert_eq!(
            completion
                .iter()
                .filter(|candidate| **candidate == name)
                .count(),
            1,
            "shadowed recursive/type-binder identities must not be duplicated: {completion:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_singleton_binders_shadow_outer_type_heads() {
    let dir = TempDir::new("recursive-singleton-binder-shadowing");
    let source = concat!(
        "module pkg/main;\n",
        "type Universal = .;\n",
        "type Existential = .;\n",
        "rec newtype Ubox[Universal] : . | Ubox(Universal) { constructor mk_u; projector un_u; };\n",
        "rec newtype Ebox <Existential> : . | (Ebox & Existential) { constructor mk_e; projector un_e; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    for (binder_marker, binder_prefix, use_marker, use_prefix, name) in [
        (
            "Ubox[Universal]",
            "Ubox[",
            "Ubox(Universal)",
            "Ubox(",
            "Universal",
        ),
        (
            "Ebox <Existential>",
            "Ebox <",
            "Ebox & Existential",
            "Ebox & ",
            "Existential",
        ),
    ] {
        let binder = source.find(binder_marker).unwrap() + binder_prefix.len();
        let use_offset = source.find(use_marker).unwrap() + use_prefix.len();
        let binder_position = source_position(source, binder);
        let use_position = source_position(source, use_offset);
        let definition = send_definition(&mut lsp, &uri, use_position.0, use_position.1);
        assert_eq!(
            json_range(definition.get("range").expect("binder definition range")),
            (
                binder_position.0,
                binder_position.1,
                binder_position.0,
                binder_position.1 + name.len() as u64,
            ),
            "a singleton-local type binder must shadow the same-named outer head: {definition:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_type_identity_does_not_cross_same_leaf_module_names() {
    let dir = TempDir::new("recursive-type-same-leaf-modules");
    let main_source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  type Node = Edge;\n",
        "  newtype Edge : Node { constructor mk; projector un; };\n",
        "}\n",
    );
    let other_source = concat!(
        "module pkg/other;\n",
        "\n",
        "rec {\n",
        "  type Node = Edge;\n",
        "  newtype Edge : Node { constructor mk; projector un; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", main_source);
    dir.write("pkg/other.kio", other_source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let main_uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &main_uri, main_source);

    let definition = send_definition(&mut lsp, &main_uri, 4, 18);
    assert_eq!(
        definition_uri(&definition),
        main_uri,
        "a same-leaf alias in another module must not capture definition identity: {definition:?}"
    );
    assert_eq!(
        json_range(definition.get("range").expect("definition range")),
        (3, 7, 3, 11),
    );

    let renamed = send_rename(&mut lsp, &main_uri, 4, 18, "Main_node");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let changes = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recursive alias rename must publish edits: {renamed:?}"));
    assert_eq!(
        changes.len(),
        1,
        "same-leaf declarations in the other module must not be renamed: {renamed:?}"
    );
    assert_eq!(
        changes[0]
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Value::as_str),
        Some(main_uri.as_str()),
    );
    let edits = changes[0]
        .get("edits")
        .and_then(Value::as_array)
        .expect("main-module rename edits");
    assert_eq!(edits.len(), 2, "{renamed:?}");
    let renamed_source = apply_lsp_text_edits(main_source, edits);
    assert!(renamed_source.contains("type Main_node = Edge"));
    assert!(renamed_source.contains("newtype Edge : Main_node"));

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_later_type_diagnostic_does_not_create_navigation_authority() {
    let dir = TempDir::new("later-type-is-diagnostic-only");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type Before = Later;\n",
        "type Later = .;\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected directed later-type diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Later` is declared later and is not visible here"),
    );
    assert_eq!(
        json_range(diagnostic.get("range").expect("primary range")),
        (2, 14, 2, 19),
    );
    let related = diagnostic
        .get("relatedInformation")
        .and_then(Value::as_array)
        .and_then(|related| related.first())
        .unwrap_or_else(|| panic!("later declaration must be labelled: {diagnostic:?}"));
    assert_eq!(
        json_range(
            related
                .get("location")
                .and_then(|location| location.get("range"))
                .expect("later-declaration range"),
        ),
        (3, 5, 3, 10),
    );

    let definition = send_definition(&mut lsp, &uri, 2, 16);
    assert!(
        definition.is_null(),
        "diagnostic-only later-name lookup must not bind definition identity: {definition:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_alias_only_recursive_components_publish_totality_diagnostics() {
    for (case, source) in [
        (
            "mutual-aliases",
            concat!(
                "module pkg/main;\n",
                "rec {\n",
                "  type A = B;\n",
                "  type B = A;\n",
                "}\n",
            ),
        ),
        (
            "self-alias",
            concat!("module pkg/main;\n", "type Loop = Loop;\n"),
        ),
    ] {
        let dir = TempDir::new(&format!("alias-only-recursive-component-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish_version(&mut lsp, &uri, 1);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("{case}: expected alias-only diagnostic: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(16));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("recursive type component has no `newtype` boundary"),
        );
        let actions = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            actions
                .get("result")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "{case}: a transparent alias cycle must not acquire a nominal wrapper action: {actions:?}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_forbidden_recursive_type_spellings_offer_no_unsound_rewrite() {
    for (case, source, expected_message) in [
        (
            "rec-type",
            "module pkg/main;\n\nrec type Loop = Loop;\n",
            "`rec type` is not a declaration form",
        ),
        (
            "function-in-type-group",
            "module pkg/main;\n\nrec { fn loop() -> . { () } }\n",
            "a bare `rec { ... }` type group admits only type declarations",
        ),
    ] {
        let dir = TempDir::new(&format!("forbidden-recursive-type-spelling-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected {case} diagnostic: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(11));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some(expected_message),
        );
        let actions = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            actions
                .get("result")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "{case} must not acquire an alias-to-newtype or function-to-type rewrite: {actions:?}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_type_group_visibility_diagnostic_highlights_only_the_modifier() {
    for (case, modifier, member_modifier) in [
        ("public", "pub", "pub "),
        ("scoped", "pub(pkg)", "pub(pkg) "),
    ] {
        let dir = TempDir::new(&format!("recursive-type-group-leading-visibility-{case}"));
        let source = format!(
            "module pkg/main;\n\n{modifier} rec {{\n  // alias stays attached\n  type A = B;\n  /// nominal docs stay attached\n  newtype B : A {{ constructor mk; projector un; }};\n  // trailing group comment survives\n}}\n"
        );
        let main_path = dir.write("pkg/main.kio", &source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected group-visibility diagnostic: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(11));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("a bare `rec { ... }` type group has no leading visibility"),
        );
        assert_eq!(
            json_range(diagnostic.get("range").expect("visibility range")),
            (2, 0, 2, modifier.len() as u64),
            "the group braces are not the invalid visibility modifier",
        );

        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("title").and_then(Value::as_str)
                        == Some("Put visibility on each recursive type member")
                })
            })
            .unwrap_or_else(|| panic!("missing visibility-distribution action: {response:?}"));
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 3);
        let repaired = apply_lsp_text_edits(&source, edits);
        assert!(!repaired.contains(&format!("{modifier} rec")));
        assert!(repaired.contains(&format!("{member_modifier}type A = B")));
        assert!(repaired.contains(&format!("{member_modifier}newtype B : A")));
        for preserved in [
            "// alias stays attached",
            "/// nominal docs stay attached",
            "// trailing group comment survives",
        ] {
            assert!(
                repaired.contains(preserved),
                "lost {preserved:?}: {repaired}"
            );
        }

        send_full_text_change(&mut lsp, &uri, 2, &repaired);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the validated distribution action must produce a clean group: {cleared:?}\n{repaired}"
        );
        let stale = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            stale
                .get("result")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the version-1 diagnostic must not edit version 2: {stale:?}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_type_group_visibility_fix_is_withheld_when_a_member_already_has_visibility() {
    let dir = TempDir::new("recursive-type-group-visibility-no-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "pub rec {\n",
        "  pub type A = B;\n",
        "  newtype B : A { constructor mk; projector un; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected group-visibility diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("a bare `rec { ... }` type group has no leading visibility")
    );
    let actions = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        actions
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "member-local visibility makes distribution intent ambiguous: {actions:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_labels_keep_alias_and_generated_nominal_identities_distinct() {
    let dir = TempDir::new("definition-recursive-label-identities");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec labels Tree = { leaf: . | Leaf } | { branch: Tree & Branch };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let alias_use = source.find("branch: Tree").unwrap() + "branch: ".len();
    let alias_use_position = source_position(source, alias_use);
    let alias = send_definition(&mut lsp, &uri, alias_use_position.0, alias_use_position.1);
    assert_eq!(
        json_range(alias.get("range").expect("alias definition range")),
        (2, 11, 2, 15),
        "the recursive alias reference must navigate to the named labels alias: {alias:?}"
    );
    for (use_marker, head_marker, name) in [
        (". | Leaf", "leaf:", "Leaf"),
        ("& Branch", "branch:", "Branch"),
    ] {
        let use_offset = source.find(use_marker).unwrap() + use_marker.len() - name.len();
        let use_position = source_position(source, use_offset);
        let head_offset = source.find(head_marker).unwrap();
        let head_position = source_position(source, head_offset);
        let generated = send_definition(&mut lsp, &uri, use_position.0, use_position.1);
        assert_eq!(
            json_range(
                generated
                    .get("range")
                    .expect("generated-label definition range")
            ),
            (
                head_position.0,
                head_position.1,
                head_position.0,
                head_position.1 + head_marker.len() as u64 - 1,
            ),
            "the generated nominal {name} must navigate to its source label: {generated:?}"
        );
    }

    let alias_hover = wait_for_hover(&mut lsp, &uri, alias_use_position.0, alias_use_position.1);
    assert!(
        hover_type(&alias_hover).contains("Tree"),
        "the transparent recursive labels alias must have finite hover text: {alias_hover:?}"
    );
    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": {
                "line": alias_use_position.0,
                "character": alias_use_position.1,
            },
            "context": { "includeDeclaration": true },
        }),
    );
    let references = lsp
        .recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(references_id));
    let mut ranges = references
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recursive labels alias references must resolve: {references:?}"))
        .iter()
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let alias_head = source.find("labels Tree").unwrap() + "labels ".len();
    let mut expected = vec![
        source_range(source, alias_head, "Tree".len()),
        source_range(source, alias_use, "Tree".len()),
    ];
    expected.sort_unstable();
    assert_eq!(ranges, expected);

    let branch_use = source.find("& Branch").unwrap() + "& ".len();
    let branch_use_position = source_position(source, branch_use);
    let generated_prepare =
        send_prepare_rename(&mut lsp, &uri, branch_use_position.0, branch_use_position.1);
    assert!(
        generated_prepare.get("error").is_none(),
        "{generated_prepare:?}"
    );
    assert!(
        generated_prepare.get("result").is_none_or(Value::is_null),
        "the generated `Branch` nominal cannot be renamed independently of its lower-case label surface: {generated_prepare:?}"
    );
    let generated_rename = send_rename(
        &mut lsp,
        &uri,
        branch_use_position.0,
        branch_use_position.1,
        "Fork",
    );
    assert!(
        generated_rename.get("error").is_some(),
        "a generated nominal rename must be refused instead of emitting an incomplete edit: {generated_rename:?}"
    );

    let alias_generated_collision = send_rename(
        &mut lsp,
        &uri,
        alias_use_position.0,
        alias_use_position.1,
        "Branch",
    );
    assert!(
        alias_generated_collision.get("error").is_some(),
        "a named labels alias must not collide with one of its generated nominal arms: \
         {alias_generated_collision:?}"
    );

    let symbols_id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(symbols_id));
    let symbol_names = symbols
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recursive labels must remain a document symbol: {symbols:?}"))
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(symbol_names, vec!["Tree"]);

    let renamed = send_rename(
        &mut lsp,
        &uri,
        alias_use_position.0,
        alias_use_position.1,
        "Wood",
    );
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let edits = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("edits"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recursive labels alias rename must publish edits: {renamed:?}"));
    assert_eq!(edits.len(), 2, "{renamed:?}");
    let renamed_source = apply_lsp_text_edits(source, edits);
    assert!(renamed_source.contains("rec labels Wood ="));
    assert!(renamed_source.contains("branch: Wood & Branch"));
    assert!(renamed_source.contains("branch"));

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rename_checks_generated_nominals_in_the_complete_module_namespace() {
    let dir = TempDir::new("rename-generated-label-collision");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type Bar = .;\n",
        "labels { foo: . };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let collision = send_rename(&mut lsp, &uri, 2, 6, "Foo");
    assert!(
        collision.get("error").is_some(),
        "rename must include anonymous-label generated heads in namespace conflicts: \
         {collision:?}"
    );

    let accepted = send_rename(&mut lsp, &uri, 2, 6, "Baz");
    assert!(accepted.get("error").is_none(), "{accepted:?}");
    let edits = accepted
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("edits"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("non-colliding rename must publish edits: {accepted:?}"));
    let renamed_source = apply_lsp_text_edits(source, edits);
    assert!(renamed_source.contains("type Baz = .;"), "{renamed_source}");
    assert!(
        renamed_source.contains("labels { foo: . };"),
        "{renamed_source}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_labels_support_multiple_internal_knots_and_mutual_named_peers() {
    let dir = TempDir::new("recursive-labels-multiple-and-mutual");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec labels Pair = { first: . | First, second: . | Second };\n",
        "rec {\n",
        "  labels A = { to_b <X>: X & B };\n",
        "  labels B = { to_a: A };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let b_use = source.find("X & B").unwrap() + "X & ".len();
    let b_use_position = source_position(source, b_use);
    let b_head = source.find("labels B").unwrap() + "labels ".len();
    let b_head_position = source_position(source, b_head);
    let b_definition = send_definition(&mut lsp, &uri, b_use_position.0, b_use_position.1);
    assert_eq!(
        json_range(
            b_definition
                .get("range")
                .expect("mutual labels B definition")
        ),
        (
            b_head_position.0,
            b_head_position.1,
            b_head_position.0,
            b_head_position.1 + 1,
        ),
        "the existential-bearing edge must resolve to the exact named labels alias: {b_definition:?}"
    );
    let b_hover = wait_for_hover(&mut lsp, &uri, b_use_position.0, b_use_position.1);
    assert!(
        hover_type(&b_hover).contains("B"),
        "the mutual named-label edge must have finite hover text: {b_hover:?}"
    );
    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": {
                "line": b_use_position.0,
                "character": b_use_position.1,
            },
            "context": { "includeDeclaration": true },
        }),
    );
    let references = lsp
        .recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(references_id));
    let mut ranges = references
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("mutual named-label references must resolve: {references:?}"))
        .iter()
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut expected = vec![
        source_range(source, b_head, "B".len()),
        source_range(source, b_use, "B".len()),
    ];
    expected.sort_unstable();
    assert_eq!(ranges, expected);

    let a_use = source.find("to_a: A").unwrap() + "to_a: ".len();
    let a_use_position = source_position(source, a_use);
    let a_head = source.find("labels A").unwrap() + "labels ".len();
    let a_head_position = source_position(source, a_head);
    let a_definition = send_definition(&mut lsp, &uri, a_use_position.0, a_use_position.1);
    assert_eq!(
        json_range(
            a_definition
                .get("range")
                .expect("mutual labels A definition")
        ),
        (
            a_head_position.0,
            a_head_position.1,
            a_head_position.0,
            a_head_position.1 + 1,
        ),
        "the back edge must resolve to the exact named labels alias: {a_definition:?}"
    );

    let renamed = send_rename(&mut lsp, &uri, b_use_position.0, b_use_position.1, "Bee");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let edits = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("edits"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("mutual labels rename must publish edits: {renamed:?}"));
    assert_eq!(edits.len(), 2, "{renamed:?}");
    let renamed_source = apply_lsp_text_edits(source, edits);
    assert!(renamed_source.contains("X & Bee"));
    assert!(renamed_source.contains("labels Bee ="));

    let symbols_id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(symbols_id));
    let symbol_names = symbols
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recursive labels must remain document symbols: {symbols:?}"))
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(symbol_names, vec!["Pair", "A", "B"]);

    let folding_id = lsp.send_request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let folding =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(folding_id));
    assert!(
        folding
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(|ranges| ranges.iter().any(|range| {
                range.get("startLine").and_then(Value::as_u64) == Some(3)
                    && range.get("endLine").and_then(Value::as_u64) == Some(6)
            })),
        "the mutual named-label group must remain foldable: {folding:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_unmarked_labels_keep_only_earlier_generated_heads_in_scope() {
    let dir = TempDir::new("unmarked-label-source-order");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "labels { early: ., uses_early: Early, uses_later: Later, later: . };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected generated-head source-order diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Later` is declared later and is not visible here")
    );

    let completion_offset = source.find("uses_early: ").unwrap() + "uses_early: ".len();
    let completion_position = source_position(source, completion_offset);
    let completion_response =
        send_completion(&mut lsp, &uri, completion_position.0, completion_position.1);
    let completion = completion_labels(&completion_response);
    assert!(completion.contains(&"Early"));
    assert!(
        !completion.contains(&"Later"),
        "an unmarked labels declaration must not gain simultaneous generated-head scope: {completion:?}"
    );

    let actions = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        actions
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(|actions| actions.iter().all(|action| {
                let title = action.get("title").and_then(Value::as_str).unwrap_or("");
                title != "Add `rec` to this recursive labels declaration"
                    && !title.starts_with("Move `Later`")
            })),
        "an acyclic generated-label edge must not receive an invalid marker or source-splitting edit: {actions:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// textDocument/semanticTokens/full — added in lsp/09.
// ============================================================

/// Send a `textDocument/semanticTokens/full` request and wait for the
/// response. Returns the JSON `result` field (`null` or a
/// `SemanticTokens` object).
fn send_semantic_tokens_full(lsp: &mut LspProcess, uri: &str) -> Value {
    let id = lsp.send_request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    resp.get("result").cloned().unwrap_or(Value::Null)
}

#[test]
fn lsp_capabilities_advertise_semantic_tokens_provider() {
    // The initialize response must advertise semanticTokensProvider with
    // full = true and range = false, and a non-empty legend.
    let dir = TempDir::new("caps-09");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    let provider = caps
        .get("semanticTokensProvider")
        .expect("server must advertise semanticTokensProvider; caps = {caps:?}");

    // full: true — supports semanticTokens/full.
    assert_eq!(
        provider.get("full").and_then(Value::as_bool),
        Some(true),
        "semanticTokensProvider.full must be true; provider = {provider:?}"
    );
    // range: false — partial-range requests not implemented in v1.
    assert_eq!(
        provider.get("range").and_then(Value::as_bool),
        Some(false),
        "semanticTokensProvider.range must be false; provider = {provider:?}"
    );
    // Legend must contain at least one type and one modifier.
    let legend = provider.get("legend").expect("legend must be present");
    let types = legend
        .get("tokenTypes")
        .and_then(Value::as_array)
        .expect("tokenTypes must be an array");
    assert!(
        !types.is_empty(),
        "legend.tokenTypes must be non-empty; legend = {legend:?}"
    );
    let modifiers = legend
        .get("tokenModifiers")
        .and_then(Value::as_array)
        .expect("tokenModifiers must be an array");
    assert!(
        !modifiers.is_empty(),
        "legend.tokenModifiers must be non-empty; legend = {legend:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_semantic_tokens_full_returns_tokens_for_open_document() {
    // Open a clean package. semanticTokens/full must return a
    // SemanticTokens object (not null) with a non-empty `data` array.
    //
    // Source layout:
    //   line 0: "module pkg/main;"   (has keywords)
    //   line 1: ""
    //   line 2: "pub fn run() -> . { () }"
    let dir = TempDir::new("semtok-basic");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);

    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    // Brief pause to let the overlay register; semanticTokens/full is
    // lexer-based and does not need the analysis to finish.
    let _ = lsp.try_recv_for(Duration::from_millis(200));

    let result = send_semantic_tokens_full(&mut lsp, &uri);
    assert!(
        !result.is_null(),
        "semanticTokens/full must return a SemanticTokens object; got null"
    );
    // `data` is the flat u32 array; it must be non-empty for a
    // non-trivial source.
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .expect("SemanticTokens must have a data array");
    assert!(
        !data.is_empty(),
        "data must be non-empty for a file with keywords and identifiers; got empty"
    );
    // Each element must be a non-negative integer (u32 in JSON).
    for (i, v) in data.iter().enumerate() {
        assert!(
            v.as_u64().is_some(),
            "data[{i}] must be a non-negative integer; got {v:?}"
        );
    }
    // The data length must be a multiple of 5 (5 u32s per token).
    assert_eq!(
        data.len() % 5,
        0,
        "data.len() must be divisible by 5 (5 fields per token); got {}",
        data.len()
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_semantic_tokens_full_returns_null_for_unknown_document() {
    // Request semanticTokens/full for a URI that was never opened.
    // The server must respond without error; result must be null.
    let dir = TempDir::new("semtok-unknown");
    let main_path = dir.path().join("missing.kio");

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));

    let uri = path_to_file_uri(&main_path);
    let id = lsp.send_request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    assert!(
        resp.get("error").is_none(),
        "semanticTokens/full on unknown document must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_null() || result.is_object(),
        "result must be null or a SemanticTokens object; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_semantic_tokens_full_data_has_keyword_tokens() {
    // Verify that the returned data contains tokens for the `fn`
    // keyword. The legend's `tokenTypes` array must contain "keyword";
    // find its index, then decode the delta-encoded data array looking
    // for a token of that type.
    let dir = TempDir::new("semtok-kw");
    let source = "module pkg/main;\n\nfn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    // Capture the initialize response so we can read the legend.
    let init_id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let init_resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(init_id));
    lsp.send_notification("initialized", json!({}));

    let caps = init_resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    let legend = caps
        .get("semanticTokensProvider")
        .and_then(|p| p.get("legend"))
        .expect("legend");
    let token_types: Vec<&str> = legend
        .get("tokenTypes")
        .and_then(Value::as_array)
        .expect("tokenTypes")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let keyword_idx = token_types
        .iter()
        .position(|&t| t == "keyword")
        .expect("\"keyword\" must appear in the legend's tokenTypes") as u64;

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let _ = lsp.try_recv_for(Duration::from_millis(200));

    let result = send_semantic_tokens_full(&mut lsp, &uri);
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .expect("data array");

    // Scan the token data for any token whose tokenType == keyword_idx.
    let has_keyword = data
        .chunks(5)
        .any(|chunk| chunk[3].as_u64() == Some(keyword_idx));
    assert!(
        has_keyword,
        "semanticTokens/full must include at least one keyword token (type index {}); \
         data = {data:?}",
        keyword_idx
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_semantic_tokens_keep_rec_contextual_across_value_and_declaration_sites() {
    let dir = TempDir::new("semtok-contextual-rec");
    let source = concat!(
        "module pkg/main;\n",
        "fn use(rec: .) -> . { rec }\n",
        "rec(loop) fn walk(value: .) -> . { value }\n",
        "rec {\n",
        "  newtype A : B { constructor mk_a; projector un_a; };\n",
        "  newtype B : A { constructor mk_b; projector un_b; };\n",
        "}\n",
        "rec newtype Cell : . | Cell { constructor mk_cell; projector un_cell; };\n",
        "rec labels Rows = { row: . | Row };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    let init_id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let init =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(init_id));
    lsp.send_notification("initialized", json!({}));
    let token_types = init
        .pointer("/result/capabilities/semanticTokensProvider/legend/tokenTypes")
        .and_then(Value::as_array)
        .expect("semantic-token legend")
        .iter()
        .map(|token_type| token_type.as_str().expect("token type").to_owned())
        .collect::<Vec<_>>();

    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );
    let _ = lsp.try_recv_for(Duration::from_millis(200));
    let result = send_semantic_tokens_full(&mut lsp, &uri);
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .expect("semantic-token data");
    let mut line = 0_u64;
    let mut column = 0_u64;
    let mut decoded = Vec::new();
    for token in data.chunks_exact(5) {
        let delta_line = token[0].as_u64().expect("delta line");
        let delta_start = token[1].as_u64().expect("delta start");
        if delta_line == 0 {
            column += delta_start;
        } else {
            line += delta_line;
            column = delta_start;
        }
        let length = token[2].as_u64().expect("token length");
        let token_type = token[3].as_u64().expect("token type") as usize;
        decoded.push((line, column, length, token_types[token_type].as_str()));
    }

    let type_at = |marker: &str, prefix: &str| {
        let offset = source.find(marker).unwrap() + prefix.len();
        let position = source_position(source, offset);
        decoded
            .iter()
            .find(|(line, column, length, _)| {
                (*line, *column, *length) == (position.0, position.1, 3)
            })
            .map(|(_, _, _, token_type)| *token_type)
            .unwrap_or_else(|| panic!("missing semantic token for {marker:?}: {decoded:?}"))
    };

    assert_eq!(type_at("use(rec", "use("), "parameter");
    assert_eq!(type_at("{ rec }", "{ "), "variable");
    for (marker, prefix) in [
        ("rec(loop)", ""),
        ("rec {", ""),
        ("rec newtype", ""),
        ("rec labels", ""),
    ] {
        assert_eq!(
            type_at(marker, prefix),
            "keyword",
            "{marker:?} must classify contextual `rec` as a declaration keyword"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

// ============================================================
// textDocument/prepareRename and textDocument/rename — added in lsp/10.
// ============================================================

/// Send a `textDocument/prepareRename` request and wait for the response.
/// Returns the full response value (including `result` and `error` fields).
fn send_prepare_rename(lsp: &mut LspProcess, uri: &str, line: u64, character: u64) -> Value {
    let id = lsp.send_request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );
    lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id))
}

/// Send a `textDocument/rename` request and wait for the response.
/// Returns the full response value.
fn send_rename(
    lsp: &mut LspProcess,
    uri: &str,
    line: u64,
    character: u64,
    new_name: &str,
) -> Value {
    let id = lsp.send_request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name,
        }),
    );
    lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id))
}

fn send_code_action(lsp: &mut LspProcess, uri: &str, diagnostic: &Value) -> Value {
    let range = diagnostic.get("range").cloned().expect("diagnostic range");
    let id = lsp.send_request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": range,
            "context": {
                "diagnostics": [diagnostic.clone()],
                "only": ["quickfix"],
            },
        }),
    );
    lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id))
}

fn send_code_actions_for_kind(
    lsp: &mut LspProcess,
    uri: &str,
    diagnostics: &[Value],
    kind: &str,
) -> Value {
    let range = diagnostics
        .first()
        .and_then(|diagnostic| diagnostic.get("range"))
        .cloned()
        .expect("diagnostic range");
    let id = lsp.send_request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": range,
            "context": {
                "diagnostics": diagnostics,
                "only": [kind],
            },
        }),
    );
    lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(id))
}

fn send_code_action_resolve(lsp: &mut LspProcess, action: &Value) -> Value {
    let id = lsp.send_request("codeAction/resolve", action.clone());
    lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id))
}

fn send_full_text_change(lsp: &mut LspProcess, uri: &str, version: i32, text: &str) {
    lsp.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }],
        }),
    );
}

fn workspace_edit_edits_for_uri<'a>(
    edit: &'a Value,
    uri: &str,
) -> Option<(&'a [Value], Option<i64>)> {
    if let Some(edits) = edit
        .get("changes")
        .and_then(|changes| changes.get(uri))
        .and_then(Value::as_array)
    {
        return Some((edits, None));
    }
    edit.get("documentChanges")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|change| {
            let text_document = change.get("textDocument")?;
            (text_document.get("uri").and_then(Value::as_str) == Some(uri)).then(|| {
                (
                    change
                        .get("edits")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    text_document.get("version").and_then(Value::as_i64),
                )
            })
        })
}

fn action_edits_for_uri<'a>(action: &'a Value, uri: &str) -> (&'a [Value], Option<i64>) {
    action
        .get("edit")
        .and_then(|edit| workspace_edit_edits_for_uri(edit, uri))
        .unwrap_or_else(|| panic!("action should contain changes for {uri}: {action:?}"))
}

fn resolved_edits_for_uri<'a>(response: &'a Value, uri: &str) -> &'a [Value] {
    response
        .get("result")
        .and_then(|result| result.get("edit"))
        .and_then(|edit| workspace_edit_edits_for_uri(edit, uri))
        .map(|(edits, _)| edits)
        .unwrap_or_else(|| panic!("resolved action should contain changes for {uri}: {response:?}"))
}

#[test]
fn lsp_capabilities_advertise_rename_provider() {
    // The initialize response must advertise renameProvider with
    // prepareProvider = true.
    let dir = TempDir::new("caps-10");
    let mut lsp = LspProcess::spawn();
    let id = lsp.send_request(
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": path_to_file_uri(dir.path()),
            "capabilities": {},
        }),
    );
    let resp = lsp.recv_matching(|v| v.get("id").and_then(|x| x.as_i64()) == Some(id));
    let caps = resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities");
    lsp.send_notification("initialized", json!({}));

    let provider = caps
        .get("renameProvider")
        .expect("server must advertise renameProvider; caps = {caps:?}");
    assert_eq!(
        provider.get("prepareProvider").and_then(Value::as_bool),
        Some(true),
        "renameProvider.prepareProvider must be true; provider = {provider:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_prepare_rename_returns_range_on_identifier() {
    // Open a clean package. prepareRename on the `run` identifier in
    // `pub fn run()` must return a RangeWithPlaceholder whose
    // `placeholder` matches the identifier text.
    let dir = TempDir::new("prep-rename");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // `run` in "pub fn run()" is on line 2.
    // "pub fn run() -> . { () }"
    //  0123456789...
    // `run` starts at character 7.
    let resp = send_prepare_rename(&mut lsp, &uri, 2, 7);
    assert!(
        resp.get("error").is_none(),
        "prepareRename must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    // Result is null (no analysis ready yet — possible in slow CI)
    // or a RangeWithPlaceholder. Both are acceptable; the test just
    // asserts the response structure is valid.
    if !result.is_null() {
        assert!(
            result.is_object(),
            "prepareRename result must be an object or null; got: {result:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_prepare_rename_returns_null_on_whitespace() {
    // prepareRename on a whitespace-only position (between tokens)
    // must return null — there is no identifier to rename.
    let dir = TempDir::new("prep-rename-ws");
    let source = "module pkg/main;\n\npub fn run() -> . { () }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Blank line (line 1, char 0) is whitespace — no binder there.
    let resp = send_prepare_rename(&mut lsp, &uri, 1, 0);
    assert!(
        resp.get("error").is_none(),
        "prepareRename on whitespace must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_null(),
        "prepareRename on whitespace must return null; got: {result:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rename_produces_workspace_edit_for_local_binder() {
    // A simple single-file rename: rename the local `x` referenced
    // in the function body to `y`. The cursor is placed on the `x`
    // in the body expression — the position index records binders at
    // use/reference sites, not at declaration sites, so the cursor
    // must be on a use of `x`.
    //
    // Source (line 2): "pub fn run(x: .) -> . { x }"
    //                   0         1         2
    //                   0123456789012345678901234567
    // The `x` in `{ x }` is at character 24.
    let dir = TempDir::new("rename-local");
    let source = "module pkg/main;\n\npub fn run(x: .) -> . { x }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // `x` in the body `{ x }` is on line 2, character 24.
    let resp = send_rename(&mut lsp, &uri, 2, 24, "y");
    assert!(
        resp.get("error").is_none(),
        "rename of local binder in body must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    // Result must be null (no analysis ready) or a WorkspaceEdit.
    if !result.is_null() {
        assert!(
            result.is_object(),
            "rename result must be a WorkspaceEdit object or null; got: {result:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rename_returns_versioned_document_changes_for_open_file() {
    let dir = TempDir::new("rename-versioned");
    let source = "module pkg/main;\n\npub fn run(x: .) -> . { x }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let mut result = Value::Null;
    for _ in 0..10 {
        let resp = send_rename(&mut lsp, &uri, 2, 24, "y");
        assert!(
            resp.get("error").is_none(),
            "rename of local binder must not error: {resp:?}"
        );
        result = resp.get("result").cloned().unwrap_or(Value::Null);
        if !result.is_null() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    assert!(
        result.is_object(),
        "rename should eventually return a WorkspaceEdit, got {result:?}"
    );
    assert!(
        result.get("changes").is_none(),
        "LSP rename should use versioned documentChanges, got {result:?}"
    );
    let document_changes = result
        .get("documentChanges")
        .and_then(Value::as_array)
        .expect("versioned documentChanges array");
    assert_eq!(document_changes.len(), 1);
    let text_document = document_changes[0]
        .get("textDocument")
        .expect("textDocument edit target");
    assert_eq!(
        text_document.get("uri").and_then(Value::as_str),
        Some(uri.as_str())
    );
    assert_eq!(
        text_document.get("version").and_then(Value::as_i64),
        Some(1)
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_row_let_rename_preserves_lexical_identity_and_selected_labels() {
    let dir = TempDir::new("row-let-rename-identity");
    let source = concat!(
        "module pkg/main;\n",
        "labels { field: ., value: . };\n",
        "fn first() -> . { let .({field as value}) = {field = ()}; value }\n",
        "fn second() -> . { let .({field as value}) = {field = ()}; value }\n",
        "fn shorthand() -> . { let .({value}) = {value = ()}; value }\n",
        "fn ordinary(value: .) -> . { value }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write(
        "pkg/other.kio",
        concat!(
            "module pkg/other;\n",
            "labels { field: . };\n",
            "fn other() -> . { let .({field as value}) = {field = ()}; value }\n",
        ),
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let first_use = source.find("; value }").expect("first alias use") + 2;
    let first_position = source_position(source, first_use + 1);
    assert!(
        !wait_for_hover(&mut lsp, &uri, first_position.0, first_position.1).is_null(),
        "row-let fixture must reach typed analysis"
    );

    for (function, declaration_prefix, shorthand) in [
        ("first", "let .({field as ", false),
        ("second", "let .({field as ", false),
        ("shorthand", "let .({", true),
    ] {
        let function_start = source.find(&format!("fn {function}()")).unwrap();
        let body = &source[function_start..];
        let declaration_start =
            function_start + body.find(declaration_prefix).unwrap() + declaration_prefix.len();
        let use_start = function_start + body.find("; value }").unwrap() + 2;
        let declaration_edit = if shorthand {
            (
                source_range(source, declaration_start + "value".len(), 0),
                " as payload".to_owned(),
            )
        } else {
            (
                source_range(source, declaration_start, "value".len()),
                "payload".to_owned(),
            )
        };
        let mut expected = vec![
            declaration_edit,
            (
                source_range(source, use_start, "value".len()),
                "payload".to_owned(),
            ),
        ];
        expected.sort_unstable();

        for cursor in [declaration_start, use_start] {
            let position = source_position(source, cursor + 1);
            let prepared = send_prepare_rename(&mut lsp, &uri, position.0, position.1);
            assert!(prepared.get("error").is_none(), "{prepared:?}");
            assert_eq!(
                prepared["result"]["placeholder"], "value",
                "{function} cursor {cursor}: {prepared:?}"
            );
            assert_eq!(
                json_range(&prepared["result"]["range"]),
                source_range(source, cursor, "value".len()),
                "{function} cursor {cursor}"
            );

            let renamed = send_rename(&mut lsp, &uri, position.0, position.1, "payload");
            assert!(renamed.get("error").is_none(), "{renamed:?}");
            let result = &renamed["result"];
            assert!(result.get("changes").is_none(), "{renamed:?}");
            let documents = result["documentChanges"]
                .as_array()
                .unwrap_or_else(|| panic!("{function} cursor {cursor}: {renamed:?}"));
            assert_eq!(
                documents.len(),
                1,
                "{function} cursor {cursor}: {renamed:?}"
            );
            assert_eq!(documents[0]["textDocument"]["uri"], uri);
            assert_eq!(documents[0]["textDocument"]["version"], 1);
            let edits = documents[0]["edits"].as_array().expect("rename edits");
            let mut actual = edits
                .iter()
                .map(|edit| {
                    (
                        json_range(&edit["range"]),
                        edit["newText"].as_str().expect("replacement").to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            actual.sort_unstable();
            assert_eq!(actual, expected, "{function} cursor {cursor}: {renamed:?}");
        }
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_newtype_member_rename_rejects_sibling_collisions() {
    let dir = TempDir::new("rename-newtype-sibling");
    let source = concat!(
        "module pkg/main;\n",
        "newtype Wrap : . { constructor mk; projector get; };\n",
        "rec {\n",
        "  type Link = Node;\n",
        "  newtype Node : . | Link { constructor pack; projector unpack; };\n",
        "}\n",
        "fn wrapped(value: .) -> . { Wrap.get(Wrap.mk(value)) }\n",
        "fn packed(value: . | Link) -> Node { Node.pack(value) }\n",
        "fn unpacked(value: Node) -> . | Link { Node.unpack(value) }\n",
        "fn helper(value: .) -> . { value }\n",
        "fn invoke() -> . { helper(()) }\n",
        "labels { field: . };\n",
        "fn labeled(value: .) -> Field { Field.mk(value) }\n",
    );
    let path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);
    let first_use = source.find("Wrap.mk").unwrap() + "Wrap.".len();
    let position = source_position(source, first_use);
    assert!(!wait_for_hover(&mut lsp, &uri, position.0, position.1).is_null());

    for (owner, kind, old_name, new_name) in [
        ("Wrap", "constructor", "mk", "get"),
        ("Wrap", "projector", "get", "mk"),
        ("Node", "constructor", "pack", "unpack"),
        ("Node", "projector", "unpack", "pack"),
    ] {
        let declaration = source.find(&format!("{kind} {old_name};")).unwrap() + kind.len() + 1;
        let use_offset = source.find(&format!("{owner}.{old_name}(")).unwrap() + owner.len() + 1;
        for cursor in [declaration, use_offset] {
            let position = source_position(source, cursor);
            let prepared = send_prepare_rename(&mut lsp, &uri, position.0, position.1);
            assert_eq!(prepared["result"]["placeholder"], old_name, "{prepared:?}");
            let response = send_rename(&mut lsp, &uri, position.0, position.1, new_name);
            assert_eq!(response["error"]["code"], -32803, "{response:?}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("same newtype")),
                "{response:?}"
            );
            assert!(
                response.get("result").is_none_or(Value::is_null),
                "{response:?}"
            );
        }
        let position = source_position(source, use_offset);
        let unchanged = send_rename(&mut lsp, &uri, position.0, position.1, old_name);
        assert!(unchanged.get("error").is_none(), "{unchanged:?}");
        let (edits, version) = workspace_edit_edits_for_uri(&unchanged["result"], &uri)
            .unwrap_or_else(|| panic!("same-name rename should remain valid: {unchanged:?}"));
        assert_eq!(version, Some(1));
        assert_eq!(edits.len(), 2, "{unchanged:?}");
        assert_eq!(apply_lsp_text_edits(source, edits), source);
    }

    for (cursor, new_name) in [
        (source.find("fn helper").unwrap() + "fn ".len(), "get"),
        (source.find("helper(value").unwrap() + "helper(".len(), "mk"),
    ] {
        let position = source_position(source, cursor);
        let response = send_rename(&mut lsp, &uri, position.0, position.1, new_name);
        assert!(response.get("error").is_none(), "{response:?}");
        let (edits, _) = workspace_edit_edits_for_uri(&response["result"], &uri)
            .unwrap_or_else(|| panic!("unrelated members must not reserve names: {response:?}"));
        assert_eq!(edits.len(), 2, "{response:?}");
    }
    let generated_member = source.find("Field.mk").unwrap() + "Field.".len();
    let position = source_position(source, generated_member);
    let prepared = send_prepare_rename(&mut lsp, &uri, position.0, position.1);
    assert!(
        prepared.get("result").is_none_or(Value::is_null),
        "{prepared:?}"
    );
    let refused = send_rename(&mut lsp, &uri, position.0, position.1, "renamed");
    assert_eq!(refused["error"]["code"], -32803, "{refused:?}");
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_newtype_member_rename_applies_only_to_the_resolved_owner() {
    let dir = TempDir::new("rename-newtype-owner");
    let provider = concat!(
        "module pkg/provider;\n",
        "pub newtype Wrap : . { pub constructor mk; pub projector get; };\n",
        "newtype Spare : . { constructor wrapped; projector opened; };\n",
    );
    let consumer = concat!(
        "module pkg/main;\n",
        "import pkg/provider(Wrap);\n",
        "fn wrapped(value: .) -> . { value }\n",
        "pub fn run(wrapped: .) -> . { Wrap.get(Wrap.mk(wrapped)) }\n",
    );
    let other = concat!(
        "module pkg/other;\n",
        "pub newtype Wrap : . { pub constructor wrapped; pub projector get; };\n",
        "pub fn run(value: .) -> Wrap { Wrap.wrapped(value) }\n",
    );
    let provider_path = dir.write("pkg/provider.kio", provider);
    let consumer_path = dir.write("pkg/main.kio", consumer);
    let other_path = dir.write("pkg/other.kio", other);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let provider_uri = path_to_file_uri(&provider_path);
    let consumer_uri = path_to_file_uri(&consumer_path);
    open_and_wait_for_analysis(&mut lsp, &provider_uri, provider);
    open_and_wait_for_analysis(&mut lsp, &consumer_uri, consumer);
    let declaration = provider.find("constructor mk").unwrap() + "constructor ".len();
    let use_offset = consumer.find("Wrap.mk").unwrap() + "Wrap.".len();
    let position = source_position(consumer, use_offset);
    assert!(!wait_for_hover(&mut lsp, &consumer_uri, position.0, position.1).is_null());

    let mut applied_provider = String::new();
    let mut applied_consumer = String::new();
    for (uri, source, cursor) in [
        (&provider_uri, provider, declaration),
        (&consumer_uri, consumer, use_offset),
    ] {
        let position = source_position(source, cursor);
        let response = send_rename(&mut lsp, uri, position.0, position.1, "wrapped");
        assert!(response.get("error").is_none(), "{response:?}");
        let result = &response["result"];
        assert_eq!(
            result["documentChanges"].as_array().unwrap().len(),
            2,
            "{response:?}"
        );
        for (changed_uri, original, offset) in [
            (&provider_uri, provider, declaration),
            (&consumer_uri, consumer, use_offset),
        ] {
            let (edits, version) = workspace_edit_edits_for_uri(result, changed_uri)
                .unwrap_or_else(|| panic!("missing exact member edit: {response:?}"));
            assert_eq!(version, Some(1));
            assert_eq!(edits.len(), 1, "{response:?}");
            assert_eq!(
                json_range(&edits[0]["range"]),
                source_range(original, offset, 2)
            );
            assert_eq!(edits[0]["newText"], "wrapped");
        }
        applied_provider = apply_lsp_text_edits(
            provider,
            workspace_edit_edits_for_uri(result, &provider_uri)
                .unwrap()
                .0,
        );
        applied_consumer = apply_lsp_text_edits(
            consumer,
            workspace_edit_edits_for_uri(result, &consumer_uri)
                .unwrap()
                .0,
        );
    }
    assert_eq!(
        applied_provider,
        provider.replacen("constructor mk;", "constructor wrapped;", 1)
    );
    assert_eq!(
        applied_consumer,
        consumer.replace("Wrap.mk(", "Wrap.wrapped(")
    );
    assert_eq!(fs::read_to_string(&other_path).unwrap(), other);
    fs::write(&provider_path, &applied_provider).unwrap();
    fs::write(&consumer_path, &applied_consumer).unwrap();
    send_full_text_change(&mut lsp, &provider_uri, 2, &applied_provider);
    send_full_text_change(&mut lsp, &consumer_uri, 2, &applied_consumer);
    let new_use = applied_consumer.find("Wrap.wrapped").unwrap() + "Wrap.".len();
    let position = source_position(&applied_consumer, new_use);
    let mut fresh = false;
    for _ in 0..20 {
        let prepared = send_prepare_rename(&mut lsp, &consumer_uri, position.0, position.1);
        if prepared["result"]["placeholder"] == "wrapped" {
            fresh = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(fresh, "renamed documents must reach a fresh typed snapshot");
    let definition = send_definition(&mut lsp, &consumer_uri, position.0, position.1);
    assert_eq!(definition_uri(&definition), provider_uri);
    assert_eq!(
        json_range(&definition["range"]),
        source_range(&applied_provider, declaration, "wrapped".len())
    );
    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": consumer_uri },
            "position": { "line": position.0, "character": position.1 },
            "context": { "includeDeclaration": true },
        }),
    );
    let references = lsp.recv_matching(|value| value["id"].as_i64() == Some(references_id));
    let mut actual = references["result"]
        .as_array()
        .unwrap_or_else(|| panic!("fresh references must resolve: {references:?}"))
        .iter()
        .map(|location| {
            (
                location["uri"].as_str().unwrap().to_owned(),
                json_range(&location["range"]),
            )
        })
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = vec![
        (
            provider_uri,
            source_range(&applied_provider, declaration, "wrapped".len()),
        ),
        (
            consumer_uri,
            source_range(&applied_consumer, new_use, "wrapped".len()),
        ),
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
    let checked = Command::new(test_binary!("kio"))
        .arg("check")
        .current_dir(dir.path())
        .output()
        .expect("check the exact applied workspace");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_ordinary_newtype_head_rename_covers_checked_type_positions() {
    let dir = TempDir::new("rename-nominal-type-positions");
    let provider = concat!(
        "module pkg/provider;\n",
        "pub newtype Token : . { pub constructor mk; pub projector get; };\n",
        "type Alias = Token;\n",
        "newtype Carrier : Token { constructor pack; projector unpack; };\n",
        "type Pair = Token & Token;\n",
        "type Choice = Token | .;\n",
        "type Applied[A] = A;\n",
        "type Nested = Applied(Token);\n",
        "type Transform = Token -> Token;\n",
        "type Scheme = [A] A -> Token;\n",
        "type Shadow = [Token] Token -> Token;\n",
        "fn identity(value: Token) -> Token { let .(local: Token) = value; local }\n",
        "fn closure() -> Token -> Token { .(input: Token) { input } }\n",
        "fn generic[Token](value: Token) -> Token { value }\n",
        "fn make() -> Token { Token.mk(()) }\n",
        "fn project(value: Token) -> . { Token.get(value) }\n",
        "fn explicit(value: Token) -> Token { generic(Token, value) }\n",
    );
    let consumer = concat!(
        "module pkg/main;\n",
        "import pkg/provider(Token);\n",
        "type Alias = Token;\n",
        "newtype Carrier : Token { constructor pack; projector unpack; };\n",
        "type Pair = Token & Token;\n",
        "type Choice = Token | .;\n",
        "type Applied[A] = A;\n",
        "type Nested = Applied(Token);\n",
        "type Transform = Token -> Token;\n",
        "type Scheme = [A] A -> Token;\n",
        "type Shadow = [Token] Token -> Token;\n",
        "fn identity(value: Token) -> Token { let .(local: Token) = value; local }\n",
        "fn closure() -> Token -> Token { .(input: Token) { input } }\n",
        "fn generic[Token](value: Token) -> Token { value }\n",
        "fn make() -> Token { Token.mk(()) }\n",
        "fn project(value: Token) -> . { Token.get(value) }\n",
        "fn explicit(value: Token) -> Token { generic(Token, value) }\n",
    );
    let qualified = concat!(
        "module pkg/qualified;\n",
        "import pkg/provider as p;\n",
        "type Alias = p.Token;\n",
        "fn identity(value: p.Token) -> p.Token { value }\n",
    );
    let decoy = concat!(
        "module pkg/other;\n",
        "pub newtype Token : . { pub constructor mk; pub projector get; };\n",
        "pub fn identity(value: Token) -> Token { value }\n",
    );
    let paths = [
        dir.write("pkg/provider.kio", provider),
        dir.write("pkg/main.kio", consumer),
        dir.write("pkg/qualified.kio", qualified),
    ];
    let decoy_path = dir.write("pkg/other.kio", decoy);
    dir.write_pkg_root_package();
    let checked = Command::new(test_binary!("kio"))
        .arg("check")
        .current_dir(dir.path())
        .output()
        .expect("check the original nominal workspace");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let uris = paths.each_ref().map(|path| path_to_file_uri(path));
    let sources = [provider, consumer, qualified];
    // Lines 10 and 13 bind their own Token; all other written Token heads
    // in these three files refer to the provider's ordinary newtype.
    let offsets = |source: &str, name: &str, excluded: &[usize]| {
        let mut start = 0;
        let mut found = Vec::new();
        for (line, text) in source.split_inclusive('\n').enumerate() {
            if !excluded.contains(&line) {
                found.extend(text.match_indices(name).map(|(offset, _)| start + offset));
            }
            start += text.len();
        }
        found
    };
    let excluded: [&[usize]; 3] = [&[10, 13], &[10, 13], &[]];
    let positions =
        std::array::from_fn::<_, 3, _>(|index| offsets(sources[index], "Token", excluded[index]));
    assert_eq!(
        positions.each_ref().map(|positions| positions.len()),
        [23, 23, 3]
    );
    let declaration = (uris[0].clone(), source_range(provider, positions[0][0], 5));
    let mut expected = Vec::new();
    for file in 0..3 {
        for offset in &positions[file] {
            expected.push((uris[file].clone(), source_range(sources[file], *offset, 5)));
        }
    }
    expected.sort_unstable();
    let references_at = |lsp: &mut LspProcess, uri: &str, position: (u64, u64), include: bool| {
        let id = lsp.send_request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": position.0, "character": position.1 },
                "context": { "includeDeclaration": include },
            }),
        );
        let response = lsp.recv_matching(|value| value["id"].as_i64() == Some(id));
        let mut locations = response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("references must resolve: {response:?}"))
            .iter()
            .map(|location| (definition_uri(location), json_range(&location["range"])))
            .collect::<Vec<_>>();
        locations.sort_unstable();
        locations
    };
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    for (uri, source) in uris.iter().zip(sources) {
        open_and_wait_for_analysis(&mut lsp, uri, source);
    }
    for (index, positions) in positions.iter().enumerate() {
        for offset in positions {
            let position = source_position(sources[index], *offset);
            let definition = send_definition(&mut lsp, &uris[index], position.0, position.1);
            assert!(
                !definition.is_null(),
                "missing nominal definition in file {index} at offset {offset}, position {position:?}"
            );
            assert_eq!(definition_uri(&definition), declaration.0);
            assert_eq!(json_range(&definition["range"]), declaration.1);
            let prepared = send_prepare_rename(&mut lsp, &uris[index], position.0, position.1);
            assert_eq!(prepared["result"]["placeholder"], "Token", "{prepared:?}");
            assert_eq!(
                json_range(&prepared["result"]["range"]),
                source_range(sources[index], *offset, 5)
            );
        }
    }
    let alias_offset = qualified.find("as p").unwrap() + "as ".len();
    let alias_position = source_position(qualified, alias_offset);
    let prepared = send_prepare_rename(&mut lsp, &uris[2], alias_position.0, alias_position.1);
    assert!(
        prepared.get("result").is_none_or(Value::is_null),
        "{prepared:?}"
    );
    let refused = send_rename(
        &mut lsp,
        &uris[2],
        alias_position.0,
        alias_position.1,
        "other",
    );
    assert_eq!(refused["error"]["code"], -32803, "{refused:?}");

    let mut applied = [String::new(), String::new(), String::new()];
    for (index, offset) in [
        (0, positions[0][0]),
        (0, positions[0][1]),
        (1, positions[1][2]),
        (2, positions[2][1]),
    ] {
        let position = source_position(sources[index], offset);
        for include in [false, true] {
            let wanted = expected
                .iter()
                .filter(|location| include || **location != declaration)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                references_at(&mut lsp, &uris[index], position, include),
                wanted
            );
        }
        let response = send_rename(&mut lsp, &uris[index], position.0, position.1, "Parcel");
        assert!(response.get("error").is_none(), "{response:?}");
        let result = &response["result"];
        assert_eq!(
            result["documentChanges"].as_array().unwrap().len(),
            3,
            "{response:?}"
        );
        let mut actual = Vec::new();
        for file in 0..3 {
            let (edits, version) = workspace_edit_edits_for_uri(result, &uris[file])
                .unwrap_or_else(|| panic!("missing nominal edit file: {response:?}"));
            assert_eq!(version, Some(1));
            for edit in edits {
                assert_eq!(edit["newText"], "Parcel");
                actual.push((uris[file].clone(), json_range(&edit["range"])));
            }
            applied[file] = apply_lsp_text_edits(sources[file], edits);
        }
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }
    for file in 0..3 {
        fs::write(&paths[file], &applied[file]).unwrap();
        send_full_text_change(&mut lsp, &uris[file], 2, &applied[file]);
    }
    for source in &applied[..2] {
        assert!(source.contains("type Shadow = [Token] Token -> Token;"));
        assert!(source.contains("fn generic[Token](value: Token) -> Token { value }"));
    }
    assert_eq!(fs::read_to_string(&decoy_path).unwrap(), decoy);
    let fresh_offsets = offsets(&applied[1], "Parcel", &[]);
    let position = source_position(&applied[1], fresh_offsets[2]);
    let mut fresh = false;
    for _ in 0..20 {
        let prepared = send_prepare_rename(&mut lsp, &uris[1], position.0, position.1);
        if prepared["result"]["placeholder"] == "Parcel" {
            fresh = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        fresh,
        "all edited nominal references must reach fresh analysis"
    );
    let new_declaration = (
        uris[0].clone(),
        source_range(&applied[0], positions[0][0], 6),
    );
    let mut new_expected = Vec::new();
    for file in 0..3 {
        for offset in offsets(&applied[file], "Parcel", &[]) {
            new_expected.push((uris[file].clone(), source_range(&applied[file], offset, 6)));
        }
    }
    new_expected.sort_unstable();
    assert_eq!(new_expected.len(), expected.len());
    let definition = send_definition(&mut lsp, &uris[1], position.0, position.1);
    assert_eq!(definition_uri(&definition), new_declaration.0);
    assert_eq!(json_range(&definition["range"]), new_declaration.1);
    for include in [false, true] {
        let wanted = new_expected
            .iter()
            .filter(|location| include || **location != new_declaration)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(references_at(&mut lsp, &uris[1], position, include), wanted);
    }
    let checked = Command::new(test_binary!("kio"))
        .arg("check")
        .current_dir(dir.path())
        .output()
        .expect("check the exact nominal rename edits");
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rename_invalid_name_returns_error() {
    // Rename with an invalid new name (reserved slot token `____`)
    // must return a JSON-RPC error response. Cursor on the `x` in
    // the body (use site, which is what the position index records).
    //
    // Source (line 2): "pub fn run(x: .) -> . { x }"
    // The `x` body reference is at character 27.
    let dir = TempDir::new("rename-invalid");
    let source = "module pkg/main;\n\npub fn run(x: .) -> . { x }\n";
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // Attempt to rename `x` (at body use-site) to `____` (reserved slot token run).
    let resp = send_rename(&mut lsp, &uri, 2, 27, "____");
    // Must return a JSON-RPC error (not a result).
    assert!(
        resp.get("error").is_some(),
        "rename with reserved slot name must return a JSON-RPC error; got: {resp:?}"
    );
    let err = resp.get("error").unwrap();
    let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
    assert!(
        !msg.is_empty(),
        "error message must be non-empty; err = {err:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rename_cross_file_produces_edits_in_multiple_files() {
    // A two-file package: `main.kio` defines `helper` and calls it;
    // `lib.kio` also calls `helper`. Renaming `helper` in `main.kio`
    // must produce a WorkspaceEdit with edits in both files.
    //
    // Note: cross-file rename requires the analysis to have indexed
    // both files. The test waits for the analysis via the debounce
    // timeout before sending the rename request.
    let dir = TempDir::new("rename-xfile");
    let lib_source = "module pkg/lib;\n\npub fn helper() -> . { () }\n";
    let main_source = concat!(
        "module pkg/main;\n",
        "import pkg/lib(helper);\n",
        "\n",
        "pub fn run() -> . { helper() }\n",
    );
    dir.write("pkg.kio", "module pkg;\n");
    dir.write("pkg/lib.kio", lib_source);
    let main_path = dir.write("pkg/main.kio", main_source);
    dir.write(
        "pkg.pkg.kio",
        "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
    );

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, main_source);

    // `helper` in `pub fn run() -> . { helper() }` is on line 3.
    // "pub fn run() -> . { helper() }"
    //  0        1         2
    //  0123456789012345678901234567890
    // `helper` starts at character 22.
    let resp = send_rename(&mut lsp, &uri, 3, 22, "compute");
    assert!(
        resp.get("error").is_none(),
        "cross-file rename must not return a JSON-RPC error; got: {resp:?}"
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    // Accept null (analysis not yet ready) or a WorkspaceEdit.
    if !result.is_null() {
        assert!(
            result.is_object(),
            "cross-file rename result must be a WorkspaceEdit or null; got: {result:?}"
        );
        // When we get an edit, it should have a `changes` map.
        if let Some(changes) = result.get("changes") {
            assert!(
                changes.is_object(),
                "WorkspaceEdit.changes must be an object; got: {changes:?}"
            );
        }
    }

    assert_eq!(lsp.shutdown(), 0);
}

/// A package root declaring role-bearing host types, so a module can hold
/// `I32` / `String` literals. `write_pkg_root_package` declares none, which is
/// why no hover test could reach a literal until now.
fn write_pkg_root_with_host_types(dir: &TempDir) {
    dir.write(
        "pkg.kio",
        "module pkg;\n\nhost type I32 role(i32);\n\nhost type String role(str);\n",
    );
    dir.write(
        "pkg.pkg.kio",
        "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
    );
}

/// Hover the pieces of a call whose callee is monomorphic. Its arguments are
/// *checked*, not synthesized, and a checked expression used to leave no entry
/// in the position index — so hover walked outward and answered with the
/// enclosing call's type. Every literal in the file reported `.`.
///
/// The `range` assertions are the general guard: a hover that falls back to an
/// enclosing node returns that node's range, so a wrong range catches the whole
/// class, for any expression, without depending on what the type string says.
#[test]
fn lsp_hover_returns_literal_types_in_checking_position() {
    // line 4: `pub fn use_it() -> I32 { take(7, "seven") }`
    //          0123456789...
    let dir = TempDir::new("hover-literal");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "import pkg(I32, String);\n",
        "\n",
        "fn take(n: I32, label: String) -> I32 { n }\n",
        "\n",
        "pub fn use_it() -> I32 { take(7, \"seven\") }\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    write_pkg_root_with_host_types(&dir);

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    // `pub fn use_it() -> I32 { take(7, "seven") }`
    //  0         1         2         3         4
    //  0123456789012345678901234567890123456789012
    //                                ^ 30: the `7`
    //                                   ^ 33: the `"seven"` literal
    let int_lit = wait_for_hover(&mut lsp, &uri, 6, 30);
    assert_eq!(
        hover_type(&int_lit),
        "I32",
        "an int literal checked against a monomorphic parameter must hover as its own type, \
         not as the enclosing call's: {int_lit:?}"
    );
    assert_eq!(
        hover_range(&int_lit),
        (6, 30, 6, 31),
        "hover must report the literal's own range; an enclosing-node range means the position \
         index has no entry for the literal: {int_lit:?}"
    );

    let str_lit = wait_for_hover(&mut lsp, &uri, 6, 34);
    assert_eq!(hover_type(&str_lit), "String", "{str_lit:?}");
    assert_eq!(hover_range(&str_lit), (6, 33, 6, 40), "{str_lit:?}");

    assert_eq!(lsp.shutdown(), 0);
}

/// The type string a hover response carries, stripped of its Markdown fence.
fn hover_type(hover: &Value) -> String {
    hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .replace("```kio", "")
        .replace("```", "")
        .trim()
        .to_string()
}

/// `(start_line, start_char, end_line, end_char)` of a hover response.
fn hover_range(hover: &Value) -> (u64, u64, u64, u64) {
    let r = hover.get("range").expect("hover carries a range");
    let get = |k: &str, f: &str| {
        r.get(k)
            .and_then(|p| p.get(f))
            .and_then(Value::as_u64)
            .expect("range field")
    };
    (
        get("start", "line"),
        get("start", "character"),
        get("end", "line"),
        get("end", "character"),
    )
}

fn source_position(source: &str, offset: usize) -> (u64, u64) {
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u64;
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    let character = source[line_start..offset].encode_utf16().count() as u64;
    (line, character)
}

fn source_offset(source: &str, line: u64, character: u64) -> usize {
    let mut line_start = 0;
    for _ in 0..line {
        let newline = source[line_start..]
            .find('\n')
            .unwrap_or_else(|| panic!("LSP line {line} is outside source"));
        line_start += newline + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |newline| line_start + newline);
    let mut utf16 = 0;
    for (relative, ch) in source[line_start..line_end].char_indices() {
        if utf16 == character {
            return line_start + relative;
        }
        utf16 += ch.len_utf16() as u64;
        assert!(
            utf16 <= character,
            "LSP character {character} splits a UTF-16 scalar"
        );
    }
    assert_eq!(
        utf16, character,
        "LSP character {character} is outside line {line}"
    );
    line_end
}

fn apply_lsp_text_edits(source: &str, edits: &[Value]) -> String {
    let mut byte_edits = edits
        .iter()
        .map(|edit| {
            let range = edit.get("range").expect("text edit range");
            let endpoint = |name: &str| {
                let position = range.get(name).expect("range endpoint");
                let line = position.get("line").and_then(Value::as_u64).expect("line");
                let character = position
                    .get("character")
                    .and_then(Value::as_u64)
                    .expect("character");
                source_offset(source, line, character)
            };
            (
                endpoint("start"),
                endpoint("end"),
                edit.get("newText")
                    .and_then(Value::as_str)
                    .expect("newText"),
            )
        })
        .collect::<Vec<_>>();
    byte_edits.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in byte_edits.windows(2) {
        assert!(
            pair[0].1 <= pair[1].0,
            "workspace edit ranges overlap: {byte_edits:?}"
        );
    }

    let mut out = source.to_owned();
    for (start, end, replacement) in byte_edits.into_iter().rev() {
        out.replace_range(start..end, replacement);
    }
    out
}

fn source_range(source: &str, offset: usize, len: usize) -> (u64, u64, u64, u64) {
    let (start_line, start_character) = source_position(source, offset);
    let (end_line, end_character) = source_position(source, offset + len);
    (start_line, start_character, end_line, end_character)
}

fn json_range(range: &Value) -> (u64, u64, u64, u64) {
    let field = |endpoint: &str, coordinate: &str| {
        range
            .get(endpoint)
            .and_then(|position| position.get(coordinate))
            .and_then(Value::as_u64)
            .expect("range coordinate")
    };
    (
        field("start", "line"),
        field("start", "character"),
        field("end", "line"),
        field("end", "character"),
    )
}

fn assert_lsp_nested_rec_call_identity(tag: &str, source: &str) {
    let dir = TempDir::new(tag);
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let call_start = source
        .find("rec(cont) second")
        .expect("mode-qualified recursive call")
        + "rec(cont) ".len();
    let declaration_start = source.find("fn second").expect("member declaration") + "fn ".len();
    let call_position = source_position(source, call_start + 1);
    let call_range = source_range(source, call_start, "second".len());
    let declaration_range = source_range(source, declaration_start, "second".len());

    let hover = wait_for_hover(&mut lsp, &uri, call_position.0, call_position.1);
    assert!(!hover.is_null(), "recursive-call hover must resolve");
    assert_eq!(hover_range(&hover), call_range);
    assert!(hover_type(&hover).contains(". -> ."), "{hover:?}");
    assert!(!hover_type(&hover).contains("Rec_state"), "{hover:?}");

    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": {
                "line": call_position.0,
                "character": call_position.1,
            },
            "context": { "includeDeclaration": true },
        }),
    );
    let references_response =
        lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(references_id));
    assert!(
        references_response.get("error").is_none(),
        "{references_response:?}"
    );
    let mut ranges = references_response
        .get("result")
        .and_then(Value::as_array)
        .expect("recursive references")
        .iter()
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut expected = vec![call_range, declaration_range];
    expected.sort_unstable();
    assert_eq!(ranges, expected);

    let prepared = send_prepare_rename(&mut lsp, &uri, call_position.0, call_position.1);
    assert!(prepared.get("error").is_none(), "{prepared:?}");
    let prepared = prepared.get("result").expect("prepareRename result");
    assert_eq!(
        prepared.get("placeholder").and_then(Value::as_str),
        Some("second")
    );
    assert_eq!(
        json_range(prepared.get("range").expect("prepareRename range")),
        call_range
    );

    let renamed = send_rename(&mut lsp, &uri, call_position.0, call_position.1, "renamed");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let document_changes = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .expect("rename documentChanges");
    assert_eq!(document_changes.len(), 1, "{renamed:?}");
    assert_eq!(
        document_changes[0]
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Value::as_str),
        Some(uri.as_str())
    );
    let edits = document_changes[0]
        .get("edits")
        .and_then(Value::as_array)
        .expect("rename text edits");
    assert_eq!(edits.len(), 2, "{renamed:?}");
    assert!(
        edits
            .iter()
            .all(|edit| edit.get("newText").and_then(Value::as_str) == Some("renamed")),
        "{renamed:?}"
    );
    let mut edit_ranges = edits
        .iter()
        .map(|edit| json_range(edit.get("range").expect("rename edit range")))
        .collect::<Vec<_>>();
    edit_ranges.sort_unstable();
    assert_eq!(edit_ranges, expected);

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_rec_call_inside_normal_operator_operand_keeps_member_identity() {
    let source = concat!(
        "module pkg/main;\n",
        "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
        "fn choose(left: ., right: .) -> . { left }\n",
        "op _ + _ { impl choose; };\n",
        "rec(loop) {\n",
        "  fn first(value: .) -> . { rec(cont) second(value) + value };\n",
        "  fn second(value: .) -> . { rec first(value) }\n",
        "}\n",
    );
    assert_lsp_nested_rec_call_identity("rec-normal-op", source);
}

#[test]
fn lsp_rec_call_inside_variadic_operator_operand_keeps_member_identity() {
    let source = concat!(
        "module pkg/main;\n",
        "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
        "fn empty() -> . { () }\n",
        "fn prepend(head: ., tail: .) -> . { head }\n",
        "varop [* *] { foldr prepend empty; };\n",
        "rec(loop) {\n",
        "  fn first(value: .) -> . { [* rec(cont) second(value), value *] };\n",
        "  fn second(value: .) -> . { rec first(value) }\n",
        "}\n",
    );
    assert_lsp_nested_rec_call_identity("rec-variadic-op", source);
}

/// A clean file must publish nothing — including a file whose surface forms
/// lower into bindings the compiler mints for itself.
///
/// The bug this pins: the `rec` lowering stamped its `rec_rest_*` binding with
/// the rec member's own span, for both the declaration *and* its reference. The
/// "used" filter discards a use whose span equals the declaration's (the guard
/// that stops a declaration counting as its own use), so the binding's only use
/// was thrown away, a live binding read as unused, and the warning surfaced on
/// the user's source — at the span of the whole `fn`. Its preferred,
/// machine-applicable quickfix replaced that span, so accepting the lightbulb
/// deleted the function.
///
/// Asserting a *negative* is the point. Every other assertion about this warning
/// is positive — a file that does have an unused binding produces one — and no
/// positive assertion can see a warning the user should never have been shown.
#[test]
fn lsp_publishes_no_diagnostics_for_a_clean_rec_loop() {
    let dir = TempDir::new("rec-loop-clean");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "import pkg(Bool, I32, add_i32, leq_i32, loop);\n",
        "import control(if);\n",
        "\n",
        // Three value params: the lowering unpacks the rec state tuple with a
        // `rec_rest` binding per param past the first, which is what the leak
        // needs. A single-param `rec` never produced the warning.
        "pub rec(loop) fn sum_to(index: I32, last: I32, total: I32) -> I32 {\n",
        "  if! leq_i32(index, last) {\n",
        "    rec sum_to(add_i32(index, 1), last, add_i32(total, index))\n",
        "  } else {\n",
        "    total\n",
        "  }\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write(
        "control.kio",
        include_str!("../../test-data/poc/elab/workdir/control.kio"),
    );
    dir.write(
        "pkg.kio",
        concat!(
            "module pkg;\n",
            "\n",
            "host type Bool role(bool);\n",
            "\n",
            "host type I32 role(i32);\n",
            "\n",
            "host fn add_i32(a: I32, b: I32) -> I32;\n",
            "\n",
            "host fn leq_i32(a: I32, b: I32) -> Bool;\n",
            "\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
        ),
    );
    dir.write(
        "pkg.pkg.kio",
        "package pkg;\n\nbridge {\n  pkg;\n  pkg/**;\n}\n",
    );

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    // A clean file publishes no diagnostics at all, so there is no empty
    // publish to wait on — listen for a while and assert the stream stays
    // quiet. Any publish naming this file with a non-empty array is the bug.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some(msg) = lsp.try_recv_for(Duration::from_millis(500)) else {
            continue;
        };
        if msg.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
            continue;
        }
        let params = msg.get("params").expect("publish params");
        if params.get("uri").and_then(Value::as_str) != Some(uri.as_str()) {
            continue;
        }
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .expect("diagnostics array");
        assert!(
            diagnostics.is_empty(),
            "a clean `rec(loop)` function must not produce diagnostics — a lowering-synthesized \
             binding has leaked into user-visible output: {diagnostics:?}"
        );
    }

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_newtype_marker_fix_applies_and_retracts_the_diagnostic() {
    let dir = TempDir::new("recursive-newtype-marker-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "newtype Tree : . | Tree { constructor mk; projector un; };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec newtype Tree : . | Tree { constructor mk; projector un; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostics = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("diagnostics array");
    assert_eq!(
        diagnostics.len(),
        1,
        "expected missing-marker diagnostic: {publish:?}"
    );
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("recursive data declaration requires `rec`")
    );
    let invalid_use = source.find("| Tree").expect("unmarked recursive use") + "| ".len();
    let invalid_use_position = source_position(source, invalid_use);
    let invalid_definition = send_definition(
        &mut lsp,
        &uri,
        invalid_use_position.0,
        invalid_use_position.1,
    );
    assert!(
        invalid_definition.is_null(),
        "the tailored missing-marker diagnostic must not grant binding authority: {invalid_definition:?}"
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Add `rec` to this recursive newtype")
            })
        })
        .unwrap_or_else(|| panic!("missing recursive-newtype quick fix: {response:?}"));
    assert_eq!(action.get("kind").and_then(Value::as_str), Some("quickfix"));
    assert_eq!(
        action.get("isPreferred").and_then(Value::as_bool),
        Some(true)
    );
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1), "marker edit must be versioned");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].get("newText").and_then(Value::as_str),
        Some("rec ")
    );

    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, repaired);
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(
        cleared.get("version").and_then(Value::as_i64),
        Some(2),
        "reanalysis must be versioned to the repaired document"
    );
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the applied marker must retract the diagnostic: {cleared:?}"
    );
    let definition = send_definition(&mut lsp, &uri, 2, 24);
    assert_eq!(
        json_range(
            definition
                .get("range")
                .expect("recursive head definition range")
        ),
        (2, 12, 2, 16),
        "the repaired singleton self reference must bind its exact declaration head: {definition:?}"
    );
    let completion_offset =
        applied.find("Tree : ").expect("recursive declaration head") + "Tree : ".len();
    let completion_position = source_position(&applied, completion_offset);
    let completion_response =
        send_completion(&mut lsp, &uri, completion_position.0, completion_position.1);
    let completion = completion_labels(&completion_response);
    assert!(
        completion.contains(&"Tree"),
        "the marked singleton head must be completable inside its own payload: {completion:?}"
    );
    let stale_actions = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        stale_actions
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the version-1 marker action must disappear after the version-2 repair: {stale_actions:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_marked_recursive_newtype_supports_every_navigation_surface() {
    let dir = TempDir::new("recursive-newtype-navigation-surfaces");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec newtype Tree :\n",
        "  . | Tree {\n",
        "  constructor mk;\n",
        "  projector un;\n",
        "};\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    open_and_wait_for_analysis(&mut lsp, &uri, source);

    let head = source.find("newtype Tree").unwrap() + "newtype ".len();
    let use_offset = source.find("| Tree").unwrap() + "| ".len();
    let use_position = source_position(source, use_offset);
    let definition = send_definition(&mut lsp, &uri, use_position.0, use_position.1);
    assert_eq!(
        json_range(definition.get("range").expect("definition range")),
        source_range(source, head, "Tree".len()),
    );

    let hover = wait_for_hover(&mut lsp, &uri, use_position.0, use_position.1);
    assert!(
        hover_type(&hover).contains("Tree"),
        "the marked singleton must have finite hover text: {hover:?}"
    );

    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": {
                "line": use_position.0,
                "character": use_position.1,
            },
            "context": { "includeDeclaration": true },
        }),
    );
    let references = lsp
        .recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(references_id));
    let mut ranges = references
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("marked singleton references must resolve: {references:?}"))
        .iter()
        .map(|location| json_range(location.get("range").expect("reference range")))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut expected = vec![
        source_range(source, head, "Tree".len()),
        source_range(source, use_offset, "Tree".len()),
    ];
    expected.sort_unstable();
    assert_eq!(ranges, expected);

    let renamed = send_rename(&mut lsp, &uri, use_position.0, use_position.1, "Forest");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    let edits = renamed
        .get("result")
        .and_then(|result| result.get("documentChanges"))
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("edits"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("marked singleton rename must publish edits: {renamed:?}"));
    assert_eq!(edits.len(), 2, "{renamed:?}");
    let renamed_source = apply_lsp_text_edits(source, edits);
    assert!(renamed_source.contains("rec newtype Forest"));
    assert!(renamed_source.contains(". | Forest"));

    let symbols_id = lsp.send_request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(symbols_id));
    let symbol_names = symbols
        .get("result")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("marked singleton must remain a document symbol: {symbols:?}"))
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(symbol_names, vec!["Tree"]);

    let folding_id = lsp.send_request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let folding =
        lsp.recv_matching(|message| message.get("id").and_then(Value::as_i64) == Some(folding_id));
    assert!(
        folding
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(|ranges| ranges.iter().any(|range| {
                range
                    .get("startLine")
                    .and_then(Value::as_u64)
                    .is_some_and(|start| start <= 3)
                    && range.get("endLine").and_then(Value::as_u64) == Some(6)
            })),
        "the marked singleton body must remain foldable: {folding:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_invalid_edit_cannot_reuse_stale_recursive_type_bindings() {
    let dir = TempDir::new("recursive-type-stale-binding-authority");
    let valid = concat!(
        "module pkg/main;\n",
        "\n",
        "rec newtype Tree : . | Tree { constructor mk; projector un; };\n",
    );
    let invalid = valid.replacen("rec ", "    ", 1);
    let main_path = dir.write("pkg/main.kio", valid);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": valid,
            }
        }),
    );
    let use_offset = valid.find("| Tree").expect("recursive use") + 2;
    let use_position = source_position(valid, use_offset);
    assert!(
        !send_definition(&mut lsp, &uri, use_position.0, use_position.1).is_null(),
        "the initial marked reference must resolve"
    );

    send_full_text_change(&mut lsp, &uri, 2, &invalid);
    let failed = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        failed
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|diagnostics| diagnostics.iter().any(|diagnostic| {
                diagnostic.get("message").and_then(Value::as_str)
                    == Some("recursive data declaration requires `rec`")
            })),
        "the invalid current version must publish the missing-marker diagnostic: {failed:?}"
    );

    let definition = send_definition(&mut lsp, &uri, use_position.0, use_position.1);
    assert!(
        definition.is_null(),
        "goto-definition must not reuse the version-1 recursive binding: {definition:?}"
    );

    let references_id = lsp.send_request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": use_position.0, "character": use_position.1 },
            "context": { "includeDeclaration": true },
        }),
    );
    let references =
        lsp.recv_matching(|value| value.get("id").and_then(Value::as_i64) == Some(references_id));
    assert!(references.get("error").is_none(), "{references:?}");
    assert!(
        references.get("result").is_none_or(Value::is_null),
        "references must not reuse stale recursive identity: {references:?}"
    );

    let prepared = send_prepare_rename(&mut lsp, &uri, use_position.0, use_position.1);
    assert!(prepared.get("error").is_none(), "{prepared:?}");
    assert!(
        prepared.get("result").is_none_or(Value::is_null),
        "prepareRename must not reuse stale recursive identity: {prepared:?}"
    );
    let renamed = send_rename(&mut lsp, &uri, use_position.0, use_position.1, "Forest");
    assert!(renamed.get("error").is_none(), "{renamed:?}");
    assert!(
        renamed.get("result").is_none_or(Value::is_null),
        "rename must not reuse stale recursive identity: {renamed:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_incomplete_recursive_declaration_retracts_recovery_diagnostics_at_new_version() {
    let dir = TempDir::new("incomplete-recursive-declaration-reanalysis");
    let incomplete = concat!("module pkg/main;\n", "\n", "rec newtype Tree : . |\n",);
    let complete = concat!(
        "module pkg/main;\n",
        "\n",
        "rec newtype Tree : . | Tree { constructor mk; projector un; };\n",
    );
    let main_path = dir.write("pkg/main.kio", incomplete);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 4,
                "text": incomplete,
            }
        }),
    );
    let recovery = wait_for_publish(&mut lsp, &uri);
    assert!(
        recovery
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|diagnostics| !diagnostics.is_empty()),
        "the incomplete declaration must exercise recovery before reanalysis: {recovery:?}"
    );

    send_full_text_change(&mut lsp, &uri, 5, complete);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 5);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the complete marked declaration must retract every stale recovery diagnostic: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_labels_marker_fix_applies_and_retracts_the_diagnostic() {
    let dir = TempDir::new("recursive-labels-marker-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "labels Tree = { leaf: . } | { branch: Tree & Tree };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec labels Tree = { leaf: . } | { branch: Tree & Tree };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-marker diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    let invalid_use = source
        .find("branch: Tree")
        .expect("unmarked recursive alias use")
        + "branch: ".len();
    let invalid_use_position = source_position(source, invalid_use);
    let invalid_definition = send_definition(
        &mut lsp,
        &uri,
        invalid_use_position.0,
        invalid_use_position.1,
    );
    assert!(
        invalid_definition.is_null(),
        "deferred validation must not grant an unmarked labels declaration binding authority: \
         {invalid_definition:?}"
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Add `rec` to this recursive labels declaration")
            })
        })
        .unwrap_or_else(|| panic!("missing recursive-labels quick fix: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1), "marker edit must be versioned");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].get("newText").and_then(Value::as_str),
        Some("rec ")
    );

    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, repaired);
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the applied labels marker must retract the diagnostic: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_anonymous_recursive_labels_marker_fix_applies_and_retracts_the_diagnostic() {
    let dir = TempDir::new("anonymous-recursive-labels-marker-fix");
    let source = concat!("module pkg/main;\n", "\n", "labels { node: . | Node };\n",);
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec labels { node: . | Node };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| {
            panic!("expected anonymous-label missing-marker diagnostic: {publish:?}")
        });
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("recursive data declaration requires `rec`")
    );
    assert!(
        diagnostic.get("data").is_some(),
        "the anonymous-label diagnostic must carry its compiler-owned repair: {diagnostic:?}"
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Add `rec` to this recursive labels declaration")
            })
        })
        .unwrap_or_else(|| panic!("missing anonymous-label quick fix: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, repaired);

    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the anonymous recursive labels repair must reanalyse cleanly: {cleared:?}"
    );
    let definition = send_definition(&mut lsp, &uri, 2, 24);
    assert_eq!(
        json_range(
            definition
                .get("range")
                .expect("generated anonymous-label definition range")
        ),
        (2, 13, 2, 17),
        "the generated recursive head must navigate to its exact label entry: {definition:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_mutual_type_group_fix_applies_and_retracts_the_diagnostic() {
    let dir = TempDir::new("mutual-type-group-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "newtype A : . | B { constructor mk_a; projector un_a; };\n",
        "newtype B : . | A { constructor mk_b; projector un_b; };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "newtype A : . | B { constructor mk_a; projector un_a; };\n",
        "newtype B : . | A { constructor mk_b; projector un_b; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-group diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("mutually recursive data declarations require `rec { ... }`")
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing mutual-group quick fix: {response:?}"));
    assert_eq!(action.get("kind").and_then(Value::as_str), Some("quickfix"));
    assert_eq!(
        action.get("isPreferred").and_then(Value::as_bool),
        Some(true)
    );
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1), "group edit must be versioned");
    assert_eq!(edits.len(), 2);
    assert_eq!(
        edits[0].get("newText").and_then(Value::as_str),
        Some("rec {\n")
    );
    assert_eq!(edits[1].get("newText").and_then(Value::as_str), Some("\n}"));

    let applied = apply_lsp_text_edits(source, edits);
    assert_eq!(applied, repaired);
    send_full_text_change(&mut lsp, &uri, 2, &applied);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert_eq!(
        cleared.get("version").and_then(Value::as_i64),
        Some(2),
        "reanalysis must be versioned to the repaired document"
    );
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the applied group wrapper must retract the diagnostic: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_mutual_type_group_fix_preserves_visibility_and_attached_docs() {
    for (case, source, repaired) in [
        (
            "scoped-public",
            concat!(
                "module pkg/main ;\n",
                "\n",
                "pub(pkg) type A = B;\n",
                "pub(pkg) newtype B : A { constructor mk_b; projector un_b; };\n",
            ),
            concat!(
                "module pkg/main ;\n",
                "\n",
                "rec {\n",
                "pub(pkg) type A = B;\n",
                "pub(pkg) newtype B : A { constructor mk_b; projector un_b; };\n",
                "}\n",
            ),
        ),
        (
            "documented",
            concat!(
                "module pkg/main;\n",
                "\n",
                "/// A docs\n",
                "type A = B;\n",
                "/// B docs\n",
                "newtype B : A { constructor mk_b; projector un_b; };\n",
            ),
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "/// A docs\n",
                "type A = B;\n",
                "/// B docs\n",
                "newtype B : A { constructor mk_b; projector un_b; };\n",
                "}\n",
            ),
        ),
        (
            "scoped-public-label",
            concat!(
                "module pkg/main;\n",
                "\n",
                "pub(pkg) labels { foo: Bar };\n",
                "pub(pkg) newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
            ),
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "pub(pkg) labels { foo: Bar };\n",
                "pub(pkg) newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
                "}\n",
            ),
        ),
    ] {
        let dir = TempDir::new(&format!("mutual-type-group-{case}-fix"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected missing-group diagnostic for {case}: {publish:?}"));
        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
                })
            })
            .unwrap_or_else(|| panic!("missing mutual-group quick fix for {case}: {response:?}"));
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1));
        let applied = apply_lsp_text_edits(source, edits);
        assert_eq!(applied, repaired, "{case}");

        send_full_text_change(&mut lsp, &uri, 2, &applied);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the {case} repair must reanalyse cleanly: {cleared:?}\n{applied}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_mutual_label_group_fix_wraps_the_complete_labels_owner() {
    let dir = TempDir::new("mutual-label-group-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "/// label docs\n",
        "labels { foo: Bar };\n",
        "newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "/// label docs\n",
        "labels { foo: Bar };\n",
        "newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-group diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("mutually recursive data declarations require `rec { ... }`")
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing complete-label-owner quick fix: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(apply_lsp_text_edits(source, edits), repaired);

    send_full_text_change(&mut lsp, &uri, 2, repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the whole-owner group repair must reanalyse cleanly: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_mutual_label_group_fix_includes_acyclic_generated_siblings() {
    let dir = TempDir::new("mutual-label-group-acyclic-sibling-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "labels { other: ., foo: Bar };\n",
        "newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "labels { other: ., foo: Bar };\n",
        "newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-group diagnostic: {publish:?}"));
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing complete-label-owner quick fix: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(apply_lsp_text_edits(source, edits), repaired);

    send_full_text_change(&mut lsp, &uri, 2, repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the whole-owner repair with an acyclic sibling must reanalyse cleanly: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_higher_kinded_type_group_fix_applies_and_retracts_the_diagnostic() {
    let dir = TempDir::new("higher-kinded-mutual-type-group-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type A[*F] = B(F);\n",
        "newtype B[*G] : A(G) { constructor mk_b; projector un_b; };\n",
    );
    let repaired = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "type A[*F] = B(F);\n",
        "newtype B[*G] : A(G) { constructor mk_b; projector un_b; };\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-group diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("mutually recursive data declarations require `rec { ... }`")
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing higher-kinded group repair: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(apply_lsp_text_edits(source, edits), repaired);

    send_full_text_change(&mut lsp, &uri, 2, repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the higher-kinded group repair must reanalyse cleanly: {cleared:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_higher_kinded_type_group_fix_is_withheld_when_invalid() {
    let dir = TempDir::new("ill-kinded-mutual-type-group-fix");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type A[X] = B(X);\n",
        "newtype B[*G] : A(.) { constructor mk_b; projector un_b; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected missing-group diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("mutually recursive data declarations require `rec { ... }`")
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        response
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(|actions| actions.iter().all(|action| {
                action.get("title").and_then(Value::as_str) != Some("Fix recursive type groups")
            })),
        "an ill-kinded wrapped program must not receive a group repair: {response:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_recursive_group_and_marker_repairs_are_applied_sequentially() {
    let dir = TempDir::new("sequential-recursive-data-fixes");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "newtype A : . | B { constructor mk_a; projector un_a; };\n",
        "newtype B : . | A { constructor mk_b; projector un_b; };\n",
        "\n",
        "labels Tree = { leaf: . } | { branch: Tree & Tree };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let first_publish = wait_for_publish(&mut lsp, &uri);
    let first_diagnostics = first_publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("first diagnostics");
    assert_eq!(
        first_diagnostics.len(),
        1,
        "analysis stops at the deterministic first recursive-scope defect: {first_publish:?}"
    );
    assert_eq!(
        first_diagnostics[0].get("message").and_then(Value::as_str),
        Some("mutually recursive data declarations require `rec { ... }`")
    );
    let first_actions = send_code_action(&mut lsp, &uri, &first_diagnostics[0]);
    let group_action = first_actions
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing first group repair: {first_actions:?}"));
    let (group_edits, group_version) = action_edits_for_uri(group_action, &uri);
    assert_eq!(group_version, Some(1));
    let grouped = apply_lsp_text_edits(source, group_edits);

    send_full_text_change(&mut lsp, &uri, 2, &grouped);
    let second_publish = wait_for_publish_version(&mut lsp, &uri, 2);
    let second_diagnostics = second_publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .expect("second diagnostics");
    assert_eq!(
        second_diagnostics.len(),
        1,
        "reanalysis exposes the independent singleton defect: {second_publish:?}"
    );
    assert_eq!(
        second_diagnostics[0].get("message").and_then(Value::as_str),
        Some("recursive data declaration requires `rec`")
    );
    let second_actions = send_code_action(&mut lsp, &uri, &second_diagnostics[0]);
    let marker_action = second_actions
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Add `rec` to this recursive labels declaration")
            })
        })
        .unwrap_or_else(|| panic!("missing second marker repair: {second_actions:?}"));
    let (marker_edits, marker_version) = action_edits_for_uri(marker_action, &uri);
    assert_eq!(marker_version, Some(2));
    let repaired = apply_lsp_text_edits(&grouped, marker_edits);

    send_full_text_change(&mut lsp, &uri, 3, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 3);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the two reanalysed repairs must leave a valid document: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_acyclic_type_group_unwrap_preserves_text_and_retracts_the_diagnostic() {
    let dir = TempDir::new("acyclic-type-group-unwrap");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  // base comment stays attached\n",
        "  pub type Base = .;\n",
        "  /// dependent docs stay attached\n",
        "  pub(pkg) type Box = Base;\n",
        "  // trailing group comment survives\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected acyclic-group diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(11));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("this `rec` group contains no recursive cycle"),
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing safe acyclic-group unwrap: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(!repaired.contains("rec {"));
    assert!(repaired.contains("pub type Base = .;"));
    assert!(repaired.contains("pub(pkg) type Box = Base;"));
    for preserved in [
        "// base comment stays attached",
        "/// dependent docs stay attached",
        "// trailing group comment survives",
    ] {
        assert!(
            repaired.contains(preserved),
            "lost {preserved:?}: {repaired}"
        );
    }

    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the validated unwrap must produce source-ordered declarations: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_acyclic_type_group_is_topologically_ordered_before_unwrap() {
    let dir = TempDir::new("acyclic-type-group-reorder");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  type Box = Base;\n",
        "  type Base = .;\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected acyclic-group diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("this `rec` group contains no recursive cycle"),
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing topological group fix: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(!repaired.contains("rec {"));
    assert!(
        repaired.find("type Base").unwrap() < repaired.find("type Box").unwrap(),
        "dependency must precede its user: {repaired}"
    );

    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the validated reorder must retract the diagnostic: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_later_type_move_is_versioned_applied_and_retracted() {
    let dir = TempDir::new("later-type-move");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "type User = Dependency // 😀 keeps UTF-16 edits honest\n",
        ";\n",
        "type Dependency = .;\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 7,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected directed source-order diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Dependency` is declared later and is not visible here"),
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str)
                    == Some("Move `Dependency` before its use")
            })
        })
        .unwrap_or_else(|| panic!("missing stable dependency move: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(7));
    assert_eq!(edits.len(), 2);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(
        repaired.find("type Dependency").unwrap() < repaired.find("type User").unwrap(),
        "dependency must move before the unchanged user: {repaired}"
    );
    assert!(repaired.contains("😀 keeps UTF-16 edits honest"));

    send_full_text_change(&mut lsp, &uri, 8, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 8);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the validated move must retract the diagnostic: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_later_newtype_member_move_is_versioned_applied_and_retracted() {
    let dir = TempDir::new("later-newtype-member-move");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn before() {\n",
        "  Later.un(Later.mk(()))\n",
        "}\n",
        "\n",
        "newtype Later : . { constructor mk; projector un; };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 3,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected directed source-order diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Later` is declared later and is not visible here"),
    );
    assert_eq!(
        json_range(diagnostic.get("range").expect("primary range")),
        (3, 2, 3, 7),
    );
    let related = diagnostic
        .get("relatedInformation")
        .and_then(Value::as_array)
        .and_then(|related| related.first())
        .unwrap_or_else(|| panic!("later declaration must be labelled: {diagnostic:?}"));
    assert_eq!(
        json_range(
            related
                .get("location")
                .and_then(|location| location.get("range"))
                .expect("later-declaration range"),
        ),
        (6, 8, 6, 13),
    );

    let definition = send_definition(&mut lsp, &uri, 3, 4);
    assert!(
        definition.is_null(),
        "diagnostic-only later-name lookup must not bind definition identity: {definition:?}"
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Move `Later` before its use")
            })
        })
        .unwrap_or_else(|| panic!("missing stable dependency move: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(3));
    assert_eq!(edits.len(), 2);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(
        repaired.find("newtype Later").unwrap() < repaired.find("fn before").unwrap(),
        "dependency must move before the unchanged user: {repaired}"
    );
    assert!(repaired.contains("Later.un(Later.mk(()))"));

    send_full_text_change(&mut lsp, &uri, 4, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 4);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the validated move must retract the diagnostic: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_later_identity_alias_member_move_is_versioned_applied_and_retracted() {
    let dir = TempDir::new("later-identity-alias-member-move");
    let source = concat!(
        "module pkg/main;\n",
        "import pkg/origin as imported;\n",
        "\n",
        "fn before() {\n",
        "  Later.open(Later.make(()))\n",
        "}\n",
        "\n",
        "type Later = imported.Tag;\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write(
        "pkg/origin.kio",
        concat!(
            "module pkg/origin;\n",
            "pub newtype Tag : . { pub constructor make; pub projector open; };\n",
        ),
    );
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 5,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected directed source-order diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Later` is declared later and is not visible here"),
    );
    assert_eq!(
        json_range(diagnostic.get("range").expect("primary range")),
        (4, 2, 4, 7),
    );
    let related = diagnostic
        .get("relatedInformation")
        .and_then(Value::as_array)
        .and_then(|related| related.first())
        .unwrap_or_else(|| panic!("later alias declaration must be labelled: {diagnostic:?}"));
    assert_eq!(
        json_range(
            related
                .get("location")
                .and_then(|location| location.get("range"))
                .expect("later alias range"),
        ),
        (7, 5, 7, 10),
    );

    let definition = send_definition(&mut lsp, &uri, 4, 4);
    assert!(
        definition.is_null(),
        "diagnostic-only later alias lookup must not bind definition identity: {definition:?}"
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Move `Later` before its use")
            })
        })
        .unwrap_or_else(|| panic!("missing stable alias move: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(5));
    assert_eq!(edits.len(), 2);
    let repaired = apply_lsp_text_edits(source, edits);
    assert!(
        repaired.find("type Later").unwrap() < repaired.find("fn before").unwrap(),
        "alias must move before the unchanged member use: {repaired}"
    );
    assert!(repaired.contains("Later.open(Later.make(()))"));

    send_full_text_change(&mut lsp, &uri, 6, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 6);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the validated alias move must retract the diagnostic: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_invalid_later_alias_member_heads_have_no_move_action() {
    let cases = [
        (
            "structural",
            concat!(
                "module pkg/main;\n",
                "\n",
                "fn before() { Later.missing(()) }\n",
                "type Later = .;\n",
            ),
        ),
        (
            "missing-member",
            concat!(
                "module pkg/main;\n",
                "import pkg/origin as imported;\n",
                "\n",
                "fn before() { Later.missing(()) }\n",
                "type Later = imported.Tag;\n",
            ),
        ),
        (
            "no-import-edge",
            concat!(
                "module pkg/main;\n",
                "\n",
                "fn before() { Later.make(()) }\n",
                "type Later = pkg/origin.Tag;\n",
            ),
        ),
        (
            "later-local-target",
            concat!(
                "module pkg/main;\n",
                "\n",
                "fn before() { Later.make(()) }\n",
                "type Later = Target;\n",
                "pub newtype Target : . { pub constructor make; pub projector open; };\n",
            ),
        ),
    ];

    for (case, source) in cases {
        let dir = TempDir::new(&format!("later-alias-no-move-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write(
            "pkg/origin.kio",
            concat!(
                "module pkg/origin;\n",
                "pub newtype Tag : . { pub constructor make; pub projector open; };\n",
            ),
        );
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected directed diagnostic for {case}: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("`Later` is declared later and is not visible here"),
        );
        assert!(
            diagnostic
                .get("data")
                .and_then(|data| data.get("fixes"))
                .is_none(),
            "{case} must not publish an unproven machine edit: {diagnostic:?}"
        );

        let response = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            response
                .get("result")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "{case} must not offer a move that leaves the member path invalid: {response:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_later_generated_label_member_has_no_partial_move_action() {
    let dir = TempDir::new("later-generated-label-member");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "fn before() -> . {\n",
        "  let value = Foo.mk(());\n",
        "  ()\n",
        "}\n",
        "\n",
        "labels { foo: ., bar: . };\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected directed source-order diagnostic: {publish:?}"));
    assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(13));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("`Foo` is declared later and is not visible here"),
    );
    assert_eq!(
        diagnostic
            .get("data")
            .and_then(|data| data.get("help"))
            .and_then(Value::as_str),
        Some(
            "move the source declaration that provides `Foo` before this use; use a bare \
             `rec { ... }` group only when the declarations form one genuine recursive \
             component"
        ),
    );
    assert!(
        diagnostic
            .get("data")
            .and_then(|data| data.get("fixes"))
            .is_none(),
        "a partial generated-label edit must not reach diagnostic data: {diagnostic:?}"
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        response
            .get("result")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the generated label head must not offer a partial move action: {response:?}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_overbroad_type_group_is_split_into_exact_components() {
    let dir = TempDir::new("overbroad-type-group-split");
    let source = concat!(
        "module pkg/main;\n",
        "\n",
        "rec {\n",
        "  newtype A : B { constructor mk_a; projector un_a; };\n",
        "  newtype B : A { constructor mk_b; projector un_b; };\n",
        "  newtype C : D { constructor mk_c; projector un_c; };\n",
        "  newtype D : C { constructor mk_d; projector un_d; };\n",
        "  newtype Self : Self { constructor mk_self; projector un_self; };\n",
        "  labels Loop = { stop: . } | { next: Loop };\n",
        "  type Helper = .;\n",
        "}\n",
    );
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();

    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "kio",
                "version": 1,
                "text": source,
            }
        }),
    );

    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish
        .get("diagnostics")
        .and_then(Value::as_array)
        .and_then(|diagnostics| diagnostics.first())
        .unwrap_or_else(|| panic!("expected multiple-component diagnostic: {publish:?}"));
    assert_eq!(
        diagnostic.get("message").and_then(Value::as_str),
        Some("this `rec` group contains multiple independent components"),
    );

    let response = send_code_action(&mut lsp, &uri, diagnostic);
    let action = response
        .get("result")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
            })
        })
        .unwrap_or_else(|| panic!("missing component split: {response:?}"));
    let (edits, version) = action_edits_for_uri(action, &uri);
    assert_eq!(version, Some(1));
    assert_eq!(edits.len(), 1);
    let repaired = apply_lsp_text_edits(source, edits);
    assert_eq!(repaired.matches("rec {").count(), 2, "{repaired}");
    assert!(repaired.contains("rec newtype Self"), "{repaired}");
    assert!(repaired.contains("rec labels Loop"), "{repaired}");
    assert!(repaired.contains("type Helper = .;"), "{repaired}");

    send_full_text_change(&mut lsp, &uri, 2, &repaired);
    let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
    assert!(
        cleared
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "the exact component split must retract the diagnostic: {cleared:?}\n{repaired}"
    );

    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_structural_type_fixes_are_withheld_for_ambiguous_comment_ownership() {
    for (case, source) in [
        (
            "later-move",
            concat!(
                "module pkg/main;\n",
                "\n",
                "// This comment may describe User.\n",
                "type User = Dependency;\n",
                "type Dependency = .;\n",
            ),
        ),
        (
            "group-reorder",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  type User = Dependency;\n",
                "  // This comment may describe Dependency.\n",
                "  type Dependency = .;\n",
                "}\n",
            ),
        ),
        (
            "missing-mutual-group",
            concat!(
                "module pkg/main;\n",
                "\n",
                "// This comment may describe A.\n",
                "newtype A : B { constructor mk_a; projector un_a; };\n",
                "newtype B : A { constructor mk_b; projector un_b; };\n",
            ),
        ),
        (
            "missing-mutual-label-group",
            concat!(
                "module pkg/main;\n",
                "\n",
                "// This comment may describe foo.\n",
                "labels { foo: Bar };\n",
                "newtype Bar : Foo { constructor mk_bar; projector un_bar; };\n",
            ),
        ),
    ] {
        let dir = TempDir::new(&format!("comment-owned-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );
        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected structural diagnostic for {case}: {publish:?}"));
        let response = send_code_action(&mut lsp, &uri, diagnostic);
        assert!(
            !response
                .get("result")
                .and_then(Value::as_array)
                .is_some_and(|actions| actions.iter().any(|action| {
                    action
                        .get("title")
                        .and_then(Value::as_str)
                        .is_some_and(|title| {
                            title == "Fix recursive type groups" || title.starts_with("Move `")
                        })
                })),
            "comment-owned structural edit must be withheld for {case}: {response:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_host_source_order_diagnostics_retract_after_declaration_reorder() {
    for (case, name, source, repaired, use_fragment, definition_column) in [
        (
            "function",
            "later",
            "module pkg/main;\npub fn before() -> . { later() }\nhost fn later() -> .;\n",
            "module pkg/main;\nhost fn later() -> .;\npub fn before() -> . { later() }\n",
            "later() }",
            8,
        ),
        (
            "type",
            "Later",
            "module pkg/main;\npub fn before(value: Later) -> Later { value }\nhost type Later;\n",
            "module pkg/main;\nhost type Later;\npub fn before(value: Later) -> Later { value }\n",
            "Later) ->",
            10,
        ),
    ] {
        let dir = TempDir::new(&format!("host-source-order-{case}"));
        let initial = "module pkg/main;\nfn seed() -> . { missing() }\n";
        let main_path = dir.write("pkg/main.kio", initial);
        dir.write_pkg_root_package();
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": initial}}),
        );
        let seed = wait_for_publish(&mut lsp, &uri);
        assert!(seed["diagnostics"].as_array().is_some_and(|diagnostics| {
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["code"] == 13)
        }));
        send_full_text_change(&mut lsp, &uri, 2, source);
        let publish = wait_for_publish_version(&mut lsp, &uri, 2);
        let diagnostics = publish["diagnostics"]
            .as_array()
            .expect("diagnostics array");
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic["code"] == 13)
            .unwrap_or_else(|| panic!("later {case} must be a name error: {publish:?}"));
        assert_eq!(diagnostic["message"], format!("unbound name `{name}`"));
        let reference = source.find(use_fragment).expect("original reference");
        let (line, column) = source_position(source, reference);
        assert_eq!(
            json_range(&diagnostic["range"]),
            (
                line,
                column,
                line,
                column + u64::try_from(name.len()).unwrap()
            )
        );

        send_full_text_change(&mut lsp, &uri, 3, repaired);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 3);
        assert_eq!(cleared["version"], 3);
        assert!(
            cleared["diagnostics"].as_array().is_some_and(Vec::is_empty),
            "preceding {case} must analyse cleanly: {cleared:?}"
        );
        let reference = repaired.find(use_fragment).expect("repaired reference");
        let (line, column) = source_position(repaired, reference);
        let definition = send_definition(&mut lsp, &uri, line, column);
        assert_eq!(definition["uri"], uri);
        assert_eq!(
            json_range(&definition["range"]),
            (
                1,
                definition_column,
                1,
                definition_column + u64::try_from(name.len()).unwrap()
            ),
            "the legal reference keeps its exact host declaration identity"
        );
        assert!(
            !send_hover(&mut lsp, &uri, line, column).is_null(),
            "the repaired {case} reference has typed hover information"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_owned_parse_context_preserves_the_error_range() {
    let source = "module pkg/main;\nhost fn value() -> . { () }\n";
    let dir = TempDir::new("owned-parse-context");
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostics = publish["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{publish:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic["code"], 11);
    assert_eq!(
        diagnostic["message"],
        "a `host fn` declares a function supplied by the host, not a Kio function body"
    );
    assert_eq!(json_range(&diagnostic["range"]), (1, 21, 1, 22));
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(response["result"].as_array().is_none_or(Vec::is_empty));
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_type_as_value_preserves_the_inner_binding_and_has_no_missing_name_fix() {
    let source = "module pkg/main;\nfn outer[A](value: A) -> . {\n  let inner = .[A](other: A) -> . { A };\n  ()\n}\n";
    let dir = TempDir::new("type-as-value-binding");
    let main_path = dir.write("pkg/main.kio", source);
    let provider_source = "module pkg/provider;\npub type Value = .;\n";
    let provider_path = dir.write("pkg/provider.kio", provider_source);
    dir.write("pkg/decoy.kio", "module pkg/decoy;\npub type Value = .;\n");
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    let diagnostic = publish["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["message"] == "`A` is a type, not a value")
        .unwrap_or_else(|| panic!("missing known-binding diagnostic: {publish:?}"));
    assert_eq!(diagnostic["code"], 13);
    let primary = source.rfind("A }").unwrap();
    let (line, column) = source_position(source, primary);
    assert_eq!(
        json_range(&diagnostic["range"]),
        (line, column, line, column + 1)
    );
    let related = diagnostic["relatedInformation"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["message"], "type parameter declared here");
    assert_eq!(related[0]["location"]["uri"], uri);
    let declaration = source.rfind("[A]").unwrap();
    let (line, column) = source_position(source, declaration);
    assert_eq!(
        json_range(&related[0]["location"]["range"]),
        (line, column, line, column + 3)
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        response["result"].as_array().is_none_or(Vec::is_empty),
        "a known type must not get missing-import/stub fixes: {response:?}"
    );
    let imported = "module pkg/main;\nimport pkg/provider as p;\nfn value() -> . { p.Value }\n";
    send_full_text_change(&mut lsp, &uri, 2, imported);
    let publish = wait_for_publish_version(&mut lsp, &uri, 2);
    let diagnostic = publish["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["message"] == "`p.Value` is a type, not a value")
        .unwrap_or_else(|| panic!("missing imported type diagnostic: {publish:?}"));
    assert_eq!(diagnostic["code"], 14);
    let (line, column) = source_position(imported, imported.find("p.Value").unwrap());
    assert_eq!(
        json_range(&diagnostic["range"]),
        (line, column, line, column + 7)
    );
    let related = diagnostic["relatedInformation"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["message"], "type alias declared here");
    assert_eq!(
        related[0]["location"]["uri"],
        path_to_file_uri(&provider_path)
    );
    let (line, column) = source_position(provider_source, provider_source.find("Value").unwrap());
    assert_eq!(
        json_range(&related[0]["location"]["range"]),
        (line, column, line, column + 5)
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        response["result"].as_array().is_none_or(Vec::is_empty),
        "the exact selected type is not a missing-name fix: {response:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_imported_parameter_diagnostics_retain_signature_owner_across_overlays() {
    for (import, callee, binding) in [
        ("import pkg/provider as p;", "p.take", ""),
        ("import pkg/provider as p;", "f", "let f = p.take; "),
        ("import pkg/provider(take);", "f", "let f = take; "),
    ] {
        let dir = TempDir::new("imported-parameter-source");
        dir.write_pkg_root_package();
        dir.write(
            "pkg/origin.kio",
            "module pkg/origin;\npub newtype W : . { pub constructor mk; pub projector get; };\n",
        );
        dir.write("pkg/decoy.kio", "module pkg/decoy;\npub type W = .;\n");
        let provider = "module pkg/provider;\nimport pkg/origin as o;\npub type W = o.W;\n\npub fn take(value: W) -> . { () }\n";
        let provider_path = dir.write("pkg/provider.kio", provider);
        let source = format!(
            "module pkg/main;\n{import}\n// Caller-only padding makes a foreign annotation offset land on visibly unrelated source text, not a type.\nfn caller() -> . {{ {binding}{callee}(()) }}\n"
        );
        let main_path = dir.write("pkg/main.kio", &source);
        let uri = path_to_file_uri(&main_path);
        let provider_uri = path_to_file_uri(&provider_path);
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        for (file_uri, text) in [(&provider_uri, provider), (&uri, source.as_str())] {
            lsp.send_notification(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": file_uri, "languageId": "kio", "version": 1, "text": text,
                }}),
            );
        }
        let assert_signature = |publish: &Value, provider: &str| {
            assert_eq!(publish["version"], 1, "{publish:?}");
            let errors = publish["diagnostics"].as_array().unwrap();
            assert_eq!(errors.len(), 1, "{publish:?}");
            assert_eq!(errors[0]["code"], 14, "{publish:?}");
            assert_eq!(
                json_range(&errors[0]["range"]),
                source_range(&source, source.rfind("()").unwrap(), 2)
            );
            let related = errors[0]["relatedInformation"].as_array().unwrap();
            assert_eq!(related.len(), 1, "{publish:?}");
            assert_eq!(related[0]["location"]["uri"], provider_uri, "{publish:?}");
            let annotation = provider.find("value: W").unwrap() + "value: ".len();
            assert_eq!(
                json_range(&related[0]["location"]["range"]),
                source_range(provider, annotation, 1),
                "{publish:?}"
            );
        };
        let initial = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_signature(&initial, provider);
        let shifted = provider.replace("pub fn take", "// 😀 shifted signature\n\npub fn take");
        send_full_text_change(&mut lsp, &provider_uri, 2, &shifted);
        let refreshed = wait_for_publish_version(&mut lsp, &uri, 1);
        assert_signature(&refreshed, &shifted);
        assert_eq!(fs::read_to_string(provider_path).unwrap(), provider);
        let corrected = source.replace(&format!("{callee}(())"), "()");
        send_full_text_change(&mut lsp, &uri, 2, &corrected);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .all(|diagnostic| diagnostic["severity"] != 1),
            "{cleared:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_module_header_diagnostic_describes_the_local_header_shape() {
    let source = "module package main;\n";
    let dir = TempDir::new("module-header-diagnostic");
    let main_path = dir.write("pkg/main.kio", source);
    dir.write_pkg_root_package();
    let mut lsp = LspProcess::spawn();
    lsp.initialize(&path_to_file_uri(dir.path()));
    let uri = path_to_file_uri(&main_path);
    lsp.send_notification(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
    );
    let publish = wait_for_publish(&mut lsp, &uri);
    assert_eq!(
        publish.get("uri").and_then(Value::as_str),
        Some(uri.as_str())
    );
    let diagnostic = publish["diagnostics"].as_array().unwrap().first().unwrap();
    assert_eq!(diagnostic["code"], 11);
    assert_eq!(
        diagnostic["message"],
        "`module` must be followed by one module path, not adjacent names"
    );
    assert_eq!(
        diagnostic["range"],
        json!({"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 19}})
    );
    let response = send_code_action(&mut lsp, &uri, diagnostic);
    assert!(
        response["result"].as_array().is_none_or(Vec::is_empty),
        "the parser cannot guess the intended module path: {response:?}"
    );
    assert_eq!(lsp.shutdown(), 0);
}

#[test]
fn lsp_unit_and_product_type_repairs_apply_and_reanalyse() {
    for (case, source, expected, message, title) in [
        (
            "unit",
            "module pkg/main;\npub type U=();\n",
            "module pkg/main;\npub type U= . ;\n",
            "`()` is unit value syntax; write `.` for the unit type",
            "Use `.` for the unit type",
        ),
        (
            "commented-unit",
            "module pkg/main;\npub type U=(// keep unit\n);\n",
            "module pkg/main;\npub type U= . // keep unit\n;\n",
            "`()` is unit value syntax; write `.` for the unit type",
            "Use `.` for the unit type",
        ),
        (
            "product",
            "module pkg/main;\npub type Pack[A,B] = A & B;\npub type T = (Pack(., .), // keep separator\n .);\n",
            "module pkg/main;\npub type Pack[A,B] = A & B;\npub type T = (Pack(., .) &  // keep separator\n .);\n",
            "commas form tuple values, not product types; write `A & B`",
            "Use `&` for the product type",
        ),
        (
            "grouped-product",
            "module pkg/main;\npub type T = (. -> ., . | !);\n",
            "module pkg/main;\npub type T = ((. -> .) &  (. | !));\n",
            "commas form tuple values, not product types; write `A & B`",
            "Use `&` for the product type",
        ),
    ] {
        let dir = TempDir::new(&format!("type-syntax-fix-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();
        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "kio", "version": 1, "text": source}}),
        );
        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish["diagnostics"].as_array().unwrap().first().unwrap();
        assert_eq!(diagnostic["code"], 11, "wrong category for {case}");
        assert_eq!(
            diagnostic["message"], message,
            "wrong diagnostic for {case}"
        );
        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response["result"]
            .as_array()
            .and_then(|actions| {
                actions
                    .iter()
                    .find(|action| action["title"].as_str() == Some(title))
            })
            .unwrap_or_else(|| panic!("missing {case} fix: {response:?}"));
        assert_eq!(action["kind"], "quickfix");
        assert_eq!(action["isPreferred"], true);
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1), "{case} fix must be versioned");
        let applied = apply_lsp_text_edits(source, edits);
        assert_eq!(applied, expected, "wrong applied source for {case}");
        send_full_text_change(&mut lsp, &uri, 2, &applied);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert_eq!(cleared["version"], 2);
        assert!(
            cleared["diagnostics"].as_array().is_some_and(Vec::is_empty),
            "the applied {case} fix must clear diagnostics: {cleared:?}"
        );
        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_redundant_recursive_data_markers_are_removed_and_reanalysed() {
    for (case, source) in [
        (
            "newtype",
            concat!(
                "module pkg/main;\n",
                "\n",
                "// declaration ownership\n",
                "pub(pkg) rec newtype Box[A] : A { constructor mk; projector un; };\n",
            ),
        ),
        (
            "labels",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec labels Pair = { first: ., second: . };\n",
            ),
        ),
    ] {
        let dir = TempDir::new(&format!("redundant-rec-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected redundant-marker diagnostic: {publish:?}"));
        assert_eq!(
            diagnostic.get("code").and_then(Value::as_i64),
            Some(11),
            "wrong diagnostic category for {case}"
        );
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("this `rec` marker is unnecessary")
        );

        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("title").and_then(Value::as_str) == Some("Remove unnecessary `rec`")
                })
            })
            .unwrap_or_else(|| panic!("missing remove-marker quick fix: {response:?}"));
        assert_eq!(action.get("kind").and_then(Value::as_str), Some("quickfix"));
        assert_eq!(
            action.get("isPreferred").and_then(Value::as_bool),
            Some(true)
        );
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1), "marker edit must be versioned");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].get("newText").and_then(Value::as_str), Some(""));

        let applied = apply_lsp_text_edits(source, edits);
        assert!(!applied.contains("rec "));
        if case == "newtype" {
            assert!(applied.contains("// declaration ownership"));
            assert!(applied.contains("pub(pkg)"));
            assert!(applied.contains("newtype Box"));
        }
        send_full_text_change(&mut lsp, &uri, 2, &applied);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert_eq!(
            cleared.get("version").and_then(Value::as_i64),
            Some(2),
            "reanalysis must be versioned to the repaired {case} document"
        );
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the applied marker removal must retract the {case} diagnostic: {cleared:?}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_one_member_type_groups_unwrap_to_the_canonical_singleton_form() {
    for (case, source, required_fragment) in [
        (
            "recursive-newtype",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  /// recursive tree docs\n",
                "  pub(pkg) newtype Tree : . | Tree { constructor mk; projector un; };\n",
                "}\n",
            ),
            "pub(pkg) rec newtype Tree",
        ),
        (
            "recursive-labels",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  /// recursive labels docs\n",
                "  pub labels Tree = { leaf: . } | { branch: Tree & Tree };\n",
                "}\n",
            ),
            "pub rec labels Tree",
        ),
        (
            "acyclic-alias",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  /// ordinary alias docs\n",
                "  pub type Nothing_recursive = .;\n",
                "}\n",
            ),
            "pub type Nothing_recursive",
        ),
    ] {
        let dir = TempDir::new(&format!("one-member-group-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected singleton-group diagnostic: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(11));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("one recursive data declaration uses a `rec` modifier, not a group")
        );

        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
                })
            })
            .unwrap_or_else(|| panic!("missing singleton-group quick fix: {response:?}"));
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1), "group edit must be versioned");
        assert_eq!(
            edits.len(),
            1,
            "unexpected edit shape for {case}: {edits:?}"
        );
        let applied = apply_lsp_text_edits(source, edits);
        assert!(
            applied.contains(required_fragment),
            "canonical singleton spelling missing after {case} edit: {applied:?}"
        );
        assert!(!applied.contains("rec {"));

        send_full_text_change(&mut lsp, &uri, 2, &applied);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert_eq!(cleared.get("version").and_then(Value::as_i64), Some(2));
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the singleton-group fix must produce a clean {case} document: {cleared:?}\n{applied}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}

#[test]
fn lsp_nested_recursive_markers_are_removed_from_mutual_type_groups() {
    for (case, source) in [
        (
            "newtype",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  rec newtype A : B { constructor mk_a; projector un_a; };\n",
                "  newtype B : A { constructor mk_b; projector un_b; };\n",
                "}\n",
            ),
        ),
        (
            "labels",
            concat!(
                "module pkg/main;\n",
                "\n",
                "rec {\n",
                "  rec labels A = { to_b: B };\n",
                "  newtype B : A { constructor mk_b; projector un_b; };\n",
                "}\n",
            ),
        ),
    ] {
        let dir = TempDir::new(&format!("nested-rec-marker-{case}"));
        let main_path = dir.write("pkg/main.kio", source);
        dir.write_pkg_root_package();

        let mut lsp = LspProcess::spawn();
        lsp.initialize(&path_to_file_uri(dir.path()));
        let uri = path_to_file_uri(&main_path);
        lsp.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "kio",
                    "version": 1,
                    "text": source,
                }
            }),
        );

        let publish = wait_for_publish(&mut lsp, &uri);
        let diagnostic = publish
            .get("diagnostics")
            .and_then(Value::as_array)
            .and_then(|diagnostics| diagnostics.first())
            .unwrap_or_else(|| panic!("expected nested-marker diagnostic: {publish:?}"));
        assert_eq!(diagnostic.get("code").and_then(Value::as_i64), Some(11));
        assert_eq!(
            diagnostic.get("message").and_then(Value::as_str),
            Some("the enclosing `rec { ... }` already supplies recursive scope")
        );

        let response = send_code_action(&mut lsp, &uri, diagnostic);
        let action = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|actions| {
                actions.iter().find(|action| {
                    action.get("title").and_then(Value::as_str) == Some("Fix recursive type groups")
                })
            })
            .unwrap_or_else(|| panic!("missing nested-marker quick fix: {response:?}"));
        let (edits, version) = action_edits_for_uri(action, &uri);
        assert_eq!(version, Some(1), "group edit must be versioned");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].get("newText").and_then(Value::as_str), Some(""));
        let applied = apply_lsp_text_edits(source, edits);
        assert!(!applied.contains("  rec newtype"));
        assert!(!applied.contains("  rec labels"));

        send_full_text_change(&mut lsp, &uri, 2, &applied);
        let cleared = wait_for_publish_version(&mut lsp, &uri, 2);
        assert!(
            cleared
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "the nested-marker fix must produce a clean {case} group: {cleared:?}\n{applied}"
        );

        assert_eq!(lsp.shutdown(), 0);
    }
}
