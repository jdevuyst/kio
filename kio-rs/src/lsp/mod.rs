//! LSP server entry point (`kio lsp`).
//!
//! Surface: live diagnostics, hover, goto-definition,
//! find-references, document highlights, document symbols, folding
//! ranges, completion, formatting, semantic tokens, rename, inlay
//! hints, and signature help. The server speaks JSON-RPC over stdio (the LSP default;
//! every editor client supports it), accepts the handshake (`initialize` / `initialized` /
//! `shutdown` / `exit`), the sync-event quad (`textDocument/didOpen` /
//! `didChange` / `didSave` / `didClose`), emits
//! `textDocument/publishDiagnostics`, and handles
//! `textDocument/hover`, `textDocument/definition`,
//! `textDocument/references`, `textDocument/documentHighlight`,
//! `textDocument/documentSymbol`,
//! `textDocument/foldingRange`, `textDocument/completion`,
//! `textDocument/formatting`, `textDocument/semanticTokens/full`,
//! `textDocument/prepareRename`, `textDocument/rename`, and
//! `textDocument/codeAction`, `textDocument/inlayHint`, and
//! `textDocument/signatureHelp`.
//!
//! Architecture:
//!
//! - **Wire layer.** `lsp-server` provides the `Connection` /
//!   `Message` / `Request` / `Notification` plumbing and stdio
//!   framing. `lsp-types` provides the strongly-typed Position /
//!   Diagnostic / capability structs.
//! - **Overlay layer.** [`state::ServerState`] holds an in-memory
//!   [`state::OpenDocument`] per open URI. `didOpen` seeds the
//!   overlay; `didChange` applies edits in place; `didClose` drops
//!   the entry. The overlay text overrides disk for analysis as long
//!   as a document is open.
//! - **Scheduler / worker.** [`worker::Scheduler`] drives a debounce
//!   thread and an analysis worker thread. Successive `didChange`
//!   events within the debounce window coalesce to one analysis; the
//!   worker runs [`crate::cmd::check::analyze_workspace_at_with_overlay`]
//!   off the main thread and posts the result back on a
//!   [`crossbeam_channel`].
//! - **Diagnostic layer.** [`diagnostics::locate_to_diagnostic`] maps
//!   each `LocatedError` to an LSP `Diagnostic`;
//!   [`positions::LineIndex`] handles the byte-offset → UTF-16
//!   `Position` conversion.
//!
//! Main loop: a `select!` over the connection's receiver and the
//! worker's result channel. Notifications drive the overlay /
//! scheduler; results drive `publishDiagnostics`. Shutdown sends a
//! sentinel to the worker, drains any in-flight result, then joins.

// `lsp_types::Uri` carries an internal parse-cache `Cell`, which
// trips clippy's `mutable_key_type` lint when used as a map / set
// key. See `state.rs` for the equivalent suppression and rationale.
#![allow(clippy::mutable_key_type)]

pub(crate) mod block_labels;
pub(crate) mod cancel;
pub mod code_actions;
pub mod completion;
pub mod definition;
pub mod diagnostics;
pub mod docs;
pub mod folding;
pub mod formatting;
pub mod highlight;
pub mod hover;
pub mod inlay_hints;
pub(crate) mod label_reuse;
pub mod positions;
pub mod references;
pub mod rename;
pub mod semantic_tokens;
pub mod signature_help;
pub mod snapshot;
pub mod state;
pub mod symbols;
pub mod util;
pub mod worker;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, select};
use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, CodeActionResolveRequest, Completion as CompletionRequest,
    DocumentHighlightRequest, DocumentSymbolRequest, FoldingRangeRequest,
    Formatting as FormattingRequest, GotoDefinition, HoverRequest, InlayHintRequest,
    PrepareRenameRequest, References as ReferencesRequest, Rename as RenameRequest,
    Request as LspRequest, SemanticTokensFullRequest, SignatureHelpRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionParams, CodeActionProviderCapability,
    CompletionOptions, CompletionParams, CompletionResponse, Diagnostic,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentFormattingParams, DocumentHighlightParams,
    DocumentSymbolParams, DocumentSymbolResponse, FoldingRange, FoldingRangeParams,
    FoldingRangeProviderCapability, GotoDefinitionParams, HoverParams, HoverProviderCapability,
    InitializeParams, InlayHintOptions, InlayHintParams, InlayHintServerCapabilities,
    PublishDiagnosticsParams, ReferenceParams, RenameOptions, RenameParams, SaveOptions,
    SemanticTokensParams, SemanticTokensResult, ServerCapabilities, SignatureHelpOptions,
    SignatureHelpParams, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Uri,
};

use crate::ExitCode;
use crate::cmd::check::{AnalysisFailure, LspAnalysis};
use crate::lsp::cancel::CancellationToken;
use crate::lsp::code_actions::{
    FixValidationContext, handle_code_action, handle_code_action_resolve,
};
use crate::lsp::definition::handle_definition;
use crate::lsp::diagnostics::{
    locate_to_diagnostic_with_sources_counted, path_to_uri, warning_to_diagnostic,
};
use crate::lsp::docs::doc_hover_at_with_module;
use crate::lsp::folding::folding_ranges;
use crate::lsp::formatting::{handle_formatting_with_file_context, try_handle_formatting};
use crate::lsp::highlight::handle_document_highlight;
use crate::lsp::hover::handle_hover;
use crate::lsp::inlay_hints::handle_inlay_hints;
use crate::lsp::label_reuse::LabelReuseSnapshot;
use crate::lsp::positions::LineIndex;
use crate::lsp::references::handle_references;
use crate::lsp::rename::{
    RenameParseContext, handle_prepare_rename_with_module, handle_rename_versioned_with_context,
};
use crate::lsp::semantic_tokens::{semantic_tokens, semantic_tokens_with_module};
use crate::lsp::signature_help::handle_signature_help;
use crate::lsp::state::ServerState;
use crate::lsp::symbols::document_symbols;
use crate::lsp::util::{FileUriPath, file_uri_path, uri_to_canonical, uri_to_path};
use crate::lsp::worker::{
    FocusedAnalysis, Scheduler, WorkPriority, WorkRequest, WorkerOutcome, WorkerResult,
};
use crate::package_collection::{
    ModuleFileParserContext, SourceOverlay, find_package_root, parse_module_file_with_file_context,
};

const HELP_TEMPLATE: &str = "\
Usage: kio lsp

Run the Kio language server.

The server speaks the Language Server Protocol over JSON-RPC on
stdio. Publishes diagnostics on file open, edit, and save events.
Exits 0 on clean shutdown (LSP `shutdown` then `exit`); analysis
errors flow through `publishDiagnostics`, never the process exit code.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on clean shutdown; 1
(internal error) when the server hits an unrecoverable error before or
during the handshake; 2 on CLI usage error.

See {base}/specs/cli.md#kio-lsp for the LSP subcommand's documented surface.";

/// Entry point for `kio lsp`. Parses flags, opens the stdio
/// transport, runs the initialize handshake, then drives the
/// notification dispatch loop until `shutdown` / `exit`.
pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    if !args.is_empty() {
        eprintln!("error: `kio lsp` does not accept arguments");
        return ExitCode::Usage;
    }

    match run_inner() {
        Ok(()) => ExitCode::Success,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::Internal
        }
    }
}

/// Inner driver. Split out from [`run`] so the error type is a
/// boxed `dyn Error` (lsp-server uses several distinct error
/// types we just want to surface uniformly) while the public entry
/// stays exit-code-shaped.
fn run_inner() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(server_capabilities())?;
    let init_params: InitializeParams = match connection.initialize(capabilities) {
        Ok(params) => serde_json::from_value(params)?,
        Err(e) => {
            // initialize_start_while can return Err here for a
            // shutdown-during-handshake — drop the connection
            // first so the writer thread can exit, then join.
            drop(connection);
            let _ = io_threads.join();
            return Err(Box::new(e));
        }
    };

    let workspace_root = resolve_workspace_root(&init_params);
    let mut state = ServerState::new();
    state.completion_snippets = init_params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|document| document.completion.as_ref())
        .and_then(|completion| completion.completion_item.as_ref())
        .and_then(|item| item.snippet_support)
        .unwrap_or(false);
    let (mut scheduler, result_rx) = Scheduler::spawn();

    main_loop(
        &connection,
        &mut state,
        &mut scheduler,
        &result_rx,
        workspace_root.as_deref(),
    )?;

    // Drain any pending analyses and join the background threads.
    scheduler.shutdown();
    // After shutdown returns, the worker has exited; one last drain
    // of result_rx covers any result the worker posted between its
    // last analysis and the stop sentinel.
    while let Ok(result) = result_rx.try_recv() {
        // Best-effort: publish any final result. Send errors are
        // ignored — the connection might already be tearing down.
        let _ = publish_worker_result(&connection, &mut state, result);
    }
    // Drop the connection before joining: the writer thread blocks
    // on `connection.sender`, and `io_threads.join()` would deadlock
    // if we still held a sender handle.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Server capabilities advertised in the `initialize` response.
///
/// Capabilities:
///
/// - `textDocumentSync.openClose = true` — track open / closed
///   documents so the overlay store knows what to track.
/// - `textDocumentSync.change = Incremental` — receive
///   `didChange` events with range-keyed edits. The overlay applies
///   each event in order before scheduling a debounced reanalysis.
/// - `textDocumentSync.save = { includeText: false }` — the overlay
///   is already authoritative when a save fires, so the editor
///   needn't ship the buffer in the save event.
/// - `hoverProvider = true` — respond to `textDocument/hover` with
///   declaration docs, builtin docs, or the synthesized type at the cursor.
/// - `signatureHelpProvider = { triggerCharacters: ["(", ","] }` —
///   respond to `textDocument/signatureHelp` with direct-path callee
///   signatures from the latest typed snapshot.
/// - `definitionProvider = true` — respond to `textDocument/definition`
///   with the declaration site of the binder at the cursor.
/// - `referencesProvider = true` — respond to `textDocument/references`
///   with all spans that resolve to the same binder.
/// - `documentSymbolProvider = true` — respond to
///   `textDocument/documentSymbol` with the file's flat declaration outline;
///   each recursive member has its own function entry.
/// - `foldingRangeProvider = true` — respond to
///   `textDocument/foldingRange` with one range per multi-line
///   `{ … }` brace group.
/// - `completionProvider = { triggerCharacters: [":", "[", "(", ","], resolveProvider: false }`
///   — respond to `textDocument/completion` with in-scope identifiers,
///   type-position names, and available Markdown docs.
/// - `documentFormattingProvider = true` — respond to
///   `textDocument/formatting` with a single full-file replacement
///   `TextEdit` (or an empty list when the text is already canonical
///   or a parse error prevents formatting).
/// - `semanticTokensProvider = { legend: …, full: true, range: false }`
///   — respond to `textDocument/semanticTokens/full` with a per-token
///   classification using the legend declared in
///   [`semantic_tokens::server_capabilities`].
/// - `renameProvider = { prepareProvider: true }` — respond to
///   `textDocument/prepareRename` (returns the identifier span at the
///   cursor) and `textDocument/rename` (returns a `WorkspaceEdit`
///   renaming every reference within the package). Invalid names or
///   conflict-prone renames return JSON-RPC errors so the editor can
///   surface the message.
/// - `inlayHintProvider = { resolveProvider: false }` — respond to
///   `textDocument/inlayHint` with inferred `let` binder types and
///   inferred type-argument hints recorded in the latest typed
///   snapshot.
/// - `codeActionProvider = { codeActionKinds: ["quickfix", "source.fixAll"], resolveProvider: true }`
///   — respond to `textDocument/codeAction` by turning compiler
///   fixes carried in diagnostic data into quick fixes and lazily
///   resolving workspace-derived quick fixes.
///
/// The "advertise only what's implemented" rule keeps the server
/// honest about its surface; editors infer "no X" from the
/// absence of a given cap, not from a stale-truth flag.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned(), "{".to_owned()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        document_highlight_provider: Some(lsp_types::OneOf::Left(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![
                ":".to_owned(),
                "[".to_owned(),
                "(".to_owned(),
                ",".to_owned(),
            ]),
            all_commit_characters: None,
            work_done_progress_options: Default::default(),
            completion_item: None,
        }),
        document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
        semantic_tokens_provider: Some(semantic_tokens::server_capabilities()),
        rename_provider: Some(lsp_types::OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        inlay_hint_provider: Some(lsp_types::OneOf::Right(
            InlayHintServerCapabilities::Options(InlayHintOptions {
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            }),
        )),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::SOURCE_FIX_ALL,
            ]),
            resolve_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        ..Default::default()
    }
}

/// Resolve the workspace root from the client's `initialize`
/// params. Kio chooses one root for package discovery. The fallback
/// order is:
///
/// 1. The first entry of `workspace_folders` (if non-empty).
/// 2. The deprecated `root_uri` (if set).
/// 3. The deprecated `root_path` (if set).
/// 4. None — we'll resolve packages relative to each saved file's
///    directory instead.
#[allow(deprecated)] // root_uri / root_path are kept for old clients that don't set workspace_folders
fn resolve_workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folders) = &params.workspace_folders
        && let Some(first) = folders.first()
    {
        return uri_to_path(&first.uri);
    }
    if let Some(uri) = &params.root_uri {
        return uri_to_path(uri);
    }
    if let Some(p) = &params.root_path {
        return Some(PathBuf::from(p));
    }
    None
}

/// Notification / request / worker-result dispatch loop. Runs until
/// the client sends `shutdown` followed by `exit`. Selects across
/// the LSP connection's receiver (incoming messages) and the
/// worker's result channel (completed analyses) so neither side
/// blocks the other.
fn main_loop(
    connection: &Connection,
    state: &mut ServerState,
    scheduler: &mut Scheduler,
    result_rx: &Receiver<WorkerResult>,
    workspace_root: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        select! {
            recv(connection.receiver) -> msg => {
                let msg = match msg {
                    Ok(m) => m,
                    // Connection closed unexpectedly — exit the loop.
                    Err(_) => return Ok(()),
                };
                match msg {
                    Message::Request(req) => {
                        if connection.handle_shutdown(&req)? {
                            return Ok(());
                        }
                        let resp = dispatch_request(req, state, scheduler, workspace_root);
                        connection.sender.send(Message::Response(resp))?;
                    }
                    Message::Notification(not) => {
                        handle_notification(state, scheduler, workspace_root, not)?;
                    }
                    // Server-side responses are for client-initiated
                    // requests we don't issue today.
                    Message::Response(_) => {}
                }
            }
            recv(result_rx) -> r => {
                let result = match r {
                    Ok(r) => r,
                    // Worker shut down; nothing more to publish.
                    Err(_) => continue,
                };
                publish_worker_result(connection, state, result)?;
            }
        }
    }
}

/// Dispatch a client request to the appropriate handler and return
/// the JSON-RPC response. Handles:
/// `textDocument/hover`, `textDocument/definition`,
/// `textDocument/references`, `textDocument/documentSymbol`,
/// `textDocument/foldingRange`, `textDocument/completion`,
/// `textDocument/formatting`, `textDocument/semanticTokens/full`,
/// `textDocument/prepareRename`, `textDocument/rename`, and
/// `textDocument/codeAction`, `textDocument/inlayHint`, and
/// `textDocument/signatureHelp`.
/// Unrecognized requests get a `MethodNotFound` error.
fn dispatch_request(
    req: lsp_server::Request,
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
) -> lsp_server::Response {
    let method = req.method.clone();
    let start = Instant::now();
    let response = dispatch_request_inner(req, state, scheduler, workspace_root);
    if lsp_timing_enabled() {
        eprintln!(
            "lsp-timing: request method={} error={} total_ms={:.3}",
            method,
            usize::from(response.error.is_some()),
            duration_ms(start.elapsed()),
        );
    }
    response
}

fn dispatch_request_inner(
    req: lsp_server::Request,
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
) -> lsp_server::Response {
    match req.method.as_str() {
        HoverRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<HoverParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid hover params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position_params.text_document.uri;
            let position = &params.text_document_position_params.position;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, true);
            let declaration_hover = doc_hover_with_syntax_context(uri, position, state);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text());
            let overlay = overlay_text.map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let result = declaration_hover.or_else(|| {
                uri_to_canonical(uri)
                    .and_then(|canonical| state.best_analysis_entry_for_file(&canonical, uri))
                    .filter(|entry| state.block_label_query_is_current(entry, uri, position))
                    .map(|entry| entry.analysis())
                    .and_then(|analysis| handle_hover(uri, position, analysis, overlay))
            });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        GotoDefinition::METHOD => {
            let (id, params) = match serde_json::from_value::<GotoDefinitionParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid definition params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position_params.text_document.uri;
            let position = &params.text_document_position_params.position;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, true);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text());
            let overlay = overlay_text.map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let pkg_root = workspace_root.map(Path::to_path_buf).unwrap_or_else(|| {
                uri_to_canonical(uri)
                    .and_then(|p| p.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            let result = uri_to_canonical(uri)
                .and_then(|canonical| state.best_analysis_entry_for_file(&canonical, uri))
                .filter(|entry| state.block_label_query_is_current(entry, uri, position))
                .map(|entry| entry.analysis())
                .and_then(|analysis| {
                    handle_definition(uri, position, analysis, overlay, &pkg_root)
                });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        ReferencesRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<ReferenceParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid references params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position.text_document.uri;
            let position = &params.text_document_position.position;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text());
            let overlay = overlay_text.map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let include_decl = params.context.include_declaration;
            let pkg_root = workspace_root.map(Path::to_path_buf).unwrap_or_else(|| {
                uri_to_canonical(uri)
                    .and_then(|p| p.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            let result = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_entry_for_file(&canonical, uri))
                .filter(|entry| state.block_label_query_is_current(entry, uri, position))
                .map(|entry| entry.analysis())
                .and_then(|analysis| {
                    handle_references(uri, position, include_decl, analysis, overlay, &pkg_root)
                });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        DocumentHighlightRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<DocumentHighlightParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid documentHighlight params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position_params.text_document.uri;
            let position = &params.text_document_position_params.position;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text());
            let overlay = overlay_text.map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let result = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_entry_for_file(&canonical, uri))
                .filter(|entry| state.block_label_query_is_current(entry, uri, position))
                .map(|entry| entry.analysis())
                .and_then(|analysis| handle_document_highlight(uri, position, analysis, overlay));
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        InlayHintRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<InlayHintParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid inlayHint params: {e}"),
                    );
                }
            };
            let uri = &params.text_document.uri;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let overlay_text = state.document(uri).map(|d| d.text());
            let result = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_for_file(&canonical, uri))
                .and_then(|analysis| {
                    handle_inlay_hints(uri, &params.range, analysis, overlay_text)
                });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        SignatureHelpRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<SignatureHelpParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid signatureHelp params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position_params.text_document.uri;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let syntax = request_syntax_module(state, uri);
            let analysis = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_for_file(&canonical, uri));
            let block_signature = completion::handle_block_signature_request(
                uri,
                &params.text_document_position_params.position,
                workspace_root,
                state,
            );
            let result = block_signature.or_else(|| match syntax {
                RequestSyntaxModule::Contextual { module, source }
                | RequestSyntaxModule::AuthenticatedLazy { module, source } => {
                    handle_signature_help(
                        &params.text_document_position_params,
                        &source,
                        &module,
                        analysis,
                    )
                }
                RequestSyntaxModule::NonModule { .. }
                | RequestSyntaxModule::ContextInvalid
                | RequestSyntaxModule::Unavailable => None,
            });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        DocumentSymbolRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<DocumentSymbolParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid documentSymbol params: {e}"),
                    );
                }
            };
            let uri = &params.text_document.uri;
            let result = handle_document_symbol(uri, state);
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        FoldingRangeRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<FoldingRangeParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid foldingRange params: {e}"),
                    );
                }
            };
            let uri = &params.text_document.uri;
            let result = handle_folding_range(uri, state);
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        CompletionRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<CompletionParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid completion params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position.text_document.uri;
            let position = &params.text_document_position.position;
            let analysis = uri_to_canonical(uri)
                .and_then(|path| state.completion_analysis_for_file(&path, uri));
            let stale = analysis.is_none();
            let result = completion::handle_completion_request(
                uri,
                position,
                workspace_root,
                state,
                analysis,
            );
            if stale
                && result
                    .as_ref()
                    .is_some_and(|result| result.needs_local_metadata)
            {
                schedule_analysis(
                    state,
                    scheduler,
                    workspace_root,
                    uri,
                    WorkPriority::Foreground,
                    true,
                );
            }
            let lsp_result: Option<CompletionResponse> =
                result.map(|result| CompletionResponse::List(result.list));
            let json = serde_json::to_value(lsp_result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        FormattingRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<DocumentFormattingParams>(req.params)
            {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid formatting params: {e}"),
                    );
                }
            };
            let uri = &params.text_document.uri;
            let result = handle_document_formatting(uri, state);
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        SemanticTokensFullRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<SemanticTokensParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid semanticTokens/full params: {e}"),
                    );
                }
            };
            let uri = &params.text_document.uri;
            let result = handle_semantic_tokens_full(uri, state);
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        PrepareRenameRequest::METHOD => {
            let (id, params) =
                match serde_json::from_value::<TextDocumentPositionParams>(req.params) {
                    Ok(p) => (req.id, p),
                    Err(e) => {
                        return lsp_server::Response::new_err(
                            req.id,
                            lsp_server::ErrorCode::InvalidParams as i32,
                            format!("invalid prepareRename params: {e}"),
                        );
                    }
                };
            let uri = &params.text_document.uri;
            let position = &params.position;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let current_module = parsed_module_with_syntax_context(state, uri);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text().to_owned());
            let overlay = overlay_text.as_deref().map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let result = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_entry_for_file(&canonical, uri))
                .filter(|entry| state.block_label_query_is_current(entry, uri, position))
                .and_then(|entry| {
                    handle_prepare_rename_with_module(
                        uri,
                        position,
                        entry.analysis(),
                        overlay,
                        current_module.as_ref(),
                    )
                });
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        RenameRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<RenameParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid rename params: {e}"),
                    );
                }
            };
            let uri = &params.text_document_position.text_document.uri;
            let position = &params.text_document_position.position;
            let new_name = &params.new_name;
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, uri, false);
            let current_module = parsed_module_with_syntax_context(state, uri);
            let overlay_label_reuse_index = overlay_label_reuse_index(state, uri);
            let overlay_text = state.document(uri).map(|d| d.text().to_owned());
            let overlay = overlay_text.as_deref().map(|source| LabelReuseSnapshot {
                source,
                index: overlay_label_reuse_index.as_deref(),
            });
            let pkg_root =
                rename_package_root(uri, workspace_root).unwrap_or_else(|| PathBuf::from("."));
            let entry = uri_to_canonical(uri)
                .and_then(|canonical| state.current_full_analysis_entry_for_file(&canonical, uri))
                .filter(|entry| state.block_label_query_is_current(entry, uri, position));
            match entry {
                None => {
                    // No analysis available — return null (rename not
                    // possible without a type-checked snapshot).
                    lsp_server::Response {
                        id,
                        result: Some(serde_json::Value::Null),
                        error: None,
                    }
                }
                Some(entry) => {
                    match handle_rename_versioned_with_context(
                        uri,
                        position,
                        new_name,
                        entry.analysis(),
                        overlay,
                        RenameParseContext {
                            package_root: &pkg_root,
                            current_module: current_module.as_ref(),
                        },
                        entry.snapshot().versions(),
                    ) {
                        Ok(result) => {
                            let json =
                                serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
                            lsp_server::Response {
                                id,
                                result: Some(json),
                                error: None,
                            }
                        }
                        Err(msg) => lsp_server::Response::new_err(
                            id,
                            lsp_server::ErrorCode::RequestFailed as i32,
                            msg,
                        ),
                    }
                }
            }
        }

        CodeActionRequest::METHOD => {
            let (id, params) = match serde_json::from_value::<CodeActionParams>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid codeAction params: {e}"),
                    );
                }
            };
            let uri = params.text_document.uri.clone();
            schedule_foreground_analysis_if_stale(state, scheduler, workspace_root, &uri, false);
            let syntax = request_syntax_module(state, &uri);
            let (source, module) = match syntax {
                RequestSyntaxModule::Contextual { module, source }
                | RequestSyntaxModule::AuthenticatedLazy { module, source } => {
                    (Some(source), Some(module))
                }
                RequestSyntaxModule::NonModule { source } => (Some(source), None),
                RequestSyntaxModule::ContextInvalid | RequestSyntaxModule::Unavailable => {
                    (None, None)
                }
            };
            let open_source = state
                .document(&uri)
                .map(|document| document.text().to_owned());
            let source = source.or(open_source);
            let document_version = state
                .document(&uri)
                .map(crate::lsp::state::OpenDocument::version);
            let focus_file = uri_to_canonical(&uri);
            let package_root = rename_package_root(&uri, workspace_root);
            let overlay = state.source_overlay();
            let fix_validation = package_root.as_deref().zip(focus_file.as_deref()).map(
                |(package_root, focus_file)| FixValidationContext {
                    package_root,
                    focus_file,
                    overlay: &overlay,
                },
            );
            // Auto-import consults only declarations in *other* modules; the
            // current module and edit ranges come from the authenticated live
            // parse above. A previous full snapshot is therefore a safe
            // candidate catalogue after this document becomes invalid: it
            // grants no current-file binder or navigation authority.
            let analysis = focus_file
                .as_deref()
                .and_then(|path| state.analysis_for_file(path));
            // Eager fixes are self-contained in Diagnostic.data and remain safe
            // even when a concrete file URI cannot authenticate a syntax
            // context. Contextual auto-import and add-stub actions receive no
            // source/module here and are therefore suppressed.
            let result = handle_code_action(
                params,
                source.as_deref(),
                module.as_ref(),
                analysis,
                document_version,
                fix_validation,
            );
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        CodeActionResolveRequest::METHOD => {
            let (id, action) = match serde_json::from_value::<CodeAction>(req.params) {
                Ok(p) => (req.id, p),
                Err(e) => {
                    return lsp_server::Response::new_err(
                        req.id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid codeAction/resolve params: {e}"),
                    );
                }
            };
            let result = handle_code_action_resolve(action);
            let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
            lsp_server::Response {
                id,
                result: Some(json),
                error: None,
            }
        }

        _ => lsp_server::Response::new_err(
            req.id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            format!("kio lsp does not implement request: {}", req.method),
        ),
    }
}

fn lsp_timing_enabled() -> bool {
    crate::timing::lsp_enabled()
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

// Rename is a typed whole-package operation, so this is package membership,
// not the parser source-root calculation used by syntax-only requests.
fn rename_package_root(uri: &Uri, workspace_root: Option<&Path>) -> Option<PathBuf> {
    uri_to_canonical(uri)
        .as_deref()
        .and_then(find_package_root)
        .or_else(|| workspace_root.map(Path::to_path_buf))
}

enum RequestSyntaxModule {
    Contextual {
        module: crate::ast::Module<crate::ast::Surface>,
        source: String,
    },
    AuthenticatedLazy {
        module: crate::ast::Module<crate::ast::Surface>,
        source: String,
    },
    NonModule {
        source: String,
    },
    ContextInvalid,
    Unavailable,
}

enum SyntaxContextResult<T> {
    Available(Option<T>),
    Unavailable,
}

impl<T> SyntaxContextResult<T> {
    fn or_else(self, fallback: impl FnOnce() -> Option<T>) -> Option<T> {
        match self {
            Self::Available(Some(value)) => Some(value),
            Self::Available(None) => fallback(),
            Self::Unavailable => None,
        }
    }
}

/// Resolve syntax for requests that retain a lazy fallback when a body is
/// incomplete. A real file authenticates its path/header context
/// before that fallback is admitted; closed documents run the same direct
/// file-context parse instead of silently reverting to a context-free parse.
fn request_syntax_module(state: &mut ServerState, uri: &Uri) -> RequestSyntaxModule {
    if let Some((module, source)) = with_overlay_parsed_module(state, uri, |module, source| {
        (module.clone(), source.to_owned())
    }) {
        return RequestSyntaxModule::Contextual { module, source };
    }

    if let Some(source) = state
        .document(uri)
        .map(|document| document.text().to_owned())
    {
        match file_uri_path(uri) {
            FileUriPath::Path(source_path) => {
                if source_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !crate::file_kind::is_module_file(name))
                {
                    return RequestSyntaxModule::NonModule { source };
                }
                let Some(module) = state.with_overlay_lazy_module(uri, |module, _| module.clone())
                else {
                    return RequestSyntaxModule::Unavailable;
                };
                if ModuleFileParserContext::from_module(&source_path, &module).is_err() {
                    return RequestSyntaxModule::ContextInvalid;
                }
                return RequestSyntaxModule::AuthenticatedLazy { module, source };
            }
            FileUriPath::NonFile => {
                let Some(module) = state.with_overlay_lazy_module(uri, |module, _| module.clone())
                else {
                    return RequestSyntaxModule::Unavailable;
                };
                return RequestSyntaxModule::AuthenticatedLazy { module, source };
            }
            FileUriPath::InvalidFile => return RequestSyntaxModule::ContextInvalid,
        }
    }

    let Some(source) = resolve_source(uri, state) else {
        return RequestSyntaxModule::Unavailable;
    };
    request_syntax_module_direct(uri, source)
}

fn request_syntax_module_direct(uri: &Uri, source: String) -> RequestSyntaxModule {
    match file_uri_path(uri) {
        FileUriPath::Path(source_path) => {
            if source_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !crate::file_kind::is_module_file(name))
            {
                return RequestSyntaxModule::NonModule { source };
            }
            let Ok(header) = crate::pass::parser::parse_module_file_lazy(&source) else {
                return RequestSyntaxModule::Unavailable;
            };
            let Ok(_) = ModuleFileParserContext::from_module(&source_path, &header.module) else {
                return RequestSyntaxModule::ContextInvalid;
            };
            let lazy_module = header.module.clone();
            match header.force_all() {
                Ok(file) => RequestSyntaxModule::Contextual {
                    module: file.module,
                    source,
                },
                Err(_) => RequestSyntaxModule::AuthenticatedLazy {
                    module: lazy_module,
                    source,
                },
            }
        }
        FileUriPath::NonFile => {
            let Ok(header) = crate::pass::parser::parse_module_file_lazy(&source) else {
                return RequestSyntaxModule::Unavailable;
            };
            let lazy_module = header.module.clone();
            match header.force_all() {
                Ok(file) => RequestSyntaxModule::Contextual {
                    module: file.module,
                    source,
                },
                Err(_) => RequestSyntaxModule::AuthenticatedLazy {
                    module: lazy_module,
                    source,
                },
            }
        }
        FileUriPath::InvalidFile => RequestSyntaxModule::ContextInvalid,
    }
}

fn with_overlay_parsed_module<R>(
    state: &mut ServerState,
    uri: &Uri,
    f: impl FnOnce(&crate::ast::Module<crate::ast::Surface>, &str) -> R,
) -> Option<R> {
    let source_path = match file_uri_path(uri) {
        FileUriPath::Path(path) => path,
        FileUriPath::NonFile => {
            return state.with_overlay_parsed_module(uri, f);
        }
        FileUriPath::InvalidFile => return None,
    };
    state.with_overlay_parsed_module_for_file(uri, &source_path, f)
}

fn parsed_module_with_syntax_context(
    state: &mut ServerState,
    uri: &Uri,
) -> Option<crate::ast::Module<crate::ast::Surface>> {
    if state.document(uri).is_some() {
        return with_overlay_parsed_module(state, uri, |module, _| module.clone());
    }

    let source = resolve_source(uri, state)?;
    match file_uri_path(uri) {
        FileUriPath::Path(source_path) => {
            parse_module_file_with_file_context(&source, &source_path)
                .ok()
                .map(|file| file.module)
        }
        FileUriPath::NonFile => crate::pass::parser::parse_module_file(&source)
            .ok()
            .map(|file| file.module),
        FileUriPath::InvalidFile => None,
    }
}

fn doc_hover_with_syntax_context(
    uri: &Uri,
    position: &lsp_types::Position,
    state: &mut ServerState,
) -> SyntaxContextResult<lsp_types::Hover> {
    let render = |module: &crate::ast::Module<crate::ast::Surface>, source: &str| {
        let line_index = LineIndex::new(source);
        let byte_offset = line_index.position_to_offset(crate::lsp::positions::LspPosition {
            line: position.line,
            character: position.character,
        });
        doc_hover_at_with_module(source, module, byte_offset, &line_index)
    };

    match request_syntax_module(state, uri) {
        RequestSyntaxModule::Contextual { module, source } => {
            SyntaxContextResult::Available(render(&module, &source))
        }
        RequestSyntaxModule::NonModule { .. } => SyntaxContextResult::Available(None),
        RequestSyntaxModule::AuthenticatedLazy { .. }
        | RequestSyntaxModule::ContextInvalid
        | RequestSyntaxModule::Unavailable => SyntaxContextResult::Unavailable,
    }
}

fn overlay_label_reuse_index(
    state: &mut ServerState,
    uri: &Uri,
) -> Option<std::sync::Arc<crate::lsp::label_reuse::LabelReuseIndex>> {
    let source_path = match file_uri_path(uri) {
        FileUriPath::Path(path) => path,
        FileUriPath::NonFile => return state.overlay_label_reuse_index(uri),
        FileUriPath::InvalidFile => return None,
    };
    state.overlay_label_reuse_index_for_file(uri, &source_path)
}

/// Handle one `textDocument/documentSymbol` request.
///
/// Obtains the source text from the overlay (if the document is
/// open) or from the latest analysis snapshot, parses it on demand,
/// and walks the resulting `Module<Surface>` to build the symbol
/// list. Returns `None` (→ JSON `null`) when the source text is not
/// available (URI absent from both overlay and analysis state).
fn handle_document_symbol(uri: &Uri, state: &mut ServerState) -> Option<DocumentSymbolResponse> {
    if let Some(symbols) = state.with_overlay_lazy_module(uri, |module, source| {
        let line_index = LineIndex::new(source);
        document_symbols(module, source, &line_index)
    }) {
        return Some(DocumentSymbolResponse::Nested(symbols));
    }
    let source = resolve_source(uri, state)?;
    let lazy = crate::pass::parser::parse_lazy(&source).ok()?;
    let line_index = LineIndex::new(&source);
    let symbols = document_symbols(lazy.module(), &source, &line_index);
    Some(DocumentSymbolResponse::Nested(symbols))
}

/// Handle one `textDocument/foldingRange` request.
///
/// Obtains the source text from the overlay (if the document is
/// open) or from the latest analysis snapshot, then walks the CST
/// tree-skeleton to emit brace-group folding ranges. Returns `None`
/// (→ JSON `null`) when the source is not available.
fn handle_folding_range(uri: &Uri, state: &ServerState) -> Option<Vec<FoldingRange>> {
    let source = resolve_source(uri, state)?;
    let line_index = LineIndex::new(&source);
    Some(folding_ranges(&source, &line_index))
}

/// Handle one `textDocument/formatting` request.
///
/// Reads the overlay text (the authoritative editor buffer) and runs
/// it through the formatter. Returns a list of zero or one
/// [`TextEdit`]s:
///
/// - **Empty list** — the text is already canonical, or a parse error
///   prevents formatting (silently no-op'd; the diagnostics path
///   already surfaces parse errors).
/// - **One full-file replacement** — replaces the entire buffer with
///   the canonical form.
///
/// Returns `None` (→ JSON `null`) when the source text is not
/// available (the document has never been opened).
fn handle_document_formatting(uri: &Uri, state: &ServerState) -> Option<Vec<TextEdit>> {
    let source = resolve_source(uri, state)?;
    let formatted = match file_uri_path(uri) {
        FileUriPath::Path(source_path) => {
            handle_formatting_with_file_context(&source_path, &source)
        }
        FileUriPath::NonFile => try_handle_formatting(uri, &source),
        FileUriPath::InvalidFile => return None,
    };
    Some(formatted.unwrap_or_default())
}

/// Handle one `textDocument/semanticTokens/full` request.
///
/// Obtains the source text from the overlay (if the document is open)
/// or from the latest analysis snapshot, parses its declared operator grammar,
/// and classifies every token with the resulting surface AST.
/// Syntactically incomplete buffers retain the lexical fallback. Returns a
/// [`SemanticTokensResult`].
///
/// Returns `None` (→ JSON `null`) when the source text is not
/// available (the document has never been opened or analyzed).
/// Returns `None` if lexing fails (extremely rare — the lexer is
/// total over valid UTF-8 Kio source).
fn handle_semantic_tokens_full(uri: &Uri, state: &mut ServerState) -> Option<SemanticTokensResult> {
    let tokens = match request_syntax_module(state, uri) {
        RequestSyntaxModule::Contextual { module, source } => {
            semantic_tokens_with_module(&source, &module).or_else(|| semantic_tokens(&source))
        }
        RequestSyntaxModule::AuthenticatedLazy { source, .. }
        | RequestSyntaxModule::NonModule { source } => semantic_tokens(&source),
        RequestSyntaxModule::Unavailable => resolve_source(uri, state)
            .as_deref()
            .and_then(semantic_tokens),
        RequestSyntaxModule::ContextInvalid => None,
    }?;
    Some(SemanticTokensResult::Tokens(tokens))
}

/// Obtain the source text for `uri`. Prefers the overlay (editor
/// buffer, authoritative for open documents) over the analysis
/// snapshot's source map. Returns `None` when the text is not
/// available in either location.
fn resolve_source(uri: &Uri, state: &ServerState) -> Option<String> {
    // Check the overlay first — clone the text so the borrow ends.
    if let Some(doc) = state.document(uri) {
        return Some(doc.text().to_owned());
    }
    // Fall back to the analysis snapshot's source map.
    let canonical = uri_to_canonical(uri)?;
    let analysis = state.analysis_for_file(&canonical)?;
    analysis
        .sources
        .get(&canonical)
        .or_else(|| analysis.sources.get(canonical.as_path()))
        .cloned()
}

/// Single notification dispatcher. Manages the overlay store and
/// schedules debounced reanalysis for events that change the
/// overlay's view of the world.
fn handle_notification(
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
    not: Notification,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = extract(not)?;
            state.open(
                params.text_document.uri.clone(),
                params.text_document.text.clone(),
                params.text_document.version,
            );
            schedule_reanalysis(state, scheduler, workspace_root, &params.text_document.uri);
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = extract(not)?;
            let uri = params.text_document.uri.clone();
            let version = params.text_document.version;
            // Apply each change to the overlay in order. A malformed
            // event range (out-of-bounds or non-char-boundary) gets
            // logged and dropped; well-behaved editors don't send
            // them, and the next correctly-targeted edit will fix the
            // overlay state.
            if let Some(doc) = state.document_mut(&uri) {
                for change in &params.content_changes {
                    if let Err(e) = doc.apply_change(change, version) {
                        eprintln!("warning: didChange for {} ignored: {e}", uri.as_str());
                    }
                }
            } else {
                // didChange without a preceding didOpen — out-of-spec
                // editor; ignore.
                eprintln!(
                    "warning: didChange for {} arrived before didOpen; ignoring",
                    uri.as_str(),
                );
                return Ok(());
            }
            schedule_reanalysis(state, scheduler, workspace_root, &uri);
        }
        DidSaveTextDocument::METHOD => {
            let params: DidSaveTextDocumentParams = extract(not)?;
            // The overlay is already authoritative; the save event is
            // informational. Schedule a reanalysis anyway so editors
            // that didn't send a didChange between the last
            // reanalysis and the save still see fresh diagnostics.
            schedule_reanalysis(state, scheduler, workspace_root, &params.text_document.uri);
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = extract(not)?;
            state.close(&params.text_document.uri);
            schedule_reanalysis(state, scheduler, workspace_root, &params.text_document.uri);
            // LSP convention: closed files keep their last
            // published diagnostics until the editor clears them
            // itself. We don't publish an empty set here.
        }
        // The `initialized` notification arrives after our
        // `initialize` response; `lsp-server` already consumed it
        // during the handshake. Other notifications (e.g.
        // `$/cancelRequest`) are noise today.
        _ => {}
    }
    Ok(())
}

/// Extract a notification's params into the strongly-typed shape.
/// Wraps `Notification::extract` with the boxed error our dispatch
/// returns.
fn extract<N>(not: Notification) -> Result<N, Box<dyn std::error::Error + Send + Sync>>
where
    N: serde::de::DeserializeOwned,
{
    serde_json::from_value(not.params).map_err(Into::into)
}

/// Resolve the triggering URI to a typed-analysis package root, build a fresh
/// overlay snapshot, and post it to the scheduler. Files outside any package
/// fall back to the workspace root (if the client gave us one), then the
/// file's parent directory. This analysis-membership choice is independent of
/// the file-context root used by syntax-only requests.
fn schedule_reanalysis(
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
    triggering_uri: &Uri,
) {
    schedule_analysis(
        state,
        scheduler,
        workspace_root,
        triggering_uri,
        WorkPriority::Background,
        false,
    );
}

fn schedule_foreground_analysis_if_stale(
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
    triggering_uri: &Uri,
    focus_current_file: bool,
) {
    if typed_analysis_is_fresh(state, triggering_uri, focus_current_file) {
        return;
    }
    schedule_analysis(
        state,
        scheduler,
        workspace_root,
        triggering_uri,
        WorkPriority::Foreground,
        focus_current_file,
    );
}

fn typed_analysis_is_fresh(state: &ServerState, uri: &Uri, allow_focused: bool) -> bool {
    let Some(canonical) = uri_to_canonical(uri) else {
        return false;
    };
    let entry = if allow_focused {
        state.best_analysis_entry_for_file(&canonical, uri)
    } else {
        state.current_full_analysis_entry_for_file(&canonical, uri)
    };
    entry.is_some()
}

fn schedule_analysis(
    state: &mut ServerState,
    scheduler: &Scheduler,
    workspace_root: Option<&Path>,
    triggering_uri: &Uri,
    priority: WorkPriority,
    focus_current_file: bool,
) {
    let triggering_path = match uri_to_path(triggering_uri) {
        Some(p) => p,
        // Non-file URIs (e.g. `untitled:`) — don't analyze.
        None => return,
    };
    let package_root = match find_package_root(&triggering_path) {
        Some(r) => r,
        None => workspace_root.map(Path::to_path_buf).unwrap_or_else(|| {
            triggering_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        }),
    };
    let package_root = crate::package_collection::canonicalize_with_missing_suffix(&package_root)
        .unwrap_or(package_root);

    // Build the overlay snapshot: for every open URI whose file path
    // exists, insert its text into the SourceOverlay keyed by the
    // canonicalized path the walker uses internally.
    let mut overlay = SourceOverlay::empty();
    let mut overlay_versions: BTreeMap<Uri, i32> = BTreeMap::new();
    let mut overlay_paths = BTreeMap::new();
    for (open_uri, text, version) in state.overlay_snapshot() {
        if let Some(path) = uri_to_path(&open_uri) {
            // Canonicalize so the key matches the walker's
            // canonical-path scheme. If canonicalize fails (the file
            // doesn't exist on disk yet — a freshly-typed `untitled`
            // becoming a `file://`), skip; the walker won't see this
            // file anyway because it's not in the file tree.
            if let Ok(canonical) = std::fs::canonicalize(&path) {
                overlay_paths.insert(open_uri.clone(), canonical.clone());
                overlay.insert(canonical, text);
                overlay_versions.insert(open_uri, version);
            }
        }
    }

    let snapshot_id = scheduler.next_snapshot_id();
    let focus = if priority == WorkPriority::Foreground
        && focus_current_file
        && state.document(triggering_uri).is_some()
    {
        std::fs::canonicalize(&triggering_path)
            .ok()
            .map(|file_path| FocusedAnalysis {
                uri: triggering_uri.clone(),
                file_path,
            })
    } else {
        None
    };
    if let Some(focus) = &focus {
        state.mark_focused_scheduled(&focus.uri, &focus.file_path, snapshot_id);
    } else {
        state.mark_scheduled(&package_root, snapshot_id);
    }
    let req = WorkRequest {
        package_root,
        overlay,
        snapshot: crate::lsp::snapshot::Snapshot::new(snapshot_id, overlay_versions)
            .with_source_paths(overlay_paths),
        priority,
        focus,
        cancel_token: CancellationToken::new(),
    };
    if lsp_timing_enabled() {
        eprintln!(
            "lsp-timing: schedule snapshot={} priority={} scope={} package={} overlays={}",
            req.snapshot.id().raw(),
            req.priority.as_str(),
            if req.focus.is_some() {
                "focused"
            } else {
                "full"
            },
            req.package_root.display(),
            req.snapshot.versions().len(),
        );
    }
    match priority {
        WorkPriority::Background => scheduler.schedule(req),
        WorkPriority::Foreground if req.focus.is_some() => {
            scheduler.schedule_immediate_preserving_debounce(req);
        }
        WorkPriority::Foreground => scheduler.schedule_immediate(req),
    }
}

/// Publish the diagnostics from one completed worker result.
fn publish_worker_result(
    connection: &Connection,
    state: &mut ServerState,
    result: WorkerResult,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let start = Instant::now();
    let WorkerResult {
        snapshot,
        overlay_versions,
        package_root,
        outcome,
    } = result;

    let obsolete = match &outcome {
        WorkerOutcome::Full(_) => state.worker_result_is_obsolete(&package_root, snapshot.id()),
        WorkerOutcome::Focused { uri, file_path, .. } => {
            state.focused_worker_result_is_obsolete(file_path, uri, &snapshot)
        }
    };
    if obsolete {
        if lsp_timing_enabled() {
            eprintln!(
                "lsp-timing: publish snapshot={} package={} obsolete=1 total_ms={:.3}",
                snapshot.id().raw(),
                package_root.display(),
                duration_ms(start.elapsed()),
            );
        }
        return Ok(());
    }

    // Group new diagnostics by URI. `publishDiagnostics` replaces a
    // URI's full diagnostic set, so all same-pass diagnostics for one
    // file must be published in one notification.
    let mut new_diags: BTreeMap<Uri, Vec<Diagnostic>> = BTreeMap::new();
    let mut related_invariant_count = 0usize;
    let clear_stale_diagnostics = match outcome {
        WorkerOutcome::Full(Ok(lsp_analysis)) => {
            // Typecheck clean: store the position index for upcoming
            // hover / goto-definition / references requests.
            collect_worker_warnings(&package_root, &lsp_analysis, &mut new_diags);
            state.store_analysis(&package_root, snapshot.clone(), lsp_analysis);
            true
        }
        WorkerOutcome::Full(Err(AnalysisFailure { errors, sources })) => {
            related_invariant_count =
                collect_worker_diagnostics(&package_root, errors, *sources, &mut new_diags);
            true
        }
        WorkerOutcome::Focused {
            uri,
            file_path,
            outcome: Ok(lsp_analysis),
        } => {
            state.store_focused_analysis(&uri, &file_path, snapshot.clone(), lsp_analysis);
            false
        }
        WorkerOutcome::Focused {
            outcome: Err(_), ..
        } => false,
    };

    // A client echoes a diagnostic back in a code-action request. Keep the
    // exact analyzed document version inside that diagnostic as well as on
    // publishDiagnostics, so a request made after another edit cannot apply
    // stale compiler-owned ranges to the newer buffer.
    for (uri, diagnostics) in &mut new_diags {
        let Some(version) = overlay_versions.get(uri).copied() else {
            continue;
        };
        for diagnostic in diagnostics {
            let data = diagnostic
                .data
                .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            let Some(data) = data.as_object_mut() else {
                unreachable!("Kio diagnostics use an object-valued data payload");
            };
            data.insert(
                "kioDocumentVersion".to_owned(),
                serde_json::Value::from(version),
            );
        }
    }

    // Clear stale diagnostics: every URI that previously had a
    // diagnostic but doesn't now publishes an empty set.
    let mut cleared_count = 0usize;
    if clear_stale_diagnostics {
        let previously_published = state.drain_published();
        for uri in &previously_published {
            if !new_diags.contains_key(uri) {
                // A clear produced by analysis of an open overlay belongs to
                // that exact document version just like a non-empty publish;
                // otherwise an older clean result can erase diagnostics from
                // a newer edit in clients that honor versioned diagnostics.
                let version = overlay_versions.get(uri).copied();
                publish(connection, uri.clone(), Vec::new(), version)?;
                cleared_count += 1;
            }
        }
    }

    // Publish the new diagnostics. The version stamp comes from the
    // overlay-versions map the worker forwarded — if the offending
    // URI had an overlay open, that overlay's version goes onto the
    // notification.
    let diagnostic_count: usize = new_diags.values().map(Vec::len).sum();
    let publish_uri_count = new_diags.len();
    for (uri, diags) in new_diags {
        let version = overlay_versions.get(&uri).copied();
        publish(connection, uri.clone(), diags, version)?;
        state.mark_published(uri);
    }

    if lsp_timing_enabled() {
        eprintln!(
            "lsp-timing: publish snapshot={} package={} obsolete=0 uris={} diagnostics={} cleared={} related_invariants={} total_ms={:.3}",
            snapshot.id().raw(),
            package_root.display(),
            publish_uri_count,
            diagnostic_count,
            cleared_count,
            related_invariant_count,
            duration_ms(start.elapsed()),
        );
    }

    Ok(())
}

fn collect_worker_diagnostics(
    package_root: &Path,
    errors: Vec<crate::pass::resolve::LocatedError>,
    sources: HashMap<PathBuf, String>,
    new_diags: &mut BTreeMap<Uri, Vec<Diagnostic>>,
) -> usize {
    let mut related_invariant_count = 0;
    for error in errors {
        let err_path = &error.file_path;
        // The `LocatedError.file_path` may be workspace-relative
        // (the pipeline's diagnostic-rendering shape) or absolute,
        // depending on which phase produced the error. The source
        // map is keyed the same way; we look up the source and
        // resolve the URI against the package root.
        if let Some(source) = sources.get(err_path) {
            let line_index = LineIndex::new(source);
            if let Some(uri) = path_to_uri(err_path, package_root) {
                let (diag, omitted) = locate_to_diagnostic_with_sources_counted(
                    &error,
                    &line_index,
                    &uri,
                    package_root,
                    &sources,
                );
                related_invariant_count += omitted;
                new_diags.entry(uri).or_default().push(diag);
            }
        }
    }
    related_invariant_count
}

fn collect_worker_warnings(
    package_root: &Path,
    analysis: &LspAnalysis,
    new_diags: &mut BTreeMap<Uri, Vec<Diagnostic>>,
) {
    for warning in &analysis.warnings {
        let source = match analysis.sources.get(&warning.file_path) {
            Some(source) => source,
            None => continue,
        };
        let Some(uri) = path_to_uri(&warning.file_path, package_root) else {
            continue;
        };
        let line_index = LineIndex::new(source);
        new_diags
            .entry(uri)
            .or_default()
            .push(warning_to_diagnostic(warning, &line_index));
    }
}

/// Send a `textDocument/publishDiagnostics` notification.
fn publish(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    let notification = Notification {
        method: PublishDiagnostics::METHOD.to_owned(),
        params: serde_json::to_value(params)?,
    };
    connection
        .sender
        .send(Message::Notification(notification))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn workspace_path(relative: &str) -> PathBuf {
        crate::lsp::util::test_file_path("/workspace").join(relative)
    }

    fn workspace_uri(relative: &str) -> Uri {
        crate::lsp::diagnostics::path_to_uri(&workspace_path(relative), &workspace_path(""))
            .expect("workspace file URI")
    }

    fn analysis_for_file(file_path: &Path, module_path: &str, source: &str) -> LspAnalysis {
        let sources = HashMap::from([(file_path.to_path_buf(), source.to_owned())]);
        LspAnalysis {
            position_index: crate::pass::typecheck_full::PositionIndex::new(),
            file_to_module: BTreeMap::from([(file_path.to_path_buf(), module_path.to_owned())]),
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn completion_context_keeps_current_provider_for_incomplete_consumer() {
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        let root = workspace_path("");
        let consumer_path = workspace_path("app.kio");
        let consumer_uri = workspace_uri("app.kio");
        let provider_path = workspace_path("provider.kio");
        let provider_uri = workspace_uri("provider.kio");
        let consumer = "module app; import provider(expand); fn run() { expand!() }";
        let changed = "module app; import provider(expand); fn run() { ex";
        let provider =
            "module provider; /// Current expansion.\npub elab expand : . -> . { impl build }";
        let mut analysis = analysis_for_file(&consumer_path, "app", consumer);
        analysis
            .sources
            .insert(provider_path.clone(), provider.into());
        analysis
            .file_to_module
            .insert(provider_path.clone(), "provider".into());
        let mut state = ServerState::new();
        state.open(consumer_uri.clone(), consumer.into(), 1);
        state.open(provider_uri.clone(), provider.into(), 1);
        state.store_analysis(
            &root,
            Snapshot::new(
                SnapshotIdGen::new().next(),
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 1)]),
            )
            .with_source_paths(BTreeMap::from([
                (consumer_uri.clone(), consumer_path),
                (provider_uri, provider_path),
            ])),
            analysis,
        );
        state.open(consumer_uri.clone(), changed.into(), 2);
        let (mut scheduler, _) = Scheduler::spawn();
        let response = dispatch_request(
            lsp_server::Request::new(
                1.into(),
                CompletionRequest::METHOD.to_owned(),
                serde_json::json!({
                    "textDocument": {"uri": consumer_uri},
                    "position": {"line": 0, "character": changed.len()},
                }),
            ),
            &mut state,
            &scheduler,
            Some(&root),
        );
        scheduler.shutdown();
        let result: lsp_types::CompletionList =
            serde_json::from_value(response.result.unwrap()).unwrap();
        let expansion = result
            .items
            .iter()
            .find(|item| item.label == "expand!")
            .unwrap_or_else(|| panic!("current selected provider lost: {:?}", result.items));
        assert_eq!(expansion.detail.as_deref(), Some(". -> ."));
        assert!(format!("{:?}", expansion.documentation).contains("Current expansion."));
    }

    #[test]
    fn completion_metadata_refreshes_dependencies_through_scheduler() {
        completion_metadata_scheduler_control(false);
    }

    #[test]
    fn completion_metadata_ignores_unrelated_package_edits_and_close() {
        completion_metadata_scheduler_control(true);
    }

    fn completion_metadata_scheduler_control(unrelated: bool) {
        use std::time::Duration;
        let directory = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(directory.path()).unwrap();
        let root = base.join("app");
        let other_root = base.join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other_root).unwrap();
        let consumer_path = root.join("main.kio");
        let provider_path = root.join("provider.kio");
        let other_path = other_root.join("main.kio");
        let consumer = "module main; import provider(produce); fn f() -> . { let value = produce(); let _ = value; () }";
        let provider = "module provider; pub fn produce() -> . { () }";
        let next_provider = "module provider; pub fn produce() -> . -> . { .(x: .) -> . { x } }";
        let other = "module main; fn unrelated() -> . { () }";
        for (path, source) in [
            (
                root.join("completion.pkg.kio"),
                "package completion; bridge { main; }",
            ),
            (consumer_path.clone(), consumer),
            (provider_path.clone(), provider),
            (
                other_root.join("other.pkg.kio"),
                "package other; bridge { main; }",
            ),
            (other_path.clone(), other),
        ] {
            std::fs::write(path, source).unwrap();
        }
        let consumer_uri = diagnostics::path_to_uri(&consumer_path, &root).unwrap();
        let provider_uri = diagnostics::path_to_uri(&provider_path, &root).unwrap();
        let other_uri = diagnostics::path_to_uri(&other_path, &other_root).unwrap();
        let mut state = ServerState::new();
        state.open(consumer_uri.clone(), consumer.into(), 1);
        state.open(provider_uri.clone(), provider.into(), 1);
        state.open(other_uri.clone(), other.into(), 1);
        let (mut scheduler, results) = Scheduler::spawn_with_debounce(Duration::from_millis(1));
        schedule_analysis(
            &mut state,
            &scheduler,
            Some(&root),
            &consumer_uri,
            WorkPriority::Foreground,
            false,
        );
        let initial = results
            .recv_timeout(Duration::from_secs(10))
            .expect("initial scheduled analysis");
        let (server, _client) = Connection::memory();
        publish_worker_result(&server, &mut state, initial).unwrap();
        let request = |state: &mut ServerState| {
            let pos = positions::LineIndex::new(consumer)
                .to_position((consumer.rfind("()").unwrap()) as u32);
            let response = dispatch_request(
                lsp_server::Request::new(
                    1.into(),
                    CompletionRequest::METHOD.into(),
                    serde_json::json!({"textDocument": {"uri": consumer_uri},
                    "position": {"line": pos.line, "character": pos.character}}),
                ),
                state,
                &scheduler,
                Some(&root),
            );
            let list: lsp_types::CompletionList =
                serde_json::from_value(response.result.unwrap()).unwrap();
            list.items
                .into_iter()
                .find(|item| item.label == "value")
                .expect("eligible local binding")
                .detail
        };
        assert_eq!(request(&mut state).as_deref(), Some("."));
        if unrelated {
            state.open(other_uri.clone(), format!("{other} // unrelated edit"), 2);
            assert_eq!(
                request(&mut state).as_deref(),
                Some("."),
                "unrelated edit is not a dependency change"
            );
            state.close(&other_uri);
            assert_eq!(
                request(&mut state).as_deref(),
                Some("."),
                "unrelated close is not a dependency change"
            );
            assert!(
                results.try_recv().is_err(),
                "current metadata needs no replacement worker"
            );
        } else {
            state.open(provider_uri.clone(), next_provider.into(), 2);
            assert!(request(&mut state).is_none());
            let refreshed = results
                .recv_timeout(Duration::from_secs(2))
                .expect("completion schedules dependency-stale foreground analysis");
            assert!(matches!(
                &refreshed.outcome,
                WorkerOutcome::Focused { outcome: Ok(_), .. }
            ));
            publish_worker_result(&server, &mut state, refreshed).unwrap();
            assert_eq!(request(&mut state).as_deref(), Some(". -> ."));
            state.close(&provider_uri);
            assert!(request(&mut state).is_none());
            let disk = results
                .recv_timeout(Duration::from_secs(2))
                .expect("completion schedules closed provider's disk analysis");
            publish_worker_result(&server, &mut state, disk).unwrap();
            assert_eq!(request(&mut state).as_deref(), Some("."));
        }
        scheduler.shutdown();
    }

    #[test]
    fn completion_metadata_tracks_dependency_versions_and_provider_identity() {
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::package_collection::SourceOverlay;
        let root = workspace_path("");
        let consumer_path = workspace_path("app/main.kio");
        let consumer_uri = workspace_uri("app/main.kio");
        let provider_path = workspace_path("provider.kio");
        let provider_uri = workspace_uri("provider.kio");
        let consumer = "module app/main; import provider(produce, expand, answer); fn f() -> . { let value = produce(); let _ = value; () }";
        let provider = |item: &str, body: &str, doc: &str| {
            format!(
                "module provider; import __comptime__; pub fn produce() -> {item} {{ {body} }}
             pure fn expand_body(ct: __Comptime__) -> __Checked_term__ {{ __term_unit__(ct) }}
             /// {doc}
             pub elab expand : . -> . {{ impl expand_body; }};
             pub literal answer = 1;"
            )
        };
        let analyze = |provider: &str| {
            let overlay = SourceOverlay::complete(
                root.clone(),
                BTreeMap::from([
                    (
                        root.join("completion.pkg.kio"),
                        "package completion; bridge { app/main; }".into(),
                    ),
                    (consumer_path.clone(), consumer.into()),
                    (provider_path.clone(), provider.into()),
                ]),
            );
            crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &overlay)
                .expect("typed dependency snapshot")
        };
        let mut state = ServerState::new();
        state.open(consumer_uri.clone(), consumer.into(), 1);
        let (mut scheduler, _results) = Scheduler::spawn();
        let ids = SnapshotIdGen::new();
        let request = |state: &mut ServerState| {
            let position = crate::lsp::positions::LineIndex::new(consumer)
                .to_position(consumer.rfind("()").unwrap() as u32);
            let response = dispatch_request(
                lsp_server::Request::new(
                    1.into(),
                    CompletionRequest::METHOD.to_owned(),
                    serde_json::json!({
                        "textDocument": {"uri": consumer_uri},
                        "position": {"line": position.line, "character": position.character},
                    }),
                ),
                state,
                &scheduler,
                Some(&root),
            );
            assert!(response.error.is_none(), "{:?}", response.error);
            serde_json::from_value::<lsp_types::CompletionList>(response.result.unwrap()).unwrap()
        };
        let old_provider = provider(".", "()", "Original expansion.");
        state.open(provider_uri.clone(), old_provider.clone(), 1);
        state.store_analysis(
            &root,
            Snapshot::new(
                ids.next(),
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 1)]),
            )
            .with_source_paths(BTreeMap::from([
                (consumer_uri.clone(), consumer_path.clone()),
                (provider_uri.clone(), provider_path.clone()),
            ])),
            analyze(&old_provider),
        );
        let old = request(&mut state);
        let old_detail = old
            .items
            .iter()
            .find(|item| item.label == "value")
            .unwrap()
            .detail
            .clone();
        assert!(old_detail.is_some(), "current local declaration type");

        let new_provider = provider(". -> .", ".(x: .) -> . { x }", "Current expansion.");
        state.open(provider_uri.clone(), new_provider.clone(), 2);
        let stale = request(&mut state);
        assert!(!stale.is_incomplete);
        assert!(stale.items.iter().any(|item| item.label == "value"));
        assert!(
            stale.items.iter().all(|item| item.detail.is_none()),
            "{stale:?}"
        );
        assert!(stale.items.iter().all(|item| item.documentation.is_none()));
        state.store_analysis(
            &root,
            Snapshot::new(
                ids.next(),
                BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 2)]),
            )
            .with_source_paths(BTreeMap::from([
                (consumer_uri.clone(), consumer_path.clone()),
                (provider_uri.clone(), provider_path.clone()),
            ])),
            analyze(&new_provider),
        );
        let fresh = request(&mut state);
        let local = fresh
            .items
            .iter()
            .find(|item| item.label == "value")
            .unwrap();
        assert!(local.detail.is_some());
        assert_ne!(local.detail, old_detail);
        let elaborator = fresh
            .items
            .iter()
            .find(|item| item.label == "expand!")
            .expect("selectively imported elaborator has bang insertion");
        assert_eq!(
            elaborator.kind,
            Some(lsp_types::CompletionItemKind::FUNCTION)
        );
        assert_eq!(elaborator.detail.as_deref(), Some(". -> ."));
        assert!(format!("{:?}", elaborator.documentation).contains("Current expansion."));
        assert_eq!(
            fresh
                .items
                .iter()
                .find(|item| item.label == "answer")
                .unwrap()
                .kind,
            Some(lsp_types::CompletionItemKind::CONSTANT)
        );

        state.close(&provider_uri);
        let closed = request(&mut state);
        assert!(
            closed.items.iter().all(|item| item.detail.is_none()),
            "{closed:?}"
        );
        scheduler.shutdown();
    }

    #[test]
    fn import_list_completion_request_uses_fresh_selected_provider() {
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::package_collection::SourceOverlay;

        let root = workspace_path("");
        let consumer_path = workspace_path("app/main.kio");
        let consumer_uri = workspace_uri("app/main.kio");
        let provider_path = workspace_path("syntax.kio");
        let provider_uri = workspace_uri("syntax.kio");
        let consumer = "module app/main;\nimport syntax(op";
        let mut state = ServerState::new();
        state.open(consumer_uri.clone(), consumer.to_owned(), 1);
        let (mut scheduler, _results) = Scheduler::spawn();
        let ids = SnapshotIdGen::new();

        let request = |state: &mut ServerState, uri: &Uri| {
            let source = state
                .document(uri)
                .expect("open completion source")
                .text()
                .to_owned();
            let position =
                crate::lsp::positions::LineIndex::new(&source).to_position(source.len() as u32);
            let response = dispatch_request(
                lsp_server::Request::new(
                    1.into(),
                    CompletionRequest::METHOD.to_owned(),
                    serde_json::json!({
                        "textDocument": {"uri": uri},
                        "position": {"line": position.line, "character": position.character},
                    }),
                ),
                state,
                &scheduler,
                Some(&root),
            );
            assert!(response.error.is_none(), "{:?}", response.error);
            serde_json::from_value::<Option<lsp_types::CompletionList>>(response.result.unwrap())
                .expect("operator import completion list")
        };

        for (event, (operator, marker)) in [("+", "*"), ("-", "%")].into_iter().enumerate() {
            let version = event as i32 + 1;
            let provider = format!(
                "module syntax;
                import __comptime__;
                pub fn pair(a: ., b: .) -> . {{ a }}
                pub fn zero() -> . {{ () }}
                pub type Unit = .;
                pub labels {{ field: . }};
                pure fn expand_body(ct: __Comptime__) -> __Checked_term__ {{ __term_unit__(ct) }}
                pub elab expand : . -> . {{ impl expand_body; }};
                pub op _ {operator} _ {{ impl pair; }};
                pub varop [{marker} {marker}] {{ foldl pair zero; }};"
            );
            state.open(provider_uri.clone(), provider.clone(), version);
            let stale = request(&mut state, &consumer_uri).expect("valid consumer context");
            assert!(stale.is_incomplete);
            assert!(
                stale.items.is_empty(),
                "a stale provider must not insert a grammar"
            );

            let mut files = BTreeMap::from([
                (
                    root.join("completion.pkg.kio"),
                    "package completion; bridge { syntax; }".to_owned(),
                ),
                (provider_path.clone(), provider.clone()),
            ]);
            let mut versions = BTreeMap::from([(provider_uri.clone(), version)]);
            if event == 0 {
                let valid_consumer =
                    "module app/main; import syntax(pair); pub fn run() -> . { () }";
                state.open(consumer_uri.clone(), valid_consumer.to_owned(), 2);
                files.insert(consumer_path.clone(), valid_consumer.to_owned());
                files.insert(
                    root.join("completion.pkg.kio"),
                    "package completion; bridge { app/main; }".to_owned(),
                );
                versions.insert(consumer_uri.clone(), 2);
            }
            let provider_overlay = SourceOverlay::complete(root.clone(), files);
            let analysis =
                crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &provider_overlay)
                    .expect("current provider declaration snapshot");
            assert_eq!(analysis.sources.get(&provider_path), Some(&provider));
            state.store_analysis(&root, Snapshot::new(ids.next(), versions), analysis);
            if event == 0 {
                assert!(
                    state
                        .best_analysis_for_file(&consumer_path, &consumer_uri)
                        .is_some()
                );
                state.open(consumer_uri.clone(), consumer.to_owned(), 3);
                assert!(
                    state.analysis_for_file(&consumer_path).is_some(),
                    "stale consumer snapshot retained"
                );
            }
            assert!(
                state
                    .best_analysis_for_file(&consumer_path, &consumer_uri)
                    .is_none()
            );

            let current = request(&mut state, &consumer_uri).expect("valid consumer context");
            assert!(!current.is_incomplete);
            assert_eq!(current.items.len(), 8);
            for name in ["pair", "zero", "Unit", "Field", "{field}", "expand"] {
                assert!(
                    current.items.iter().any(|item| item.label == name),
                    "{name}"
                );
            }
            assert_eq!(
                current
                    .items
                    .iter()
                    .filter(|item| item.kind == Some(lsp_types::CompletionItemKind::OPERATOR))
                    .count(),
                2
            );
            for item in current.items {
                let expected_fixed = format!("op _ {operator} _");
                let expected_variadic = format!("varop [{marker} {marker}]");
                let body = if item.label == expected_fixed {
                    format!("() {operator} ()")
                } else if item.label == expected_variadic {
                    format!("[{marker} (), () {marker}]")
                } else {
                    assert_ne!(item.kind, Some(lsp_types::CompletionItemKind::OPERATOR));
                    "()".to_owned()
                };
                let Some(lsp_types::CompletionTextEdit::Edit(edit)) = item.text_edit else {
                    panic!("whole grammar insertion");
                };
                assert_eq!(edit.new_text, item.label);
                let index = crate::lsp::positions::LineIndex::new(consumer);
                let start = index.position_to_offset(crate::lsp::positions::LspPosition {
                    line: edit.range.start.line,
                    character: edit.range.start.character,
                }) as usize;
                let inserted = format!(
                    "{}{});\npub fn run() -> . {{ {body} }}",
                    &consumer[..start],
                    edit.new_text,
                );
                crate::pass::parser::parse(&inserted).expect("inserted consumer grammar");
                let overlay = SourceOverlay::complete(
                    root.clone(),
                    BTreeMap::from([
                        (
                            root.join("completion.pkg.kio"),
                            "package completion; bridge { app/main; }".to_owned(),
                        ),
                        (provider_path.clone(), provider.clone()),
                        (consumer_path.clone(), inserted.clone()),
                    ]),
                );
                let analysis =
                    crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &overlay)
                        .expect(
                            "inserted grammar resolves and lowers through the selected provider",
                        );
                assert_eq!(analysis.sources.get(&consumer_path), Some(&inserted));
            }
        }
        state.open(
            consumer_uri.clone(),
            "module wrong/path;\nimport syntax(op".to_owned(),
            4,
        );
        assert!(
            request(&mut state, &consumer_uri).is_none(),
            "mismatched file/header cannot choose scoped exports"
        );
        let untitled: Uri = "untitled:completion.kio".parse().unwrap();
        state.open(untitled.clone(), consumer.to_owned(), 1);
        assert_eq!(
            request(&mut state, &untitled)
                .expect("untitled header context")
                .items
                .len(),
            8
        );
        state.open(consumer_uri.clone(), consumer.to_owned(), 5);
        state.open(provider_uri, "module syntax; broken {".to_owned(), 3);
        let broken = request(&mut state, &consumer_uri).expect("valid consumer context");
        assert!(broken.is_incomplete);
        assert!(broken.items.is_empty());
        scheduler.shutdown();
    }

    #[test]
    fn import_completion_after_provider_close_uses_disk_or_stays_incomplete() {
        use crate::lsp::snapshot::Snapshot;
        use std::time::Duration;

        for present_on_disk in [true, false] {
            let directory = tempfile::tempdir().expect("completion workspace");
            let root = std::fs::canonicalize(directory.path()).expect("canonical root");
            std::fs::create_dir(root.join("app")).expect("consumer directory");
            let consumer_path = root.join("app/main.kio");
            let provider_path = root.join("syntax.kio");
            let consumer = "module app/main; import syntax(op); pub fn run() -> . { () }";
            let provider = |operator| {
                format!(
                    "module syntax;
                pub fn op(value: .) -> . {{ value }}
                pub fn pair(a: ., b: .) -> . {{ a }}
                pub op _ {operator} _ {{ impl pair; }};"
                )
            };
            let unsaved = provider("+");
            let disk = provider("-");
            let package = "package completion; bridge { app/main; }".to_owned();
            std::fs::write(root.join("completion.pkg.kio"), &package).expect("package file");
            std::fs::write(&consumer_path, consumer).expect("consumer file");
            if present_on_disk {
                std::fs::write(&provider_path, &disk).expect("disk provider");
            }
            let consumer_uri = crate::lsp::diagnostics::path_to_uri(&consumer_path, &root).unwrap();
            let provider_uri = crate::lsp::diagnostics::path_to_uri(&provider_path, &root).unwrap();
            let mut state = ServerState::new();
            state.open(consumer_uri.clone(), consumer.to_owned(), 1);
            state.open(provider_uri.clone(), unsaved.clone(), 1);
            let initial_overlay = SourceOverlay::complete(
                root.clone(),
                BTreeMap::from([
                    (root.join("completion.pkg.kio"), package),
                    (consumer_path.clone(), consumer.to_owned()),
                    (provider_path.clone(), unsaved.clone()),
                ]),
            );
            let initial =
                crate::cmd::check::analyze_workspace_at_with_overlay_lsp(&root, &initial_overlay)
                    .expect("valid unsaved provider");
            assert_eq!(initial.sources.get(&provider_path), Some(&unsaved));
            let (mut scheduler, results) = Scheduler::spawn_with_debounce(Duration::from_millis(1));
            state.store_analysis(
                &root,
                Snapshot::new(
                    scheduler.next_snapshot_id(),
                    BTreeMap::from([(consumer_uri.clone(), 1), (provider_uri.clone(), 1)]),
                ),
                initial,
            );
            let request = |state: &mut ServerState| {
                let offset = consumer.find("(op)").unwrap() as u32 + 3;
                let position = crate::lsp::positions::LineIndex::new(consumer).to_position(offset);
                let response = dispatch_request(
                    lsp_server::Request::new(
                        1.into(),
                        CompletionRequest::METHOD.to_owned(),
                        serde_json::json!({
                            "textDocument": {"uri": consumer_uri},
                            "position": {"line": position.line, "character": position.character},
                        }),
                    ),
                    state,
                    &scheduler,
                    Some(&root),
                );
                assert!(response.error.is_none());
                serde_json::from_value::<lsp_types::CompletionList>(response.result.unwrap())
                    .expect("actual completion request")
            };
            let before = request(&mut state);
            assert!(!before.is_incomplete);
            assert!(before.items.iter().any(|item| item.label == "op _ + _"));
            assert!(!before.items.iter().any(|item| item.label == "op _ - _"));
            handle_notification(
                &mut state,
                &scheduler,
                Some(&root),
                Notification::new(
                    DidCloseTextDocument::METHOD.to_owned(),
                    serde_json::json!({"textDocument": {"uri": provider_uri}}),
                ),
            )
            .expect("close notification");
            assert!(state.document(&provider_uri).is_none());
            assert!(
                state
                    .best_analysis_for_file(&consumer_path, &consumer_uri)
                    .is_some(),
                "consumer version itself remains current"
            );
            assert!(
                state
                    .best_analysis_for_file(&provider_path, &provider_uri)
                    .is_none(),
                "closed provider cannot inherit its unsaved overlay version"
            );
            let pending = request(&mut state);
            assert!(pending.is_incomplete);
            assert!(pending.items.is_empty());
            let refreshed = results
                .recv_timeout(Duration::from_secs(10))
                .expect("DidClose schedules a full disk-backed refresh");
            assert!(refreshed.snapshot.version(&provider_uri).is_none());
            match &refreshed.outcome {
                WorkerOutcome::Full(Ok(analysis)) if present_on_disk => {
                    assert_eq!(analysis.sources.get(&provider_path), Some(&disk));
                }
                WorkerOutcome::Full(Err(failure)) if !present_on_disk => {
                    assert!(
                        failure
                            .errors
                            .iter()
                            .any(|error| error.error.diag().1.contains("syntax")),
                        "{:?}",
                        failure.errors
                    );
                }
                _ => panic!("wrong close refresh outcome"),
            }
            let (server, _client) = Connection::memory();
            publish_worker_result(&server, &mut state, refreshed)
                .expect("publish current disk result");
            let after = request(&mut state);
            if present_on_disk {
                assert!(!after.is_incomplete);
                assert!(after.items.iter().any(|item| item.label == "op _ - _"));
                assert!(!after.items.iter().any(|item| item.label == "op _ + _"));
            } else {
                assert!(after.is_incomplete);
                assert!(after.items.is_empty());
            }
            assert!(matches!(request_syntax_module(&mut state, &consumer_uri),
                RequestSyntaxModule::Contextual { source, .. } if source == consumer));
            scheduler.shutdown();
        }
    }

    #[test]
    fn server_caps_advertise_open_close_change_save() {
        let caps = server_capabilities();
        let TextDocumentSyncCapability::Options(opts) = caps
            .text_document_sync
            .expect("text_document_sync must be set")
        else {
            panic!("expected Options form for textDocumentSync");
        };
        assert_eq!(opts.open_close, Some(true));
        // change = Incremental: the server applies didChange edits
        // to the overlay store.
        assert_eq!(opts.change, Some(TextDocumentSyncKind::INCREMENTAL));
        // Save advertised as options-form with includeText=false —
        // the overlay is already authoritative when a save fires.
        let Some(TextDocumentSyncSaveOptions::SaveOptions(save)) = opts.save else {
            panic!("expected save SaveOptions form");
        };
        assert_eq!(save.include_text, Some(false));
    }

    #[test]
    fn server_caps_advertise_hover_definition_references() {
        let caps = server_capabilities();
        // hoverProvider = true
        assert!(
            matches!(
                caps.hover_provider,
                Some(HoverProviderCapability::Simple(true))
            ),
            "server must advertise hoverProvider = true"
        );
        // definitionProvider = true
        assert!(
            matches!(caps.definition_provider, Some(lsp_types::OneOf::Left(true))),
            "server must advertise definitionProvider = true"
        );
        // referencesProvider = true
        assert!(
            matches!(caps.references_provider, Some(lsp_types::OneOf::Left(true))),
            "server must advertise referencesProvider = true"
        );
        // documentHighlightProvider = true
        assert!(
            matches!(
                caps.document_highlight_provider,
                Some(lsp_types::OneOf::Left(true))
            ),
            "server must advertise documentHighlightProvider = true"
        );
    }

    #[test]
    fn server_caps_advertise_document_symbol_and_folding_range() {
        let caps = server_capabilities();
        // documentSymbolProvider = true
        assert!(
            matches!(
                caps.document_symbol_provider,
                Some(lsp_types::OneOf::Left(true))
            ),
            "server must advertise documentSymbolProvider = true"
        );
        // foldingRangeProvider = true
        assert!(
            matches!(
                caps.folding_range_provider,
                Some(FoldingRangeProviderCapability::Simple(true))
            ),
            "server must advertise foldingRangeProvider = true"
        );
    }

    #[test]
    fn server_caps_advertise_completion_provider() {
        let caps = server_capabilities();
        let opts = caps
            .completion_provider
            .expect("server must advertise completionProvider");
        assert_eq!(
            opts.resolve_provider,
            Some(false),
            "resolveProvider must be false"
        );
        assert_eq!(
            opts.trigger_characters,
            Some(vec![
                ":".to_owned(),
                "[".to_owned(),
                "(".to_owned(),
                ",".to_owned()
            ])
        );
    }

    #[test]
    fn server_caps_advertise_document_formatting_provider() {
        let caps = server_capabilities();
        assert!(
            matches!(
                caps.document_formatting_provider,
                Some(lsp_types::OneOf::Left(true))
            ),
            "server must advertise documentFormattingProvider = true"
        );
    }

    #[test]
    fn server_caps_advertise_semantic_tokens_provider() {
        use lsp_types::{SemanticTokensFullOptions, SemanticTokensServerCapabilities};
        let caps = server_capabilities();
        let provider = caps
            .semantic_tokens_provider
            .expect("server must advertise semanticTokensProvider");
        // Must be the options form (not the registration form).
        let SemanticTokensServerCapabilities::SemanticTokensOptions(opts) = provider else {
            panic!("expected SemanticTokensOptions; got registration form");
        };
        // full = true — we implement semanticTokens/full.
        assert_eq!(
            opts.full,
            Some(SemanticTokensFullOptions::Bool(true)),
            "semanticTokensProvider.full must be true"
        );
        // range = false — the server advertises full-document tokens.
        assert_eq!(
            opts.range,
            Some(false),
            "semanticTokensProvider.range must be false"
        );
        // Legend must be non-empty.
        assert!(
            !opts.legend.token_types.is_empty(),
            "semanticTokensProvider legend must declare token types"
        );
        assert!(
            !opts.legend.token_modifiers.is_empty(),
            "semanticTokensProvider legend must declare token modifiers"
        );
    }

    #[test]
    fn semantic_tokens_classify_binders_inside_imported_operator_syntax() {
        let mut state = ServerState::new();
        let provider = workspace_uri("app/syntax.kio");
        let consumer = workspace_uri("app/main.kio");
        state.open(
            provider,
            "module app/syntax; pub varop [* *] { foldr pair empty; };".to_owned(),
            1,
        );
        let source = "module app/main; import app/syntax(varop [* *]); fn make(value: .) -> . { value } fn run(value: .) -> . { let identity = .(inner: .) -> . { inner }; identity([* make(value), value *]) }";
        state.open(consumer.clone(), source.to_owned(), 1);

        let result = handle_semantic_tokens_full(&consumer, &mut state).expect("semantic tokens");
        let SemanticTokensResult::Tokens(tokens) = result else {
            panic!("expected full semantic tokens");
        };
        let deferred_binder = LineIndex::new(source)
            .to_position(source.find("inner").expect("deferred lambda binder") as u32);
        let mut line = 0;
        let mut column = 0;
        let mut deferred_binder_kind = None;
        for token in tokens.data {
            line += token.delta_line;
            column = if token.delta_line == 0 {
                column + token.delta_start
            } else {
                token.delta_start
            };
            if line == deferred_binder.line && column == deferred_binder.character {
                deferred_binder_kind =
                    crate::lsp::semantic_tokens::TOKEN_TYPES.get(token.token_type as usize);
                break;
            }
        }
        assert_eq!(
            deferred_binder_kind,
            Some(&lsp_types::SemanticTokenType::PARAMETER)
        );
    }

    #[test]
    fn semantic_tokens_keep_binder_classification_without_the_provider() {
        let mut state = ServerState::new();
        let consumer = workspace_uri("a/b/c.kio");
        let source = "module a/b/c; import a/d/e(varop [* *]); fn make(x: A) -> A { x } fn run(value: A) -> A { let identity = .(inner: A) -> A { inner }; identity([* make(value), value *]) }";
        let deferred_binder_offset = source.find("inner").expect("deferred lambda binder") as u32;
        state.open(consumer.clone(), source.to_owned(), 1);

        let result = handle_semantic_tokens_full(&consumer, &mut state)
            .expect("provider-independent semantic tokens");
        let SemanticTokensResult::Tokens(tokens) = result else {
            panic!("expected full semantic tokens");
        };
        let deferred_binder = LineIndex::new(source).to_position(deferred_binder_offset);
        let mut line = 0;
        let mut column = 0;
        let mut deferred_binder_kind = None;
        for token in tokens.data {
            line += token.delta_line;
            column = if token.delta_line == 0 {
                column + token.delta_start
            } else {
                token.delta_start
            };
            if line == deferred_binder.line && column == deferred_binder.character {
                deferred_binder_kind =
                    crate::lsp::semantic_tokens::TOKEN_TYPES.get(token.token_type as usize);
                break;
            }
        }
        assert_eq!(
            deferred_binder_kind,
            Some(&lsp_types::SemanticTokenType::PARAMETER)
        );
    }

    #[test]
    fn invalid_file_uri_never_falls_back_to_workspace_syntax_context() {
        let mut state = ServerState::new();
        let invalid_source = format!("{}?revision=1", workspace_uri("a/b/c.kio").as_str());
        let invalid = Uri::from_str(&invalid_source).expect("query-bearing file URI");
        let provider = workspace_uri("a/d/e.kio");
        let source =
            "module a/b/c; import a/d/e(varop [* *]); fn run(value: A) -> A { [* value, value *] }";
        state.open(
            provider,
            "module a/d/e; pub varop [* *] { foldr pair empty; };".to_owned(),
            1,
        );
        state.open(invalid.clone(), source.to_owned(), 1);

        assert!(with_overlay_parsed_module(&mut state, &invalid, |_, _| ()).is_none());
        assert!(handle_semantic_tokens_full(&invalid, &mut state).is_none());
        assert_eq!(handle_document_formatting(&invalid, &state), None);
    }

    #[test]
    fn direct_request_syntax_rejects_a_mismatched_file_context_before_lazy_fallback() {
        let uri = workspace_uri("a/b/c.kio");
        let mismatch = request_syntax_module_direct(
            &uri,
            "module wrong/path; fn run(value: .) -> . { value }".to_owned(),
        );
        assert!(matches!(mismatch, RequestSyntaxModule::ContextInvalid));

        let incomplete = request_syntax_module_direct(
            &uri,
            "module a/b/c; fn run(value: .) -> . { (".to_owned(),
        );
        assert!(matches!(
            incomplete,
            RequestSyntaxModule::AuthenticatedLazy { .. }
        ));

        let closed = request_syntax_module_direct(
            &uri,
            "module a/b/c; import a/d/e(varop [* *]); fn pair(k: A, v: A) -> A { [* k, v *] }"
                .to_owned(),
        );
        assert!(matches!(closed, RequestSyntaxModule::Contextual { .. }));
    }

    #[test]
    fn doc_hover_preserves_source_examples_with_imported_operator_syntax() {
        let mut state = ServerState::new();
        let provider = workspace_uri("app/syntax.kio");
        let consumer = workspace_uri("app/main.kio");
        state.open(
            provider,
            "module app/syntax; pub op ? _ : _ { impl choose }".to_owned(),
            1,
        );
        let source = "module app/main;\nimport app/syntax(op ? _ : _);\n/// Chooses with [`@source exercise`].\npub fn choose_value(value: .) -> . { value }\nfn exercise(condition: ., yes: .) -> . { ? condition : yes }\n";
        state.open(consumer.clone(), source.to_owned(), 1);
        let cursor = LineIndex::new(source).to_position(
            u32::try_from(
                source
                    .find("choose_value(")
                    .expect("documented declaration"),
            )
            .expect("test source fits in u32"),
        );
        let position = lsp_types::Position {
            line: cursor.line,
            character: cursor.character,
        };

        let hover = doc_hover_with_syntax_context(&consumer, &position, &mut state);
        let SyntaxContextResult::Available(Some(hover)) = hover else {
            panic!("documentation hover");
        };
        let lsp_types::HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(markup.value.contains("fn choose_value"));
        assert!(markup.value.contains("? condition : yes"));
    }

    #[test]
    fn unavailable_syntax_context_suppresses_a_stale_typed_hover_fallback() {
        let mut fallback_called = false;
        let result: Option<()> = SyntaxContextResult::Unavailable.or_else(|| {
            fallback_called = true;
            Some(())
        });
        assert_eq!(result, None);
        assert!(!fallback_called);

        let result = SyntaxContextResult::Available(None).or_else(|| {
            fallback_called = true;
            Some(())
        });
        assert_eq!(result, Some(()));
        assert!(fallback_called);
    }

    #[test]
    fn server_caps_advertise_rename_provider() {
        let caps = server_capabilities();
        // renameProvider must be the options form with prepareProvider = true.
        let provider = caps
            .rename_provider
            .expect("server must advertise renameProvider");
        let lsp_types::OneOf::Right(opts) = provider else {
            panic!("renameProvider must be the RenameOptions form (OneOf::Right)");
        };
        assert_eq!(
            opts.prepare_provider,
            Some(true),
            "renameProvider.prepareProvider must be true"
        );
    }

    #[test]
    fn server_caps_advertise_quickfix_and_fix_all_code_actions() {
        let caps = server_capabilities();
        let provider = caps
            .code_action_provider
            .expect("server must advertise codeActionProvider");
        let CodeActionProviderCapability::Options(opts) = provider else {
            panic!("codeActionProvider must be the options form");
        };
        assert_eq!(opts.resolve_provider, Some(true));
        assert_eq!(
            opts.code_action_kinds,
            Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::SOURCE_FIX_ALL,
            ]),
            "codeActionProvider must advertise quickfix and fix-all actions"
        );
    }

    #[test]
    fn closed_file_foreground_request_schedules_full_analysis() {
        assert_closed_file_foreground_request_schedules_full_analysis(Path::to_path_buf);
    }

    #[cfg(unix)]
    #[test]
    fn closed_file_foreground_request_uses_canonical_package_identity() {
        let aliases = tempfile::tempdir().expect("alias directory");
        assert_closed_file_foreground_request_schedules_full_analysis(|root| {
            let alias = aliases.path().join("workspace-link");
            std::os::unix::fs::symlink(root, &alias).expect("workspace symlink");
            alias
        });
    }

    fn assert_closed_file_foreground_request_schedules_full_analysis(
        request_root: impl FnOnce(&Path) -> PathBuf,
    ) {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("outer.pkg.kio"), "package outer;\n")
            .expect("write ancestor package");
        let workspace_root = dir.path().join("workspace");
        std::fs::create_dir(&workspace_root).expect("create workspace");
        let workspace_root = std::fs::canonicalize(workspace_root).expect("canonical workspace");
        std::fs::write(
            workspace_root.join("app.pkg.kio"),
            "package app; bridge { main; }\n",
        )
        .expect("write workspace package");
        let file_path = workspace_root.join("main.kio");
        std::fs::write(&file_path, "module main;\n").expect("write source");
        let request_root = request_root(&workspace_root);
        let uri =
            crate::lsp::diagnostics::path_to_uri(&request_root.join("main.kio"), &request_root)
                .expect("file URI");
        let mut state = ServerState::new();
        let (mut scheduler, results) = Scheduler::spawn_with_debounce(Duration::from_millis(1));

        schedule_analysis(
            &mut state,
            &scheduler,
            Some(&workspace_root),
            &uri,
            WorkPriority::Foreground,
            true,
        );
        let result = results
            .recv_timeout(Duration::from_secs(10))
            .expect("analysis result");
        scheduler.shutdown();

        assert_eq!(result.package_root, workspace_root);
        assert!(matches!(result.outcome, WorkerOutcome::Full(Ok(_))));
    }

    #[test]
    fn reopened_document_does_not_publish_a_previous_lifetime_focused_result() {
        let (server, _client) = Connection::memory();
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let package_root = workspace_path("kio-lsp-reopened-publish");
        let file_path = package_root.join("main.kio");
        let uri = workspace_uri("kio-lsp-reopened-publish/main.kio");
        let old_source = "module pkg/old;\n";
        state.open(uri.clone(), old_source.to_owned(), 1);
        let snapshot_id = ids.next();
        state.mark_focused_scheduled(&uri, &file_path, snapshot_id);
        let snapshot =
            crate::lsp::snapshot::Snapshot::new(snapshot_id, BTreeMap::from([(uri.clone(), 1)]));

        state.close(&uri);
        state.open(uri.clone(), "module pkg/new;\n".to_owned(), 1);
        publish_worker_result(
            &server,
            &mut state,
            WorkerResult {
                overlay_versions: snapshot.versions().clone(),
                snapshot,
                package_root,
                outcome: WorkerOutcome::Focused {
                    uri: uri.clone(),
                    file_path: file_path.clone(),
                    outcome: Ok(analysis_for_file(&file_path, "pkg/old", old_source)),
                },
            },
        )
        .expect("ignore obsolete focused result");

        assert!(state.best_analysis_for_file(&file_path, &uri).is_none());
    }

    #[test]
    fn failed_full_analysis_retains_focus_and_does_not_advance_its_watermark() {
        use crate::error::Error;
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;

        let (server, _client) = Connection::memory();
        let mut state = ServerState::new();
        let ids = crate::lsp::snapshot::SnapshotIdGen::new();
        let package_root = workspace_path("kio-lsp-full-failure-retention");
        let file_path = package_root.join("main.kio");
        let uri = workspace_uri("kio-lsp-full-failure-retention/main.kio");
        let source = "module pkg/main;\n";
        state.open(uri.clone(), source.to_owned(), 1);
        let focused_id = ids.next();
        state.mark_focused_scheduled(&uri, &file_path, focused_id);
        let focused_snapshot =
            crate::lsp::snapshot::Snapshot::new(focused_id, BTreeMap::from([(uri.clone(), 1)]));
        state.store_focused_analysis(
            &uri,
            &file_path,
            focused_snapshot.clone(),
            analysis_for_file(&file_path, "pkg/main", source),
        );
        let full_id = ids.next();
        state.mark_scheduled(&package_root, full_id);

        publish_worker_result(
            &server,
            &mut state,
            WorkerResult {
                snapshot: crate::lsp::snapshot::Snapshot::new(
                    full_id,
                    BTreeMap::from([(uri.clone(), 1)]),
                ),
                overlay_versions: BTreeMap::from([(uri.clone(), 1)]),
                package_root,
                outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                    errors: vec![LocatedError {
                        file_path: file_path.clone(),
                        error: Error::type_(Span::new(0, 1), "full analysis failed"),
                    }],
                    sources: Box::new(HashMap::from([(file_path.clone(), source.to_owned())])),
                })),
            },
        )
        .expect("publish full failure");

        let retained = state
            .best_analysis_entry_for_file(&file_path, &uri)
            .expect("focused shard retained after full failure");
        assert_eq!(retained.snapshot().id(), focused_id);
        assert!(!state.focused_worker_result_is_obsolete(&file_path, &uri, &focused_snapshot));
    }

    #[test]
    fn obsolete_worker_result_does_not_publish_diagnostics() {
        use crate::error::Error;
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;
        use std::time::Duration;

        let (server, client) = Connection::memory();
        let mut state = ServerState::new();
        let ids = SnapshotIdGen::new();
        let older = ids.next();
        let newer = ids.next();
        let package_root = if cfg!(windows) {
            PathBuf::from(r"C:\tmp\kio-lsp-stale")
        } else {
            PathBuf::from("/tmp/kio-lsp-stale")
        };
        let file_path = package_root.join("main.kio");
        // Derive the URI from the path via the same conversion the
        // publisher uses, so the version lookup matches on every OS
        // (a hardcoded `file:///tmp/...` doesn't round-trip on Windows).
        let uri = crate::lsp::diagnostics::path_to_uri(&file_path, &package_root).expect("uri");
        state.mark_scheduled(&package_root, newer);

        let stale = worker_result_with_type_error(
            package_root.clone(),
            Snapshot::new(older, BTreeMap::from([(uri.clone(), 1)])),
            file_path.clone(),
            "module pkg/main;\n\npub fn run() -> . { 1 }\n",
            Error::type_(Span::new(39, 40), "stale type error"),
        );
        publish_worker_result(&server, &mut state, stale).expect("publish stale result");
        assert!(
            client
                .receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "obsolete result must not publish diagnostics"
        );

        let fresh = worker_result_with_type_error(
            package_root.clone(),
            Snapshot::new(newer, BTreeMap::from([(uri, 2)])),
            file_path,
            "module pkg/main;\n\npub fn run() -> . { 2 }\n",
            Error::type_(Span::new(39, 40), "fresh type error"),
        );
        publish_worker_result(&server, &mut state, fresh).expect("publish fresh result");
        let msg = client
            .receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh result should publish diagnostics");
        let Message::Notification(notification) = msg else {
            panic!("expected notification, got {msg:?}");
        };
        assert_eq!(notification.method, PublishDiagnostics::METHOD);
        assert_eq!(
            notification
                .params
                .get("version")
                .and_then(serde_json::Value::as_i64),
            Some(2)
        );

        fn worker_result_with_type_error(
            package_root: PathBuf,
            snapshot: Snapshot,
            file_path: PathBuf,
            source: &str,
            error: Error,
        ) -> WorkerResult {
            let overlay_versions = snapshot.versions().clone();
            WorkerResult {
                snapshot,
                overlay_versions,
                package_root,
                outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                    errors: vec![LocatedError {
                        file_path: file_path.clone(),
                        error,
                    }],
                    sources: Box::new(HashMap::from([(file_path, source.to_owned())])),
                })),
            }
        }
    }

    #[test]
    fn full_worker_publishes_transported_origin_with_its_source_version_and_fixes() {
        use crate::error::Error;
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;
        use lsp_types::{Position, Range};
        use std::time::Duration;

        let (server, client) = Connection::memory();
        let mut state = ServerState::new();
        let snapshot_id = SnapshotIdGen::new().next();
        let package_root = workspace_path("kio-lsp-transported-origin");
        let caller_path = PathBuf::from("caller.kio");
        let declaration_path = PathBuf::from("declaration.kio");
        let caller_uri = workspace_uri("kio-lsp-transported-origin/caller.kio");
        let declaration_uri = workspace_uri("kio-lsp-transported-origin/declaration.kio");
        let caller = "// α\n\nfn run() { scope! { () } }\n";
        let declaration = "// 🦀\n// context\n// é\n/* 🦀 */ Hidden\n";
        let payload = declaration.find("Hidden").expect("payload") as u32;
        let invocation = caller.find("scope!").expect("invocation") as u32;
        let nested = LocatedError::new(
            declaration_path.clone(),
            Error::type_(Span::new(payload, payload + 6), "private payload")
                .with_help("make the payload public")
                .with_suggestion(Span::new(payload, payload + 6), "Public"),
        )
        .into_error()
        .with_secondary_in_file(
            caller_path.clone(),
            Span::new(invocation, invocation + 6),
            "expanded at this call",
        );
        let error = LocatedError::new(caller_path.clone(), nested);
        let versions = BTreeMap::from([(caller_uri.clone(), 7), (declaration_uri.clone(), 11)]);
        state.mark_scheduled(&package_root, snapshot_id);
        let result = WorkerResult {
            snapshot: Snapshot::new(snapshot_id, versions.clone()),
            overlay_versions: versions,
            package_root,
            outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                errors: vec![error],
                sources: Box::new(HashMap::from([
                    (caller_path, caller.to_owned()),
                    (declaration_path, declaration.to_owned()),
                ])),
            })),
        };
        publish_worker_result(&server, &mut state, result).expect("publish source diagnostic");
        let message = client
            .receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("publication");
        let Message::Notification(notification) = message else {
            panic!("expected notification, got {message:?}");
        };
        let params: PublishDiagnosticsParams =
            serde_json::from_value(notification.params).expect("diagnostics");
        assert_eq!(params.uri, declaration_uri);
        assert_eq!(params.version, Some(11));
        assert_eq!(params.diagnostics.len(), 1);
        let diagnostic = &params.diagnostics[0];
        assert_eq!(
            diagnostic.range,
            Range::new(Position::new(3, 9), Position::new(3, 15))
        );
        let related = diagnostic
            .related_information
            .as_ref()
            .expect("call context");
        assert_eq!(related[0].location.uri, caller_uri);
        assert_eq!(
            related[0].location.range,
            Range::new(Position::new(2, 11), Position::new(2, 17))
        );
        let data = diagnostic.data.as_ref().expect("help and fix");
        assert_eq!(data["help"], "make the payload public");
        assert_eq!(data["fixes"][0]["edits"][0]["replacement"], "Public");
        let fix_range: Range =
            serde_json::from_value(data["fixes"][0]["edits"][0]["range"].clone())
                .expect("fix range");
        assert_eq!(fix_range, diagnostic.range);
        assert!(
            client
                .receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
    }

    #[test]
    fn full_worker_publishes_cross_file_related_information_on_the_primary_uri() {
        use crate::error::Error;
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;
        use std::time::Duration;

        let (server, client) = Connection::memory();
        let mut state = ServerState::new();
        let snapshot_id = SnapshotIdGen::new().next();
        let package_root = workspace_path("kio-lsp-cross-file");
        let caller_path = PathBuf::from("caller.kio");
        let provider_path = PathBuf::from("provider.kio");
        let caller_uri = workspace_uri("kio-lsp-cross-file/caller.kio");
        let provider_uri = workspace_uri("kio-lsp-cross-file/provider.kio");
        let caller = "fn use(value: Wrap(_)) { value }\n";
        let provider = "pub type Wrap[T] = [A] T -> A;\n";
        let hole = caller.find('_').expect("hole") as u32;
        let binder = provider.find('A').expect("binder") as u32;
        state.mark_scheduled(&package_root, snapshot_id);

        let result = WorkerResult {
            snapshot: Snapshot::new(snapshot_id, BTreeMap::from([(caller_uri.clone(), 7)])),
            overlay_versions: BTreeMap::from([(caller_uri.clone(), 7)]),
            package_root,
            outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                errors: vec![LocatedError {
                    file_path: caller_path.clone(),
                    error: Error::type_(Span::new(hole, hole + 1), "invalid placeholder")
                        .with_secondary_in_file(
                            provider_path.clone(),
                            Span::new(binder, binder + 1),
                            "provider binder",
                        ),
                }],
                sources: Box::new(HashMap::from([
                    (caller_path, caller.to_owned()),
                    (provider_path, provider.to_owned()),
                ])),
            })),
        };
        publish_worker_result(&server, &mut state, result).expect("publish diagnostics");

        let message = client
            .receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("one caller diagnostic publication");
        let Message::Notification(notification) = message else {
            panic!("expected notification, got {message:?}");
        };
        assert_eq!(notification.method, PublishDiagnostics::METHOD);
        let params: PublishDiagnosticsParams =
            serde_json::from_value(notification.params).expect("diagnostic params");
        assert_eq!(params.uri, caller_uri);
        assert_eq!(params.version, Some(7));
        assert_eq!(params.diagnostics.len(), 1);
        let related = params.diagnostics[0]
            .related_information
            .as_ref()
            .expect("provider related information");
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].location.uri, provider_uri);
        assert_eq!(related[0].message, "provider binder");
        assert!(
            client
                .receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "the provider receives no separate diagnostic publication"
        );
    }

    #[test]
    fn missing_related_source_still_publishes_primary_and_counts_once() {
        use crate::error::Error;
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;
        use std::time::Duration;

        let package_root = workspace_path("kio-lsp-missing-related");
        let caller_path = PathBuf::from("caller.kio");
        let missing_path = PathBuf::from("missing-provider.kio");
        let caller_uri = workspace_uri("kio-lsp-missing-related/caller.kio");
        let caller = "fn use(value: Wrap(_)) { value }\n";
        let hole = caller.find('_').expect("hole") as u32;
        let located = || LocatedError {
            file_path: caller_path.clone(),
            error: Error::type_(Span::new(hole, hole + 1), "invalid placeholder")
                .with_secondary_in_file(
                    missing_path.clone(),
                    Span::new(10, 11),
                    "missing provider binder",
                ),
        };
        let sources = HashMap::from([(caller_path.clone(), caller.to_owned())]);
        let mut collected = BTreeMap::new();
        assert_eq!(
            collect_worker_diagnostics(
                &package_root,
                vec![located()],
                sources.clone(),
                &mut collected,
            ),
            1,
            "one unavailable exact related source must be counted once",
        );
        let [diagnostic] = collected
            .get(&caller_uri)
            .expect("the primary diagnostic remains publishable")
            .as_slice()
        else {
            panic!("the caller must retain exactly one diagnostic")
        };
        assert!(diagnostic.related_information.is_none());

        let (server, client) = Connection::memory();
        let mut state = ServerState::new();
        let snapshot_id = SnapshotIdGen::new().next();
        state.mark_scheduled(&package_root, snapshot_id);
        let result = WorkerResult {
            snapshot: Snapshot::new(snapshot_id, BTreeMap::from([(caller_uri.clone(), 11)])),
            overlay_versions: BTreeMap::from([(caller_uri.clone(), 11)]),
            package_root,
            outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                errors: vec![located()],
                sources: Box::new(sources),
            })),
        };
        publish_worker_result(&server, &mut state, result)
            .expect("publish primary diagnostic without related source");
        let message = client
            .receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("one primary publication");
        let Message::Notification(notification) = message else {
            panic!("expected notification, got {message:?}");
        };
        let params: PublishDiagnosticsParams =
            serde_json::from_value(notification.params).expect("diagnostic params");
        assert_eq!(params.uri, caller_uri);
        assert_eq!(params.version, Some(11));
        assert_eq!(params.diagnostics.len(), 1);
        assert!(params.diagnostics[0].related_information.is_none());
        assert!(
            client
                .receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "the missing provider must not receive a publication",
        );
    }

    #[test]
    fn focused_failure_does_not_replace_full_diagnostics() {
        use crate::error::Error;
        use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
        use crate::pass::resolve::LocatedError;
        use crate::span::Span;
        use std::time::Duration;

        let (server, client) = Connection::memory();
        let mut state = ServerState::new();
        let ids = SnapshotIdGen::new();
        let package_root = PathBuf::from("/tmp/kio-lsp-focused-fail");
        let file_path = package_root.join("main.kio");
        let uri = Uri::from_str("file:///tmp/kio-lsp-focused-fail/main.kio").expect("uri");
        let versions = BTreeMap::from([(uri.clone(), 1)]);

        let full = WorkerResult {
            snapshot: Snapshot::new(ids.next(), versions.clone()),
            overlay_versions: versions.clone(),
            package_root: package_root.clone(),
            outcome: WorkerOutcome::Full(Err(AnalysisFailure {
                errors: vec![LocatedError {
                    file_path: file_path.clone(),
                    error: Error::type_(Span::new(39, 40), "full type error"),
                }],
                sources: Box::new(HashMap::from([(
                    file_path.clone(),
                    "module pkg/main;\n\npub fn run() -> . { 1 }\n".to_owned(),
                )])),
            })),
        };
        publish_worker_result(&server, &mut state, full).expect("publish full diagnostics");
        let msg = client
            .receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("full diagnostics should publish");
        let Message::Notification(notification) = msg else {
            panic!("expected notification, got {msg:?}");
        };
        assert_eq!(notification.method, PublishDiagnostics::METHOD);

        let focused = WorkerResult {
            snapshot: Snapshot::new(ids.next(), versions.clone()),
            overlay_versions: versions,
            package_root,
            outcome: WorkerOutcome::Focused {
                uri,
                file_path: file_path.clone(),
                outcome: Err(AnalysisFailure {
                    errors: vec![LocatedError {
                        file_path: file_path.clone(),
                        error: Error::type_(Span::new(39, 40), "focused partial error"),
                    }],
                    sources: Box::new(HashMap::from([(
                        file_path,
                        "module pkg/main;\n\npub fn run() -> . { 2 }\n".to_owned(),
                    )])),
                }),
            },
        };
        publish_worker_result(&server, &mut state, focused).expect("ignore focused diagnostics");
        assert!(
            client
                .receiver
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "focused failures must not replace full published diagnostics"
        );
    }

    #[cfg(unix)]
    #[test]
    fn uri_to_path_decodes_file_uri() {
        let uri = Uri::from_str("file:///tmp/foo.kio").unwrap();
        assert_eq!(uri_to_path(&uri), Some(PathBuf::from("/tmp/foo.kio")));
    }

    #[cfg(unix)]
    #[test]
    fn uri_to_path_decodes_spaces() {
        let uri = Uri::from_str("file:///tmp/with%20space/foo.kio").unwrap();
        assert_eq!(
            uri_to_path(&uri),
            Some(PathBuf::from("/tmp/with space/foo.kio"))
        );
    }

    #[test]
    fn uri_to_path_rejects_non_file_scheme() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(uri_to_path(&uri), None);
    }
}
