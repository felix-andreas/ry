//! Behavioral tests for the language server, driving the real `ry
//! server` binary over LSP stdio: capability negotiation, document sync,
//! push/pull diagnostics, position encodings, and the feature endpoints.

use async_lsp::LanguageServer;
use async_lsp::concurrency::{Concurrency, ConcurrencyLayer};
use async_lsp::lsp_types::notification::{PublishDiagnostics, ShowMessage};
use async_lsp::lsp_types::request::{RegisterCapability, WorkspaceDiagnosticRefresh};
use async_lsp::lsp_types::{
    ClientCapabilities, DiagnosticClientCapabilities, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DocumentDiagnosticParams, DocumentDiagnosticReport,
    DocumentDiagnosticReportResult, DocumentFormattingParams, DocumentRangeFormattingParams,
    FormattingOptions, GeneralClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse,
    HoverContents, HoverParams, InitializeParams, InitializeResult, InitializedParams, OneOf,
    PartialResultParams, Position, PositionEncodingKind, PublishDiagnosticsParams, Range,
    ShowMessageParams, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit, Url,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceFolder,
};
use async_lsp::panic::{CatchUnwind, CatchUnwindLayer};
use async_lsp::router::Router;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

struct TestClientState {
    diagnostics_sender: mpsc::UnboundedSender<PublishDiagnosticsParams>,
    refresh_sender: mpsc::UnboundedSender<()>,
    messages_sender: mpsc::UnboundedSender<ShowMessageParams>,
}

struct Stop;

type TestService = CatchUnwind<Concurrency<Router<TestClientState>>>;

fn build_test_client(
    diagnostics_sender: mpsc::UnboundedSender<PublishDiagnosticsParams>,
    refresh_sender: mpsc::UnboundedSender<()>,
    messages_sender: mpsc::UnboundedSender<ShowMessageParams>,
) -> (async_lsp::MainLoop<TestService>, async_lsp::ServerSocket) {
    async_lsp::MainLoop::new_client(|_server| {
        let mut router = Router::new(TestClientState {
            diagnostics_sender,
            refresh_sender,
            messages_sender,
        });
        router.notification::<PublishDiagnostics>(|state, params| {
            state
                .diagnostics_sender
                .send(params)
                .expect("diagnostics channel closed unexpectedly");
            ControlFlow::Continue(())
        });
        router.request::<RegisterCapability, _>(|_, _| std::future::ready(Ok(())));
        router.request::<WorkspaceDiagnosticRefresh, _>(|state, _| {
            let _ = state.refresh_sender.send(());
            std::future::ready(Ok(()))
        });
        router.notification::<ShowMessage>(|state, params| {
            let _ = state.messages_sender.send(params);
            ControlFlow::Continue(())
        });
        router.event(|_, _: Stop| ControlFlow::Break(Ok(())));
        ServiceBuilder::new()
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(router)
    })
}

fn spawn_server(server_cwd: &Path, envs: &[(&str, &str)], args: &[&str]) -> tokio::process::Child {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("server")
        .args(args)
        .current_dir(server_cwd)
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn ry server")
}

/// The push channel plus a stash: deferred semantic publishes for different
/// documents interleave in idle order, so a wait targeted at one URI must
/// keep — not drop — the publishes it skips for other URIs.
struct DiagnosticsChannel {
    receiver: mpsc::UnboundedReceiver<PublishDiagnosticsParams>,
    stash: Vec<PublishDiagnosticsParams>,
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// The SETTLED diagnostics for `uri`: skips (and consumes) the versioned
/// first-wave publishes and returns the next version-less publish.
async fn recv_diagnostics(
    channel: &mut DiagnosticsChannel,
    uri: &Url,
    timeout_duration: Duration,
) -> PublishDiagnosticsParams {
    if let Some(index) = channel
        .stash
        .iter()
        .position(|params| params.uri == *uri && params.version.is_none())
    {
        let params = channel.stash.remove(index);
        let kept_prefix: Vec<_> = channel
            .stash
            .drain(..index)
            .filter(|stashed| stashed.uri != *uri)
            .collect();
        channel.stash.splice(..0, kept_prefix);
        return params;
    }
    tokio::time::timeout(timeout_duration, async {
        loop {
            let params = channel
                .receiver
                .recv()
                .await
                .expect("diagnostics channel closed");
            if params.uri == *uri {
                if params.version.is_none() {
                    return params;
                }
                continue;
            }
            channel.stash.push(params);
        }
    })
    .await
    .expect("timed out waiting for diagnostics")
}

/// The FIRST publish for `uri`, whichever wave it is.
async fn recv_first_diagnostics(
    channel: &mut DiagnosticsChannel,
    uri: &Url,
    timeout_duration: Duration,
) -> PublishDiagnosticsParams {
    if let Some(index) = channel.stash.iter().position(|params| params.uri == *uri) {
        return channel.stash.remove(index);
    }
    tokio::time::timeout(timeout_duration, async {
        loop {
            let params = channel
                .receiver
                .recv()
                .await
                .expect("diagnostics channel closed");
            if params.uri == *uri {
                return params;
            }
            channel.stash.push(params);
        }
    })
    .await
    .expect("timed out waiting for diagnostics")
}

struct TestContext {
    server: async_lsp::ServerSocket,
    diagnostics_receiver: DiagnosticsChannel,
    /// Wired for the diagnostic-refresh tests that follow in the full port.
    #[allow(dead_code)]
    refresh_receiver: mpsc::UnboundedReceiver<()>,
    messages_receiver: mpsc::UnboundedReceiver<ShowMessageParams>,
    mainloop_handle: tokio::task::JoinHandle<()>,
    init_result: InitializeResult,
    _temp_dir: tempfile::TempDir,
    workspace_dir: PathBuf,
}

impl TestContext {
    fn workspace_uri(&self, relative: &str) -> Url {
        Url::from_file_path(self.workspace_dir.join(relative)).expect("workspace file URI")
    }

    async fn open(&mut self, relative: &str, text: &str) -> Url {
        let uri = self.workspace_uri(relative);
        self.server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "r".into(),
                    version: 1,
                    text: text.to_owned(),
                },
            })
            .expect("didOpen failed");
        uri
    }

    async fn shutdown(mut self) {
        let _ = self.server.shutdown(()).await;
        let _ = self.server.exit(());
        let _ = tokio::time::timeout(TIMEOUT, self.mainloop_handle).await;
    }
}

async fn setup_test_inner(
    create_r_directory: bool,
    initial_files: &[(&str, &str)],
    capabilities: ClientCapabilities,
) -> TestContext {
    setup_test_with_env(create_r_directory, initial_files, capabilities, &[]).await
}

async fn setup_test_with_env(
    create_r_directory: bool,
    initial_files: &[(&str, &str)],
    capabilities: ClientCapabilities,
    envs: &[(&str, &str)],
) -> TestContext {
    setup_test_with_env_and_args(create_r_directory, initial_files, capabilities, envs, &[]).await
}

async fn setup_test_with_env_and_args(
    create_r_directory: bool,
    initial_files: &[(&str, &str)],
    capabilities: ClientCapabilities,
    envs: &[(&str, &str)],
    args: &[&str],
) -> TestContext {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    // The server process runs with the temp ROOT as its cwd while the client
    // announces the `workspace` subdirectory as the workspace root, so every
    // test exercises root-from-initialize rather than root-from-cwd.
    let server_cwd = temp_dir.path().to_path_buf();
    let workspace_dir = temp_dir.path().join("workspace");
    std::fs::create_dir_all(&workspace_dir).expect("failed to create workspace directory");
    if create_r_directory {
        std::fs::create_dir_all(workspace_dir.join("R")).expect("failed to create R directory");
    }
    for (relative_path, text) in initial_files {
        let path = workspace_dir.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        std::fs::write(path, text).expect("failed to write initial test file");
    }

    let (diagnostics_sender, diagnostics_receiver) = mpsc::unbounded_channel();
    let (refresh_sender, refresh_receiver) = mpsc::unbounded_channel();
    let (messages_sender, messages_receiver) = mpsc::unbounded_channel();
    let (mainloop, mut server) =
        build_test_client(diagnostics_sender, refresh_sender, messages_sender);

    let mut child = spawn_server(&server_cwd, envs, args);
    let stdout = child.stdout.take().expect("missing stdout").compat();
    let stdin = child.stdin.take().expect("missing stdin").compat_write();
    let mainloop_handle = tokio::spawn(async move {
        if let Err(error) = mainloop.run_buffered(stdout, stdin).await {
            eprintln!("mainloop error: {error}");
        }
        drop(child);
    });

    let root_uri = Url::from_file_path(&workspace_dir).expect("invalid workspace path");
    let init_result = server
        .initialize(InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "root".into(),
            }]),
            capabilities,
            ..InitializeParams::default()
        })
        .await
        .expect("initialize failed");
    server
        .initialized(InitializedParams {})
        .expect("initialized notification failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    TestContext {
        server,
        diagnostics_receiver: DiagnosticsChannel {
            receiver: diagnostics_receiver,
            stash: Vec::new(),
        },
        refresh_receiver,
        messages_receiver,
        mainloop_handle,
        init_result,
        _temp_dir: temp_dir,
        workspace_dir,
    }
}

async fn setup_test(initial_files: &[(&str, &str)]) -> TestContext {
    setup_test_inner(true, initial_files, ClientCapabilities::default()).await
}

async fn setup_test_with_position_encodings(
    initial_files: &[(&str, &str)],
    position_encodings: &[PositionEncodingKind],
) -> TestContext {
    let capabilities = ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(position_encodings.to_vec()),
            ..GeneralClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    setup_test_inner(true, initial_files, capabilities).await
}

async fn setup_test_with_pull_diagnostics(initial_files: &[(&str, &str)]) -> TestContext {
    let capabilities = ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..TextDocumentClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    setup_test_inner(true, initial_files, capabilities).await
}

fn position_params(uri: &Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(line, character),
    }
}

//
// Tests
//

#[tokio::test]
async fn initialize_reports_capabilities() {
    let context = setup_test(&[]).await;
    let capabilities = &context.init_result.capabilities;
    assert!(capabilities.hover_provider.is_some());
    assert!(capabilities.definition_provider.is_some());
    assert!(capabilities.references_provider.is_some());
    assert!(capabilities.rename_provider.is_some());
    assert!(capabilities.completion_provider.is_some());
    assert!(capabilities.document_formatting_provider.is_some());
    assert_eq!(
        capabilities.document_range_formatting_provider,
        Some(OneOf::Left(true)),
        "range formatting is a shipped feature, not one a flag turns on"
    );
    assert!(capabilities.document_symbol_provider.is_some());
    assert!(capabilities.inlay_hint_provider.is_some());
    assert!(capabilities.signature_help_provider.is_some());
    assert!(capabilities.semantic_tokens_provider.is_some());
    assert!(capabilities.diagnostic_provider.is_some());
    assert_eq!(
        context
            .init_result
            .server_info
            .as_ref()
            .map(|info| info.name.as_str()),
        Some("ry")
    );
    context.shutdown().await;
}

#[tokio::test]
async fn initialize_negotiates_utf16_by_default() {
    let context = setup_test(&[]).await;
    assert_eq!(
        context.init_result.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16)
    );
    context.shutdown().await;
}

#[tokio::test]
async fn initialize_negotiates_utf8_when_offered() {
    let context = setup_test_with_position_encodings(
        &[],
        &[PositionEncodingKind::UTF8, PositionEncodingKind::UTF16],
    )
    .await;
    assert_eq!(
        context.init_result.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF8)
    );
    context.shutdown().await;
}

#[tokio::test]
async fn diagnostics_on_open() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/bad.R", "x = 1\n").await;
    let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert_eq!(params.diagnostics.len(), 1, "{:?}", params.diagnostics);
    assert!(
        params.diagnostics[0]
            .message
            .contains("Use <-, not =, for assignment"),
        "{:?}",
        params.diagnostics
    );
    context.shutdown().await;
}

#[tokio::test]
async fn no_diagnostics_for_clean_file() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/clean.R", "x <- 1\n").await;
    let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(params.diagnostics.is_empty(), "{:?}", params.diagnostics);
    context.shutdown().await;
}

#[tokio::test]
async fn diagnostics_on_syntax_error() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/bad.R", "x <- (\n").await;
    let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(
        !params.diagnostics.is_empty(),
        "expected syntax diagnostics"
    );
    assert!(
        params.diagnostics[0].message.contains("unclosed `(`"),
        "{:?}",
        params.diagnostics
    );
    context.shutdown().await;
}

#[tokio::test]
async fn first_wave_publishes_syntax_errors_before_semantics() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open("R/bad.R", "x <- (\nbad_type <- 1L + \"a\"\n")
        .await;
    let first = recv_first_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    // The first wave is versioned and carries the cheap classes.
    assert_eq!(first.version, Some(1));
    assert!(
        first
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unclosed `(`")),
        "{:?}",
        first.diagnostics
    );
    context.shutdown().await;
}

#[tokio::test]
async fn burst_of_edits_settles_on_the_final_text() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/burst.R", "x <- 1\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    for version in 2..=6 {
        let text = if version == 6 {
            "x = 1\n".to_owned()
        } else {
            format!("x <- {version}\n")
        };
        context
            .server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            })
            .expect("didChange failed");
    }
    // Eventually the settled set reflects the final text: one lint warning.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
        if params.diagnostics.len() == 1
            && params.diagnostics[0]
                .message
                .contains("Use <-, not =, for assignment")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never settled on the final text: {:?}",
            params.diagnostics
        );
    }
    context.shutdown().await;
}

#[tokio::test]
async fn hover_returns_type() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/hover.R", "count <- 1L\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: position_params(&uri, 0, 2),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover failed")
        .expect("expected a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markdown hover");
    };
    assert!(markup.value.contains("integer"), "{}", markup.value);
    context.shutdown().await;
}

#[tokio::test]
async fn hover_range_under_utf16_with_non_bmp_emoji() {
    let mut context = setup_test(&[]).await;
    // The emoji is 4 UTF-8 bytes / 2 UTF-16 units inside the string.
    let uri = context
        .open("R/emoji.R", "s <- \"😀\"\ncount <- 1L\n")
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: position_params(&uri, 1, 2),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover failed")
        .expect("expected a hover");
    let range = hover.range.expect("hover range");
    assert_eq!(range.start, Position::new(1, 0));
    assert_eq!(range.end, Position::new(1, 5));
    context.shutdown().await;
}

#[tokio::test]
async fn goto_definition() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "R/def.R",
            "helper <- function(x) x\ncaller <- function() helper(1)\n",
        )
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let response = context
        .server
        .definition(GotoDefinitionParams {
            text_document_position_params: position_params(&uri, 1, 22),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("definition failed")
        .expect("expected a definition");
    let GotoDefinitionResponse::Scalar(location) = response else {
        panic!("expected a scalar definition");
    };
    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start, Position::new(0, 0));
    assert_eq!(location.range.end, Position::new(0, 6));
    context.shutdown().await;
}

#[tokio::test]
async fn goto_definition_into_stub_corpus() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/stub.R", "sizes <- head(1:9)\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let response = context
        .server
        .definition(GotoDefinitionParams {
            text_document_position_params: position_params(&uri, 0, 10),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("definition failed")
        .expect("expected a definition into the stub corpus");
    let GotoDefinitionResponse::Scalar(location) = response else {
        panic!("expected a scalar definition");
    };
    let path = location.uri.path();
    assert!(
        path.ends_with("utils.Rtypes"),
        "expected the materialized utils stub, got {path}"
    );
    let text = std::fs::read_to_string(
        location
            .uri
            .to_file_path()
            .expect("stub location is a file path"),
    )
    .expect("the materialized stub file exists on disk");
    let line = text
        .lines()
        .nth(location.range.start.line as usize)
        .expect("the location's line exists");
    assert!(
        line.starts_with("head :"),
        "the location points at head's declaration, got: {line}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn hover_names_the_stub_declaration_site() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/stubhover.R", "sizes <- head(1:9)\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: position_params(&uri, 0, 10),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover failed")
        .expect("expected a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup hover contents");
    };
    assert!(
        markup.value.contains("From the `utils` package")
            && markup.value.contains("Declared at `utils.Rtypes:"),
        "expected the stub declaration location in: {}",
        markup.value
    );
    context.shutdown().await;
}

#[tokio::test]
async fn formatting() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting failed")
        .expect("expected formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "x <- 1\n");
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    context.shutdown().await;
}

/// Ask for the edits that format `range` of the open document.
async fn range_edits(context: &mut TestContext, uri: &Url, range: Range) -> Option<Vec<TextEdit>> {
    context
        .server
        .range_formatting(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range,
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("range formatting failed")
}

#[tokio::test]
async fn range_formatting_takes_the_statement_the_selection_touches() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\nz<-3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    // Part of the middle line only: the edit covers that whole line and
    // leaves its neighbours alone.
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 1), Position::new(1, 2)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\n");
    assert_eq!(edits[0].range.start, Position::new(1, 0));
    assert_eq!(edits[0].range.end, Position::new(2, 0));
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_takes_the_line_a_bare_caret_sits_on() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\nz<-3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 2), Position::new(1, 2)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\n");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_leaves_the_line_a_whole_line_selection_stops_at() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\nz<-3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    // Selecting one whole line sends (1,0)..(2,0), which covers nothing of
    // line 2 — an editor sends exactly this for "these lines".
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 0), Position::new(2, 0)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range.end, Position::new(2, 0));
    assert_eq!(edits[0].new_text, "y <- 2\n");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_accepts_a_selection_sent_end_first() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\nz<-3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 3), Position::new(1, 1)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\n");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_reaches_a_statement_inside_a_function() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open("R/fmt.R", "f<-function(x){\n  a<-1\n  b<-2\n  c<-3\n}\n")
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(2, 3), Position::new(2, 3)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "  b <- 2\n");
    assert_eq!(edits[0].range.start, Position::new(2, 0));
    assert_eq!(edits[0].range.end, Position::new(3, 0));
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_skips_a_formatting_off_region_inside_the_selection() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "R/fmt.R",
            "before<-1\n# fmt: off\nweird   <-  1\n# fmt: on\nafter<-1\n",
        )
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(0, 0), Position::new(4, 8)),
    )
    .await
    .expect("expected range formatting edits");
    let texts: Vec<&str> = edits.iter().map(|edit| edit.new_text.as_str()).collect();
    assert_eq!(texts, ["before <- 1\n", "after <- 1\n"]);
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    assert_eq!(edits[1].range.start, Position::new(4, 0));
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_returns_no_edits_for_lines_already_laid_out() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x <- 1\ny <- 2\nz<-3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(0, 0), Position::new(1, 6)),
    )
    .await
    .expect("expected an answer, not a refusal");
    assert!(edits.is_empty(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_refuses_on_syntax_errors() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/bad.R", "x <- (\ny<-2\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 0), Position::new(1, 4)),
    )
    .await;
    assert!(edits.is_none(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_clamps_a_range_past_the_end_of_the_document() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 0), Position::new(99, 99)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\n");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_positions_count_utf16_units() {
    let mut context = setup_test(&[]).await;
    // Each emoji is 4 UTF-8 bytes and 2 UTF-16 units, so a byte-counting
    // server would pick the wrong line and the wrong column here.
    let uri = context
        .open("R/emoji.R", "a<-\"😀😀\"\nb<-\"🎉\"\nc<-1\n")
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 5), Position::new(1, 5)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "b <- \"🎉\"\n");
    assert_eq!(edits[0].range.start, Position::new(1, 0));
    assert_eq!(edits[0].range.end, Position::new(2, 0));
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_keeps_the_documents_line_endings() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/crlf.R", "x<-1\r\ny<-2\r\nz<-3\r\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = range_edits(
        &mut context,
        &uri,
        Range::new(Position::new(1, 1), Position::new(1, 1)),
    )
    .await
    .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\r\n");
    context.shutdown().await;
}

#[tokio::test]
async fn formatting_only_replaces_the_lines_that_change() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x <- 1\ny<-2\nz <- 3\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting failed")
        .expect("expected formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2\n");
    assert_eq!(edits[0].range.start, Position::new(1, 0));
    assert_eq!(edits[0].range.end, Position::new(2, 0));
    context.shutdown().await;
}

#[tokio::test]
async fn formatting_an_already_formatted_document_makes_no_edits() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x <- 1\ny <- 2\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting failed")
        .expect("expected an answer, not a refusal");
    assert!(edits.is_empty(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn formatting_refuses_on_syntax_errors() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/bad.R", "x <- (\n").await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting failed");
    assert!(edits.is_none(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_report_known_diagnostics() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context.open("R/warn.R", "x = 1\n").await;
    let report = context
        .server
        .document_diagnostic(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: Some("ry".into()),
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("pull diagnostics failed");
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = report
    else {
        panic!("expected a full report");
    };
    assert_eq!(full.full_document_diagnostic_report.items.len(), 1);
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_unchanged_on_repeat() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context.open("R/warn.R", "x = 1\n").await;
    let params = DocumentDiagnosticParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        identifier: Some("ry".into()),
        previous_result_id: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let first = context
        .server
        .document_diagnostic(params.clone())
        .await
        .expect("pull diagnostics failed");
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = first else {
        panic!("expected a full report");
    };
    let result_id = full
        .full_document_diagnostic_report
        .result_id
        .expect("result id");
    let second = context
        .server
        .document_diagnostic(DocumentDiagnosticParams {
            previous_result_id: Some(result_id),
            ..params
        })
        .await
        .expect("repeat pull failed");
    assert!(
        matches!(
            second,
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(_))
        ),
        "expected an unchanged report"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn pull_capable_client_suppresses_push() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let _uri = context.open("R/warn.R", "x = 1\n").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        context.diagnostics_receiver.receiver.try_recv().is_err(),
        "push must be suppressed for pull clients"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn workspace_root_comes_from_the_client_not_the_process_cwd() {
    // setup runs the server with the temp ROOT as cwd and announces
    // `workspace/` as the root; a package file under `workspace/R` must be
    // analyzed as part of the package.
    let mut context = setup_test(&[("R/lib.R", "make_count <- function() 1L\n")]).await;
    let uri = context.open("R/use.R", "total <- make_count()\n").await;
    let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(
        params.diagnostics.is_empty(),
        "cross-file resolution must succeed: {:?}",
        params.diagnostics
    );
    context.shutdown().await;
}

#[tokio::test]
async fn malformed_config_falls_back_to_defaults_and_reports() {
    let mut context = setup_test(&[("ry.toml", "[check]\ntyping = 1\n")]).await;
    let message = tokio::time::timeout(TIMEOUT, context.messages_receiver.recv())
        .await
        .expect("timed out waiting for the config error message")
        .expect("messages channel closed");
    assert!(
        message.message.contains("invalid config"),
        "{}",
        message.message
    );
    // The server still works with default config.
    let uri = context.open("R/clean.R", "x <- 1\n").await;
    let params = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(params.diagnostics.is_empty(), "{:?}", params.diagnostics);
    context.shutdown().await;
}

//
// Batch 2: document lifecycle, features, encodings, config, pull/refresh
//

use async_lsp::lsp_types::{
    CompletionClientCapabilities, CompletionItemCapability, CompletionParams, CompletionResponse,
    DiagnosticWorkspaceClientCapabilities, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, FileChangeType,
    FileEvent, InlayHintParams, InsertTextFormat, ParameterInformationSettings, ParameterLabel,
    ReferenceContext, ReferenceParams, RenameParams, SignatureHelpClientCapabilities,
    SignatureHelpParams, SignatureInformationSettings, SymbolKind, WorkspaceClientCapabilities,
};

impl TestContext {
    fn change_file(&mut self, uri: &Url, version: i32, range: Range, text: &str) {
        self.server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(range),
                    range_length: None,
                    text: text.to_owned(),
                }],
            })
            .expect("didChange failed");
    }

    fn replace_file_full(&mut self, uri: &Url, version: i32, text: &str) {
        self.server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_owned(),
                }],
            })
            .expect("didChange failed");
    }

    fn save_file(&mut self, uri: &Url) {
        self.server
            .did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                text: None,
            })
            .expect("didSave failed");
    }

    fn close_file(&mut self, uri: &Url) {
        self.server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            })
            .expect("didClose failed");
    }

    fn notify_watched_file_changed(&mut self, relative: &str, typ: FileChangeType) {
        let uri = self.workspace_uri(relative);
        self.server
            .did_change_watched_files(DidChangeWatchedFilesParams {
                changes: vec![FileEvent { uri, typ }],
            })
            .expect("didChangeWatchedFiles failed");
    }

    async fn document_diagnostic(
        &mut self,
        uri: &Url,
        previous_result_id: Option<String>,
    ) -> DocumentDiagnosticReportResult {
        self.server
            .document_diagnostic(DocumentDiagnosticParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                identifier: Some("ry".into()),
                previous_result_id,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("pull diagnostics failed")
    }
}

async fn drain_diagnostics(channel: &mut DiagnosticsChannel) {
    channel.stash.clear();
    while channel.receiver.try_recv().is_ok() {}
}

async fn setup_test_with_snippet_support(initial_files: &[(&str, &str)]) -> TestContext {
    let capabilities = ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    ..CompletionItemCapability::default()
                }),
                ..CompletionClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    setup_test_inner(true, initial_files, capabilities).await
}

async fn setup_test_with_parameter_label_offsets(initial_files: &[(&str, &str)]) -> TestContext {
    let capabilities = ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    parameter_information: Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    ..SignatureInformationSettings::default()
                }),
                ..SignatureHelpClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    setup_test_inner(true, initial_files, capabilities).await
}

async fn setup_test_with_pull_and_refresh(initial_files: &[(&str, &str)]) -> TestContext {
    let capabilities = ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..TextDocumentClientCapabilities::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            diagnostic: Some(DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..WorkspaceClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    setup_test_inner(true, initial_files, capabilities).await
}

#[tokio::test]
async fn deeply_nested_documents_do_not_kill_the_server() {
    let mut context = setup_test(&[]).await;

    let deep = format!("value <- {}1{}\n", "f(".repeat(900), ")".repeat(900));
    let deep_uri = context.open("R/deep_valid.R", &deep).await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &deep_uri, TIMEOUT).await;

    let malformed = format!("value <- {}1{}\n", "f(".repeat(2000), ")".repeat(1000));
    let malformed_uri = context.open("R/deep_malformed.R", &malformed).await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &malformed_uri, TIMEOUT).await;

    // Formatting recurses over the tree as well; it must answer or refuse
    // without dying.
    let _ = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: deep_uri.clone(),
            },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await;

    let probe_uri = context.open("R/probe.R", "x <- 1\n").await;
    let published = recv_diagnostics(&mut context.diagnostics_receiver, &probe_uri, TIMEOUT).await;
    assert!(
        published.diagnostics.is_empty(),
        "the server still answers cleanly after the deep documents: {:?}",
        published.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn namespace_buffer_publishes_import_validation() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "NAMESPACE",
            "import(stats)\nimportFrom(stats, sd, medain)\nimportFrom(dplyr, mutate)\n",
        )
        .await;
    let published = recv_first_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert_eq!(
        published.diagnostics.len(),
        1,
        "only the known-namespace typo is reported: {:?}",
        published.diagnostics
    );
    let diagnostic = &published.diagnostics[0];
    assert!(
        diagnostic
            .message
            .contains("`medain` is not exported by `stats`, so this package will not load."),
        "unexpected message: {diagnostic:?}"
    );
    assert_eq!(
        diagnostic.severity,
        Some(async_lsp::lsp_types::DiagnosticSeverity::ERROR),
        "R refuses to load such a package: {diagnostic:?}"
    );
    assert_eq!(
        diagnostic.range.start.line, 1,
        "reports on the importFrom line"
    );

    // Fixing the typo republishes a clean report.
    context.change_file(
        &uri,
        2,
        Range::new(Position::new(1, 22), Position::new(1, 28)),
        "median",
    );
    let published = recv_first_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(
        published.diagnostics.is_empty(),
        "the corrected NAMESPACE is clean: {:?}",
        published.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn initialize_without_r_directory() {
    let mut context = setup_test_inner(false, &[], ClientCapabilities::default()).await;
    assert!(
        context
            .init_result
            .capabilities
            .text_document_sync
            .is_some(),
        "expected initialize to succeed without an R directory"
    );

    std::fs::create_dir_all(context.workspace_dir.join("R")).expect("create R directory");
    std::fs::write(context.workspace_dir.join("R/created_later.R"), "x <- T\n")
        .expect("write test file");
    context.notify_watched_file_changed("R/created_later.R", FileChangeType::CREATED);

    let file_uri = context.open("R/created_later.R", "x <- T\n").await;
    let diagnostics = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TRUE")),
        "expected diagnostics after creating R directory and file, got: {:?}",
        diagnostics.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn requests_for_unopened_non_file_uris_answer_gracefully() {
    let mut context = setup_test(&[("R/main.R", "x <- 1L")]).await;
    let untitled = Url::parse("untitled:Untitled-9").expect("untitled uri should parse");

    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: untitled.clone(),
                },
                position: Position::new(0, 0),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await;
    assert!(
        matches!(hover, Err(_) | Ok(None)),
        "hover on an unopened non-file uri must be answered, got: {hover:?}"
    );

    let report = context.document_diagnostic(&untitled, None).await;
    assert!(
        matches!(
            report,
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(_))
        ),
        "a pull on an unopened non-file uri returns an empty full report"
    );

    // The server is still alive and serving regular documents.
    let file_uri = context.open("R/main.R", "x <- 1L").await;
    recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;

    context.shutdown().await;
}

#[tokio::test]
async fn untitled_document_is_served_as_a_standalone_script() {
    let mut context = setup_test(&[]).await;
    let untitled = Url::parse("untitled:Untitled-1").expect("untitled uri should parse");

    context
        .server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: untitled.clone(),
                language_id: "r".into(),
                version: 1,
                text: "answer <- 42L\nfinal = answer\n".to_owned(),
            },
        })
        .expect("didOpen failed");

    let diagnostics = recv_diagnostics(&mut context.diagnostics_receiver, &untitled, TIMEOUT).await;
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("<-")),
        "expected the `=` lint on the untitled buffer, got: {:?}",
        diagnostics.diagnostics
    );

    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: untitled.clone(),
                },
                position: Position::new(1, 10),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover on an untitled document failed")
        .expect("hover response missing");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup hover contents");
    };
    assert!(markup.value.contains("integer"), "{}", markup.value);

    context.shutdown().await;
}

#[tokio::test]
async fn full_text_did_change_replaces_the_document() {
    let mut context = setup_test(&[]).await;
    let file_uri = context.open("R/full_sync.R", "x <- T\n").await;

    let initial = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        initial
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TRUE")),
        "expected the T-vs-TRUE lint before the replacement, got: {:?}",
        initial.diagnostics
    );

    context.replace_file_full(&file_uri, 2, "y = 1\n");

    let after = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        after
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("<-")),
        "expected the `=` lint from the replacement text, got: {:?}",
        after.diagnostics
    );
    assert!(
        !after
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TRUE")),
        "the old text must be fully replaced, not merged, got: {:?}",
        after.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn did_close_survives_a_failing_disk_reread() {
    let mut context = setup_test(&[]).await;
    std::fs::create_dir_all(context.workspace_dir.join("R/casualty.R"))
        .expect("create directory posing as a source file");

    let casualty_uri = context.open("R/casualty.R", "x <- 1\n").await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;
    context.close_file(&casualty_uri);

    // The server treated the unreadable file as deleted and keeps serving.
    let file_uri = context.open("R/still_alive.R", "x <- T\n").await;
    let diagnostics = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        diagnostics
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TRUE")),
        "expected the server to survive the failing close-time re-read, got: {:?}",
        diagnostics.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn completion() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open("R/comp.R", "my_function <- function(x) x\nmy_f\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .completion(CompletionParams {
            text_document_position: position_params(&file_uri, 1, 4),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .expect("completion request failed")
        .expect("expected completions");
    let CompletionResponse::List(list) = result else {
        panic!("expected a CompletionList response carrying isIncomplete");
    };
    assert!(
        !list.is_incomplete,
        "small candidate set should not be marked incomplete"
    );
    let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"my_function"),
        "expected 'my_function' in completions, got: {labels:?}"
    );
    let my_function = list
        .items
        .iter()
        .find(|item| item.label == "my_function")
        .expect("my_function item");
    assert!(
        my_function.insert_text.is_none() && my_function.insert_text_format.is_none(),
        "a non-snippet client must get plain-label insertion"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn completion_inserts_call_snippets_for_functions() {
    let mut context = setup_test_with_snippet_support(&[]).await;
    let file_uri = context
        .open(
            "R/snip.R",
            "snip_args <- function(x) x\nsnip_none <- function() 1L\nsn\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .completion(CompletionParams {
            text_document_position: position_params(&file_uri, 2, 2),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .expect("completion request failed")
        .expect("expected completions");
    let CompletionResponse::List(list) = result else {
        panic!("expected a list response");
    };
    let snip_args = list
        .items
        .iter()
        .find(|item| item.label == "snip_args")
        .expect("snip_args item");
    assert_eq!(
        snip_args.insert_text.as_deref(),
        Some("snip_args($0)"),
        "a function taking arguments drops the cursor between the parens"
    );
    assert_eq!(
        snip_args.insert_text_format,
        Some(InsertTextFormat::SNIPPET)
    );
    assert_eq!(
        snip_args
            .command
            .as_ref()
            .map(|command| command.command.as_str()),
        Some("editor.action.triggerParameterHints"),
        "inserting a call asks the editor for parameter hints"
    );
    let snip_none = list
        .items
        .iter()
        .find(|item| item.label == "snip_none")
        .expect("snip_none item");
    assert_eq!(
        snip_none.insert_text.as_deref(),
        Some("snip_none()$0"),
        "a zero-argument function drops the cursor past the parens"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn inlay_hints_respect_requested_viewport() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open(
            "R/hints.R",
            "count <- 1L\nlabel <- \"hello\"\nratio <- 2L\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let hints = context
        .server
        .inlay_hint(InlayHintParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            range: Range::new(Position::new(1, 0), Position::new(1, 16)),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("inlay hint request failed")
        .expect("expected inlay hints");
    let lines: Vec<u32> = hints.iter().map(|hint| hint.position.line).collect();
    assert_eq!(
        lines,
        vec![1],
        "only the in-viewport hint should be returned"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn rename_uses_analysis_across_files() {
    let mut context =
        setup_test(&[("R/a.R", "value <- 1L\n"), ("R/b.R", "result <- value\n")]).await;
    let file_uri = context.open("R/a.R", "value <- 1L\n").await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .rename(RenameParams {
            text_document_position: position_params(&file_uri, 0, 1),
            new_name: "renamed".into(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("rename request failed")
        .expect("expected rename edit");
    let changes = result.changes.expect("expected rename changes");

    let file_b_uri = context.workspace_uri("R/b.R");
    let file_a_edits = changes.get(&file_uri).expect("missing file A edits");
    assert_eq!(file_a_edits.len(), 1);
    assert_eq!(file_a_edits[0].new_text, "renamed");
    assert_eq!(file_a_edits[0].range.start, Position::new(0, 0));
    let file_b_edits = changes.get(&file_b_uri).expect("missing file B edits");
    assert_eq!(file_b_edits.len(), 1);
    assert_eq!(file_b_edits[0].range.start, Position::new(0, 10));

    context.shutdown().await;
}

#[tokio::test]
async fn references_use_analysis_across_files() {
    let mut context =
        setup_test(&[("R/a.R", "value <- 1L\n"), ("R/b.R", "result <- value\n")]).await;
    let file_uri = context.open("R/a.R", "value <- 1L\n").await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let reference_params = |include_declaration: bool| ReferenceParams {
        text_document_position: position_params(&file_uri, 0, 1),
        context: ReferenceContext {
            include_declaration,
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };

    let locations = context
        .server
        .references(reference_params(true))
        .await
        .expect("references request failed")
        .expect("expected reference locations");
    let file_b_uri = context.workspace_uri("R/b.R");
    assert!(
        locations
            .iter()
            .any(|location| location.uri == file_uri && location.range.start.line == 0),
        "expected the declaration in file A, got: {locations:?}"
    );
    assert!(
        locations
            .iter()
            .any(|location| location.uri == file_b_uri && location.range.start.character == 10),
        "expected the cross-file use in file B, got: {locations:?}"
    );

    let without_declaration = context
        .server
        .references(reference_params(false))
        .await
        .expect("references request failed")
        .expect("expected reference locations");
    assert!(
        without_declaration
            .iter()
            .all(|location| location.uri != file_uri),
        "expected the declaration to be excluded, got: {without_declaration:?}"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn document_symbols() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open(
            "R/syms.R",
            "add <- function(x, y) x + y\nmultiply <- function(a, b) a * b\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("document_symbol request failed")
        .expect("expected document symbols");
    let DocumentSymbolResponse::Nested(symbols) = result else {
        panic!("expected nested symbols");
    };
    let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    assert!(names.contains(&"add"), "got: {names:?}");
    assert!(names.contains(&"multiply"), "got: {names:?}");
    let add = symbols.iter().find(|symbol| symbol.name == "add").unwrap();
    assert_eq!(add.kind, SymbolKind::FUNCTION);

    context.shutdown().await;
}

#[tokio::test]
async fn document_symbols_include_type_definitions() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open(
            "R/typed_syms.R",
            "#: @type point {list{x: double, y: double}}\n\n#: @alias points {list[point]}\nmake <- function() 1L\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("document_symbol request failed")
        .expect("expected document symbols");
    let DocumentSymbolResponse::Nested(symbols) = result else {
        panic!("expected nested symbols: {result:?}");
    };
    let point = symbols
        .iter()
        .find(|symbol| symbol.name == "point")
        .unwrap_or_else(|| panic!("expected `point` in the outline: {symbols:?}"));
    assert_eq!(point.kind, SymbolKind::STRUCT);
    assert_eq!(point.detail.as_deref(), Some("@type"));
    let points = symbols
        .iter()
        .find(|symbol| symbol.name == "points")
        .unwrap_or_else(|| panic!("expected `points` in the outline: {symbols:?}"));
    assert_eq!(points.kind, SymbolKind::INTERFACE);
    assert!(
        symbols.iter().any(|symbol| symbol.name == "make"),
        "item bindings still appear alongside type definitions"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn config_indent_width() {
    let mut context = setup_test(&[("ry.toml", "[format]\nindent-width = 4\n")]).await;
    let file_uri = context
        .open("R/indent.R", "f <- function(x) {\nx + 1\n}\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting request failed")
        .expect("expected formatting edits");
    assert!(
        edits[0].new_text.contains("    x + 1"),
        "expected 4-space indentation, got:\n{}",
        edits[0].new_text
    );

    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_on_change() {
    let mut context = setup_test(&[("ry.toml", "[format]\nindent-width = 2\n")]).await;
    let file_uri = context
        .open("R/reload.R", "f <- function(x) {\nx + 1\n}\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let formatted = |context: &mut TestContext| {
        let uri = file_uri.clone();
        let mut server = context.server.clone();
        async move {
            server
                .formatting(DocumentFormattingParams {
                    text_document: TextDocumentIdentifier { uri },
                    options: FormattingOptions::default(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .await
                .expect("formatting request failed")
                .expect("expected formatting edits")[0]
                .new_text
                .clone()
        }
    };

    let initial = formatted(&mut context).await;
    assert!(
        initial.contains("  x + 1"),
        "expected 2-space indentation before reload, got:\n{initial}"
    );

    std::fs::write(
        context.workspace_dir.join("ry.toml"),
        "[format]\nindent-width = 4\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);

    let reloaded = formatted(&mut context).await;
    assert!(
        reloaded.contains("    x + 1"),
        "expected 4-space indentation after reload, got:\n{reloaded}"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn hover_range_under_utf16_with_bmp_non_ascii() {
    let mut context = setup_test(&[]).await;
    // `é` is one UTF-16 code unit but two UTF-8 bytes, so the byte column of
    // `target` (16) differs from its UTF-16 column (15).
    let file_uri = context
        .open("R/bmp.R", "target <- 1L\ny <- f(\"café\", target)\n")
        .await;

    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: position_params(&file_uri, 1, 17),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover request failed")
        .expect("hover response missing");
    let range = hover.range.expect("hover should report a range");
    assert_eq!(range.start.line, 1);
    assert_eq!(
        (range.start.character, range.end.character),
        (15, 21),
        "expected the UTF-16 span of `target`, got: {range:?}"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn goto_definition_range_under_utf16_with_non_ascii() {
    let mut context = setup_test(&[]).await;
    // `caféx` is five scalars / five UTF-16 units but six UTF-8 bytes.
    let file_uri = context.open("R/goto.R", "caféx <- 1L\ny <- caféx\n").await;

    let result = context
        .server
        .definition(GotoDefinitionParams {
            text_document_position_params: position_params(&file_uri, 1, 7),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("definition request failed")
        .expect("expected a definition response");
    let GotoDefinitionResponse::Scalar(location) = result else {
        panic!("expected a scalar definition");
    };
    assert_eq!(location.range.start, Position::new(0, 0));
    assert_eq!(
        location.range.end.character, 5,
        "expected the UTF-16 end column of `caféx`, got: {:?}",
        location.range
    );

    context.shutdown().await;
}

#[tokio::test]
async fn document_symbol_selection_range_under_utf16_with_non_ascii() {
    let mut context = setup_test(&[]).await;
    // `café_fn` is seven UTF-16 units but eight UTF-8 bytes.
    let file_uri = context
        .open("R/symbol.R", "café_fn <- function() 1\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("document_symbol request failed")
        .expect("expected document symbols");
    let DocumentSymbolResponse::Nested(symbols) = result else {
        panic!("expected nested document symbols");
    };
    let symbol = symbols
        .iter()
        .find(|symbol| symbol.name == "café_fn")
        .expect("expected the café_fn symbol");
    assert_eq!(symbol.selection_range.start.character, 0);
    assert_eq!(
        symbol.selection_range.end.character, 7,
        "expected the UTF-16 end column of `café_fn`, got: {:?}",
        symbol.selection_range
    );

    context.shutdown().await;
}

#[tokio::test]
async fn diagnostics_range_under_utf16_with_non_bmp_emoji() {
    let mut context = setup_test(&[]).await;
    // `🦀` is 2 UTF-16 units / 4 UTF-8 bytes / 1 scalar; the `T` lint span
    // diverges: UTF-16 = 11..12, UTF-8 byte = 13..14.
    let file_uri = context.open("R/emoji_diag.R", "\"🦀\"; x <- T\n").await;

    let diagnostics = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    let true_diagnostic = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("TRUE"))
        .unwrap_or_else(|| {
            panic!(
                "expected a T-vs-TRUE diagnostic, got: {:?}",
                diagnostics.diagnostics
            )
        });
    assert_eq!(true_diagnostic.range.start.line, 0);
    assert_eq!(
        (
            true_diagnostic.range.start.character,
            true_diagnostic.range.end.character,
        ),
        (11, 12),
        "expected the UTF-16 span of `T`, got: {:?}",
        true_diagnostic.range
    );

    context.shutdown().await;
}

#[tokio::test]
async fn hover_range_under_utf8_with_non_bmp_emoji() {
    let mut context = setup_test_with_position_encodings(&[], &[PositionEncodingKind::UTF8]).await;
    let file_uri = context
        .open("R/utf8_emoji.R", "target <- 1L\ny <- f(\"🦀\", target)\n")
        .await;

    let hover = context
        .server
        .hover(HoverParams {
            // Byte column 17 is inside `target` (bytes 15..21).
            text_document_position_params: position_params(&file_uri, 1, 17),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover request failed")
        .expect("hover response missing");
    let range = hover.range.expect("hover should report a range");
    assert_eq!(
        (range.start.character, range.end.character),
        (15, 21),
        "expected the UTF-8 byte span of `target` after an emoji, got: {range:?}"
    );

    context.shutdown().await;
}

const OOB_DOC: &str = "alpha <- 1\n\nbeta <- 2\n";

fn oob_positions() -> [Position; 2] {
    // A character past the end of the empty line 1, and a line past the end.
    [Position::new(1, 50), Position::new(50, 0)]
}

#[tokio::test]
async fn out_of_bounds_positions_are_safe_on_every_entry_point() {
    let mut context = setup_test(&[]).await;
    let file_uri = context.open("R/oob.R", OOB_DOC).await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    for position in oob_positions() {
        let _ = context
            .server
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .expect("hover must not panic the server");
        let _ = context
            .server
            .definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("definition must not panic the server");
        let _ = context
            .server
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("references must not panic the server");
        let _ = context
            .server
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            })
            .await
            .expect("completion must not panic the server");
        let signature = context
            .server
            .signature_help(SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .expect("signature help must not panic the server");
        assert!(signature.is_none(), "OOB signature help should be None");
        let rename = context
            .server
            .rename(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: file_uri.clone(),
                    },
                    position,
                },
                new_name: "renamed".into(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .expect("rename must not panic the server");
        assert!(rename.is_none(), "OOB rename should be None");
    }

    // Inlay hints over an oversized viewport are clamped, not fatal.
    let _ = context
        .server
        .inlay_hint(InlayHintParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            range: Range::new(Position::new(0, 0), Position::new(99, 99)),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("inlay hints must not panic the server");

    context.shutdown().await;
}

#[tokio::test]
async fn signature_help_sends_label_with_parameter_offsets() {
    let mut context = setup_test_with_parameter_label_offsets(&[]).await;
    let file_uri = context.open("R/sig.R", "result <- lapply()\n").await;

    let help = context
        .server
        .signature_help(SignatureHelpParams {
            context: None,
            text_document_position_params: position_params(&file_uri, 0, 17),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("signature_help request failed")
        .expect("signature help expected inside the call");
    // `lapply` is an overload set, so the list carries every declared
    // candidate; the offsets under test are the active one's.
    let active = help.active_signature.expect("an active signature") as usize;
    let signature = &help.signatures[active];
    let parameters = signature
        .parameters
        .as_ref()
        .expect("signature parameters expected");
    let parameter_texts: Vec<&str> = parameters
        .iter()
        .map(|parameter| match parameter.label {
            ParameterLabel::LabelOffsets([start, end]) => signature
                .label
                .get(start as usize..end as usize)
                .expect("parameter offsets must slice the label"),
            ParameterLabel::Simple(_) => {
                panic!("offset-capable client must receive label offsets")
            }
        })
        .collect();
    assert_eq!(
        parameter_texts,
        ["x: list[named: T]", "f: fn(T) -> U", "...: Any"]
    );
    assert_eq!(help.active_parameter, Some(0));

    context.shutdown().await;
}

#[tokio::test]
async fn signature_help_falls_back_to_substring_parameter_labels() {
    let mut context = setup_test(&[]).await;
    let file_uri = context.open("R/sig.R", "result <- lapply()\n").await;

    let help = context
        .server
        .signature_help(SignatureHelpParams {
            context: None,
            text_document_position_params: position_params(&file_uri, 0, 17),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("signature_help request failed")
        .expect("signature help expected inside the call");
    let active = help.active_signature.expect("an active signature") as usize;
    let signature = &help.signatures[active];
    let parameters = signature
        .parameters
        .as_ref()
        .expect("signature parameters expected");
    let parameter_texts: Vec<&str> = parameters
        .iter()
        .map(|parameter| match &parameter.label {
            ParameterLabel::Simple(text) => text.as_str(),
            ParameterLabel::LabelOffsets(_) => {
                panic!("client without offset support must receive substring labels")
            }
        })
        .collect();
    assert_eq!(
        parameter_texts,
        ["x: list[named: T]", "f: fn(T) -> U", "...: Any"]
    );

    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_empty_for_clean_file() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context.open("R/clean.R", "x <- 1\n").await;
    let report = context.document_diagnostic(&uri, None).await;
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = report
    else {
        panic!("expected a full report");
    };
    assert!(full.full_document_diagnostic_report.items.is_empty());
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_untracked_document_is_empty() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context.workspace_uri("R/never_opened.R");
    let report = context.document_diagnostic(&uri, None).await;
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = report
    else {
        panic!("expected a full report");
    };
    assert!(full.full_document_diagnostic_report.items.is_empty());
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_match_pushed_across_files() {
    let mut context = setup_test(&[]).await;
    let file_a_uri = context.open("R/match_a.R", "x <- T\n").await;
    let file_b_uri = context.open("R/match_b.R", "y = 1\n").await;

    let pushed_a = recv_diagnostics(&mut context.diagnostics_receiver, &file_a_uri, TIMEOUT).await;
    let pushed_b = recv_diagnostics(&mut context.diagnostics_receiver, &file_b_uri, TIMEOUT).await;
    let pushed_a: Vec<String> = pushed_a
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    let pushed_b: Vec<String> = pushed_b
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect();

    let report_messages = |report: DocumentDiagnosticReportResult| match report {
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => full
            .full_document_diagnostic_report
            .items
            .iter()
            .map(|item| item.message.clone())
            .collect::<Vec<_>>(),
        other => panic!("expected a full report, got: {other:?}"),
    };
    let pulled_a = report_messages(context.document_diagnostic(&file_a_uri, None).await);
    let pulled_b = report_messages(context.document_diagnostic(&file_b_uri, None).await);

    assert_eq!(pulled_a, pushed_a);
    assert_eq!(pulled_b, pushed_b);
    assert!(!pulled_a.is_empty() && !pulled_b.is_empty());

    context.shutdown().await;
}

#[tokio::test]
async fn semantic_classes_arrive_with_the_settled_wave() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open("R/unresolved.R", "main <- function() missing_helper()\n")
        .await;

    let first = recv_first_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        !first
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missing_helper")),
        "name resolution is not part of the first wave: {:?}",
        first.diagnostics
    );

    let settled = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        settled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missing_helper")),
        "the settled wave carries the unresolved reference: {:?}",
        settled.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn breaking_one_file_leaves_its_dependents_untouched() {
    let mut context = setup_test(&[]).await;
    let library_uri = context
        .open("R/library.R", "shared_helper <- function() 1L\n")
        .await;
    let consumer_uri = context
        .open("R/consumer.R", "value <- shared_helper()\n")
        .await;
    let settled = recv_diagnostics(&mut context.diagnostics_receiver, &consumer_uri, TIMEOUT).await;
    assert!(
        settled.diagnostics.is_empty(),
        "the consumer starts clean: {:?}",
        settled.diagnostics
    );

    // Break the library mid-edit: append an unclosed function definition.
    context.change_file(
        &library_uri,
        2,
        Range::new(Position::new(1, 0), Position::new(1, 0)),
        "broken <- function() {\n",
    );
    let broken = loop {
        let settled =
            recv_diagnostics(&mut context.diagnostics_receiver, &library_uri, TIMEOUT).await;
        if !settled.diagnostics.is_empty() {
            break settled;
        }
    };
    assert!(
        broken.diagnostics.iter().all(|diagnostic| diagnostic.code
            == Some(async_lsp::lsp_types::NumberOrString::String(
                "syntax-error".to_owned()
            ))),
        "the broken file reports its syntax error and nothing else: {:?}",
        broken.diagnostics
    );

    // Saving while broken refreshes every open document; the consumer must
    // still be clean — `shared_helper` never stopped resolving.
    context.save_file(&consumer_uri);
    let after_break =
        recv_diagnostics(&mut context.diagnostics_receiver, &consumer_uri, TIMEOUT).await;
    assert!(
        after_break.diagnostics.is_empty(),
        "the consumer is untouched while its dependency is mid-edit: {:?}",
        after_break.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn dependency_affecting_save_requests_diagnostic_refresh() {
    let mut context = setup_test_with_pull_and_refresh(&[]).await;
    let uri = context.open("R/dep.R", "x <- 1\n").await;
    context.save_file(&uri);
    tokio::time::timeout(TIMEOUT, context.refresh_receiver.recv())
        .await
        .expect("timed out waiting for the diagnostic refresh request")
        .expect("refresh channel closed");
    context.shutdown().await;
}

#[tokio::test]
async fn pull_client_without_refresh_support_gets_no_refresh() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context.open("R/dep.R", "x <- 1\n").await;
    context.save_file(&uri);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        context.refresh_receiver.try_recv().is_err(),
        "a client without refresh support must not receive refresh requests"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn ancestor_config_governs_a_workspace_without_its_own() {
    // The config sits in the PARENT of the workspace root (the temp root);
    // discovery walks ancestors from the announced workspace.
    let temp_dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        temp_dir.path().join("ry.toml"),
        "[format]\nindent-width = 7\n",
    )
    .expect("write ancestor config");
    let workspace_dir = temp_dir.path().join("workspace");
    std::fs::create_dir_all(workspace_dir.join("R")).expect("create workspace");

    let (diagnostics_sender, diagnostics_receiver) = mpsc::unbounded_channel();
    let (refresh_sender, refresh_receiver) = mpsc::unbounded_channel();
    let (messages_sender, messages_receiver) = mpsc::unbounded_channel();
    let (mainloop, mut server) =
        build_test_client(diagnostics_sender, refresh_sender, messages_sender);
    let mut child = spawn_server(temp_dir.path(), &[], &[]);
    let stdout = child.stdout.take().expect("missing stdout").compat();
    let stdin = child.stdin.take().expect("missing stdin").compat_write();
    let mainloop_handle = tokio::spawn(async move {
        let _ = mainloop.run_buffered(stdout, stdin).await;
        drop(child);
    });
    let root_uri = Url::from_file_path(&workspace_dir).expect("workspace uri");
    let init_result = server
        .initialize(InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "root".into(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize failed");
    server
        .initialized(InitializedParams {})
        .expect("initialized failed");
    let mut context = TestContext {
        server,
        diagnostics_receiver: DiagnosticsChannel {
            receiver: diagnostics_receiver,
            stash: Vec::new(),
        },
        refresh_receiver,
        messages_receiver,
        mainloop_handle,
        init_result,
        _temp_dir: temp_dir,
        workspace_dir,
    };

    let file_uri = context
        .open("R/indent.R", "f <- function(x) {\nx + 1\n}\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting request failed")
        .expect("expected formatting edits");
    assert!(
        edits[0].new_text.contains("       x + 1"),
        "expected 7-space indentation from the ancestor config, got:\n{}",
        edits[0].new_text
    );

    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_refreshes_push_diagnostics() {
    let mut context = setup_test(&[("ry.toml", "[check]\nunused = false\n")]).await;
    let file_uri = context
        .open("R/dead.R", "f <- function() {\n  dead <- 1\n  2\n}\n")
        .await;
    let settled = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        settled.diagnostics.is_empty(),
        "unused is off initially: {:?}",
        settled.diagnostics
    );

    std::fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\nunused = true\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);

    let refreshed = recv_diagnostics(&mut context.diagnostics_receiver, &file_uri, TIMEOUT).await;
    assert!(
        refreshed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("never used")),
        "the config toggle re-publishes with the unused finding: {:?}",
        refreshed.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_requests_refresh_for_pull_clients() {
    let mut context =
        setup_test_with_pull_and_refresh(&[("ry.toml", "[check]\nunused = false\n")]).await;
    let _uri = context.open("R/dead.R", "f <- function() 1\n").await;

    std::fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\nunused = true\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);

    tokio::time::timeout(TIMEOUT, context.refresh_receiver.recv())
        .await
        .expect("timed out waiting for the refresh request")
        .expect("refresh channel closed");

    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_failure_keeps_previous_config_and_reports() {
    let mut context = setup_test(&[("ry.toml", "[format]\nindent-width = 4\n")]).await;
    let file_uri = context
        .open("R/keep.R", "f <- function(x) {\nx + 1\n}\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    std::fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\ntyping = 1\n",
    )
    .expect("write broken config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);

    let message = tokio::time::timeout(TIMEOUT, context.messages_receiver.recv())
        .await
        .expect("timed out waiting for the reload error")
        .expect("messages channel closed");
    assert!(
        message
            .message
            .contains("keeping the previous configuration"),
        "{}",
        message.message
    );

    // Formatting still uses the previous (4-space) configuration.
    let edits = context
        .server
        .formatting(DocumentFormattingParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            options: FormattingOptions::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("formatting request failed")
        .expect("expected formatting edits");
    assert!(
        edits[0].new_text.contains("    x + 1"),
        "expected the previous config to keep governing, got:\n{}",
        edits[0].new_text
    );

    context.shutdown().await;
}

#[tokio::test]
async fn malformed_config_publishes_a_diagnostic_on_the_config_file() {
    let mut context = setup_test(&[("ry.toml", "[check]\ntyping = 1\n")]).await;
    let config_uri = context.workspace_uri("ry.toml");
    let published =
        recv_first_diagnostics(&mut context.diagnostics_receiver, &config_uri, TIMEOUT).await;
    assert_eq!(
        published.diagnostics.len(),
        1,
        "{:?}",
        published.diagnostics
    );
    let diagnostic = &published.diagnostics[0];
    assert_eq!(
        diagnostic.code,
        Some(async_lsp::lsp_types::NumberOrString::String(
            "config".to_owned()
        ))
    );
    assert!(
        diagnostic.message.contains("invalid config"),
        "{}",
        diagnostic.message
    );
    context.shutdown().await;
}

#[tokio::test]
async fn annotation_bodies_get_semantic_tokens() {
    let mut context = setup_test(&[]).await;
    let file_uri = context
        .open("R/tokens.R", "#: <T> fn(x: T) -> T\nid <- function(x) x\n")
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .semantic_tokens_full(async_lsp::lsp_types::SemanticTokensParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.clone(),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("semantic tokens request failed")
        .expect("expected semantic tokens");
    let async_lsp::lsp_types::SemanticTokensResult::Tokens(tokens) = result else {
        panic!("expected a tokens result");
    };
    assert!(
        !tokens.data.is_empty(),
        "the #: annotation body must produce semantic tokens"
    );

    context.shutdown().await;
}

//
// Stub (.Rtypes) buffers: served standalone, never entering the database.
//

#[tokio::test]
async fn stub_documents_are_served_with_parse_diagnostics() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "stubs/project.Rtypes",
            "good_helper : fn(x: double) -> double\nsize : Frobnicate\n",
        )
        .await;
    let published = recv_first_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert_eq!(
        published.diagnostics.len(),
        1,
        "{:?}",
        published.diagnostics
    );
    let diagnostic = &published.diagnostics[0];
    assert!(
        diagnostic.message.contains("does not load") && diagnostic.message.contains("Frobnicate"),
        "{:?}",
        diagnostic
    );
    assert_eq!(diagnostic.range.start.line, 1, "whole-line on the bad line");

    // Fixing the declaration republishes clean.
    context.replace_file_full(&uri, 2, "good_helper : fn(x: double) -> double\n");
    let published = recv_first_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;
    assert!(
        published.diagnostics.is_empty(),
        "{:?}",
        published.diagnostics
    );

    context.shutdown().await;
}

#[tokio::test]
async fn stub_documents_answer_pull_diagnostics() {
    let mut context = setup_test_with_pull_diagnostics(&[]).await;
    let uri = context
        .open("stubs/project.Rtypes", "size : Frobnicate\n")
        .await;
    let report = context.document_diagnostic(&uri, None).await;
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = report
    else {
        panic!("expected a full report");
    };
    assert_eq!(full.full_document_diagnostic_report.items.len(), 1);
    assert!(
        full.full_document_diagnostic_report.items[0]
            .message
            .contains("does not load"),
        "{:?}",
        full.full_document_diagnostic_report.items
    );
    context.shutdown().await;
}

#[tokio::test]
async fn stub_documents_get_semantic_tokens() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "stubs/project.Rtypes",
            "@type frame\nmake_frame : fn(n: integer) -> frame\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .semantic_tokens_full(async_lsp::lsp_types::SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("semantic tokens request failed")
        .expect("expected semantic tokens");
    let async_lsp::lsp_types::SemanticTokensResult::Tokens(tokens) = result else {
        panic!("expected a tokens result");
    };
    assert!(
        !tokens.data.is_empty(),
        "a stub buffer must produce semantic tokens"
    );

    context.shutdown().await;
}

#[tokio::test]
async fn stub_type_name_jumps_to_its_type_declaration() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "stubs/project.Rtypes",
            "@type frame\nmake_frame : fn(n: integer) -> frame\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    // The cursor sits on the `frame` return-type reference (line 1, col 32).
    let result = context
        .server
        .definition(GotoDefinitionParams {
            text_document_position_params: position_params(&uri, 1, 32),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("definition request failed")
        .expect("expected a definition into the stub file");
    let GotoDefinitionResponse::Scalar(location) = result else {
        panic!("expected a scalar definition");
    };
    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start, Position::new(0, 6));
    assert_eq!(location.range.end, Position::new(0, 11));

    context.shutdown().await;
}

#[tokio::test]
async fn hover_shows_definition_summaries() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "R/hover.R",
            "count <- 1L\nuse <- function() {\n  value <- count + 1\n  value + print(value)\n}\n",
        )
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;

    let hover_markdown = |context: &mut TestContext, line: u32, character: u32| {
        let params = HoverParams {
            text_document_position_params: position_params(&uri, line, character),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let mut server = context.server.clone();
        async move {
            let hover = server
                .hover(params)
                .await
                .expect("hover failed")
                .expect("expected a hover");
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markdown hover");
            };
            markup.value
        }
    };

    // A read of the file's own top-level binding is a package global.
    let global = hover_markdown(&mut context, 2, 12).await;
    assert!(
        global.contains("Package global, defined at `R/hover.R:1:1`"),
        "{global}"
    );

    // A local read points at its defining write.
    let local = hover_markdown(&mut context, 3, 3).await;
    assert!(
        local.contains("Local variable, defined at `R/hover.R:3:3`"),
        "{local}"
    );

    // A stub name reports its declaring namespace.
    let stub = hover_markdown(&mut context, 3, 11).await;
    assert!(stub.contains("From the `base` package."), "{stub}");

    // Debug sections stay hidden without the developer switch.
    assert!(!global.contains("### Debug"), "{global}");
    context.shutdown().await;
}

#[tokio::test]
async fn debug_flag_adds_hover_debug_sections() {
    let mut context =
        setup_test_with_env_and_args(true, &[], ClientCapabilities::default(), &[], &["--debug"])
            .await;
    let uri = context
        .open("R/debug.R", "count <- 1L\nuse <- count\n")
        .await;
    let _ = recv_diagnostics(&mut context.diagnostics_receiver, &uri, TIMEOUT).await;

    let hover = context
        .server
        .hover(HoverParams {
            text_document_position_params: position_params(&uri, 1, 8),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("hover failed")
        .expect("expected a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markdown hover");
    };
    for marker in ["### Debug", "**Lowering**", "**Naming**", "**Parsing**"] {
        assert!(markup.value.contains(marker), "{}", markup.value);
    }
    context.shutdown().await;
}

#[tokio::test]
async fn document_symbols_nest_s4_and_r6_declarations() {
    let mut context = setup_test(&[]).await;
    let uri = context
        .open(
            "R/classes.R",
            "setClass(\"Person\", representation(name = \"character\"))\n\
             Account <- R6Class(\"Account\",\n\
             \x20 public = list(\n\
             \x20   balance = 0,\n\
             \x20   deposit = function(amount) invisible(self)\n\
             \x20 )\n\
             )\n",
        )
        .await;
    drain_diagnostics(&mut context.diagnostics_receiver).await;

    let result = context
        .server
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("document_symbol request failed")
        .expect("expected document symbols");
    let DocumentSymbolResponse::Nested(symbols) = result else {
        panic!("expected nested symbols");
    };

    let person = symbols
        .iter()
        .find(|symbol| symbol.name == "Person")
        .expect("setClass declaration in the outline");
    assert_eq!(person.kind, SymbolKind::CLASS);

    let account = symbols
        .iter()
        .find(|symbol| symbol.name == "Account")
        .expect("R6 class in the outline");
    assert_eq!(account.kind, SymbolKind::CLASS);
    let members = account.children.as_ref().expect("R6 members nest");
    let balance = members
        .iter()
        .find(|member| member.name == "balance")
        .expect("field member");
    assert_eq!(balance.kind, SymbolKind::FIELD);
    let deposit = members
        .iter()
        .find(|member| member.name == "deposit")
        .expect("method member");
    assert_eq!(deposit.kind, SymbolKind::METHOD);
    assert_eq!(deposit.detail.as_deref(), Some("fn(amount)"));

    context.shutdown().await;
}

/// The cancelled-pull contract, made deterministic by the server's
/// fault-injection seam: the pull announces itself through the marker file
/// and holds, the edit is sent only after the marker appears — so its
/// cancellation flip provably lands while the pull is in flight — and the
/// response must be the retryable SERVER_CANCELLED error, with an immediate
/// re-pull succeeding on the edited content.
#[tokio::test]
async fn cancelled_pull_is_retryable_and_recovers() {
    let capabilities = ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..TextDocumentClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    let marker_dir = tempfile::tempdir().expect("marker dir");
    let marker = marker_dir.path().join("pull.marker");
    let marker_text = marker.to_str().expect("utf-8 marker path").to_owned();
    let mut context = setup_test_with_env(
        true,
        &[],
        capabilities,
        &[
            ("RY_TEST_DELAY_PULL_MS", "1000"),
            ("RY_TEST_PULL_MARKER", &marker_text),
        ],
    )
    .await;
    let uri = context.open("R/cancel.R", "x = 1\n").await;

    // The request is written first (socket sends are eager); the edit waits
    // for the marker, so it always hits the held pull.
    let pending = context
        .server
        .document_diagnostic(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: Some("ry".into()),
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        });
    tokio::time::timeout(TIMEOUT, async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the server never started holding the pull");
    context.replace_file_full(&uri, 2, "y = 2\n");

    let result = pending.await;
    let Err(async_lsp::Error::Response(error)) = result else {
        panic!("expected the cancelled-pull error, got {result:?}");
    };
    assert_eq!(
        error.code,
        async_lsp::ErrorCode::SERVER_CANCELLED,
        "{error:?}"
    );
    let data = error.data.as_ref().expect("cancellation data");
    assert_eq!(
        data.get("retriggerRequest")
            .and_then(|value| value.as_bool()),
        Some(true),
        "{data:?}"
    );

    // The retry lands on the edited content.
    let report = context.document_diagnostic(&uri, None).await;
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) = report
    else {
        panic!("expected a full report on retry");
    };
    let messages: Vec<&str> = full
        .full_document_diagnostic_report
        .items
        .iter()
        .map(|item| item.message.as_str())
        .collect();
    assert!(
        messages.iter().any(|message| message.contains("<-")),
        "expected the assignment-operator lint on the edited text: {messages:?}"
    );

    context.shutdown().await;
}
