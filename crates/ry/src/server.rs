//! The language server: an async-lsp frontend on the tokio thread and one
//! dedicated worker thread owning the salsa database and every document.
//! Edits cancel in-flight analysis cooperatively (salsa's cancellation
//! token), so the latest edit always wins; a panic on the worker is a
//! coherence failure and terminates the process deterministically.

use crate::config::{CONFIG_FILE_NAME, Config, ConfigError, ExperimentalFeatures};
use crate::diagnostics::{apply_suppressions, document_diagnostics, first_wave_diagnostics};
use crate::position::{LineColumn, LineIndex};
use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{self, request};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, LanguageClient, LanguageServer, ResponseError};
use futures::future::BoxFuture;
use semantics::diagnostics::{Diagnostic, Severity};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use syntax::{SyntaxKind, TextRange, TextSize};
use tokio::sync::oneshot;
use tower::ServiceBuilder;

pub fn run(experimental_features: ExperimentalFeatures, debug: bool) {
    run_async(experimental_features, debug);
}

#[tokio::main(flavor = "current_thread")]
async fn run_async(experimental_features: ExperimentalFeatures, debug: bool) {
    install_panic_hook();
    let runtime = tokio::runtime::Handle::current();

    let (server, _) = async_lsp::MainLoop::new_server(|client| {
        let (sender, receiver) = mpsc::channel::<Job>();
        let cancel = CancelHandle::default();
        let idle_interrupt = Arc::new(AtomicBool::new(false));

        let seed = WorkerSeed {
            client: client.clone(),
            experimental_features,
            debug,
            cancel: cancel.clone(),
            idle_interrupt: idle_interrupt.clone(),
            runtime: runtime.clone(),
        };
        std::thread::Builder::new()
            .name("ry-analysis".to_owned())
            .stack_size(crate::ANALYSIS_STACK_SIZE)
            .spawn(move || run_worker(seed, receiver))
            .expect("analysis worker thread should spawn");

        // No ConcurrencyLayer: the serial worker thread is the concurrency
        // bound, and that layer's backpressure deadlocks pending request
        // futures against the mainloop's dispatch. The edit-driven
        // cancellation supersedes `$/cancelRequest` early-abort.
        let _ = ConcurrencyLayer::default();
        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ClientProcessMonitorLayer::new(client.clone()))
            .service(Router::from_language_server(ServerState {
                sender,
                cancel,
                idle_interrupt,
            }))
    });

    #[cfg(unix)]
    let (stdin, stdout) = (
        async_lsp::stdio::PipeStdin::lock_tokio().expect("stdin"),
        async_lsp::stdio::PipeStdout::lock_tokio().expect("stdout"),
    );
    #[cfg(not(unix))]
    let (stdin, stdout) = (
        tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
    );

    server
        .run_buffered(stdin, stdout)
        .await
        .expect("language server main loop failed");
}

/// The frontend's handle to the worker's salsa cancellation token: the token
/// only exists once the worker constructs the database, so the flag arrives
/// late through the shared slot.
#[derive(Clone, Default)]
struct CancelHandle {
    token: Arc<std::sync::Mutex<Option<salsa::CancellationToken>>>,
}

impl CancelHandle {
    fn cancel(&self) {
        if let Some(token) = self.token.lock().expect("cancel token lock").as_ref() {
            token.cancel();
        }
    }

    fn install(&self, token: salsa::CancellationToken) {
        *self.token.lock().expect("cancel token lock") = Some(token);
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Cooperative cancellation is not a fault; everything else prints.
        // Process death is the worker loop's decision, never the hook's.
        if info.payload().is::<salsa::Cancelled>() {
            return;
        }
        default_hook(info);
    }));
}

enum Job {
    Initialize(
        Box<lsp_types::InitializeParams>,
        oneshot::Sender<Result<lsp_types::InitializeResult, ResponseError>>,
    ),
    Read(Box<dyn FnOnce(&mut Worker) + Send>),
    Write(Box<dyn FnOnce(&mut Worker) + Send>),
}

struct WorkerSeed {
    client: ClientSocket,
    experimental_features: ExperimentalFeatures,
    /// The developer switch (`ry server --debug`, or `RY_DEBUG=1`):
    /// surfaces internal analysis facts such as the hover debug sections.
    /// This is deliberately not a `ry.toml` key. The configuration file is
    /// user-facing contract, and this switch is an aid for people working on ry
    /// itself.
    debug: bool,
    cancel: CancelHandle,
    idle_interrupt: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
}

fn run_worker(seed: WorkerSeed, receiver: mpsc::Receiver<Job>) {
    let mut seed = Some(seed);
    let mut worker: Option<Worker> = None;
    loop {
        let job = if worker.as_ref().is_some_and(Worker::has_idle_work) {
            // Reset before the poll: a job enqueued before the poll is simply
            // received; one enqueued after flags the token after this reset,
            // so the idle unit observes it at its next cancellation check.
            seed_idle_interrupt(&worker).store(false, Ordering::SeqCst);
            match receiver.try_recv() {
                Ok(job) => Some(job),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match receiver.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            }
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            Some(Job::Initialize(params, reply)) => {
                let seed = seed.take().expect("initialize arrives exactly once");
                let (initialized, result) = Worker::initialize(seed, *params);
                worker = Some(initialized);
                let _ = reply.send(Ok(result));
            }
            Some(Job::Read(work)) => {
                if let Some(worker) = worker.as_mut() {
                    worker.refresh_cancellation();
                    work(worker);
                }
            }
            Some(Job::Write(work)) => match worker.as_mut() {
                Some(worker) => {
                    worker.refresh_cancellation();
                    work(worker);
                }
                // LSP allows dropping notifications before initialize;
                // nothing exists yet to corrupt.
                None => tracing::warn!("dropping a notification before initialize"),
            },
            None => {
                if let Some(worker) = worker.as_mut() {
                    worker.refresh_cancellation();
                    worker.run_idle_unit();
                }
            }
        }));
        if outcome.is_err() {
            // A read catches its own cancellation; anything reaching here is
            // a coherence panic. Serving a corrupted analysis state is worse
            // than dying, so terminate deterministically.
            tracing::error!("analysis worker panicked; exiting");
            std::process::exit(1);
        }
    }
}

fn seed_idle_interrupt(worker: &Option<Worker>) -> &AtomicBool {
    &worker
        .as_ref()
        .expect("idle work implies an initialized worker")
        .idle_interrupt
}

struct ServerState {
    sender: mpsc::Sender<Job>,
    cancel: CancelHandle,
    idle_interrupt: Arc<AtomicBool>,
}

impl ServerState {
    fn read<T, F>(&self, build: F) -> BoxFuture<'static, Result<T, ResponseError>>
    where
        T: Send + 'static,
        F: FnOnce(&mut Worker) -> Result<T, ResponseError> + Send + 'static,
    {
        let (reply, receive) = oneshot::channel();
        let job = Job::Read(Box::new(move |worker| {
            let _ = reply.send(build(worker));
        }));
        if self.sender.send(job).is_err() {
            return Box::pin(async { Err(worker_gone_error()) });
        }
        self.idle_interrupt.store(true, Ordering::SeqCst);
        Box::pin(async move { receive.await.unwrap_or_else(|_| Err(worker_gone_error())) })
    }

    /// Input-mutating notifications: flip the cancellation token FIRST so an
    /// in-flight read abandons, then enqueue. A send failure is unrecoverable,
    /// because the analysis state can no longer mirror the client.
    fn notify_edit<F>(&self, build: F) -> ControlFlow<async_lsp::Result<()>>
    where
        F: FnOnce(&mut Worker) + Send + 'static,
    {
        self.cancel.cancel();
        if self.sender.send(Job::Write(Box::new(build))).is_err() {
            panic!("analysis worker is gone; cannot apply a document-sync edit");
        }
        self.idle_interrupt.store(true, Ordering::SeqCst);
        ControlFlow::Continue(())
    }

    fn notify<F>(&self, build: F) -> ControlFlow<async_lsp::Result<()>>
    where
        F: FnOnce(&mut Worker) + Send + 'static,
    {
        if self.sender.send(Job::Write(Box::new(build))).is_err() {
            panic!("analysis worker is gone; cannot apply a notification");
        }
        self.idle_interrupt.store(true, Ordering::SeqCst);
        ControlFlow::Continue(())
    }
}

fn worker_gone_error() -> ResponseError {
    ResponseError::new(ErrorCode::INTERNAL_ERROR, "analysis worker unavailable")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    fn negotiate(offered: Option<&[lsp_types::PositionEncodingKind]>) -> PositionEncoding {
        if offered.is_some_and(|kinds| kinds.contains(&lsp_types::PositionEncodingKind::UTF8)) {
            PositionEncoding::Utf8
        } else {
            PositionEncoding::Utf16
        }
    }

    fn kind(self) -> lsp_types::PositionEncodingKind {
        match self {
            PositionEncoding::Utf8 => lsp_types::PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => lsp_types::PositionEncodingKind::UTF16,
        }
    }
}

struct Worker {
    client: ClientSocket,
    runtime: tokio::runtime::Handle,
    cancel: CancelHandle,
    current_token: salsa::CancellationToken,
    idle_interrupt: Arc<AtomicBool>,
    experimental_features: ExperimentalFeatures,
    /// The developer switch (see [`WorkerSeed::debug`]).
    debug: bool,
    encoding: PositionEncoding,
    supports_pull_diagnostics: bool,
    supports_diagnostic_refresh: bool,
    supports_label_offsets: bool,
    supports_snippets: bool,
    workspace_root: PathBuf,
    config: Config,
    pending_config_error: Option<String>,
    db: RootDatabase,
    /// Every tracked R document (open buffers and on-disk package files).
    files: HashMap<PathBuf, SourceFile>,
    open_documents: HashSet<PathBuf>,
    stub_documents: HashMap<PathBuf, String>,
    /// On-disk file per installed stub source, aligned with the `StubSources`
    /// order: shipped sources point at their materialized cache copies,
    /// project overrides at their real `stubs/*.Rtypes` files. `None` when
    /// materialization failed. A location then degrades, and a feature never
    /// fails.
    stub_source_paths: Vec<Option<PathBuf>>,
    namespace_documents: HashMap<PathBuf, String>,
    /// The DESCRIPTION `Collate` file names, in declared order.
    collate_order: Vec<String>,
    virtual_document_uris: HashMap<PathBuf, lsp_types::Url>,
    /// Documents owed a settled (full) diagnostics publish, most recent last.
    pending_semantic_publishes: Vec<PathBuf>,
    /// Workspace files to warm at idle time; results are never published.
    prime_queue: VecDeque<PathBuf>,
    /// Per-file `library()`-family attach facts, non-empty sets only. Kept
    /// current per synced or primed file, and never by a whole-project sweep,
    /// so keystroke work stays proportional to the open documents. The idle
    /// prime fills in the rest of the workspace shortly after startup.
    attached_by_file: HashMap<PathBuf, std::collections::BTreeSet<String>>,
}

/// The parsed NAMESPACE/DESCRIPTION facts a workspace currently declares.
struct WorkspaceMetadata {
    imports: Vec<(String, Option<String>)>,
    dependencies: std::collections::BTreeSet<String>,
    collate: Vec<String>,
    package: Option<String>,
}

impl Worker {
    fn initialize(
        seed: WorkerSeed,
        params: lsp_types::InitializeParams,
    ) -> (Worker, lsp_types::InitializeResult) {
        let encoding = PositionEncoding::negotiate(
            params
                .capabilities
                .general
                .as_ref()
                .and_then(|general| general.position_encodings.as_deref()),
        );
        let supports_pull_diagnostics = params
            .capabilities
            .text_document
            .as_ref()
            .is_some_and(|text| text.diagnostic.is_some());
        let supports_diagnostic_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.diagnostic.as_ref())
            .and_then(|diagnostic| diagnostic.refresh_support)
            .unwrap_or(false);
        let supports_label_offsets = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|text| text.signature_help.as_ref())
            .and_then(|help| help.signature_information.as_ref())
            .and_then(|info| info.parameter_information.as_ref())
            .and_then(|parameter| parameter.label_offset_support)
            .unwrap_or(false);
        let supports_snippets = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|text| text.completion.as_ref())
            .and_then(|completion| completion.completion_item.as_ref())
            .and_then(|item| item.snippet_support)
            .unwrap_or(false);

        // The workspace root comes from the client, never the process cwd.
        let workspace_root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path().ok())
            .or_else(|| {
                #[allow(deprecated)]
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path().ok())
            })
            .unwrap_or_else(|| PathBuf::from("."));

        let (config, pending_config_error) = match Config::discover(&workspace_root) {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error)),
        };

        let db = RootDatabase::default();
        let current_token = salsa::Database::cancellation_token(&db);
        seed.cancel.install(current_token.clone());

        let mut worker = Worker {
            client: seed.client,
            runtime: seed.runtime,
            cancel: seed.cancel,
            current_token,
            idle_interrupt: seed.idle_interrupt,
            experimental_features: seed.experimental_features,
            debug: seed.debug,
            encoding,
            supports_pull_diagnostics,
            supports_diagnostic_refresh,
            supports_label_offsets,
            supports_snippets,
            workspace_root,
            config,
            pending_config_error: pending_config_error.as_ref().map(config_message),
            db,
            files: HashMap::new(),
            open_documents: HashSet::new(),
            stub_documents: HashMap::new(),
            stub_source_paths: Vec::new(),
            namespace_documents: HashMap::new(),
            collate_order: Vec::new(),
            virtual_document_uris: HashMap::new(),
            pending_semantic_publishes: Vec::new(),
            prime_queue: VecDeque::new(),
            attached_by_file: HashMap::new(),
        };
        worker.install_stubs();
        worker.install_metadata();
        worker.load_workspace_sources();

        let result = initialize_result(encoding, worker.experimental_features);
        (worker, result)
    }

    fn install_stubs(&mut self) {
        let mut sources = semantics::stubs::shipped_stub_sources();
        self.stub_source_paths = materialize_shipped_stubs(&sources);
        let stubs_dir = self.workspace_root.join("stubs");
        if let Ok(entries) = std::fs::read_dir(&stubs_dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "Rtypes")
                })
                .collect();
            paths.sort();
            for path in paths {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let stem = path
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        sources.push((stem, text));
                        self.stub_source_paths.push(Some(path));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "skipping unreadable override stub {}: {error}",
                            path.display()
                        );
                    }
                }
            }
        }
        semantics::stubs::StubSources::new(
            &self.db,
            sources,
            semantics::stubs::shipped_export_manifests(),
        );
    }

    /// The current NAMESPACE/DESCRIPTION facts: the open NAMESPACE buffer
    /// wins over the on-disk file. DESCRIPTION is read from disk, because it
    /// is not a tracked document and its saves arrive through the file
    /// watcher.
    fn current_metadata(&self) -> WorkspaceMetadata {
        let namespace_path = self.workspace_root.join("NAMESPACE");
        let namespace_text = self
            .namespace_documents
            .get(&namespace_path)
            .cloned()
            .or_else(|| std::fs::read_to_string(&namespace_path).ok());
        let imports = namespace_text
            .as_deref()
            .map(|text| {
                semantics::metadata::normalized_imports(
                    &semantics::metadata::parse_namespace_imports(text),
                )
            })
            .unwrap_or_default();
        let description_text = std::fs::read_to_string(self.workspace_root.join("DESCRIPTION"));
        let dependencies = description_text
            .as_deref()
            .map(semantics::metadata::parse_description_dependencies)
            .unwrap_or_default();
        let collate = description_text
            .as_deref()
            .map(semantics::metadata::parse_description_collate)
            .unwrap_or_default();
        let package = description_text
            .as_deref()
            .ok()
            .and_then(semantics::metadata::parse_description_package);
        WorkspaceMetadata {
            imports,
            dependencies,
            collate,
            package,
        }
    }

    fn install_metadata(&mut self) {
        let metadata = self.current_metadata();
        self.collate_order = metadata.collate;
        semantics::metadata::PackageMetadata::new(
            &self.db,
            metadata.imports,
            metadata.dependencies,
            self.attached_union(),
            metadata.package,
        );
    }

    fn attached_union(&self) -> std::collections::BTreeSet<String> {
        self.attached_by_file
            .values()
            .flat_map(|namespaces| namespaces.iter().cloned())
            .collect()
    }

    /// Re-scans ONE file's `library()`-family attach facts (riding the parse
    /// the surrounding sync or prime work forces anyway) and, when the
    /// project-wide union moved, updates the metadata input. Every file's
    /// resolution universe changes with it, so all diagnostics refresh.
    fn refresh_attached(&mut self, path: &Path) {
        let scanned = match self.files.get(path) {
            Some(&file) => {
                let scan = self.cancellable(|worker| {
                    semantics::metadata::file_attached_namespaces(&worker.db, file)
                });
                match scan {
                    Ok(scanned) => scanned,
                    // A newer edit is already queued; its own sync re-scans.
                    Err(_) => return,
                }
            }
            None => Default::default(),
        };
        let changed = if scanned.is_empty() {
            self.attached_by_file.remove(path).is_some()
        } else if self.attached_by_file.get(path) != Some(&scanned) {
            self.attached_by_file.insert(path.to_owned(), scanned);
            true
        } else {
            false
        };
        if !changed {
            return;
        }
        let union = self.attached_union();
        match semantics::metadata::PackageMetadata::try_get(&self.db) {
            Some(metadata) => {
                if metadata.attached(&self.db) == &union {
                    return;
                }
                use salsa::Setter;
                metadata.set_attached(&mut self.db).to(union);
            }
            None => {
                let current = self.current_metadata();
                semantics::metadata::PackageMetadata::new(
                    &self.db,
                    current.imports,
                    current.dependencies,
                    union,
                    current.package,
                );
            }
        }
        self.refresh_all_diagnostics();
    }

    /// Re-derives the metadata input; on a real change every file's
    /// resolution universe moved, so all diagnostics refresh. A `Collate`
    /// change reorders the project instead.
    fn refresh_metadata(&mut self) {
        let current = self.current_metadata();
        let collate_changed = current.collate != self.collate_order;
        if collate_changed {
            self.collate_order = current.collate;
            self.rebuild_project_files();
        }
        let Some(metadata) = semantics::metadata::PackageMetadata::try_get(&self.db) else {
            semantics::metadata::PackageMetadata::new(
                &self.db,
                current.imports,
                current.dependencies,
                self.attached_union(),
                current.package,
            );
            self.refresh_all_diagnostics();
            return;
        };
        let facts_changed = metadata.imports(&self.db) != &current.imports
            || metadata.dependencies(&self.db) != &current.dependencies
            || metadata.package(&self.db) != &current.package;
        if facts_changed {
            use salsa::Setter;
            metadata.set_imports(&mut self.db).to(current.imports);
            metadata
                .set_dependencies(&mut self.db)
                .to(current.dependencies);
            metadata.set_package(&mut self.db).to(current.package);
        }
        if facts_changed || collate_changed {
            self.refresh_all_diagnostics();
        }
    }

    fn load_workspace_sources(&mut self) {
        let r_path = self.workspace_root.join("R");
        if r_path.is_dir() {
            let entries =
                std::fs::read_dir(&r_path).expect("listing the workspace R directory must succeed");
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension == "R" || extension == "r")
                })
                .collect();
            paths.sort();
            for path in paths {
                let text = std::fs::read_to_string(&path)
                    .expect("reading a workspace source file must succeed");
                let file = SourceFile::new(&self.db, text, DocumentKind::Package);
                self.files.insert(path.clone(), file);
                self.prime_queue.push_back(path);
            }
        }
        self.rebuild_project_files();
    }

    /// Recomputes the `ProjectFiles` input. Package documents come first, in
    /// the DESCRIPTION `Collate` order when it is declared, with unlisted files
    /// after the listed ones, then ascending by workspace-relative path.
    /// Scripts follow. This is the order the last-writer-wins symbol index and
    /// the CLI agree on.
    fn rebuild_project_files(&mut self) {
        use salsa::Setter;
        let r_path = self.workspace_root.join("R");
        let collate_rank: HashMap<&str, usize> = self
            .collate_order
            .iter()
            .enumerate()
            .map(|(rank, name)| (name.as_str(), rank))
            .collect();
        let mut ordered: Vec<(&PathBuf, &SourceFile)> = self.files.iter().collect();
        ordered.sort_by_key(|(path, _)| {
            let rank = path
                .file_name()
                .and_then(|name| collate_rank.get(name.to_string_lossy().as_ref()))
                .copied()
                .unwrap_or(usize::MAX);
            (
                !path.starts_with(&r_path),
                rank,
                path.strip_prefix(&self.workspace_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        });
        let files: Vec<SourceFile> = ordered.into_iter().map(|(_, file)| *file).collect();
        match ProjectFiles::try_get(&self.db) {
            Some(project) => {
                project.set_files(&mut self.db).to(files);
            }
            None => {
                ProjectFiles::new(&self.db, files);
            }
        }
    }

    /// An edit's cancellation flip is consumed by whatever query was in
    /// flight when it happened; every job afterwards starts on a fresh
    /// uncancelled handle (cloning the storage handle is cheap).
    fn refresh_cancellation(&mut self) {
        if self.current_token.is_cancelled() {
            self.db = self.db.clone();
            self.current_token = salsa::Database::cancellation_token(&self.db);
            self.cancel.install(self.current_token.clone());
        }
    }

    fn has_idle_work(&self) -> bool {
        !self.pending_semantic_publishes.is_empty() || !self.prime_queue.is_empty()
    }

    fn run_idle_unit(&mut self) {
        if let Some(path) = self.pending_semantic_publishes.pop() {
            if !self.open_documents.contains(&path) {
                return;
            }
            match self.cancellable(|worker| worker.settled_diagnostics(&path)) {
                Ok(Some(diagnostics)) => {
                    let uri = self.document_uri(&path);
                    self.publish(uri, diagnostics, None);
                }
                Ok(None) => {}
                Err(_) => self.pending_semantic_publishes.push(path),
            }
            return;
        }
        if let Some(path) = self.prime_queue.pop_front() {
            let Some(&file) = self.files.get(&path) else {
                return;
            };
            let config = self.config.clone();
            let outcome = self.cancellable(|worker| {
                let _ = document_diagnostics(&worker.db, file, &config);
            });
            match outcome {
                Ok(()) => {
                    // The prime is also how the startup workspace gets its
                    // attach facts scanned without a blocking sweep.
                    self.refresh_attached(&path);
                }
                Err(_) => self.prime_queue.push_front(path),
            }
        }
    }

    /// Runs `body` catching cooperative cancellation; the caller decides the
    /// per-feature degradation policy.
    fn cancellable<T>(
        &mut self,
        body: impl FnOnce(&mut Worker) -> T,
    ) -> Result<T, salsa::Cancelled> {
        let this: *mut Worker = self;
        salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
            // The closure runs synchronously on this thread; the raw pointer
            // only bridges catch_unwind's UnwindSafe bound.
            body(unsafe { &mut *this })
        }))
    }

    //
    // Paths and URIs
    //

    fn document_path(&mut self, uri: &lsp_types::Url) -> Option<PathBuf> {
        if uri.scheme() == "file" {
            return uri.to_file_path().ok();
        }
        let path = self.virtual_document_path(uri);
        self.virtual_document_uris.insert(path.clone(), uri.clone());
        Some(path)
    }

    /// A deterministic, injective synthetic path for a non-`file:` URI,
    /// under the workspace root. It is a pure table key and is never read from
    /// disk.
    fn virtual_document_path(&self, uri: &lsp_types::Url) -> PathBuf {
        let encoded = uri
            .as_str()
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace('\\', "%5C");
        self.workspace_root.join(".ry-virtual").join(encoded)
    }

    fn document_uri(&self, path: &Path) -> lsp_types::Url {
        if let Some(uri) = self.virtual_document_uris.get(path) {
            return uri.clone();
        }
        lsp_types::Url::from_file_path(path).expect("tracked paths convert to URIs")
    }

    fn is_package_path(&self, path: &Path) -> bool {
        crate::cli::shares_a_namespace(path, &self.workspace_root)
    }

    fn is_stub_document(path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension == "Rtypes")
    }

    fn is_namespace_document(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "NAMESPACE")
    }

    //
    // Text and positions
    //

    fn text(&self, file: SourceFile) -> String {
        file.text(&self.db).to_owned()
    }

    fn to_offset(&self, text: &str, position: lsp_types::Position) -> TextSize {
        let index = LineIndex::new(text);
        let column = LineColumn {
            line: position.line,
            column: position.character,
        };
        match self.encoding {
            PositionEncoding::Utf8 => index.offset(column, text),
            PositionEncoding::Utf16 => index.offset_utf16(column, text),
        }
    }

    fn to_position(&self, text: &str, offset: TextSize) -> lsp_types::Position {
        let index = LineIndex::new(text);
        let column = match self.encoding {
            PositionEncoding::Utf8 => index.line_column(offset),
            PositionEncoding::Utf16 => index.line_column_utf16(offset, text),
        };
        lsp_types::Position::new(column.line, column.column)
    }

    fn to_range(&self, text: &str, range: TextRange) -> lsp_types::Range {
        lsp_types::Range {
            start: self.to_position(text, range.start()),
            end: self.to_position(text, range.end()),
        }
    }

    /// A range in another (tracked) document, converted against that
    /// document's own text.
    fn to_range_in(&self, file: SourceFile, range: TextRange) -> lsp_types::Range {
        let text = self.text(file);
        self.to_range(&text, range)
    }

    fn path_of(&self, file: SourceFile) -> Option<&PathBuf> {
        self.files
            .iter()
            .find(|(_, candidate)| **candidate == file)
            .map(|(path, _)| path)
    }

    /// The hover's definition summary line: where the hovered name is
    /// defined, with a workspace-relative `path:line:column` location.
    fn render_hover_definition(&self, definition: &ide::HoverDefinition) -> Option<String> {
        match definition {
            ide::HoverDefinition::Local {
                target,
                maybe_undefined,
            } => {
                let location = self.render_source_location(target)?;
                let mut summary = format!("Local variable, defined at `{location}`");
                if *maybe_undefined {
                    summary.push_str("\n\n_May be undefined on some paths._");
                }
                Some(summary)
            }
            ide::HoverDefinition::Global { target } => {
                let location = self.render_source_location(target)?;
                Some(format!("Package global, defined at `{location}`"))
            }
            ide::HoverDefinition::Stub {
                namespace,
                overloads,
                declaration,
            } => {
                let mut summary = if *overloads > 1 {
                    format!(
                        "From the `{namespace}` package (+{} overloads).",
                        overloads - 1
                    )
                } else {
                    format!("From the `{namespace}` package.")
                };
                if let Some(location) =
                    declaration.and_then(|target| self.render_stub_source_location(target))
                {
                    summary.push_str(&format!(" Declared at `{location}`."));
                }
                Some(summary)
            }
        }
    }

    /// A `utils.Rtypes:12:1`-style location of a stub declaration. The file
    /// name is enough for a human, and goto-definition does the jumping.
    fn render_stub_source_location(&self, target: ide::StubTarget) -> Option<String> {
        let path = self.stub_source_paths.get(target.source_index)?.as_ref()?;
        let sources = semantics::stubs::StubSources::try_get(&self.db)?;
        let (_, text) = sources.sources(&self.db).get(target.source_index)?;
        let index = LineIndex::new(text);
        let position = index.line_column_chars(target.range.start(), text);
        Some(format!(
            "{}:{}:{}",
            path.file_name()?.to_string_lossy(),
            position.line + 1,
            position.column + 1
        ))
    }

    /// A goto response's `Location` for either target kind.
    fn definition_location(&self, target: ide::DefinitionTarget) -> Option<lsp_types::Location> {
        match target {
            ide::DefinitionTarget::Project(target) => {
                let path = self.path_of(target.file)?;
                Some(lsp_types::Location {
                    uri: self.document_uri(path),
                    range: self.to_range_in(target.file, target.range),
                })
            }
            ide::DefinitionTarget::Stub(target) => self.stub_location(target),
        }
    }

    /// The on-disk `Location` of a stub declaration: the materialized cache
    /// copy for shipped sources, the real file for project overrides.
    fn stub_location(&self, target: ide::StubTarget) -> Option<lsp_types::Location> {
        let path = self.stub_source_paths.get(target.source_index)?.as_ref()?;
        let sources = semantics::stubs::StubSources::try_get(&self.db)?;
        let (_, text) = sources.sources(&self.db).get(target.source_index)?;
        Some(lsp_types::Location {
            uri: lsp_types::Url::from_file_path(path).ok()?,
            range: self.to_range(text, target.range),
        })
    }

    /// `R/main.R:2:1`-style location: workspace-relative path, 1-based line
    /// and column.
    fn render_source_location(&self, target: &ide::NavigationTarget) -> Option<String> {
        let path = self.path_of(target.file)?;
        let display = path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .display();
        let text = self.text(target.file);
        let index = LineIndex::new(&text);
        let position = index.line_column_chars(target.range.start(), &text);
        Some(format!(
            "{display}:{}:{}",
            position.line + 1,
            position.column + 1
        ))
    }

    //
    // Document sync
    //

    fn did_open(&mut self, params: lsp_types::DidOpenTextDocumentParams) {
        let Some(path) = self.document_path(&params.text_document.uri) else {
            tracing::warn!("unusable URI in didOpen: {}", params.text_document.uri);
            return;
        };
        let version = params.text_document.version;
        if Worker::is_stub_document(&path) {
            self.stub_documents
                .insert(path.clone(), params.text_document.text.clone());
            if !self.supports_pull_diagnostics {
                let diagnostics = self.stub_diagnostics(&params.text_document.text);
                let uri = self.document_uri(&path);
                self.publish(uri, diagnostics, Some(version));
            }
            return;
        }
        if Worker::is_namespace_document(&path) {
            self.namespace_documents
                .insert(path.clone(), params.text_document.text.clone());
            self.refresh_metadata();
            if !self.supports_pull_diagnostics {
                let diagnostics = self.namespace_diagnostics(&params.text_document.text);
                let uri = self.document_uri(&path);
                self.publish(uri, diagnostics, Some(version));
            }
            return;
        }

        let is_package = self.is_package_path(&path);
        let kind = if is_package {
            DocumentKind::Package
        } else {
            DocumentKind::Script
        };
        use salsa::Setter;
        match self.files.get(&path) {
            Some(&file) => {
                file.set_text(&mut self.db).to(params.text_document.text);
            }
            None => {
                let file = SourceFile::new(&self.db, params.text_document.text, kind);
                self.files.insert(path.clone(), file);
                self.rebuild_project_files();
            }
        }
        self.open_documents.insert(path.clone());
        self.refresh_attached(&path);
        if !self.supports_pull_diagnostics {
            self.publish_first_wave(&path, version);
        }
    }

    fn did_change(&mut self, params: lsp_types::DidChangeTextDocumentParams) {
        let Some(path) = self.document_path(&params.text_document.uri) else {
            tracing::warn!("unusable URI in didChange: {}", params.text_document.uri);
            return;
        };
        let version = params.text_document.version;

        if let Some(text) = self.stub_documents.get(&path).cloned() {
            let updated = self.apply_content_changes(text, &params.content_changes);
            self.stub_documents.insert(path.clone(), updated.clone());
            if !self.supports_pull_diagnostics {
                let diagnostics = self.stub_diagnostics(&updated);
                let uri = self.document_uri(&path);
                self.publish(uri, diagnostics, Some(version));
            }
            return;
        }
        if let Some(text) = self.namespace_documents.get(&path).cloned() {
            let updated = self.apply_content_changes(text, &params.content_changes);
            self.namespace_documents
                .insert(path.clone(), updated.clone());
            self.refresh_metadata();
            if !self.supports_pull_diagnostics {
                let diagnostics = self.namespace_diagnostics(&updated);
                let uri = self.document_uri(&path);
                self.publish(uri, diagnostics, Some(version));
            }
            return;
        }

        if !self.open_documents.contains(&path) {
            self.report_error(&format!(
                "didChange for a document that is not open: {}",
                path.display()
            ));
            return;
        }
        let &file = self
            .files
            .get(&path)
            .expect("open documents are always tracked");
        let text = self.text(file);
        let updated = self.apply_content_changes(text, &params.content_changes);
        use salsa::Setter;
        file.set_text(&mut self.db).to(updated);
        self.refresh_attached(&path);
        if !self.supports_pull_diagnostics {
            self.publish_first_wave(&path, version);
        }
    }

    /// Applies each change against the evolving buffer (each ranged change
    /// refers to the state after the previous ones; a change without a range
    /// replaces the whole document).
    fn apply_content_changes(
        &self,
        mut text: String,
        changes: &[lsp_types::TextDocumentContentChangeEvent],
    ) -> String {
        for change in changes {
            match change.range {
                None => text = change.text.clone(),
                Some(range) => {
                    let index = LineIndex::new(&text);
                    let (start, end) = match self.encoding {
                        PositionEncoding::Utf8 => (
                            index.offset(
                                LineColumn {
                                    line: range.start.line,
                                    column: range.start.character,
                                },
                                &text,
                            ),
                            index.offset(
                                LineColumn {
                                    line: range.end.line,
                                    column: range.end.character,
                                },
                                &text,
                            ),
                        ),
                        PositionEncoding::Utf16 => (
                            index.offset_utf16(
                                LineColumn {
                                    line: range.start.line,
                                    column: range.start.character,
                                },
                                &text,
                            ),
                            index.offset_utf16(
                                LineColumn {
                                    line: range.end.line,
                                    column: range.end.character,
                                },
                                &text,
                            ),
                        ),
                    };
                    text.replace_range(usize::from(start)..usize::from(end), &change.text);
                }
            }
        }
        text
    }

    fn did_save(&mut self, params: lsp_types::DidSaveTextDocumentParams) {
        let Some(path) = self.document_path(&params.text_document.uri) else {
            return;
        };
        if !self.open_documents.contains(&path) {
            // Editors send didSave for config and other buffers; the config
            // reload runs off the file watcher.
            return;
        }
        assert!(
            self.files.contains_key(&path),
            "open documents must be tracked"
        );
        // A package-visible save can move diagnostics in dependent files.
        self.refresh_all_diagnostics();
    }

    fn did_close(&mut self, params: lsp_types::DidCloseTextDocumentParams) {
        let Some(path) = self.document_path(&params.text_document.uri) else {
            return;
        };
        if self.stub_documents.remove(&path).is_some() {
            return;
        }
        if self.namespace_documents.remove(&path).is_some() {
            // The closed buffer reverts to the on-disk NAMESPACE.
            self.refresh_metadata();
            return;
        }
        self.open_documents.remove(&path);
        self.pending_semantic_publishes.retain(|owed| owed != &path);

        if self.is_package_path(&path) {
            // A closed package file reverts to its on-disk text, discarding
            // unsaved buffer edits.
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Some(&file) = self.files.get(&path) {
                        use salsa::Setter;
                        file.set_text(&mut self.db).to(text);
                    }
                    self.refresh_attached(&path);
                    return;
                }
                Err(error) => {
                    // The close can race a delete/rename; retract below.
                    tracing::warn!("re-reading closed file failed: {error}");
                }
            }
        }
        self.files.remove(&path);
        self.virtual_document_uris.remove(&path);
        self.rebuild_project_files();
        self.refresh_attached(&path);
    }

    fn did_change_watched_files(&mut self, params: lsp_types::DidChangeWatchedFilesParams) {
        let mut config_changed = false;
        let mut metadata_changed = false;
        let mut reloaded_documents: Vec<PathBuf> = Vec::new();
        for change in &params.changes {
            let Ok(path) = change.uri.to_file_path() else {
                continue;
            };
            // Matched by file name, not full path: tolerates symlinked roots
            // and case-normalizing clients.
            if path
                .file_name()
                .is_some_and(|name| name == "NAMESPACE" || name == "DESCRIPTION")
            {
                metadata_changed = true;
            }
            if path
                .file_name()
                .is_some_and(|name| name == CONFIG_FILE_NAME)
            {
                // Re-discover rather than read the changed file: a deleted
                // workspace config falls back to an ancestor or defaults.
                match Config::discover(&self.workspace_root) {
                    Ok(config) => {
                        config_changed |= config != self.config;
                        self.config = config;
                        self.report_unknown_config_keys();
                        self.publish_config_diagnostics(None);
                    }
                    Err(error) => {
                        let message = config_message(&error);
                        self.report_error(&format!(
                            "{message}; keeping the previous configuration"
                        ));
                        self.publish_config_diagnostics(Some(&error));
                    }
                }
            } else if self.is_package_path(&path)
                && path
                    .extension()
                    .is_some_and(|extension| extension == "R" || extension == "r")
                && !self.open_documents.contains(&path)
            {
                use salsa::Setter;
                match std::fs::read_to_string(&path) {
                    Ok(text) => match self.files.get(&path) {
                        Some(&file) => {
                            file.set_text(&mut self.db).to(text);
                            reloaded_documents.push(path.clone());
                        }
                        None => {
                            let file = SourceFile::new(&self.db, text, DocumentKind::Package);
                            self.files.insert(path.clone(), file);
                            self.rebuild_project_files();
                            reloaded_documents.push(path.clone());
                        }
                    },
                    Err(_) => {
                        if self.files.remove(&path).is_some() {
                            self.rebuild_project_files();
                            reloaded_documents.push(path.clone());
                        }
                    }
                }
            }
        }
        for path in &reloaded_documents {
            self.refresh_attached(path);
        }
        if metadata_changed {
            // No-op when the parsed facts are unchanged; refreshes all
            // diagnostics itself otherwise.
            self.refresh_metadata();
        }
        if config_changed {
            self.refresh_all_diagnostics();
        }
    }

    fn initialized(&mut self) {
        if let Some(message) = self.pending_config_error.take() {
            self.report_error(&message);
            let location = Config::discover_path(&self.workspace_root)
                .and_then(|path| Config::from_path(path).err());
            self.publish_config_diagnostics(location.as_ref());
        }
        self.report_unknown_config_keys();
        self.register_file_watchers();
    }

    fn register_file_watchers(&self) {
        let mut watchers = vec![
            lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::Relative(lsp_types::RelativePattern {
                    base_uri: lsp_types::OneOf::Right(
                        lsp_types::Url::from_file_path(self.workspace_root.join("R"))
                            .unwrap_or_else(|_| self.document_uri(&self.workspace_root)),
                    ),
                    pattern: "*.[rR]".to_owned(),
                }),
                kind: None,
            },
            lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::Relative(lsp_types::RelativePattern {
                    base_uri: lsp_types::OneOf::Right(self.document_uri(&self.workspace_root)),
                    pattern: CONFIG_FILE_NAME.to_owned(),
                }),
                kind: None,
            },
            lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::Relative(lsp_types::RelativePattern {
                    base_uri: lsp_types::OneOf::Right(self.document_uri(&self.workspace_root)),
                    pattern: "{NAMESPACE,DESCRIPTION}".to_owned(),
                }),
                kind: None,
            },
        ];
        // A governing config above the workspace root must reload live too.
        if let Some(config_path) = Config::discover_path(&self.workspace_root)
            && let Some(parent) = config_path.parent()
            && parent != self.workspace_root
        {
            watchers.push(lsp_types::FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::Relative(lsp_types::RelativePattern {
                    base_uri: lsp_types::OneOf::Right(self.document_uri(parent)),
                    pattern: CONFIG_FILE_NAME.to_owned(),
                }),
                kind: None,
            });
        }
        let registration = lsp_types::Registration {
            id: "workspace/didChangeWatchedFiles".to_owned(),
            method: "workspace/didChangeWatchedFiles".to_owned(),
            register_options: Some(
                serde_json::to_value(lsp_types::DidChangeWatchedFilesRegistrationOptions {
                    watchers,
                })
                .expect("watcher options serialize"),
            ),
        };
        let mut client = self.client.clone();
        self.runtime.spawn(async move {
            if let Err(error) = client
                .register_capability(lsp_types::RegistrationParams {
                    registrations: vec![registration],
                })
                .await
            {
                tracing::error!("file watcher registration failed: {error}");
            }
        });
    }

    //
    // Diagnostics
    //

    fn publish(
        &self,
        uri: lsp_types::Url,
        diagnostics: Vec<lsp_types::Diagnostic>,
        version: Option<i32>,
    ) {
        let mut client = self.client.clone();
        let params = lsp_types::PublishDiagnosticsParams::new(uri, diagnostics, version);
        if let Err(error) = client.publish_diagnostics(params) {
            tracing::error!("publishing diagnostics failed: {error}");
        }
    }

    fn publish_first_wave(&mut self, path: &Path, version: i32) {
        let Some(&file) = self.files.get(path) else {
            return;
        };
        let config = self.config.clone();
        let diagnostics = self
            .cancellable(|worker| {
                let rendered = first_wave_diagnostics(&worker.db, file, &config);
                worker.finish_diagnostics(file, rendered)
            })
            .unwrap_or_default();
        let uri = self.document_uri(path);
        self.publish(uri, diagnostics, Some(version));
        self.defer_semantic_publish(path);
    }

    fn defer_semantic_publish(&mut self, path: &Path) {
        self.pending_semantic_publishes.retain(|owed| owed != path);
        self.pending_semantic_publishes.push(path.to_path_buf());
    }

    /// The full (settled) diagnostic set for one tracked document.
    fn settled_diagnostics(&mut self, path: &Path) -> Option<Vec<lsp_types::Diagnostic>> {
        let &file = self.files.get(path)?;
        let config = self.config.clone();
        let rendered = document_diagnostics(&self.db, file, &config);
        Some(self.finish_diagnostics(file, rendered))
    }

    /// The shared rendering tail: suppression comments, then LSP encoding.
    fn finish_diagnostics(
        &self,
        file: SourceFile,
        rendered: Vec<Diagnostic>,
    ) -> Vec<lsp_types::Diagnostic> {
        let text = self.text(file);
        let rendered = apply_suppressions(rendered, &text);
        rendered
            .into_iter()
            .map(|diagnostic| self.convert_diagnostic(&text, diagnostic))
            .collect()
    }

    fn convert_diagnostic(&self, text: &str, diagnostic: Diagnostic) -> lsp_types::Diagnostic {
        // The unnecessary tag lets an editor render dead code faded, which is
        // the conventional presentation for a value no read uses.
        let tags = matches!(
            diagnostic.code,
            "unused" | "unused-parameter" | "unused-import"
        )
        .then(|| vec![lsp_types::DiagnosticTag::UNNECESSARY]);
        let related_information: Vec<lsp_types::DiagnosticRelatedInformation> = diagnostic
            .related
            .iter()
            .filter_map(|related| {
                let path = self.path_of(related.file)?;
                Some(lsp_types::DiagnosticRelatedInformation {
                    location: lsp_types::Location {
                        uri: self.document_uri(path),
                        range: self.to_range_in(related.file, related.range),
                    },
                    message: related.message.to_owned(),
                })
            })
            .collect();
        lsp_types::Diagnostic {
            range: self.to_range(text, diagnostic.range),
            severity: Some(match diagnostic.severity {
                Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
            }),
            code: Some(lsp_types::NumberOrString::String(
                diagnostic.code.to_owned(),
            )),
            code_description: None,
            source: Some("ry".into()),
            message: diagnostic.message,
            related_information: (!related_information.is_empty()).then_some(related_information),
            tags,
            data: None,
        }
    }

    /// Whole-line errors for the declarations a `.Rtypes` buffer's loader
    /// would drop.
    fn stub_diagnostics(&self, text: &str) -> Vec<lsp_types::Diagnostic> {
        let index = LineIndex::new(text);
        semantics::stubs::stub_source_problems(&self.db, text)
            .into_iter()
            .map(|problem| {
                let start = index.line_start(problem.line as u32);
                let end = start + index.line_length(problem.line as u32, text);
                self.convert_diagnostic(
                    text,
                    Diagnostic {
                        range: TextRange::new(start.into(), end.into()),
                        severity: Severity::Error,
                        code: "stub",
                        message: problem.message,
                        related: Vec::new(),
                    },
                )
            })
            .collect()
    }

    fn namespace_diagnostics(&self, text: &str) -> Vec<lsp_types::Diagnostic> {
        let imports = crate::namespace::parse_namespace_imports(text);
        let knows =
            |package: &str| semantics::stubs::namespace_known(&self.db, package).unwrap_or(false);
        let exports = |package: &str, name: &str| {
            semantics::stubs::namespace_exports(&self.db, package, name)
        };
        crate::namespace::namespace_import_problems(&imports, &knows, &exports)
            .into_iter()
            .map(|diagnostic| self.convert_diagnostic(text, diagnostic))
            .collect()
    }

    /// A malformed config file is published as a diagnostic on the config
    /// file itself; `None` clears it.
    fn publish_config_diagnostics(&self, error: Option<&ConfigError>) {
        let config_path = self.workspace_root.join(CONFIG_FILE_NAME);
        let Ok(uri) = lsp_types::Url::from_file_path(&config_path) else {
            return;
        };
        let diagnostics = match error {
            Some(error) => {
                let (line, column) = error.parse_location().unwrap_or((1, 1));
                let start = lsp_types::Position::new(
                    line.saturating_sub(1) as u32,
                    column.saturating_sub(1) as u32,
                );
                let end = lsp_types::Position::new(start.line, start.character + 1);
                vec![lsp_types::Diagnostic {
                    range: lsp_types::Range { start, end },
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    code: Some(lsp_types::NumberOrString::String("config".to_owned())),
                    code_description: None,
                    source: Some("ry".into()),
                    message: error.to_string(),
                    related_information: None,
                    tags: None,
                    data: None,
                }]
            }
            None => Vec::new(),
        };
        self.publish(uri, diagnostics, None);
    }

    /// Cross-file diagnostics move even on a save or config change: pull
    /// clients with refresh support are asked to re-pull, push clients get
    /// every open document re-published at idle time.
    fn refresh_all_diagnostics(&mut self) {
        if self.supports_pull_diagnostics {
            if self.supports_diagnostic_refresh {
                let mut client = self.client.clone();
                self.runtime.spawn(async move {
                    let _ = client.workspace_diagnostic_refresh(()).await;
                });
            }
            return;
        }
        let open: Vec<PathBuf> = self
            .open_documents
            .iter()
            .filter(|path| self.files.contains_key(*path))
            .cloned()
            .collect();
        for path in open {
            self.defer_semantic_publish(&path);
        }
    }

    fn report_error(&self, message: &str) {
        let mut client = self.client.clone();
        let _ = client.show_message(lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::ERROR,
            message: message.to_owned(),
        });
    }

    /// Unknown config keys warn visibly but never block startup or a reload:
    /// the rest of the file is honored (forward compatibility for configs
    /// written against newer versions).
    fn report_unknown_config_keys(&self) {
        if self.config.unknown_keys.is_empty() {
            return;
        }
        let keys = self
            .config
            .unknown_keys
            .iter()
            .map(|key| match crate::config::suggested_table(key) {
                Some(table) => format!("`{key}` (it belongs under `[{table}]`)"),
                None => format!("`{key}`"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Name the file that was actually loaded. A project may still be on
        // `roughly.toml`, the name the configuration file used to have.
        let file = self
            .config
            .source_directory
            .as_deref()
            .map(|directory| {
                crate::config::CONFIG_FILE_NAMES
                    .into_iter()
                    .find(|name| directory.join(name).is_file())
                    .unwrap_or(crate::config::CONFIG_FILE_NAME)
            })
            .unwrap_or(crate::config::CONFIG_FILE_NAME);
        let mut client = self.client.clone();
        let _ = client.show_message(lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::WARNING,
            message: format!(
                "{file} sets {keys}, which this version of ry does not know, so it is \
                 ignoring {}. Check the spelling, or update ry.",
                if self.config.unknown_keys.len() == 1 {
                    "it"
                } else {
                    "them"
                }
            ),
        });
    }
}

/// A configuration failure as a client message. The CLI draws the offending
/// line of the file; a popup has no room for a snippet, so it names the file
/// the published finding is anchored in.
fn config_message(error: &ConfigError) -> String {
    match error.file() {
        Some(file) => format!("{file}: {error}"),
        None => error.to_string(),
    }
}

fn initialize_result(
    encoding: PositionEncoding,
    experimental: ExperimentalFeatures,
) -> lsp_types::InitializeResult {
    lsp_types::InitializeResult {
        capabilities: lsp_types::ServerCapabilities {
            position_encoding: Some(encoding.kind()),
            code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
                lsp_types::CodeActionOptions {
                    code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
                    ..Default::default()
                },
            )),
            completion_provider: Some(lsp_types::CompletionOptions {
                trigger_characters: Some(["$", "@", ":", "\""].map(str::to_owned).to_vec()),
                ..Default::default()
            }),
            definition_provider: Some(lsp_types::OneOf::Left(true)),
            type_definition_provider: Some(lsp_types::TypeDefinitionProviderCapability::Simple(
                true,
            )),
            diagnostic_provider: Some(lsp_types::DiagnosticServerCapabilities::Options(
                lsp_types::DiagnosticOptions {
                    identifier: Some("ry".to_owned()),
                    inter_file_dependencies: true,
                    workspace_diagnostics: false,
                    work_done_progress_options: Default::default(),
                },
            )),
            document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
            document_range_formatting_provider: Some(lsp_types::OneOf::Left(
                experimental.range_formatting,
            )),
            document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
            hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
            inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
            signature_help_provider: Some(lsp_types::SignatureHelpOptions {
                trigger_characters: Some(["(", ","].map(str::to_owned).to_vec()),
                retrigger_characters: None,
                work_done_progress_options: Default::default(),
            }),
            references_provider: Some(lsp_types::OneOf::Left(true)),
            document_highlight_provider: Some(lsp_types::OneOf::Left(true)),
            folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
            rename_provider: Some(lsp_types::OneOf::Left(true)),
            semantic_tokens_provider: Some(
                lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                    lsp_types::SemanticTokensOptions {
                        legend: lsp_types::SemanticTokensLegend {
                            token_types: semantic_token_legend(),
                            token_modifiers: Vec::new(),
                        },
                        full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                        range: None,
                        work_done_progress_options: Default::default(),
                    },
                ),
            ),
            text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Options(
                lsp_types::TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(lsp_types::TextDocumentSyncKind::INCREMENTAL),
                    save: Some(lsp_types::TextDocumentSyncSaveOptions::SaveOptions(
                        lsp_types::SaveOptions {
                            include_text: Some(false),
                        },
                    )),
                    ..Default::default()
                },
            )),
            workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
            ..Default::default()
        },
        server_info: Some(lsp_types::ServerInfo {
            // Not `CARGO_PKG_NAME`: the crate is published as `ry-lang`
            // because the registry name `ry` was taken, but this string is
            // what editors show the user, and the product is `ry`.
            name: "ry".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    }
}

/// The semantic-token legend, in index order. `#:` annotation bodies are the
/// one surface colored here (they are type syntax invisible to R grammars).
fn semantic_token_legend() -> Vec<lsp_types::SemanticTokenType> {
    vec![
        lsp_types::SemanticTokenType::TYPE,
        lsp_types::SemanticTokenType::TYPE_PARAMETER,
        lsp_types::SemanticTokenType::PARAMETER,
        lsp_types::SemanticTokenType::OPERATOR,
        lsp_types::SemanticTokenType::DECORATOR,
    ]
}

impl LanguageServer for ServerState {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        params: lsp_types::InitializeParams,
    ) -> BoxFuture<'static, Result<lsp_types::InitializeResult, Self::Error>> {
        let (reply, receive) = oneshot::channel();
        if self
            .sender
            .send(Job::Initialize(Box::new(params), reply))
            .is_err()
        {
            return Box::pin(async { Err(worker_gone_error()) });
        }
        self.idle_interrupt.store(true, Ordering::SeqCst);
        Box::pin(async move { receive.await.unwrap_or_else(|_| Err(worker_gone_error())) })
    }

    fn initialized(&mut self, _: lsp_types::InitializedParams) -> Self::NotifyResult {
        self.notify(|worker| worker.initialized())
    }

    fn did_open(&mut self, params: lsp_types::DidOpenTextDocumentParams) -> Self::NotifyResult {
        self.notify_edit(move |worker| worker.did_open(params))
    }

    fn did_change(&mut self, params: lsp_types::DidChangeTextDocumentParams) -> Self::NotifyResult {
        self.notify_edit(move |worker| worker.did_change(params))
    }

    fn did_save(&mut self, params: lsp_types::DidSaveTextDocumentParams) -> Self::NotifyResult {
        self.notify_edit(move |worker| worker.did_save(params))
    }

    fn did_close(&mut self, params: lsp_types::DidCloseTextDocumentParams) -> Self::NotifyResult {
        self.notify_edit(move |worker| worker.did_close(params))
    }

    fn did_change_watched_files(
        &mut self,
        params: lsp_types::DidChangeWatchedFilesParams,
    ) -> Self::NotifyResult {
        self.notify_edit(move |worker| worker.did_change_watched_files(params))
    }

    fn did_change_configuration(
        &mut self,
        _: lsp_types::DidChangeConfigurationParams,
    ) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn hover(
        &mut self,
        params: lsp_types::HoverParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::Hover>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position_params;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let hover = worker
                .cancellable(|worker| ide::hover(&worker.db, files, file, offset))
                .unwrap_or_default();
            let debug = if worker.debug {
                worker
                    .cancellable(|worker| ide::hover_debug(&worker.db, file, offset))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(hover.map(|hover| {
                let mut value = format!("```r\n{}\n```", hover.lines.join("\n"));
                if let Some(summary) = hover
                    .definition
                    .as_ref()
                    .and_then(|definition| worker.render_hover_definition(definition))
                {
                    value.push_str("\n\n");
                    value.push_str(&summary);
                }
                if !debug.is_empty() {
                    value.push_str("\n\n---\n\n### Debug");
                    for section in &debug {
                        value.push_str(&format!(
                            "\n\n**{}**\n\n```\n{}\n```",
                            section.title, section.body
                        ));
                    }
                }
                lsp_types::Hover {
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value,
                    }),
                    range: Some(worker.to_range(&text, hover.range)),
                }
            }))
        })
    }

    fn definition(
        &mut self,
        params: lsp_types::GotoDefinitionParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::GotoDefinitionResponse>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position_params;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            if let Some(stub) = worker.stub_documents.get(&path).cloned() {
                let offset = worker.to_offset(&stub, position.position);
                return Ok(stub_type_definition(&stub, offset).map(|range| {
                    lsp_types::GotoDefinitionResponse::Scalar(lsp_types::Location {
                        uri: position.text_document.uri.clone(),
                        range: worker.to_range(&stub, range),
                    })
                }));
            }
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let target = worker
                .cancellable(|worker| ide::definition(&worker.db, files, file, offset))
                .unwrap_or_default();
            Ok(target.and_then(|target| {
                Some(lsp_types::GotoDefinitionResponse::Scalar(
                    worker.definition_location(target)?,
                ))
            }))
        })
    }

    fn type_definition(
        &mut self,
        params: request::GotoTypeDefinitionParams,
    ) -> BoxFuture<'static, Result<Option<request::GotoTypeDefinitionResponse>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position_params;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let target = worker
                .cancellable(|worker| ide::type_definition(&worker.db, files, file, offset))
                .unwrap_or_default();
            Ok(target.and_then(|target| {
                Some(lsp_types::GotoDefinitionResponse::Scalar(
                    worker.definition_location(target)?,
                ))
            }))
        })
    }

    fn references(
        &mut self,
        params: lsp_types::ReferenceParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::Location>>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position;
            let include_declaration = params.context.include_declaration;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let occurrences = worker
                .cancellable(|worker| {
                    ide::references(&worker.db, files, file, offset, include_declaration)
                })
                .unwrap_or_default();
            let locations: Vec<lsp_types::Location> = occurrences
                .into_iter()
                .filter_map(|occurrence| {
                    let path = worker.path_of(occurrence.file)?;
                    Some(lsp_types::Location {
                        uri: worker.document_uri(path),
                        range: worker.to_range_in(occurrence.file, occurrence.range),
                    })
                })
                .collect();
            Ok((!locations.is_empty()).then_some(locations))
        })
    }

    fn document_highlight(
        &mut self,
        params: lsp_types::DocumentHighlightParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::DocumentHighlight>>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position_params;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let occurrences = worker
                .cancellable(|worker| ide::references(&worker.db, files, file, offset, true))
                .unwrap_or_default();
            let highlights: Vec<lsp_types::DocumentHighlight> = occurrences
                .into_iter()
                .filter(|occurrence| occurrence.file == file)
                .map(|occurrence| lsp_types::DocumentHighlight {
                    range: worker.to_range(&text, occurrence.range),
                    kind: None,
                })
                .collect();
            Ok((!highlights.is_empty()).then_some(highlights))
        })
    }

    fn rename(
        &mut self,
        params: lsp_types::RenameParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::WorkspaceEdit>, Self::Error>> {
        self.read(move |worker| {
            let new_name = params.new_name;
            if !is_valid_r_identifier(&new_name) {
                return Err(ResponseError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("`{new_name}` is not a valid R identifier"),
                ));
            }
            let position = params.text_document_position;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let occurrences =
                match worker.cancellable(|worker| ide::rename(&worker.db, files, file, offset)) {
                    Ok(occurrences) => occurrences,
                    // A mutation must never degrade to a null edit.
                    Err(_) => {
                        return Err(ResponseError::new(
                            ErrorCode::CONTENT_MODIFIED,
                            "rename was cancelled by a concurrent edit",
                        ));
                    }
                };
            Ok(occurrences.map(|occurrences| {
                let mut changes: HashMap<lsp_types::Url, Vec<lsp_types::TextEdit>> = HashMap::new();
                for occurrence in occurrences {
                    let Some(path) = worker.path_of(occurrence.file) else {
                        continue;
                    };
                    changes.entry(worker.document_uri(path)).or_default().push(
                        lsp_types::TextEdit {
                            range: worker.to_range_in(occurrence.file, occurrence.range),
                            new_text: new_name.clone(),
                        },
                    );
                }
                lsp_types::WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }
            }))
        })
    }

    fn completion(
        &mut self,
        params: lsp_types::CompletionParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::CompletionResponse>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let result = worker
                .cancellable(|worker| ide::completion(&worker.db, files, file, offset))
                .unwrap_or_default();
            Ok(result.map(|result| {
                let items = result
                    .items
                    .into_iter()
                    .map(|item| convert_completion_item(item, worker.supports_snippets))
                    .collect();
                lsp_types::CompletionResponse::List(lsp_types::CompletionList {
                    is_incomplete: result.is_incomplete,
                    items,
                })
            }))
        })
    }

    fn inlay_hint(
        &mut self,
        params: lsp_types::InlayHintParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::InlayHint>>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let viewport = {
                let start = worker.to_offset(&text, params.range.start);
                let end = worker.to_offset(&text, params.range.end);
                TextRange::new(start, end)
            };
            let hints = worker
                .cancellable(|worker| ide::inlay_hints(&worker.db, file, Some(viewport)))
                .unwrap_or_default();
            Ok(Some(
                hints
                    .into_iter()
                    .map(|hint| lsp_types::InlayHint {
                        position: worker.to_position(&text, hint.offset),
                        label: lsp_types::InlayHintLabel::String(hint.label),
                        kind: Some(lsp_types::InlayHintKind::TYPE),
                        text_edits: None,
                        tooltip: None,
                        padding_left: Some(false),
                        padding_right: Some(false),
                        data: None,
                    })
                    .collect(),
            ))
        })
    }

    fn signature_help(
        &mut self,
        params: lsp_types::SignatureHelpParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::SignatureHelp>, Self::Error>> {
        self.read(move |worker| {
            let position = params.text_document_position_params;
            let Some(path) = worker.document_path(&position.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let offset = worker.to_offset(&text, position.position);
            let help = worker
                .cancellable(|worker| ide::signature_help(&worker.db, file, offset))
                .unwrap_or_default();
            Ok(help.map(|help| {
                let active_parameter = help
                    .signatures
                    .get(help.active_signature)
                    .and_then(|signature| signature.active_parameter);
                let signatures = help
                    .signatures
                    .into_iter()
                    .map(|signature| convert_signature(signature, worker.supports_label_offsets))
                    .collect();
                lsp_types::SignatureHelp {
                    signatures,
                    active_signature: Some(help.active_signature as u32),
                    active_parameter: active_parameter.map(|index| index as u32),
                }
            }))
        })
    }

    fn code_action(
        &mut self,
        params: lsp_types::CodeActionParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::CodeActionResponse>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let range = TextRange::new(
                worker.to_offset(&text, params.range.start),
                worker.to_offset(&text, params.range.end),
            );
            let actions = worker
                .cancellable(|worker| ide::code_actions(&worker.db, file, range))
                .unwrap_or_default();
            if actions.is_empty() {
                return Ok(None);
            }
            let uri = worker.document_uri(&path);
            Ok(Some(
                actions
                    .into_iter()
                    .map(|action| {
                        let edits: Vec<lsp_types::TextEdit> = action
                            .edits
                            .into_iter()
                            .map(|edit| lsp_types::TextEdit {
                                range: worker.to_range(&text, edit.range),
                                new_text: edit.replacement,
                            })
                            .collect();
                        lsp_types::CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
                            title: action.title,
                            kind: Some(lsp_types::CodeActionKind::QUICKFIX),
                            edit: Some(lsp_types::WorkspaceEdit {
                                changes: Some(HashMap::from([(uri.clone(), edits)])),
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ))
        })
    }

    fn formatting(
        &mut self,
        params: lsp_types::DocumentFormattingParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::TextEdit>>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            match format::format(&text, worker.config.format) {
                Ok(formatted) => {
                    let index = LineIndex::new(&text);
                    let end_line = index.line_count() - 1;
                    let end =
                        lsp_types::Position::new(end_line, index.line_length(end_line, &text));
                    Ok(Some(vec![lsp_types::TextEdit {
                        range: lsp_types::Range {
                            start: lsp_types::Position::new(0, 0),
                            end,
                        },
                        new_text: formatted,
                    }]))
                }
                // A refusal (syntax errors) surfaces as "no edits".
                Err(error) => {
                    tracing::error!("formatting failed: {error:?}");
                    Ok(None)
                }
            }
        })
    }

    /// Format the selection, snapped outwards to whole top-level statements
    /// and whole lines. R's top-level statements are laid out independently,
    /// because each starts at column zero and nothing above it changes its
    /// indentation. Formatting that slice alone therefore gives exactly what
    /// formatting the whole file would have given for those lines.
    fn range_formatting(
        &mut self,
        params: lsp_types::DocumentRangeFormattingParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::TextEdit>>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let selection = TextRange::new(
                worker.to_offset(&text, params.range.start),
                worker.to_offset(&text, params.range.end),
            );

            let root = syntax::parse(&text).syntax_node();
            let mut covered: Option<TextRange> = None;
            for node in root.children() {
                let node_range = node.text_range();
                // An empty selection (a bare cursor) still picks the statement
                // it sits in, so "format selection" with nothing selected
                // formats the statement under the caret rather than nothing.
                let intersects = node_range.start() < selection.end()
                    && selection.start() < node_range.end()
                    || node_range.contains(selection.start());
                if !intersects {
                    continue;
                }
                covered = Some(match covered {
                    Some(current) => current.cover(node_range),
                    None => node_range,
                });
            }
            // A selection holding no statement (blank lines, comments only)
            // has nothing to lay out.
            let Some(covered) = covered else {
                return Ok(None);
            };

            let start = text[..usize::from(covered.start())]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            let end = text[usize::from(covered.end())..]
                .find('\n')
                .map_or(text.len(), |index| usize::from(covered.end()) + index);
            let slice = &text[start..end];

            match format::format(slice, worker.config.format) {
                Ok(formatted) => {
                    // The replaced span stops before the line terminator, so
                    // the formatter's trailing newline would add a blank line.
                    let formatted = formatted.trim_end_matches('\n').to_owned();
                    if formatted == slice {
                        return Ok(Some(Vec::new()));
                    }
                    let range = worker.to_range(
                        &text,
                        TextRange::new(
                            TextSize::try_from(start).unwrap_or_default(),
                            TextSize::try_from(end).unwrap_or_default(),
                        ),
                    );
                    Ok(Some(vec![lsp_types::TextEdit {
                        range,
                        new_text: formatted,
                    }]))
                }
                // A refusal (syntax errors) surfaces as "no edits", exactly as
                // whole-document formatting does.
                Err(error) => {
                    tracing::error!("range formatting failed: {error:?}");
                    Ok(None)
                }
            }
        })
    }

    fn document_symbol(
        &mut self,
        params: lsp_types::DocumentSymbolParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::DocumentSymbolResponse>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let symbols = worker
                .cancellable(|worker| ide::document_symbols(&worker.db, file))
                .unwrap_or_default();
            let symbols: Vec<lsp_types::DocumentSymbol> = symbols
                .into_iter()
                .filter_map(|symbol| to_lsp_document_symbol(worker, &text, symbol))
                .collect();
            Ok(Some(lsp_types::DocumentSymbolResponse::Nested(symbols)))
        })
    }

    fn symbol(
        &mut self,
        params: lsp_types::WorkspaceSymbolParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::WorkspaceSymbolResponse>, Self::Error>> {
        self.read(move |worker| {
            let Some(files) = ProjectFiles::try_get(&worker.db) else {
                return Ok(None);
            };
            let query = params.query;
            let symbols = worker
                .cancellable(|worker| ide::workspace_symbols(&worker.db, files, &query))
                .unwrap_or_default();
            #[allow(deprecated)]
            let symbols: Vec<lsp_types::SymbolInformation> = symbols
                .into_iter()
                .filter_map(|symbol| {
                    let path = worker.path_of(symbol.target.file)?;
                    Some(lsp_types::SymbolInformation {
                        name: symbol.name,
                        kind: lsp_document_symbol_kind(symbol.kind),
                        tags: None,
                        deprecated: None,
                        location: lsp_types::Location {
                            uri: worker.document_uri(path),
                            range: worker.to_range_in(symbol.target.file, symbol.target.range),
                        },
                        container_name: None,
                    })
                })
                .collect();
            Ok(Some(lsp_types::WorkspaceSymbolResponse::Flat(symbols)))
        })
    }

    fn folding_range(
        &mut self,
        params: lsp_types::FoldingRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<lsp_types::FoldingRange>>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            Ok(Some(folding_ranges(&worker.db, file, &text)))
        })
    }

    fn semantic_tokens_full(
        &mut self,
        params: lsp_types::SemanticTokensParams,
    ) -> BoxFuture<'static, Result<Option<lsp_types::SemanticTokensResult>, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(None);
            };
            if let Some(stub) = worker.stub_documents.get(&path).cloned() {
                let tokens = stub_semantic_tokens(&stub, worker.encoding);
                return Ok(Some(lsp_types::SemanticTokensResult::Tokens(
                    lsp_types::SemanticTokens {
                        result_id: None,
                        data: tokens,
                    },
                )));
            }
            let Some(&file) = worker.files.get(&path) else {
                return Ok(None);
            };
            let text = worker.text(file);
            let tokens = annotation_semantic_tokens(&worker.db, file, &text, worker.encoding);
            Ok(Some(lsp_types::SemanticTokensResult::Tokens(
                lsp_types::SemanticTokens {
                    result_id: None,
                    data: tokens,
                },
            )))
        })
    }

    fn document_diagnostic(
        &mut self,
        params: lsp_types::DocumentDiagnosticParams,
    ) -> BoxFuture<'static, Result<lsp_types::DocumentDiagnosticReportResult, Self::Error>> {
        self.read(move |worker| {
            let Some(path) = worker.document_path(&params.text_document.uri) else {
                return Ok(empty_full_diagnostic_report());
            };
            if let Some(stub) = worker.stub_documents.get(&path).cloned() {
                let diagnostics = worker.stub_diagnostics(&stub);
                return Ok(diagnostic_report(diagnostics, params.previous_result_id));
            }
            if let Some(namespace) = worker.namespace_documents.get(&path).cloned() {
                let diagnostics = worker.namespace_diagnostics(&namespace);
                return Ok(diagnostic_report(diagnostics, params.previous_result_id));
            }
            if !worker.files.contains_key(&path) {
                // A pull may legitimately target an untracked document.
                return Ok(empty_full_diagnostic_report());
            }
            // Fault injection for the cancelled-pull test: announce the
            // in-flight pull through the marker file, then hold until the
            // edit's cancellation flip lands (bounded by the configured
            // delay). The marker lets the test order its edit strictly after
            // the pull has started, so the flip provably targets this pull's
            // token rather than one a queued job would refresh away.
            if let Some(delay) = std::env::var("RY_TEST_DELAY_PULL_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
            {
                if let Ok(marker) = std::env::var("RY_TEST_PULL_MARKER") {
                    std::fs::write(&marker, b"pulling")
                        .expect("the cancelled-pull test marker must be writable");
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(delay);
                while !worker.current_token.is_cancelled() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            match worker.cancellable(|worker| worker.settled_diagnostics(&path)) {
                Ok(Some(diagnostics)) => {
                    Ok(diagnostic_report(diagnostics, params.previous_result_id))
                }
                Ok(None) => Ok(empty_full_diagnostic_report()),
                // A pull report is authoritative and cached: a cancelled read
                // must never become "this document is clean".
                Err(_) => Err(diagnostics_cancelled_error()),
            }
        })
    }
}

/// Recursive outline conversion; entries with empty names are dropped (the
/// protocol forbids them).
#[allow(deprecated)] // `deprecated` is a required field of `DocumentSymbol`
fn to_lsp_document_symbol(
    worker: &Worker,
    text: &str,
    symbol: ide::DocumentSymbol,
) -> Option<lsp_types::DocumentSymbol> {
    if symbol.name.is_empty() {
        return None;
    }
    let children: Vec<lsp_types::DocumentSymbol> = symbol
        .children
        .into_iter()
        .filter_map(|child| to_lsp_document_symbol(worker, text, child))
        .collect();
    Some(lsp_types::DocumentSymbol {
        name: symbol.name,
        detail: symbol.detail,
        kind: lsp_document_symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: worker.to_range(text, symbol.range),
        selection_range: worker.to_range(text, symbol.selection),
        children: (!children.is_empty()).then_some(children),
    })
}

fn lsp_document_symbol_kind(kind: ide::DocumentSymbolKind) -> lsp_types::SymbolKind {
    match kind {
        ide::DocumentSymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        ide::DocumentSymbolKind::Value => lsp_types::SymbolKind::VARIABLE,
        ide::DocumentSymbolKind::TypeDefinition => lsp_types::SymbolKind::STRUCT,
        ide::DocumentSymbolKind::AliasDefinition | ide::DocumentSymbolKind::S4Generic => {
            lsp_types::SymbolKind::INTERFACE
        }
        ide::DocumentSymbolKind::S4Class | ide::DocumentSymbolKind::R6Class => {
            lsp_types::SymbolKind::CLASS
        }
        ide::DocumentSymbolKind::S4Method | ide::DocumentSymbolKind::R6Method => {
            lsp_types::SymbolKind::METHOD
        }
        ide::DocumentSymbolKind::R6Field => lsp_types::SymbolKind::FIELD,
    }
}

fn convert_completion_item(item: ide::CompletionItem, snippets: bool) -> lsp_types::CompletionItem {
    let kind = match item.kind {
        ide::CompletionKind::Keyword => lsp_types::CompletionItemKind::KEYWORD,
        ide::CompletionKind::Variable => lsp_types::CompletionItemKind::VARIABLE,
        ide::CompletionKind::Function => lsp_types::CompletionItemKind::FUNCTION,
        ide::CompletionKind::Field => lsp_types::CompletionItemKind::FIELD,
    };
    let mut converted = lsp_types::CompletionItem {
        label: item.label.clone(),
        kind: Some(kind),
        detail: item.detail,
        documentation: item.documentation.map(|documentation| {
            lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: documentation,
            })
        }),
        ..Default::default()
    };
    if snippets && item.kind == ide::CompletionKind::Function {
        // A known zero-argument signature drops the cursor past the parens;
        // everything else (arguments or unknown) drops it between them.
        converted.insert_text = Some(if item.takes_arguments == Some(false) {
            format!("{}()$0", item.label)
        } else {
            format!("{}($0)", item.label)
        });
        converted.insert_text_format = Some(lsp_types::InsertTextFormat::SNIPPET);
        converted.command = Some(lsp_types::Command {
            title: "trigger parameter hints".to_owned(),
            command: "editor.action.triggerParameterHints".to_owned(),
            arguments: None,
        });
    }
    converted
}

/// Label offsets are always UTF-16 code units per the LSP spec, independent
/// of the negotiated document encoding.
fn convert_signature(
    signature: ide::SignatureData,
    label_offsets: bool,
) -> lsp_types::SignatureInformation {
    let parameters: Vec<lsp_types::ParameterInformation> = signature
        .parameters
        .iter()
        .map(|span| {
            let prefix = &signature.label[..usize::from(span.start())];
            let text = &signature.label[usize::from(span.start())..usize::from(span.end())];
            let label = if label_offsets {
                let start = prefix.encode_utf16().count() as u32;
                let length = text.encode_utf16().count() as u32;
                lsp_types::ParameterLabel::LabelOffsets([start, start + length])
            } else {
                lsp_types::ParameterLabel::Simple(text.to_owned())
            };
            lsp_types::ParameterInformation {
                label,
                documentation: None,
            }
        })
        .collect();
    lsp_types::SignatureInformation {
        label: signature.label,
        documentation: None,
        parameters: Some(parameters),
        active_parameter: signature.active_parameter.map(|index| index as u32),
    }
}

fn empty_full_diagnostic_report() -> lsp_types::DocumentDiagnosticReportResult {
    lsp_types::DocumentDiagnosticReportResult::Report(lsp_types::DocumentDiagnosticReport::Full(
        lsp_types::RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                result_id: None,
                items: Vec::new(),
            },
        },
    ))
}

/// A content hash of the serialized diagnostics. It stays correct under an
/// inter-file dependency, where a document's own version does not move but its
/// diagnostics do.
fn diagnostics_result_id(diagnostics: &[lsp_types::Diagnostic]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    serde_json::to_string(diagnostics)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn diagnostic_report(
    items: Vec<lsp_types::Diagnostic>,
    previous_result_id: Option<String>,
) -> lsp_types::DocumentDiagnosticReportResult {
    let result_id = diagnostics_result_id(&items);
    if previous_result_id.as_deref() == Some(result_id.as_str()) {
        return lsp_types::DocumentDiagnosticReportResult::Report(
            lsp_types::DocumentDiagnosticReport::Unchanged(
                lsp_types::RelatedUnchangedDocumentDiagnosticReport {
                    related_documents: None,
                    unchanged_document_diagnostic_report:
                        lsp_types::UnchangedDocumentDiagnosticReport { result_id },
                },
            ),
        );
    }
    lsp_types::DocumentDiagnosticReportResult::Report(lsp_types::DocumentDiagnosticReport::Full(
        lsp_types::RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: lsp_types::FullDocumentDiagnosticReport {
                result_id: Some(result_id),
                items,
            },
        },
    ))
}

/// Retryable cancellation: the wire shape `{"retriggerRequest": true}` makes
/// the client re-pull instead of caching an empty answer.
fn diagnostics_cancelled_error() -> ResponseError {
    ResponseError::new_with_data(
        ErrorCode::SERVER_CANCELLED,
        "diagnostics were cancelled by a concurrent edit",
        serde_json::to_value(lsp_types::DiagnosticServerCancellationData {
            retrigger_request: true,
        })
        .expect("cancellation data serializes"),
    )
}

/// Valid R identifier: a letter or `.` (not followed by a digit) first, then
/// alphanumerics, `.`, `_`; reserved words excluded.
fn is_valid_r_identifier(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "if",
        "else",
        "repeat",
        "while",
        "function",
        "for",
        "in",
        "next",
        "break",
        "TRUE",
        "FALSE",
        "NULL",
        "Inf",
        "NaN",
        "NA",
        "NA_integer_",
        "NA_real_",
        "NA_character_",
        "NA_complex_",
    ];
    if RESERVED.contains(&name) {
        return false;
    }
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '.') {
        return false;
    }
    if first == '.'
        && name
            .chars()
            .nth(1)
            .is_some_and(|second| second.is_ascii_digit())
    {
        return false;
    }
    name.chars()
        .all(|character| character.is_alphanumeric() || character == '.' || character == '_')
}

/// Region folds for multi-line brace/paren/argument-list nodes (keeping the
/// closing delimiter visible) and comment folds for consecutive comment runs
/// (`#:` blocks fold as one region).
fn folding_ranges(db: &RootDatabase, file: SourceFile, text: &str) -> Vec<lsp_types::FoldingRange> {
    let index = LineIndex::new(text);
    let parse = semantics::parse(db, file);
    let mut ranges = Vec::new();
    for node in parse.syntax_node().descendants() {
        if matches!(
            node.kind(),
            SyntaxKind::BRACE_EXPR | SyntaxKind::PAREN_EXPR | SyntaxKind::ARGUMENT_LIST
        ) {
            let start = index.line_column(node.text_range().start());
            let end = index.line_column(node.text_range().end());
            if end.line > start.line {
                ranges.push(lsp_types::FoldingRange {
                    start_line: start.line,
                    start_character: None,
                    end_line: end.line - 1,
                    end_character: None,
                    kind: Some(lsp_types::FoldingRangeKind::Region),
                    collapsed_text: None,
                });
            }
        }
    }
    // Comment runs: consecutive lines whose first token is a comment.
    let mut run_start: Option<u32> = None;
    let mut previous_line: Option<u32> = None;
    let mut comment_lines: Vec<u32> = Vec::new();
    for element in parse.syntax_node().descendants_with_tokens() {
        if let syntax::SyntaxElement::Token(token) = element
            && matches!(token.kind(), SyntaxKind::COMMENT)
        {
            comment_lines.push(index.line_column(token.text_range().start()).line);
        }
    }
    comment_lines.sort_unstable();
    comment_lines.dedup();
    for line in comment_lines {
        match (run_start, previous_line) {
            (Some(start), Some(previous)) if line == previous + 1 => {
                previous_line = Some(line);
                let _ = start;
            }
            (Some(start), Some(previous)) => {
                if previous > start {
                    ranges.push(comment_fold(start, previous));
                }
                run_start = Some(line);
                previous_line = Some(line);
            }
            _ => {
                run_start = Some(line);
                previous_line = Some(line);
            }
        }
    }
    if let (Some(start), Some(previous)) = (run_start, previous_line)
        && previous > start
    {
        ranges.push(comment_fold(start, previous));
    }
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges
}

fn comment_fold(start: u32, end: u32) -> lsp_types::FoldingRange {
    lsp_types::FoldingRange {
        start_line: start,
        start_character: None,
        end_line: end,
        end_character: None,
        kind: Some(lsp_types::FoldingRangeKind::Comment),
        collapsed_text: None,
    }
}

/// Delta-encoded semantic tokens for the `#:` annotation regions: type names
/// color as types, `<T>` binders as type parameters, record/parameter names
/// as parameters, punctuation as operators, `@` directives as decorators.
fn annotation_semantic_tokens(
    db: &RootDatabase,
    file: SourceFile,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<lsp_types::SemanticToken> {
    let parse = semantics::parse(db, file);
    let mut spans: Vec<(TextRange, u32)> = Vec::new();
    for annotation in parse
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::ANNOTATION)
    {
        for element in annotation.descendants_with_tokens() {
            let syntax::SyntaxElement::Token(token) = element else {
                continue;
            };
            if let Some(token_type) = classify_annotation_token(&token)
                && !token.text_range().is_empty()
            {
                spans.push((token.text_range(), token_type));
            }
        }
    }
    delta_encode_tokens(spans, text, encoding)
}

/// The semantic-token class of one token inside an annotation region, per
/// the advertised legend: type names 0, `<T>` binders 1, field/parameter
/// names 2, punctuation 3, `@` directives 4.
fn classify_annotation_token(token: &syntax::SyntaxToken) -> Option<u32> {
    let parent_kind = token.parent().map(|parent| parent.kind());
    match token.kind() {
        SyntaxKind::IDENT => match parent_kind {
            Some(SyntaxKind::TYPE_BINDER) => Some(1),
            Some(SyntaxKind::TYPE_FIELD) | Some(SyntaxKind::TYPE_FUNCTION) => Some(2),
            Some(SyntaxKind::ANNOTATION_DIRECTIVE) => Some(4),
            _ => Some(0),
        },
        SyntaxKind::COMMA
        | SyntaxKind::PIPE
        | SyntaxKind::COLON
        | SyntaxKind::L_BRACKET
        | SyntaxKind::R_BRACKET
        | SyntaxKind::L_PAREN
        | SyntaxKind::R_PAREN
        | SyntaxKind::L_BRACE
        | SyntaxKind::R_BRACE
        | SyntaxKind::LESS
        | SyntaxKind::GREATER
        | SyntaxKind::DOTS => Some(3),
        SyntaxKind::AT => Some(4),
        SyntaxKind::NULL_KW => Some(0),
        _ => None,
    }
}

/// Tokens for a `.Rtypes` stub buffer: each declaration's type half runs
/// through the annotation grammar (offset back into the buffer), `@type` and
/// `@masked` color as directives, and declared type names as types.
fn stub_semantic_tokens(text: &str, encoding: PositionEncoding) -> Vec<lsp_types::SemanticToken> {
    let index = LineIndex::new(text);
    let mut spans: Vec<(TextRange, u32)> = Vec::new();
    for (line_number, raw_line) in text.lines().enumerate() {
        let line_start = index.line_start(line_number as u32) as usize;
        let content = match raw_line.find('#') {
            Some(at) => &raw_line[..at],
            None => raw_line,
        };
        let trimmed = content.trim_start();
        let indent = content.len() - trimmed.len();
        let trimmed = trimmed.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let push = |spans: &mut Vec<(TextRange, u32)>, start: usize, len: usize, class: u32| {
            let absolute = (line_start + start) as u32;
            spans.push((
                TextRange::new(absolute.into(), (absolute + len as u32).into()),
                class,
            ));
        };
        if let Some(rest) = trimmed.strip_prefix("@type") {
            push(&mut spans, indent, "@type".len(), 4);
            let name = rest.trim_start();
            if !name.is_empty() {
                let name_start = indent + trimmed.len() - name.len();
                push(&mut spans, name_start, name.len(), 0);
            }
            continue;
        }
        let Some(colon) = trimmed.find(" :") else {
            continue;
        };
        let mut type_offset = indent + colon + 2;
        let mut type_text = &trimmed[colon + 2..];
        let leading = type_text.len() - type_text.trim_start().len();
        type_offset += leading;
        type_text = type_text.trim_start();
        if let Some(rest) = type_text.strip_prefix("@masked") {
            push(&mut spans, type_offset, "@masked".len(), 4);
            let after = rest.trim_start();
            type_offset += type_text.len() - after.len();
            type_text = after;
        }
        if type_text.is_empty() {
            continue;
        }
        // The wrapped parse prefixes "#: " (3 bytes) before the type text.
        let parse = syntax::parse(&format!("#: {type_text}\n"));
        for element in parse.syntax_node().descendants_with_tokens() {
            let syntax::SyntaxElement::Token(token) = element else {
                continue;
            };
            let range = token.text_range();
            if u32::from(range.start()) < 3 || range.is_empty() {
                continue;
            }
            if let Some(class) = classify_annotation_token(&token) {
                let start = usize::from(range.start()) - 3;
                let length = usize::from(range.len());
                if start + length <= type_text.len() + 1 {
                    push(&mut spans, type_offset + start, length, class);
                }
            }
        }
    }
    delta_encode_tokens(spans, text, encoding)
}

fn delta_encode_tokens(
    mut spans: Vec<(TextRange, u32)>,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<lsp_types::SemanticToken> {
    let index = LineIndex::new(text);
    spans.sort_by_key(|(range, _)| range.start());
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for (range, token_type) in spans {
        let position = match encoding {
            PositionEncoding::Utf8 => index.line_column(range.start()),
            PositionEncoding::Utf16 => index.line_column_utf16(range.start(), text),
        };
        let length = match encoding {
            PositionEncoding::Utf8 => range.len().into(),
            PositionEncoding::Utf16 => text[usize::from(range.start())..usize::from(range.end())]
                .encode_utf16()
                .count() as u32,
        };
        let delta_line = position.line - previous_line;
        let delta_start = if delta_line == 0 {
            position.column - previous_start
        } else {
            position.column
        };
        data.push(lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });
        previous_line = position.line;
        previous_start = position.column;
    }
    data
}

/// Goto for a type name inside a `.Rtypes` buffer: the `@type NAME`
/// Writes the embedded shipped stubs to a per-version cache directory so
/// hover locations and goto-definition have real files to land in (the
/// sources are compiled into the binary, and rust-analyzer materializes
/// sysroot sources the same way). Any failure degrades that source to no
/// location, and analysis itself never depends on these files.
fn materialize_shipped_stubs(shipped: &[(String, String)]) -> Vec<Option<PathBuf>> {
    let Some(cache) = dirs::cache_dir() else {
        return vec![None; shipped.len()];
    };
    let directory = cache
        .join("ry")
        .join("stubs")
        .join(env!("CARGO_PKG_VERSION"));
    if let Err(error) = std::fs::create_dir_all(&directory) {
        tracing::warn!(
            "stub locations unavailable: cannot create {}: {error}",
            directory.display()
        );
        return vec![None; shipped.len()];
    }
    shipped
        .iter()
        .map(|(namespace, text)| {
            let path = directory.join(format!("{namespace}.Rtypes"));
            let current = std::fs::read_to_string(&path).ok();
            if current.as_deref() != Some(text.as_str())
                && let Err(error) = std::fs::write(&path, text)
            {
                tracing::warn!("stub location unavailable: {}: {error}", path.display());
                return None;
            }
            Some(path)
        })
        .collect()
}

/// declaration line in the same file (stub files are self-contained).
fn stub_type_definition(text: &str, offset: TextSize) -> Option<TextRange> {
    let is_name_char =
        |character: char| character.is_alphanumeric() || character == '.' || character == '_';
    let at = usize::from(offset).min(text.len());
    let start = text[..at]
        .rfind(|character| !is_name_char(character))
        .map(|found| found + 1)
        .unwrap_or(0);
    let end = text[at..]
        .find(|character| !is_name_char(character))
        .map(|found| at + found)
        .unwrap_or(text.len());
    if start >= end {
        return None;
    }
    let word = &text[start..end];
    let index = LineIndex::new(text);
    for (line_number, raw_line) in text.lines().enumerate() {
        let trimmed = raw_line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("@type")
            && rest.trim() == word
        {
            let indent = raw_line.len() - trimmed.len();
            let name_offset = indent + trimmed.len() - rest.trim_start().len();
            let line_start = index.line_start(line_number as u32) as usize;
            let start = (line_start + name_offset) as u32;
            return Some(TextRange::new(
                start.into(),
                (start + word.len() as u32).into(),
            ));
        }
    }
    None
}
