//! Deterministic simulation of the language server.
//!
//! [`Simulation`] runs the real server (the same `ServerState` handlers, the same worker and the
//! same [`run_job`] scheduling step) with every source of nondeterminism replaced by a choice the
//! caller makes: there is no worker thread, so the caller decides when a job or a unit of idle
//! work runs; there is no socket, so outgoing traffic lands in a transcript; and a cancellation
//! lands at a chosen checkpoint of a chosen job rather than whenever another thread gets there.
//! The workspace is a directory that only the caller writes.
//!
//! [`run_session`] builds a seeded editor session on top: random edits, typing bursts, saves,
//! opens and closes, file-system changes whose watcher events arrive late, duplicated or
//! reordered, requests at valid and invalid positions, requests the client abandons, and edits
//! that land while a query is in flight. At checkpoints it compares the session against a fresh
//! server started on the same disk and buffers, which is the from-scratch oracle: incremental
//! state must never be distinguishable from a cold start.

use super::{CancelHandle, Job, Outbox, ServerState, Worker, WorkerSeed, run_job};
use crate::config::ExperimentalFeatures;
use async_lsp::lsp_types::{self, Position, Url};
use async_lsp::{LanguageServer, ResponseError};
use futures::future::BoxFuture;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll};
use std::{fs, mem};
use syntax::testing::Entropy;

/// One message the server sent to the client.
#[derive(Debug, Clone)]
pub enum Outgoing {
    PublishDiagnostics(lsp_types::PublishDiagnosticsParams),
    ShowMessage(lsp_types::ShowMessageParams),
    RegisterCapability(lsp_types::RegistrationParams),
    DiagnosticRefresh,
}

/// The client capabilities a simulated session negotiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOptions {
    pub pull_diagnostics: bool,
    pub utf8: bool,
}

pub type Response = Result<serde_json::Value, ResponseError>;

/// A client notification, as the client sends it.
#[derive(Debug, Clone)]
pub enum Notification {
    DidOpen(lsp_types::DidOpenTextDocumentParams),
    DidChange(lsp_types::DidChangeTextDocumentParams),
    DidSave(lsp_types::DidSaveTextDocumentParams),
    DidClose(lsp_types::DidCloseTextDocumentParams),
    DidChangeWatchedFiles(lsp_types::DidChangeWatchedFilesParams),
}

/// A client request. Every variant names a document and, where the feature takes one, a
/// position in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestKind {
    Hover,
    Definition,
    References,
    Rename,
    Completion,
    SignatureHelp,
    DocumentSymbol,
    FoldingRange,
    SemanticTokens,
    Formatting,
    InlayHint,
    CodeAction,
    Diagnostic,
}

pub const REQUEST_KINDS: &[RequestKind] = &[
    RequestKind::Hover,
    RequestKind::Definition,
    RequestKind::References,
    RequestKind::Rename,
    RequestKind::Completion,
    RequestKind::SignatureHelp,
    RequestKind::DocumentSymbol,
    RequestKind::FoldingRange,
    RequestKind::SemanticTokens,
    RequestKind::Formatting,
    RequestKind::InlayHint,
    RequestKind::CodeAction,
    RequestKind::Diagnostic,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(usize);

/// The server, driven step by step.
pub struct Simulation {
    state: ServerState,
    receiver: mpsc::Receiver<Job>,
    seed: Option<WorkerSeed>,
    worker: Option<Worker>,
    outbox: Arc<Mutex<Vec<Outgoing>>>,
    in_flight: Vec<(RequestId, BoxFuture<'static, Response>)>,
    responses: BTreeMap<RequestId, Response>,
    next_request: usize,
}

impl Simulation {
    /// Starts a server on `root`, completes the initialize handshake and settles.
    pub fn start(root: &Path, options: ClientOptions) -> Simulation {
        syntax::testing::enable_invariants();
        let (sender, receiver) = mpsc::channel();
        let cancel = CancelHandle::default();
        let outbox = Arc::new(Mutex::new(Vec::new()));
        let seed = WorkerSeed {
            outbox: Outbox::Recorder(outbox.clone()),
            experimental_features: ExperimentalFeatures::default(),
            debug: false,
            cancel: cancel.clone(),
        };
        let mut simulation = Simulation {
            state: ServerState { sender, cancel },
            receiver,
            seed: Some(seed),
            worker: None,
            outbox,
            in_flight: Vec::new(),
            responses: BTreeMap::new(),
            next_request: 0,
        };
        let initialize = simulation
            .state
            .initialize(initialize_params(root, options));
        let id = simulation.track(Box::pin(async move {
            initialize.await.map(|result| to_json(&result))
        }));
        simulation.settle();
        if let Some(Err(error)) = simulation.take_response(id) {
            panic!("initialize failed: {error:?}");
        }
        simulation.notify(SimulatedNotification::Initialized);
        simulation.settle();
        simulation
    }

    pub fn notify(&mut self, notification: impl Into<SimulatedNotification>) {
        // The handlers return `Continue` unconditionally; a send failure panics inside them.
        let _flow = match notification.into() {
            SimulatedNotification::Initialized => {
                self.state.initialized(lsp_types::InitializedParams {})
            }
            SimulatedNotification::Client(Notification::DidOpen(params)) => {
                self.state.did_open(params)
            }
            SimulatedNotification::Client(Notification::DidChange(params)) => {
                self.state.did_change(params)
            }
            SimulatedNotification::Client(Notification::DidSave(params)) => {
                self.state.did_save(params)
            }
            SimulatedNotification::Client(Notification::DidClose(params)) => {
                self.state.did_close(params)
            }
            SimulatedNotification::Client(Notification::DidChangeWatchedFiles(params)) => {
                self.state.did_change_watched_files(params)
            }
        };
    }

    /// Sends a request about `uri` at `position`; its response arrives once the worker has run
    /// it and [`Simulation::take_response`] collects it.
    pub fn request(&mut self, kind: RequestKind, uri: &Url, position: Position) -> RequestId {
        self.request_with_name(kind, uri, position, "renamed")
    }

    /// Sends a rename to `new_name`; every other request kind ignores the name.
    pub fn request_rename(&mut self, uri: &Url, position: Position, new_name: &str) -> RequestId {
        self.request_with_name(RequestKind::Rename, uri, position, new_name)
    }

    fn request_with_name(
        &mut self,
        kind: RequestKind,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> RequestId {
        let document = lsp_types::TextDocumentIdentifier::new(uri.clone());
        let at = lsp_types::TextDocumentPositionParams::new(document.clone(), position);
        let work = lsp_types::WorkDoneProgressParams::default();
        let partial = lsp_types::PartialResultParams::default();
        let whole = lsp_types::Range::new(Position::new(0, 0), Position::new(u32::MAX / 2, 0));
        let future: BoxFuture<'static, Response> = match kind {
            RequestKind::Hover => json(self.state.hover(lsp_types::HoverParams {
                text_document_position_params: at,
                work_done_progress_params: work,
            })),
            RequestKind::Definition => {
                json(self.state.definition(lsp_types::GotoDefinitionParams {
                    text_document_position_params: at,
                    work_done_progress_params: work,
                    partial_result_params: partial,
                }))
            }
            RequestKind::References => json(self.state.references(lsp_types::ReferenceParams {
                text_document_position: at,
                work_done_progress_params: work,
                partial_result_params: partial,
                context: lsp_types::ReferenceContext {
                    include_declaration: true,
                },
            })),
            RequestKind::Rename => json(self.state.rename(lsp_types::RenameParams {
                text_document_position: at,
                new_name: new_name.to_owned(),
                work_done_progress_params: work,
            })),
            RequestKind::Completion => json(self.state.completion(lsp_types::CompletionParams {
                text_document_position: at,
                work_done_progress_params: work,
                partial_result_params: partial,
                context: None,
            })),
            RequestKind::SignatureHelp => {
                json(self.state.signature_help(lsp_types::SignatureHelpParams {
                    context: None,
                    text_document_position_params: at,
                    work_done_progress_params: work,
                }))
            }
            RequestKind::DocumentSymbol => {
                json(self.state.document_symbol(lsp_types::DocumentSymbolParams {
                    text_document: document,
                    work_done_progress_params: work,
                    partial_result_params: partial,
                }))
            }
            RequestKind::FoldingRange => {
                json(self.state.folding_range(lsp_types::FoldingRangeParams {
                    text_document: document,
                    work_done_progress_params: work,
                    partial_result_params: partial,
                }))
            }
            RequestKind::SemanticTokens => json(self.state.semantic_tokens_full(
                lsp_types::SemanticTokensParams {
                    work_done_progress_params: work,
                    partial_result_params: partial,
                    text_document: document,
                },
            )),
            RequestKind::Formatting => {
                json(self.state.formatting(lsp_types::DocumentFormattingParams {
                    text_document: document,
                    options: lsp_types::FormattingOptions {
                        tab_size: 2,
                        insert_spaces: true,
                        ..Default::default()
                    },
                    work_done_progress_params: work,
                }))
            }
            RequestKind::InlayHint => json(self.state.inlay_hint(lsp_types::InlayHintParams {
                work_done_progress_params: work,
                text_document: document,
                range: whole,
            })),
            RequestKind::CodeAction => json(self.state.code_action(lsp_types::CodeActionParams {
                text_document: document,
                range: lsp_types::Range::new(position, position),
                context: lsp_types::CodeActionContext::default(),
                work_done_progress_params: work,
                partial_result_params: partial,
            })),
            RequestKind::Diagnostic => json(self.state.document_diagnostic(
                lsp_types::DocumentDiagnosticParams {
                    text_document: document,
                    identifier: None,
                    previous_result_id: None,
                    work_done_progress_params: work,
                    partial_result_params: partial,
                },
            )),
        };
        self.track(future)
    }

    /// The client gave up on a request (`$/cancelRequest`): its response is dropped, but the
    /// job it queued still runs, as it does over a real connection.
    pub fn abandon(&mut self, id: RequestId) {
        self.in_flight.retain(|(candidate, _)| *candidate != id);
        self.responses.remove(&id);
    }

    pub fn take_response(&mut self, id: RequestId) -> Option<Response> {
        self.responses.remove(&id)
    }

    pub fn is_pending(&self, id: RequestId) -> bool {
        self.in_flight.iter().any(|(candidate, _)| *candidate == id)
    }

    /// Runs one queued job, or one unit of idle work when nothing is queued, exactly as the
    /// worker loop would. Returns false when the worker has nothing to do.
    pub fn step(&mut self) -> bool {
        let job = self.receiver.try_recv().ok();
        let idle = self.worker.as_ref().is_some_and(Worker::has_idle_work);
        if job.is_none() && !idle {
            return false;
        }
        run_job(&mut self.seed, &mut self.worker, job);
        self.poll_requests();
        true
    }

    /// Runs one step during which `notification` arrives: the cancellation token flips at the
    /// step's `checkpoint`-th cancellable query, then the notification is enqueued. This is the
    /// interleaving a real edit produces when it lands while the worker is busy.
    pub fn step_interrupted(
        &mut self,
        checkpoint: usize,
        notification: impl Into<SimulatedNotification>,
    ) -> bool {
        if let Some(worker) = self.worker.as_mut() {
            worker.cancel_at_checkpoint = Some(checkpoint);
        }
        let stepped = self.step();
        if let Some(worker) = self.worker.as_mut() {
            worker.cancel_at_checkpoint = None;
        }
        self.notify(notification);
        stepped
    }

    /// Runs the worker until it has nothing queued and no idle work left.
    pub fn settle(&mut self) {
        let mut budget = 100_000;
        while self.step() {
            budget -= 1;
            assert!(budget > 0, "the worker never went idle");
        }
        self.poll_requests();
    }

    pub fn is_idle(&self) -> bool {
        !self.worker.as_ref().is_some_and(Worker::has_idle_work)
    }

    pub fn outgoing(&self) -> Vec<Outgoing> {
        self.outbox.lock().expect("simulation outbox lock").clone()
    }

    pub fn drain_outgoing(&mut self) -> Vec<Outgoing> {
        mem::take(&mut *self.outbox.lock().expect("simulation outbox lock"))
    }

    /// The text the server holds for `uri`, when it tracks the document as an analyzed file.
    pub fn document_text(&mut self, uri: &Url) -> Option<String> {
        let worker = self.worker.as_mut()?;
        worker.document(uri).map(|(_, text)| text)
    }

    /// A rendering of every input the worker feeds the analysis: tracked files and their text,
    /// open documents, standalone buffers, package metadata, data-file layouts and settings.
    /// Two servers that agree on this agree on every answer the analysis can give.
    pub fn input_state(&self) -> String {
        let Some(worker) = self.worker.as_ref() else {
            return "uninitialized".to_owned();
        };
        let db = &worker.db;
        let mut out = String::new();
        let files: BTreeMap<&PathBuf, &semantics::SourceFile> = worker.files.iter().collect();
        for (path, file) in files {
            let open = worker.open_documents.contains(path);
            writeln!(
                out,
                "file {} open={open} kind={:?}\n{}",
                path.display(),
                file.kind(db),
                file.text(db)
            )
            .ok();
        }
        let project: Vec<String> = semantics::ProjectFiles::get(db)
            .files(db)
            .iter()
            .map(|file| {
                worker
                    .path_of(*file)
                    .map_or_else(|| "?".to_owned(), |path| path.display().to_string())
            })
            .collect();
        writeln!(out, "project order {project:?}").ok();
        let stubs: BTreeMap<_, _> = worker.stub_documents.iter().collect();
        let namespaces: BTreeMap<_, _> = worker.namespace_documents.iter().collect();
        writeln!(
            out,
            "stub buffers {stubs:?}\nnamespace buffers {namespaces:?}"
        )
        .ok();
        let metadata = semantics::metadata::PackageMetadata::get(db);
        writeln!(
            out,
            "imports {:?}\ndependencies {:?}\nattached {:?}\npackage {:?}\nnative {:?}",
            metadata.imports(db),
            metadata.dependencies(db),
            metadata.attached(db),
            metadata.package(db),
            metadata.native_symbols(db)
        )
        .ok();
        let layouts: BTreeMap<_, _> = semantics::DataFiles::get(db).layouts(db).iter().collect();
        writeln!(out, "data files {layouts:?}").ok();
        writeln!(
            out,
            "inline annotations {:?}\nconfig {:?}",
            semantics::AnalysisSettings::get(db).inline_annotations(db),
            ConfigView(&worker.config)
        )
        .ok();
        out
    }

    fn track(&mut self, future: BoxFuture<'static, Response>) -> RequestId {
        let id = RequestId(self.next_request);
        self.next_request += 1;
        self.in_flight.push((id, future));
        self.poll_requests();
        id
    }

    fn poll_requests(&mut self) {
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        let mut ready = Vec::new();
        self.in_flight
            .retain_mut(|(id, future)| match future.as_mut().poll(&mut context) {
                Poll::Ready(response) => {
                    ready.push((*id, response));
                    false
                }
                Poll::Pending => true,
            });
        self.responses.extend(ready);
    }
}

/// What [`Simulation::notify`] accepts: a client notification, or the handshake's
/// `initialized`.
#[derive(Debug, Clone)]
pub enum SimulatedNotification {
    Initialized,
    Client(Notification),
}

impl From<Notification> for SimulatedNotification {
    fn from(notification: Notification) -> Self {
        SimulatedNotification::Client(notification)
    }
}

/// The configuration minus its source directory, which names the checkout.
struct ConfigView<'a>(&'a crate::config::Config);

impl std::fmt::Debug for ConfigView<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:?} {:?} {:?} {:?}",
            self.0.format, self.0.lint, self.0.check, self.0.unknown_keys
        )
    }
}

fn json<T: serde::Serialize + 'static>(
    future: BoxFuture<'static, Result<T, ResponseError>>,
) -> BoxFuture<'static, Response> {
    Box::pin(async move { future.await.map(|value| to_json(&value)) })
}

fn to_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

fn initialize_params(root: &Path, options: ClientOptions) -> lsp_types::InitializeParams {
    let root_uri = Url::from_file_path(root).expect("the simulated workspace path is absolute");
    let mut capabilities = lsp_types::ClientCapabilities::default();
    if options.utf8 {
        capabilities.general = Some(lsp_types::GeneralClientCapabilities {
            position_encodings: Some(vec![lsp_types::PositionEncodingKind::UTF8]),
            ..Default::default()
        });
    }
    let mut text_document = lsp_types::TextDocumentClientCapabilities::default();
    if options.pull_diagnostics {
        text_document.diagnostic = Some(lsp_types::DiagnosticClientCapabilities::default());
        capabilities.workspace = Some(lsp_types::WorkspaceClientCapabilities {
            diagnostic: Some(lsp_types::DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..Default::default()
        });
    }
    capabilities.text_document = Some(text_document);
    lsp_types::InitializeParams {
        capabilities,
        workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
            uri: root_uri,
            name: "simulated".to_owned(),
        }]),
        ..Default::default()
    }
}

/// What a session did, for the test's summary line.
#[derive(Debug, Default, Clone)]
pub struct SessionReport {
    pub actions: usize,
    pub steps: usize,
    pub requests: usize,
    pub interrupted_steps: usize,
    pub checkpoints: usize,
    /// Every outgoing message and every response, rendered; two runs of one seed must produce
    /// the same transcript.
    pub transcript: Vec<String>,
}

/// A session failure: the invariant that broke and the actions that led there.
#[derive(Debug)]
pub struct SessionFailure {
    pub message: String,
    pub log: Vec<String>,
}

impl std::fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "{}", self.message)?;
        writeln!(formatter, "--- the session's last actions ---")?;
        let start = self.log.len().saturating_sub(60);
        for line in &self.log[start..] {
            writeln!(formatter, "{line}")?;
        }
        Ok(())
    }
}

/// Runs one seeded editor session of `actions` actions in `root`, which must be an empty or
/// absent directory the session owns, and checks it against the from-scratch oracle at random
/// checkpoints and at the end.
pub fn run_session(
    entropy: &mut impl Entropy,
    root: &Path,
    actions: usize,
) -> Result<SessionReport, SessionFailure> {
    let options = ClientOptions {
        pull_diagnostics: entropy.chance(1, 2),
        utf8: entropy.chance(1, 3),
    };
    let mut session = Session::new(root, options);
    session.log(format!("start {options:?}"));
    for _ in 0..actions {
        session.report.actions += 1;
        session.act(entropy)?;
    }
    session.checkpoint(entropy)?;
    Ok(session.report)
}

struct Session {
    root: PathBuf,
    options: ClientOptions,
    simulation: Simulation,
    /// The client's view of the disk, relative path to content. Only the session writes the
    /// workspace, so this mirrors it exactly.
    disk: BTreeMap<String, String>,
    /// Open buffers by URI: version and text.
    open: BTreeMap<String, (i32, String)>,
    /// Watcher events not yet delivered.
    pending_events: Vec<lsp_types::FileEvent>,
    /// Requests whose responses the session still expects.
    outstanding: Vec<(RequestId, RequestKind, String)>,
    /// Open documents whose last push must match a fresh pull at quiescence.
    current_pushes: BTreeSet<String>,
    /// Open documents synced since the last other input change: only for them does the
    /// first wave describe the same inputs as the settled wave, so only for them must the
    /// settled wave contain it. Unlike `current_pushes`, a save does not restore membership.
    unchanged_since_sync: BTreeSet<String>,
    /// The most recent publishes per URI, read at checkpoints.
    publishes: BTreeMap<String, Publishes>,
    /// The last version each URI was synced at, kept across a close. A reopened document
    /// continues from it, as VS Code's does, because a version that restarted at zero would
    /// let a push owed to the closed buffer pass as one about the reopened text.
    last_versions: BTreeMap<String, i32>,
    next_untitled: usize,
    report: SessionReport,
    log: Vec<String>,
}

impl Session {
    fn new(root: &Path, options: ClientOptions) -> Session {
        if root.exists() {
            fs::remove_dir_all(root).expect("clearing the simulated workspace must succeed");
        }
        let mut disk = BTreeMap::new();
        for (path, text) in INITIAL_WORKSPACE {
            disk.insert((*path).to_owned(), (*text).to_owned());
        }
        for (path, text) in &disk {
            write_file(root, path, text);
        }
        let simulation = Simulation::start(root, options);
        Session {
            root: root.to_owned(),
            options,
            simulation,
            disk,
            open: BTreeMap::new(),
            pending_events: Vec::new(),
            outstanding: Vec::new(),
            current_pushes: BTreeSet::new(),
            unchanged_since_sync: BTreeSet::new(),
            publishes: BTreeMap::new(),
            last_versions: BTreeMap::new(),
            next_untitled: 1,
            report: SessionReport::default(),
            log: Vec::new(),
        }
    }

    fn log(&mut self, line: String) {
        self.log.push(line);
    }

    fn fail(&self, message: String) -> SessionFailure {
        SessionFailure {
            message,
            log: self.log.clone(),
        }
    }

    fn act(&mut self, entropy: &mut impl Entropy) -> Result<(), SessionFailure> {
        match entropy.below(100) {
            0..=29 => self.edit(entropy),
            30..=34 => self.type_burst(entropy),
            35..=41 => self.open_document(entropy),
            42..=45 => self.close_document(entropy),
            46..=50 => self.save(entropy),
            51..=58 => self.change_disk(entropy),
            59..=63 => self.deliver_events(entropy),
            64..=75 => self.send_request(entropy),
            76..=86 => {
                let count = entropy.below(6);
                self.run_steps(count);
            }
            87..=91 => self.interrupted_step(entropy),
            92..=94 => self.protocol_violation(entropy),
            95..=96 => self.abandon_request(entropy),
            _ => self.checkpoint(entropy)?,
        }
        self.collect_responses()
    }

    fn uri(&self, relative: &str) -> Url {
        Url::from_file_path(self.root.join(relative)).expect("workspace paths are absolute")
    }

    fn notify(&mut self, notification: Notification) {
        self.log(describe_notification(&notification));
        self.simulation.notify(notification);
    }

    fn run_steps(&mut self, count: usize) {
        for _ in 0..count {
            if !self.simulation.step() {
                break;
            }
            self.report.steps += 1;
        }
        self.log(format!("ran up to {count} worker steps"));
    }

    fn pick_open(&self, entropy: &mut impl Entropy) -> Option<String> {
        if self.open.is_empty() {
            return None;
        }
        let index = entropy.below(self.open.len());
        self.open.keys().nth(index).cloned()
    }

    fn edit(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            self.open_document(entropy);
            return;
        };
        let notification = self.random_change(entropy, &uri);
        self.mark_input_change();
        self.unchanged_since_sync.insert(uri.clone());
        self.current_pushes.insert(uri);
        self.notify(notification);
    }

    /// One `didChange` against the client's buffer, applied to the client's copy too.
    fn random_change(&mut self, entropy: &mut impl Entropy, uri: &str) -> Notification {
        let utf8 = self.options.utf8;
        let (version, text) = self.open.get_mut(uri).expect("edits target open documents");
        *version += 1;
        let mut changes = Vec::new();
        let count = 1 + entropy.below(3);
        for _ in 0..count {
            if entropy.chance(1, 12) {
                let replacement = random_document(entropy);
                *text = replacement.clone();
                changes.push(lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: replacement,
                });
                continue;
            }
            let start = char_boundary(text, entropy.below(text.len() + 1));
            let end = if entropy.chance(1, 2) {
                start
            } else {
                char_boundary(text, start + entropy.below(24))
            };
            let inserted = if start != end && entropy.chance(1, 3) {
                String::new()
            } else {
                SNIPPETS[entropy.below(SNIPPETS.len())].to_owned()
            };
            let range =
                lsp_types::Range::new(position_of(text, start, utf8), position_of(text, end, utf8));
            text.replace_range(start..end, &inserted);
            changes.push(lsp_types::TextDocumentContentChangeEvent {
                range: Some(range),
                range_length: None,
                text: inserted,
            });
        }
        Notification::DidChange(lsp_types::DidChangeTextDocumentParams {
            text_document: lsp_types::VersionedTextDocumentIdentifier::new(
                Url::parse(uri).expect("open URIs parse"),
                *version,
            ),
            content_changes: changes,
        })
    }

    /// Single-character edits with no worker step in between, the way typing arrives.
    fn type_burst(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            return;
        };
        let utf8 = self.options.utf8;
        let word = TYPED_WORDS[entropy.below(TYPED_WORDS.len())];
        let mut offset = {
            let (_, text) = &self.open[&uri];
            char_boundary(text, entropy.below(text.len() + 1))
        };
        self.mark_input_change();
        self.unchanged_since_sync.insert(uri.clone());
        self.current_pushes.insert(uri.clone());
        for character in word.chars() {
            let (version, text) = self
                .open
                .get_mut(&uri)
                .expect("the burst's document is open");
            *version += 1;
            let at = position_of(text, offset, utf8);
            text.insert(offset, character);
            offset += character.len_utf8();
            let notification = Notification::DidChange(lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier::new(
                    Url::parse(&uri).expect("open URIs parse"),
                    *version,
                ),
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: Some(lsp_types::Range::new(at, at)),
                    range_length: None,
                    text: character.to_string(),
                }],
            });
            self.notify(notification);
            if entropy.chance(1, 4) {
                self.run_steps(1);
            }
        }
    }

    fn open_document(&mut self, entropy: &mut impl Entropy) {
        let (uri, text) = if entropy.chance(1, 6) {
            let uri = format!("untitled:Untitled-{}", self.next_untitled);
            self.next_untitled += 1;
            (uri, random_document(entropy))
        } else {
            let candidates: Vec<&String> = self
                .disk
                .keys()
                .filter(|path| is_text_document(path))
                .filter(|path| !self.open.contains_key(self.uri(path).as_str()))
                .collect();
            if candidates.is_empty() {
                return;
            }
            let path = candidates[entropy.below(candidates.len())].clone();
            let text = self.disk[&path].clone();
            (self.uri(&path).to_string(), text)
        };
        let version = self.last_versions.get(&uri).map_or(0, |last| last + 1);
        self.open.insert(uri.clone(), (version, text.clone()));
        self.mark_input_change();
        self.unchanged_since_sync.insert(uri.clone());
        self.current_pushes.insert(uri.clone());
        let language = if uri.ends_with("NAMESPACE") {
            "namespace"
        } else {
            "r"
        };
        self.notify(Notification::DidOpen(
            lsp_types::DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem::new(
                    Url::parse(&uri).expect("open URIs parse"),
                    language.to_owned(),
                    version,
                    text,
                ),
            },
        ));
    }

    fn close_document(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            return;
        };
        if let Some((version, _)) = self.open.remove(&uri) {
            self.last_versions.insert(uri.clone(), version);
        }
        self.mark_input_change();
        self.notify(Notification::DidClose(
            lsp_types::DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier::new(
                    Url::parse(&uri).expect("open URIs parse"),
                ),
            },
        ));
    }

    /// Writes the buffer to disk, then sends `didSave`; the watcher reports the write later.
    fn save(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            return;
        };
        let parsed = Url::parse(&uri).expect("open URIs parse");
        if parsed.scheme() == "file" {
            let relative = self.relative(&parsed);
            let text = self.open[&uri].1.clone();
            write_file(&self.root, &relative, &text);
            self.disk.insert(relative, text);
            self.pending_events.push(lsp_types::FileEvent::new(
                parsed.clone(),
                lsp_types::FileChangeType::CHANGED,
            ));
        }
        self.mark_input_change();
        // A save refreshes every open document's settled publish.
        self.current_pushes = self.open.keys().cloned().collect();
        self.notify(Notification::DidSave(
            lsp_types::DidSaveTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier::new(parsed),
                text: None,
            },
        ));
    }

    fn relative(&self, uri: &Url) -> String {
        let path = uri.to_file_path().expect("file URIs convert");
        path.strip_prefix(&self.root)
            .expect("documents live in the workspace")
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Another program changes the workspace; the watcher event is queued, not delivered.
    fn change_disk(&mut self, entropy: &mut impl Entropy) {
        let (relative, content, kind) = match entropy.below(8) {
            0 => {
                let name = format!("R/new_{}.R", entropy.below(4));
                let kind = if self.disk.contains_key(&name) {
                    lsp_types::FileChangeType::CHANGED
                } else {
                    lsp_types::FileChangeType::CREATED
                };
                (name, Some(random_document(entropy)), kind)
            }
            1 | 2 => {
                let candidates: Vec<String> = self
                    .disk
                    .keys()
                    .filter(|path| path.starts_with("R/"))
                    .cloned()
                    .collect();
                if candidates.is_empty() {
                    return;
                }
                let path = candidates[entropy.below(candidates.len())].clone();
                if entropy.chance(1, 3) {
                    (path, None, lsp_types::FileChangeType::DELETED)
                } else {
                    (
                        path,
                        Some(random_document(entropy)),
                        lsp_types::FileChangeType::CHANGED,
                    )
                }
            }
            3 => (
                "DESCRIPTION".to_owned(),
                Some(DESCRIPTIONS[entropy.below(DESCRIPTIONS.len())].to_owned()),
                lsp_types::FileChangeType::CHANGED,
            ),
            4 => (
                "NAMESPACE".to_owned(),
                Some(NAMESPACES[entropy.below(NAMESPACES.len())].to_owned()),
                lsp_types::FileChangeType::CHANGED,
            ),
            5 => (
                "data.csv".to_owned(),
                Some(DATA_FILES[entropy.below(DATA_FILES.len())].to_owned()),
                lsp_types::FileChangeType::CHANGED,
            ),
            6 => {
                if entropy.chance(1, 4) {
                    (
                        "ry.toml".to_owned(),
                        None,
                        lsp_types::FileChangeType::DELETED,
                    )
                } else {
                    (
                        "ry.toml".to_owned(),
                        Some(CONFIGS[entropy.below(CONFIGS.len())].to_owned()),
                        lsp_types::FileChangeType::CHANGED,
                    )
                }
            }
            _ => (
                "tests/testthat/test-a.R".to_owned(),
                Some(random_document(entropy)),
                lsp_types::FileChangeType::CHANGED,
            ),
        };
        let path = self.root.join(&relative);
        match &content {
            Some(text) => {
                write_file(&self.root, &relative, text);
                self.disk.insert(relative.clone(), text.clone());
            }
            None => {
                if path.exists() {
                    fs::remove_file(&path).expect("deleting a simulated file must succeed");
                }
                self.disk.remove(&relative);
            }
        }
        self.log(format!("disk {kind:?} {relative}"));
        let uri = self.uri(&relative);
        self.pending_events
            .push(lsp_types::FileEvent::new(uri.clone(), kind));
        if entropy.chance(1, 8) {
            // Watchers coalesce and duplicate; a stale event type must not matter either.
            self.pending_events.push(lsp_types::FileEvent::new(
                uri,
                lsp_types::FileChangeType::CHANGED,
            ));
        }
    }

    /// Delivers some queued watcher events, in a shuffled order and possibly split across
    /// several notifications.
    fn deliver_events(&mut self, entropy: &mut impl Entropy) {
        if self.pending_events.is_empty() {
            return;
        }
        let count = 1 + entropy.below(self.pending_events.len());
        let mut batch = Vec::new();
        for _ in 0..count {
            let index = entropy.below(self.pending_events.len());
            batch.push(self.pending_events.remove(index));
        }
        self.mark_input_change();
        self.notify(Notification::DidChangeWatchedFiles(
            lsp_types::DidChangeWatchedFilesParams { changes: batch },
        ));
    }

    fn flush_events(&mut self) {
        if self.pending_events.is_empty() {
            return;
        }
        let changes = mem::take(&mut self.pending_events);
        self.mark_input_change();
        self.notify(Notification::DidChangeWatchedFiles(
            lsp_types::DidChangeWatchedFilesParams { changes },
        ));
    }

    fn send_request(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            return;
        };
        let kind = REQUEST_KINDS[entropy.below(REQUEST_KINDS.len())];
        let position = self.random_position(entropy, &uri);
        let parsed = Url::parse(&uri).expect("open URIs parse");
        let id = self.simulation.request(kind, &parsed, position);
        self.report.requests += 1;
        self.log(format!("request {kind:?} {uri} {position:?} -> {id:?}"));
        self.outstanding.push((id, kind, uri));
    }

    /// Usually a valid position; sometimes one past the end of a line or of the document.
    fn random_position(&self, entropy: &mut impl Entropy, uri: &str) -> Position {
        let text = &self.open[uri].1;
        match entropy.below(10) {
            0 => Position::new(entropy.below(400) as u32, entropy.below(400) as u32),
            1 => {
                let end = position_of(text, text.len(), self.options.utf8);
                Position::new(end.line, end.character + 1 + entropy.below(5) as u32)
            }
            _ => position_of(
                text,
                char_boundary(text, entropy.below(text.len() + 1)),
                self.options.utf8,
            ),
        }
    }

    fn abandon_request(&mut self, entropy: &mut impl Entropy) {
        if self.outstanding.is_empty() {
            return;
        }
        let (id, kind, uri) = self
            .outstanding
            .remove(entropy.below(self.outstanding.len()));
        self.log(format!("abandon {id:?} {kind:?} {uri}"));
        self.simulation.abandon(id);
    }

    fn interrupted_step(&mut self, entropy: &mut impl Entropy) {
        let Some(uri) = self.pick_open(entropy) else {
            return;
        };
        let checkpoint = entropy.below(4);
        let notification = self.random_change(entropy, &uri);
        self.mark_input_change();
        self.unchanged_since_sync.insert(uri.clone());
        self.current_pushes.insert(uri);
        self.log(format!(
            "interrupt at checkpoint {checkpoint} with {}",
            describe_notification(&notification)
        ));
        self.simulation.step_interrupted(checkpoint, notification);
        self.report.interrupted_steps += 1;
    }

    /// Messages a well-behaved client never sends; the server must survive them.
    fn protocol_violation(&mut self, entropy: &mut impl Entropy) {
        let ghost = self.uri("R/ghost.R");
        let notification = match entropy.below(3) {
            0 => Notification::DidChange(lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier::new(ghost, 7),
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "x <- 1\n".to_owned(),
                }],
            }),
            1 => Notification::DidClose(lsp_types::DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier::new(ghost),
            }),
            _ => Notification::DidSave(lsp_types::DidSaveTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier::new(ghost),
                text: None,
            }),
        };
        if matches!(notification, Notification::DidSave(_)) {
            // A save of anything refreshes every open document's publish.
            self.current_pushes = self.open.keys().cloned().collect();
        }
        self.notify(notification);
        let kind = REQUEST_KINDS[entropy.below(REQUEST_KINDS.len())];
        let id = self
            .simulation
            .request(kind, &self.uri("R/ghost.R"), Position::new(3, 9));
        self.outstanding.push((id, kind, "ghost".to_owned()));
    }

    /// An input changed somewhere, so pushes for other documents may now be stale; the push
    /// contract only promises the documents synced since.
    fn mark_input_change(&mut self) {
        self.current_pushes.clear();
        self.unchanged_since_sync.clear();
    }

    fn collect_responses(&mut self) -> Result<(), SessionFailure> {
        let mut index = 0;
        while index < self.outstanding.len() {
            let (id, kind, _) = &self.outstanding[index];
            let (id, kind) = (*id, *kind);
            match self.simulation.take_response(id) {
                Some(response) => {
                    let (_, _, uri) = self.outstanding.remove(index);
                    self.check_response(kind, &uri, &response)?;
                    self.report
                        .transcript
                        .push(format!("{id:?} {kind:?} {}", render_response(&response)));
                }
                None => index += 1,
            }
        }
        Ok(())
    }

    /// The protocol contract every response honors whenever it arrives: only a pull may fail,
    /// and only with the retryable cancellation, and a rename may refuse a modified document.
    fn check_response(
        &self,
        kind: RequestKind,
        uri: &str,
        response: &Response,
    ) -> Result<(), SessionFailure> {
        if let Err(error) = response {
            let allowed = match kind {
                RequestKind::Diagnostic => error.code == async_lsp::ErrorCode::SERVER_CANCELLED,
                RequestKind::Rename => {
                    error.code == async_lsp::ErrorCode::CONTENT_MODIFIED
                        || error.code == async_lsp::ErrorCode::REQUEST_FAILED
                        || error.code == async_lsp::ErrorCode::INVALID_PARAMS
                }
                _ => false,
            };
            if !allowed {
                return Err(self.fail(format!("{kind:?} on {uri} failed: {error:?}")));
            }
        }
        Ok(())
    }

    /// Lets everything settle, then holds the session against a server started from scratch
    /// on the same disk and buffers.
    fn checkpoint(&mut self, entropy: &mut impl Entropy) -> Result<(), SessionFailure> {
        self.report.checkpoints += 1;
        self.log("checkpoint".to_owned());
        self.flush_events();
        self.simulation.settle();
        self.collect_responses()?;
        if let Some((id, kind, uri)) = self
            .outstanding
            .iter()
            .find(|(id, _, _)| self.simulation.is_pending(*id))
        {
            return Err(self.fail(format!(
                "{kind:?} {id:?} on {uri} never got a response although the worker is idle"
            )));
        }
        self.outstanding.clear();
        for message in self.simulation.drain_outgoing() {
            self.report.transcript.push(render_outgoing(&message));
            if let Outgoing::PublishDiagnostics(params) = &message {
                self.check_publish_geometry(params)?;
            }
            self.last_publishes(message);
        }

        // Synchronization: the server's text for every analyzed open document is the client's.
        let open: Vec<(String, (i32, String))> = self
            .open
            .iter()
            .map(|(uri, buffer)| (uri.clone(), buffer.clone()))
            .collect();
        for (uri, (_, text)) in &open {
            let parsed = Url::parse(uri).expect("open URIs parse");
            if let Some(server_text) = self.simulation.document_text(&parsed)
                && &server_text != text
            {
                return Err(self.fail(format!(
                    "the server's text for {uri} diverged from the client's\nclient:\n{text}\nserver:\n{server_text}"
                )));
            }
        }

        let open_buffers: BTreeMap<String, (i32, String)> = open.iter().cloned().collect();
        let mut fresh = verify_against_fresh_start(
            &mut self.simulation,
            &self.root,
            self.options,
            &open_buffers,
        )
        .map_err(|problem| self.fail(problem))?;

        for (uri, (_, text)) in &open {
            let parsed = Url::parse(uri).expect("open URIs parse");
            let mut kinds = vec![RequestKind::Diagnostic];
            for _ in 0..3 {
                kinds.push(REQUEST_KINDS[entropy.below(REQUEST_KINDS.len())]);
            }
            for kind in kinds {
                let position = position_of(
                    text,
                    char_boundary(text, entropy.below(text.len() + 1)),
                    self.options.utf8,
                );
                let ours = ask(&mut self.simulation, kind, &parsed, position);
                let theirs = ask(&mut fresh, kind, &parsed, position);
                let (ours, theirs) = match (ours, theirs) {
                    (Some(ours), Some(theirs)) => (ours, theirs),
                    _ => {
                        return Err(self.fail(format!(
                            "{kind:?} on {uri} got no response from an idle server"
                        )));
                    }
                };
                self.check_response(kind, uri, &ours)?;
                if render_response(&ours) != render_response(&theirs) {
                    return Err(self.fail(format!(
                        "{kind:?} at {position:?} in {uri} differs from a fresh start\n{}",
                        line_diff(&pretty(&theirs), &pretty(&ours))
                    )));
                }
                if let Ok(value) = &ours {
                    check_ranges(value, text, self.options.utf8).map_err(|problem| {
                        self.fail(format!(
                            "{kind:?} at {position:?} in {uri}: {problem}\ntext:\n{text}\nresponse:\n{}",
                            pretty_value(value)
                        ))
                    })?;
                }
                if kind == RequestKind::Diagnostic && !self.options.pull_diagnostics {
                    self.check_push_contract(uri, &ours)?;
                }
            }
        }
        // The probes above are reads, but a read may still owe nothing to the transcript.
        self.simulation.drain_outgoing();
        Ok(())
    }

    fn last_publishes(&mut self, message: Outgoing) {
        if let Outgoing::PublishDiagnostics(params) = message {
            let key = params.uri.to_string();
            let entry = self.publishes.entry(key).or_default();
            if params.version.is_some() {
                entry.first_wave = Some(params.clone());
            }
            entry.last = Some(params);
        }
    }

    /// The two-wave push contract at quiescence: the last publish for an analyzed open
    /// document is the settled wave, it contains the first wave for the same text, and when no
    /// other input changed since this document's sync it equals the settled diagnostics.
    fn check_push_contract(&self, uri: &str, pull: &Response) -> Result<(), SessionFailure> {
        let parsed = Url::parse(uri).expect("open URIs parse");
        if uri.ends_with("NAMESPACE") || parsed.path().ends_with(".Rtypes") {
            return Ok(());
        }
        let Some(publishes) = self.publishes.get(uri) else {
            return Err(self.fail(format!("no diagnostics were ever pushed for open {uri}")));
        };
        let Some(last) = &publishes.last else {
            return Ok(());
        };
        if last.version.is_some() {
            return Err(self.fail(format!(
                "the last push for {uri} is a first wave (version {:?}); the settled wave never followed",
                last.version
            )));
        }
        let version = self.open[uri].0;
        if let Some(first) = &publishes.first_wave
            && first.version == Some(version)
            && self.unchanged_since_sync.contains(uri)
        {
            let settled: Vec<String> = last.diagnostics.iter().map(render_diagnostic).collect();
            for diagnostic in &first.diagnostics {
                let rendered = render_diagnostic(diagnostic);
                if !settled.contains(&rendered) {
                    return Err(self.fail(format!(
                        "the settled push for {uri} drops a first-wave finding: {rendered}"
                    )));
                }
            }
        }
        if self.current_pushes.contains(uri)
            && let Ok(value) = pull
        {
            let pulled = value
                .get("items")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let pushed = to_json(&last.diagnostics);
            if pulled != pushed {
                return Err(self.fail(format!(
                    "the settled push for {uri} differs from the settled diagnostics\n{}",
                    line_diff(&pretty_value(&pulled), &pretty_value(&pushed))
                )));
            }
        }
        Ok(())
    }

    fn check_publish_geometry(
        &self,
        params: &lsp_types::PublishDiagnosticsParams,
    ) -> Result<(), SessionFailure> {
        // Only a versioned publish says which text it describes.
        let Some(version) = params.version else {
            return Ok(());
        };
        let Some((current, text)) = self.open.get(params.uri.as_str()) else {
            return Ok(());
        };
        if *current != version {
            return Ok(());
        }
        check_ranges(&to_json(&params.diagnostics), text, self.options.utf8)
            .map_err(|problem| self.fail(format!("push for {}: {problem}", params.uri)))
    }
}

/// Starts a server from scratch on the same disk with the same open buffers, and checks that
/// `simulation` agrees with it on every analysis input and on the diagnostics of every open
/// document. Returns the fresh server so callers can probe both further.
pub fn verify_against_fresh_start(
    simulation: &mut Simulation,
    root: &Path,
    options: ClientOptions,
    open: &BTreeMap<String, (i32, String)>,
) -> Result<Simulation, String> {
    simulation.settle();
    let mut fresh = Simulation::start(root, options);
    for (uri, (version, text)) in open {
        let parsed = Url::parse(uri).map_err(|error| format!("{uri} is not a URI: {error}"))?;
        let language = if uri.ends_with("NAMESPACE") {
            "namespace"
        } else {
            "r"
        };
        fresh.notify(Notification::DidOpen(
            lsp_types::DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem::new(
                    parsed,
                    language.to_owned(),
                    *version,
                    text.clone(),
                ),
            },
        ));
    }
    fresh.settle();

    let incremental = simulation.input_state();
    let scratch = fresh.input_state();
    if incremental != scratch {
        return Err(format!(
            "incremental input state differs from a fresh start\n{}",
            line_diff(&scratch, &incremental)
        ));
    }
    for uri in open.keys() {
        let parsed = Url::parse(uri).map_err(|error| format!("{uri} is not a URI: {error}"))?;
        let start = Position::new(0, 0);
        let ours = ask(simulation, RequestKind::Diagnostic, &parsed, start);
        let theirs = ask(&mut fresh, RequestKind::Diagnostic, &parsed, start);
        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            return Err(format!(
                "diagnostics for {uri} got no response from an idle server"
            ));
        };
        if render_response(&ours) != render_response(&theirs) {
            return Err(format!(
                "diagnostics for {uri} differ from a fresh start\n{}",
                line_diff(&pretty(&theirs), &pretty(&ours))
            ));
        }
    }
    simulation.drain_outgoing();
    fresh.drain_outgoing();
    Ok(fresh)
}

/// Sends one request to an idle server and returns its response.
fn ask(
    simulation: &mut Simulation,
    kind: RequestKind,
    uri: &Url,
    position: Position,
) -> Option<Response> {
    let id = simulation.request(kind, uri, position);
    simulation.settle();
    simulation.take_response(id)
}

#[derive(Default)]
struct Publishes {
    first_wave: Option<lsp_types::PublishDiagnosticsParams>,
    last: Option<lsp_types::PublishDiagnosticsParams>,
}

fn write_file(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("creating a simulated directory must succeed");
    }
    fs::write(&path, text).expect("writing a simulated file must succeed");
}

/// Files an editor would open as text documents.
fn is_text_document(path: &str) -> bool {
    path.ends_with(".R") || path == "NAMESPACE"
}

/// The largest char boundary at or before `offset`, clamped to the text.
fn char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The protocol position of a byte offset, in UTF-8 bytes or UTF-16 code units.
pub fn position_of(text: &str, offset: usize, utf8: bool) -> Position {
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = &before[line_start..];
    let character = if utf8 {
        column.len()
    } else {
        column.encode_utf16().count()
    };
    Position::new(line as u32, character as u32)
}

fn random_document(entropy: &mut impl Entropy) -> String {
    let mut text = String::new();
    for _ in 0..entropy.below(7) {
        text.push_str(DOCUMENT_LINES[entropy.below(DOCUMENT_LINES.len())]);
        text.push('\n');
    }
    text
}

fn describe_notification(notification: &Notification) -> String {
    match notification {
        Notification::DidOpen(params) => format!(
            "didOpen {} v{} {:?}",
            params.text_document.uri, params.text_document.version, params.text_document.text
        ),
        Notification::DidChange(params) => format!(
            "didChange {} v{} {:?}",
            params.text_document.uri,
            params.text_document.version,
            params
                .content_changes
                .iter()
                .map(|change| (change.range, change.text.as_str()))
                .collect::<Vec<_>>()
        ),
        Notification::DidSave(params) => format!("didSave {}", params.text_document.uri),
        Notification::DidClose(params) => format!("didClose {}", params.text_document.uri),
        Notification::DidChangeWatchedFiles(params) => format!(
            "didChangeWatchedFiles {:?}",
            params
                .changes
                .iter()
                .map(|change| (change.uri.as_str(), change.typ))
                .collect::<Vec<_>>()
        ),
    }
}

fn render_response(response: &Response) -> String {
    match response {
        Ok(value) => value.to_string(),
        Err(error) => format!("error {:?}: {}", error.code, error.message),
    }
}

fn pretty(response: &Response) -> String {
    match response {
        Ok(value) => pretty_value(value),
        Err(error) => format!("error {:?}: {}\n", error.code, error.message),
    }
}

fn pretty_value(value: &serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON values serialize");
    text.push('\n');
    text
}

fn render_outgoing(message: &Outgoing) -> String {
    match message {
        Outgoing::PublishDiagnostics(params) => format!(
            "publish {} {:?} {}",
            params.uri,
            params.version,
            to_json(&params.diagnostics)
        ),
        Outgoing::ShowMessage(params) => format!("show {:?} {}", params.typ, params.message),
        Outgoing::RegisterCapability(params) => format!("register {}", to_json(params)),
        Outgoing::DiagnosticRefresh => "refresh".to_owned(),
    }
}

fn render_diagnostic(diagnostic: &lsp_types::Diagnostic) -> String {
    to_json(diagnostic).to_string()
}

fn line_diff(expected: &str, actual: &str) -> String {
    similar::TextDiff::from_lines(expected, actual)
        .unified_diff()
        .header("fresh start", "incremental session")
        .to_string()
}

/// Every range in `value` that refers to `text` lies inside it, with its start before its
/// end. A subtree that names another document (a `uri` or a `targetUri`) is not checked.
fn check_ranges(value: &serde_json::Value, text: &str, utf8: bool) -> Result<(), String> {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                check_ranges(item, text, utf8)?;
            }
            Ok(())
        }
        serde_json::Value::Object(fields) => {
            if fields.contains_key("uri") || fields.contains_key("targetUri") {
                return Ok(());
            }
            if let (Some(start), Some(end)) = (fields.get("start"), fields.get("end"))
                && let (Some(start), Some(end)) = (as_position(start), as_position(end))
            {
                check_position(start, text, utf8)?;
                check_position(end, text, utf8)?;
                if (start.line, start.character) > (end.line, end.character) {
                    return Err(format!("range {start:?}..{end:?} ends before it starts"));
                }
                return Ok(());
            }
            for (key, field) in fields {
                // A workspace edit keys its edits by document.
                if key == "changes" || key == "documentChanges" {
                    continue;
                }
                check_ranges(field, text, utf8)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn as_position(value: &serde_json::Value) -> Option<Position> {
    serde_json::from_value(value.clone()).ok()
}

fn check_position(position: Position, text: &str, utf8: bool) -> Result<(), String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(line) = lines.get(position.line as usize) else {
        return Err(format!(
            "position {position:?} is past the last line ({} lines)",
            lines.len()
        ));
    };
    let width = if utf8 {
        line.len()
    } else {
        line.encode_utf16().count()
    };
    if position.character as usize > width {
        return Err(format!(
            "position {position:?} is past the end of its line ({width} units)"
        ));
    }
    Ok(())
}

const DESCRIPTION: &str = "Package: simulated\nVersion: 0.1.0\nImports: stats, utils\n";

const INITIAL_WORKSPACE: &[(&str, &str)] = &[
    ("DESCRIPTION", DESCRIPTION),
    (
        "NAMESPACE",
        "export(area)\nexport(greet)\nimportFrom(stats, median)\n",
    ),
    (
        "R/shapes.R",
        "#: fn(width: double, height: double) -> double\narea <- function(width, height) {\n  width * height\n}\n\nsquare <- function(side) area(side, side)\n",
    ),
    (
        "R/greet.R",
        "greet <- function(name: character) -> character {\n  paste0(\"héllo, \", name, \" 👋\")\n}\n\ncentre <- function(values) median(values)\n",
    ),
    (
        "R/records.R",
        "total <- function(items) {\n  sum <- 0\n  for (item in items) {\n    sum <- sum + item\n  }\n  sum\n}\n",
    ),
    (
        "tests/testthat/test-a.R",
        "test_that(\"area multiplies\", {\n  expect_equal(area(2, 3), 6)\n})\n",
    ),
    (
        "scripts/analysis.R",
        "library(stats)\nmeasurements <- read.csv(\"data.csv\")\nsummary(measurements$height)\narea(1, \"wide\")\n",
    ),
    ("data.csv", "height,weight\n1.5,60\n1.8,80\n"),
    ("ry.toml", "[check]\n"),
];

/// Whole lines a random document is built from: definitions, calls across files, both
/// annotation spellings, attaching packages, non-ASCII text, and some broken syntax.
const DOCUMENT_LINES: &[&str] = &[
    "x <- 1",
    "y <- x + 1L",
    "area(2, 3)",
    "area(\"a\", 3)",
    "greet(\"wörld\")",
    "greet(42)",
    "square(4)",
    "total(list(1, 2, 3))",
    "#: fn(n: integer) -> integer",
    "double_it <- function(n) n * 2L",
    "halve <- function(value: double) -> double value / 2",
    "label: character <- \"日本語\"",
    "library(utils)",
    "library(stats)",
    "measurements <- read.csv(\"data.csv\")",
    "measurements$height",
    "f <- function(a, b = a) {",
    "  a + b",
    "}",
    "if (TRUE) area(1, 2) else NULL",
    "for (i in 1:3) print(i)",
    "z <- c(1, 2, 3) |> rev()",
    "s <- \"emoji 🎉 \\u00e9\"",
    "undefined_function(1)",
    "g <- function(",
    "))",
    "#: (",
    "`odd name` <- 2",
    "# ry: allow(unused)",
    "# typing: strict",
    "",
];

/// Text spliced into a document by an edit.
const SNIPPETS: &[&str] = &[
    "x",
    " + 1",
    "(",
    ")",
    "{",
    "}\n",
    "\n",
    "\"",
    "area(",
    "greet(\"é\")",
    "#: double\n",
    ": integer",
    " -> character",
    "function(a) a",
    "library(stats)\n",
    "measurements$weight",
    "👋",
    "日本",
    "\t",
    "# comment\n",
    "total <- NULL\n",
    "|>",
    "<-",
    ",",
    "`",
];

const TYPED_WORDS: &[&str] = &[
    "area(1, 2)",
    "gre",
    "x <- ",
    "medi",
    "é👋",
    "function(",
    "\n\n",
];

const DESCRIPTIONS: &[&str] = &[
    DESCRIPTION,
    "Package: simulated\nVersion: 0.1.0\nImports: stats\nCollate: 'shapes.R' 'greet.R' 'records.R'\n",
    "Package: renamed\nVersion: 0.2.0\nDepends: utils\n",
    "not a description",
];

const NAMESPACES: &[&str] = &[
    "export(area)\nexport(greet)\nimportFrom(stats, median)\n",
    "exportPattern(\"^[a-z]\")\nimport(stats)\n",
    "importFrom(missingpkg, thing)\n",
    "export(\n",
];

const DATA_FILES: &[&str] = &[
    "height,weight\n1.5,60\n1.8,80\n",
    "height,weight,age\n1.5,60,30\n",
    "a;b\n1;2\n",
    "",
];

const CONFIGS: &[&str] = &[
    "[check]\n",
    "[check]\ninline-annotations = false\n",
    "[lint]\nassignment-operator = \"error\"\nboolean-shorthand = \"off\"\n",
    "[format]\nindent-width = 4\n",
    "[check]\nunused = false\n",
];
