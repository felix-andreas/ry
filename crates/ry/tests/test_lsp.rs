use {
    async_lsp::{
        ErrorCode, LanguageServer, MainLoop, ServerSocket,
        concurrency::ConcurrencyLayer,
        lsp_types::{
            ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability,
            CompletionParams, CompletionResponse, DiagnosticClientCapabilities, DiagnosticSeverity,
            DiagnosticWorkspaceClientCapabilities, DidChangeTextDocumentParams,
            DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
            DidSaveTextDocumentParams, DocumentDiagnosticParams, DocumentDiagnosticReport,
            DocumentDiagnosticReportResult, DocumentFormattingParams,
            DocumentRangeFormattingParams, DocumentSymbol, DocumentSymbolParams,
            DocumentSymbolResponse, FileChangeType, FileEvent, FormattingOptions,
            GeneralClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse, Hover,
            HoverContents, HoverParams, InitializeParams, InitializeResult, InitializedParams,
            InlayHint, InlayHintParams, InsertTextFormat, Location, NumberOrString,
            ParameterInformationSettings, ParameterLabel, PartialResultParams, Position,
            PositionEncodingKind, PublishDiagnosticsParams, Range, ReferenceContext,
            ReferenceParams, RenameParams, SemanticTokens, SemanticTokensParams,
            SemanticTokensResult, ShowMessageParams, SignatureHelp,
            SignatureHelpClientCapabilities, SignatureHelpParams, SignatureInformationSettings,
            SymbolKind, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
            TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit, Url,
            VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceClientCapabilities,
            WorkspaceEdit, WorkspaceFolder,
            notification::{PublishDiagnostics, ShowMessage},
            request::{RegisterCapability, WorkspaceDiagnosticRefresh},
        },
        panic::CatchUnwindLayer,
        router::Router,
    },
    std::{fs, future::ready, ops::ControlFlow, path::PathBuf, process::Stdio, time::Duration},
    tempfile::TempDir,
    tokio::{
        sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
        task::JoinHandle,
        time::{self, Instant, timeout},
    },
    tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt},
    tower::ServiceBuilder,
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct TestContext {
    server: ServerSocket,
    diagnostics: UnboundedReceiver<PublishDiagnosticsParams>,
    /// Deferred publishes for different documents interleave in idle order, so a
    /// wait for one URI must stash, not drop, the others.
    stash: Vec<PublishDiagnosticsParams>,
    refreshes: UnboundedReceiver<()>,
    messages: UnboundedReceiver<ShowMessageParams>,
    mainloop: JoinHandle<()>,
    init_result: InitializeResult,
    workspace_dir: PathBuf,
    _temp_dir: TempDir,
}

struct TestClientState {
    diagnostics: UnboundedSender<PublishDiagnosticsParams>,
    refreshes: UnboundedSender<()>,
    messages: UnboundedSender<ShowMessageParams>,
}

#[derive(Default)]
struct Launch<'a> {
    files: &'a [(&'a str, &'a str)],
    capabilities: ClientCapabilities,
    env: &'a [(&'a str, &'a str)],
    args: &'a [&'a str],
    without_r_directory: bool,
}

async fn setup_test(files: &[(&str, &str)]) -> TestContext {
    setup_with(files, ClientCapabilities::default()).await
}

async fn setup_with(files: &[(&str, &str)], capabilities: ClientCapabilities) -> TestContext {
    launch(Launch {
        files,
        capabilities,
        ..Launch::default()
    })
    .await
}

async fn launch(options: Launch<'_>) -> TestContext {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    // The cwd is the temp root while the client announces `workspace/` as the root,
    // so every test exercises root-from-initialize.
    let workspace_dir = temp_dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir).expect("failed to create workspace directory");
    if !options.without_r_directory {
        fs::create_dir_all(workspace_dir.join("R")).expect("failed to create R directory");
    }
    for (relative, text) in options.files {
        let path = workspace_dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        fs::write(path, text).expect("failed to write initial test file");
    }

    let (diagnostics_sender, diagnostics) = mpsc::unbounded_channel();
    let (refresh_sender, refreshes) = mpsc::unbounded_channel();
    let (message_sender, messages) = mpsc::unbounded_channel();
    let (mainloop, mut server) = MainLoop::new_client(|_server| {
        let mut router = Router::new(TestClientState {
            diagnostics: diagnostics_sender,
            refreshes: refresh_sender,
            messages: message_sender,
        });
        router
            .notification::<PublishDiagnostics>(|state, params| {
                state
                    .diagnostics
                    .send(params)
                    .expect("diagnostics channel closed unexpectedly");
                ControlFlow::Continue(())
            })
            .notification::<ShowMessage>(|state, params| {
                state
                    .messages
                    .send(params)
                    .expect("messages channel closed unexpectedly");
                ControlFlow::Continue(())
            })
            .request::<RegisterCapability, _>(|_, _| ready(Ok(())))
            .request::<WorkspaceDiagnosticRefresh, _>(|state, _| {
                state
                    .refreshes
                    .send(())
                    .expect("refresh channel closed unexpectedly");
                ready(Ok(()))
            });
        ServiceBuilder::new()
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(router)
    });

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ry"))
        .arg("server")
        .args(options.args)
        .current_dir(temp_dir.path())
        .envs(options.env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn ry server");
    let stdout = child.stdout.take().expect("missing stdout").compat();
    let stdin = child.stdin.take().expect("missing stdin").compat_write();
    let mainloop = tokio::spawn(async move {
        if let Err(error) = mainloop.run_buffered(stdout, stdin).await {
            eprintln!("mainloop error: {error}");
        }
        drop(child);
    });

    let init_result = server
        .initialize(InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: Url::from_file_path(&workspace_dir).expect("invalid workspace path"),
                name: "root".into(),
            }]),
            capabilities: options.capabilities,
            ..InitializeParams::default()
        })
        .await
        .expect("initialize failed");
    server
        .initialized(InitializedParams {})
        .expect("initialized notification failed");
    time::sleep(Duration::from_millis(200)).await;

    TestContext {
        server,
        diagnostics,
        stash: Vec::new(),
        refreshes,
        messages,
        mainloop,
        init_result,
        workspace_dir,
        _temp_dir: temp_dir,
    }
}

impl TestContext {
    fn workspace_uri(&self, relative: &str) -> Url {
        Url::from_file_path(self.workspace_dir.join(relative)).expect("workspace file URI")
    }

    fn open(&mut self, relative: &str, text: &str) -> Url {
        let uri = self.workspace_uri(relative);
        self.open_uri(&uri, text);
        uri
    }

    fn open_uri(&mut self, uri: &Url, text: &str) {
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
    }

    /// A `None` range replaces the whole document.
    fn change(&mut self, uri: &Url, version: i32, range: Option<Range>, text: &str) {
        self.server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range,
                    range_length: None,
                    text: text.to_owned(),
                }],
            })
            .expect("didChange failed");
    }

    fn save(&mut self, uri: &Url) {
        self.server
            .did_save(DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                text: None,
            })
            .expect("didSave failed");
    }

    fn close(&mut self, uri: &Url) {
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

    fn drain_diagnostics(&mut self) {
        self.stash.clear();
        while self.diagnostics.try_recv().is_ok() {}
    }

    /// Skips (and consumes) the versioned first-wave publishes; returns the next
    /// version-less one.
    async fn recv_diagnostics(&mut self, uri: &Url) -> PublishDiagnosticsParams {
        self.recv_publish(uri, true).await
    }

    async fn recv_first_diagnostics(&mut self, uri: &Url) -> PublishDiagnosticsParams {
        self.recv_publish(uri, false).await
    }

    async fn recv_publish(&mut self, uri: &Url, settled: bool) -> PublishDiagnosticsParams {
        let wanted = |params: &PublishDiagnosticsParams| {
            params.uri == *uri && (!settled || params.version.is_none())
        };
        if let Some(index) = self.stash.iter().position(wanted) {
            let params = self.stash.remove(index);
            let mut seen = 0;
            self.stash.retain(|stashed| {
                seen += 1;
                seen > index || stashed.uri != *uri
            });
            return params;
        }
        timeout(TIMEOUT, async {
            loop {
                let params = self
                    .diagnostics
                    .recv()
                    .await
                    .expect("diagnostics channel closed");
                if wanted(&params) {
                    return params;
                }
                if params.uri != *uri {
                    self.stash.push(params);
                }
            }
        })
        .await
        .expect("timed out waiting for diagnostics")
    }

    async fn recv_message(&mut self) -> ShowMessageParams {
        timeout(TIMEOUT, self.messages.recv())
            .await
            .expect("timed out waiting for a message")
            .expect("messages channel closed")
    }

    async fn recv_refresh(&mut self) {
        timeout(TIMEOUT, self.refreshes.recv())
            .await
            .expect("timed out waiting for the diagnostic refresh request")
            .expect("refresh channel closed");
    }

    async fn hover(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
    ) -> async_lsp::Result<Option<Hover>> {
        self.server
            .hover(HoverParams {
                text_document_position_params: position_params(uri, line, character),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    async fn hover_markdown(&mut self, uri: &Url, line: u32, character: u32) -> String {
        let hover = self
            .hover(uri, line, character)
            .await
            .expect("hover failed")
            .expect("expected a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markdown hover");
        };
        markup.value
    }

    async fn hover_range(&mut self, uri: &Url, line: u32, character: u32) -> Range {
        self.hover(uri, line, character)
            .await
            .expect("hover failed")
            .expect("expected a hover")
            .range
            .expect("hover should report a range")
    }

    async fn definition(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
    ) -> async_lsp::Result<Option<GotoDefinitionResponse>> {
        self.server
            .definition(GotoDefinitionParams {
                text_document_position_params: position_params(uri, line, character),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
    }

    async fn definition_location(&mut self, uri: &Url, line: u32, character: u32) -> Location {
        let response = self
            .definition(uri, line, character)
            .await
            .expect("definition failed")
            .expect("expected a definition");
        let GotoDefinitionResponse::Scalar(location) = response else {
            panic!("expected a scalar definition");
        };
        location
    }

    async fn references(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> async_lsp::Result<Option<Vec<Location>>> {
        self.server
            .references(ReferenceParams {
                text_document_position: position_params(uri, line, character),
                context: ReferenceContext {
                    include_declaration,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
    }

    async fn completion(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
    ) -> async_lsp::Result<Option<CompletionResponse>> {
        self.server
            .completion(CompletionParams {
                text_document_position: position_params(uri, line, character),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            })
            .await
    }

    async fn signature_help(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
    ) -> async_lsp::Result<Option<SignatureHelp>> {
        self.server
            .signature_help(SignatureHelpParams {
                context: None,
                text_document_position_params: position_params(uri, line, character),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    async fn rename(
        &mut self,
        uri: &Url,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> async_lsp::Result<Option<WorkspaceEdit>> {
        self.server
            .rename(RenameParams {
                text_document_position: position_params(uri, line, character),
                new_name: new_name.to_owned(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    async fn inlay_hints(
        &mut self,
        uri: &Url,
        range: Range,
    ) -> async_lsp::Result<Option<Vec<InlayHint>>> {
        self.server
            .inlay_hint(InlayHintParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range,
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    async fn formatting(&mut self, uri: &Url) -> async_lsp::Result<Option<Vec<TextEdit>>> {
        self.server
            .formatting(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                options: FormattingOptions::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    /// The text of the first formatting edit.
    async fn formatted(&mut self, uri: &Url) -> String {
        self.formatting(uri)
            .await
            .expect("formatting failed")
            .expect("expected formatting edits")
            .into_iter()
            .next()
            .expect("expected a formatting edit")
            .new_text
    }

    async fn range_formatting(
        &mut self,
        uri: &Url,
        range: Range,
    ) -> async_lsp::Result<Option<Vec<TextEdit>>> {
        self.server
            .range_formatting(DocumentRangeFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range,
                options: FormattingOptions::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
    }

    async fn document_symbols(&mut self, uri: &Url) -> Vec<DocumentSymbol> {
        let response = self
            .server
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("document_symbol request failed")
            .expect("expected document symbols");
        let DocumentSymbolResponse::Nested(symbols) = response else {
            panic!("expected nested symbols: {response:?}");
        };
        symbols
    }

    async fn semantic_tokens(&mut self, uri: &Url) -> SemanticTokens {
        let result = self
            .server
            .semantic_tokens_full(SemanticTokensParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .expect("semantic tokens request failed")
            .expect("expected semantic tokens");
        let SemanticTokensResult::Tokens(tokens) = result else {
            panic!("expected a tokens result");
        };
        tokens
    }

    async fn document_diagnostic(
        &mut self,
        uri: &Url,
        previous_result_id: Option<String>,
    ) -> DocumentDiagnosticReportResult {
        self.server
            .document_diagnostic(diagnostic_params(uri, previous_result_id))
            .await
            .expect("pull diagnostics failed")
    }

    async fn shutdown(mut self) {
        self.server.shutdown(()).await.expect("shutdown failed");
        self.server.exit(()).expect("exit failed");
        timeout(TIMEOUT, self.mainloop)
            .await
            .expect("timed out waiting for the server to exit")
            .expect("the client main loop panicked");
    }
}

fn position_params(uri: &Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(line, character),
    }
}

fn diagnostic_params(uri: &Url, previous_result_id: Option<String>) -> DocumentDiagnosticParams {
    DocumentDiagnosticParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        identifier: Some("ry".into()),
        previous_result_id,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn pull_capabilities(refresh_support: bool) -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: Some(DiagnosticClientCapabilities::default()),
            ..TextDocumentClientCapabilities::default()
        }),
        workspace: refresh_support.then(|| WorkspaceClientCapabilities {
            diagnostic: Some(DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..WorkspaceClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    }
}

fn messages(published: PublishDiagnosticsParams) -> Vec<String> {
    published
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn report_messages(report: DocumentDiagnosticReportResult) -> Vec<String> {
    match report {
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => full
            .full_document_diagnostic_report
            .items
            .into_iter()
            .map(|item| item.message)
            .collect(),
        other => panic!("expected a full report, got: {other:?}"),
    }
}

fn mentions(messages: &[String], needle: &str) -> bool {
    messages.iter().any(|message| message.contains(needle))
}

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
    assert!(capabilities.document_symbol_provider.is_some());
    assert!(capabilities.inlay_hint_provider.is_some());
    assert!(capabilities.signature_help_provider.is_some());
    assert!(capabilities.semantic_tokens_provider.is_some());
    assert!(capabilities.diagnostic_provider.is_some());
    assert_eq!(
        capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16)
    );
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
async fn utf8_is_negotiated_when_offered_and_positions_count_bytes() {
    let capabilities = ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..GeneralClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    };
    let mut context = setup_with(&[], capabilities).await;
    assert_eq!(
        context.init_result.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF8)
    );
    let uri = context.open("R/utf8_emoji.R", "target <- 1L\ny <- f(\"🦀\", target)\n");
    // Byte column 17 is inside `target` (bytes 15..21).
    let range = context.hover_range(&uri, 1, 17).await;
    assert_eq!(
        (range.start.character, range.end.character),
        (15, 21),
        "expected the UTF-8 byte span of `target` after an emoji, got: {range:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn push_diagnostics_report_lints_and_syntax_errors() {
    let mut context = setup_test(&[]).await;
    let assignment = context.open("R/assign.R", "x = 1\n");
    let clean = context.open("R/clean.R", "y <- 1\n");
    let syntax = context.open("R/syntax.R", "z <- (\n");

    let published = messages(context.recv_diagnostics(&assignment).await);
    assert_eq!(published.len(), 1, "{published:?}");
    assert!(
        published[0].contains("Use <-, not =, for assignment"),
        "{published:?}"
    );

    let published = messages(context.recv_diagnostics(&clean).await);
    assert!(published.is_empty(), "{published:?}");

    let published = messages(context.recv_diagnostics(&syntax).await);
    assert!(
        published
            .first()
            .is_some_and(|message| message.contains("unclosed `(`")),
        "expected syntax diagnostics: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn diagnostics_arrive_in_a_fast_wave_then_a_settled_wave() {
    let mut context = setup_test(&[]).await;
    let broken = context.open("R/bad.R", "x <- (\nbad_type <- 1L + \"a\"\n");
    let unresolved = context.open("R/unresolved.R", "main <- function() missing_helper()\n");

    // The first wave is versioned and carries the cheap classes.
    let first = context.recv_first_diagnostics(&broken).await;
    assert_eq!(first.version, Some(1));
    let first = messages(first);
    assert!(mentions(&first, "unclosed `(`"), "{first:?}");

    let first = messages(context.recv_first_diagnostics(&unresolved).await);
    assert!(
        !mentions(&first, "missing_helper"),
        "name resolution is not part of the first wave: {first:?}"
    );
    let settled = messages(context.recv_diagnostics(&unresolved).await);
    assert!(
        mentions(&settled, "missing_helper"),
        "the settled wave carries the unresolved reference: {settled:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn burst_of_edits_settles_on_the_final_text() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/burst.R", "x <- 1\n");
    context.recv_diagnostics(&uri).await;
    for version in 2..=6 {
        let text = if version == 6 {
            "x = 1\n".to_owned()
        } else {
            format!("x <- {version}\n")
        };
        context.change(&uri, version, None, &text);
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let published = messages(context.recv_diagnostics(&uri).await);
        if published.len() == 1 && published[0].contains("Use <-, not =, for assignment") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never settled on the final text: {published:?}"
        );
    }
    context.shutdown().await;
}

#[tokio::test]
async fn hover_shows_types_and_definition_summaries() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "R/hover.R",
        "count <- 1L\nuse <- function() {\n  value <- count + 1\n  value + print(value)\n}\n",
    );
    context.recv_diagnostics(&uri).await;

    let declaration = context.hover_markdown(&uri, 0, 2).await;
    assert!(declaration.contains("integer"), "{declaration}");

    // A read of the file's own top-level binding is a package global.
    let global = context.hover_markdown(&uri, 2, 12).await;
    assert!(
        global.contains("Package global, defined at `R/hover.R:1:1`"),
        "{global}"
    );

    // A local read points at its defining write.
    let local = context.hover_markdown(&uri, 3, 3).await;
    assert!(
        local.contains("Local variable, defined at `R/hover.R:3:3`"),
        "{local}"
    );

    // A stub name reports its declaring namespace.
    let stub = context.hover_markdown(&uri, 3, 11).await;
    assert!(stub.contains("From the `base` package."), "{stub}");

    assert!(!global.contains("### Debug"), "{global}");
    context.shutdown().await;
}

#[tokio::test]
async fn debug_flag_adds_hover_debug_sections() {
    let mut context = launch(Launch {
        args: &["--debug"],
        ..Launch::default()
    })
    .await;
    let uri = context.open("R/debug.R", "count <- 1L\nuse <- count\n");
    context.recv_diagnostics(&uri).await;

    let markdown = context.hover_markdown(&uri, 1, 8).await;
    for marker in ["### Debug", "**Lowering**", "**Naming**", "**Parsing**"] {
        assert!(markdown.contains(marker), "{markdown}");
    }
    context.shutdown().await;
}

#[tokio::test]
async fn positions_count_utf16_units_by_default() {
    let mut context = setup_test(&[]).await;

    // The emoji is 4 UTF-8 bytes / 2 UTF-16 units inside the string.
    let emoji = context.open("R/emoji.R", "s <- \"😀\"\ncount <- 1L\n");
    context.recv_diagnostics(&emoji).await;
    let range = context.hover_range(&emoji, 1, 2).await;
    assert_eq!(range, Range::new(Position::new(1, 0), Position::new(1, 5)));

    // `é` is one UTF-16 code unit but two UTF-8 bytes, so the byte column of
    // `target` (16) differs from its UTF-16 column (15).
    let bmp = context.open("R/bmp.R", "target <- 1L\ny <- f(\"café\", target)\n");
    let range = context.hover_range(&bmp, 1, 17).await;
    assert_eq!(range.start.line, 1);
    assert_eq!(
        (range.start.character, range.end.character),
        (15, 21),
        "expected the UTF-16 span of `target`, got: {range:?}"
    );

    // `caféx` is five scalars / five UTF-16 units but six UTF-8 bytes.
    let goto = context.open("R/goto.R", "caféx <- 1L\ny <- caféx\n");
    let location = context.definition_location(&goto, 1, 7).await;
    assert_eq!(location.range.start, Position::new(0, 0));
    assert_eq!(
        location.range.end.character, 5,
        "expected the UTF-16 end column of `caféx`, got: {:?}",
        location.range
    );

    // `café_fn` is seven UTF-16 units but eight UTF-8 bytes.
    let symbol_uri = context.open("R/symbol.R", "café_fn <- function() 1\n");
    let symbols = context.document_symbols(&symbol_uri).await;
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

    // `🦀` is 2 UTF-16 units / 4 UTF-8 bytes / 1 scalar; the `T` lint span
    // diverges: UTF-16 = 11..12, UTF-8 byte = 13..14.
    let emoji_diagnostic = context.open("R/emoji_diag.R", "\"🦀\"; x <- T\n");
    let published = context.recv_diagnostics(&emoji_diagnostic).await;
    let true_diagnostic = published
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("TRUE"))
        .unwrap_or_else(|| {
            panic!(
                "expected a T-vs-TRUE diagnostic, got: {:?}",
                published.diagnostics
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
async fn goto_definition() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "R/def.R",
        "helper <- function(x) x\ncaller <- function() helper(1)\n",
    );
    context.recv_diagnostics(&uri).await;
    let location = context.definition_location(&uri, 1, 22).await;
    assert_eq!(location.uri, uri);
    assert_eq!(
        location.range,
        Range::new(Position::new(0, 0), Position::new(0, 6))
    );
    context.shutdown().await;
}

#[tokio::test]
async fn stub_names_lead_to_their_declaration_site() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/stub.R", "sizes <- head(1:9)\n");
    context.recv_diagnostics(&uri).await;

    let location = context.definition_location(&uri, 0, 10).await;
    let path = location.uri.path();
    assert!(
        path.ends_with("utils.Rtypes"),
        "expected the materialized utils stub, got {path}"
    );
    let text = fs::read_to_string(
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

    let markdown = context.hover_markdown(&uri, 0, 10).await;
    assert!(
        markdown.contains("From the `utils` package")
            && markdown.contains("Declared at `utils.Rtypes:"),
        "expected the stub declaration location in: {markdown}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn formatting_rewrites_documents_and_refuses_on_syntax_errors() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\n");
    let broken = context.open("R/bad.R", "y <- (\n");
    context.recv_diagnostics(&uri).await;
    context.recv_diagnostics(&broken).await;

    let edits = context
        .formatting(&uri)
        .await
        .expect("formatting failed")
        .expect("expected formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "x <- 1\n");
    assert_eq!(edits[0].range.start, Position::new(0, 0));

    let edits = context
        .formatting(&broken)
        .await
        .expect("formatting failed");
    assert!(edits.is_none(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn range_formatting_snaps_to_whole_statements() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/fmt.R", "x<-1\ny<-2\nz<-3\n");
    let spaced = context.open("R/spaced.R", "a <- 1\n\n\nb <- 2\n");
    context.recv_diagnostics(&uri).await;
    context.recv_diagnostics(&spaced).await;

    // Covers part of the middle line only; the edit must cover that whole statement.
    let edits = context
        .range_formatting(&uri, Range::new(Position::new(1, 1), Position::new(1, 2)))
        .await
        .expect("range formatting failed")
        .expect("expected range formatting edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "y <- 2");
    assert_eq!(
        edits[0].range,
        Range::new(Position::new(1, 0), Position::new(1, 4))
    );

    // A selection holding no statement is left alone.
    let edits = context
        .range_formatting(
            &spaced,
            Range::new(Position::new(1, 0), Position::new(2, 0)),
        )
        .await
        .expect("range formatting failed");
    assert!(edits.is_none(), "{edits:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_report_known_diagnostics_and_unchanged_repeats() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    let uri = context.open("R/warn.R", "x = 1\n");
    let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) =
        context.document_diagnostic(&uri, None).await
    else {
        panic!("expected a full report");
    };
    assert_eq!(full.full_document_diagnostic_report.items.len(), 1);

    let result_id = full
        .full_document_diagnostic_report
        .result_id
        .expect("result id");
    let repeat = context.document_diagnostic(&uri, Some(result_id)).await;
    assert!(
        matches!(
            repeat,
            DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(_))
        ),
        "expected an unchanged report"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_are_empty_for_clean_and_untracked_documents() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    let clean = context.open("R/clean.R", "x <- 1\n");
    let untracked = context.workspace_uri("R/never_opened.R");
    for uri in [clean, untracked] {
        let items = report_messages(context.document_diagnostic(&uri, None).await);
        assert!(items.is_empty(), "{uri}: {items:?}");
    }
    context.shutdown().await;
}

#[tokio::test]
async fn pull_client_without_refresh_support_gets_neither_push_nor_refresh() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    context.open("R/warn.R", "x = 1\n");
    let uri = context.open("R/dep.R", "y <- 1\n");
    context.save(&uri);
    time::sleep(Duration::from_millis(400)).await;
    assert!(
        context.diagnostics.try_recv().is_err(),
        "push must be suppressed for pull clients"
    );
    assert!(
        context.refreshes.try_recv().is_err(),
        "a client without refresh support must not receive refresh requests"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn dependency_affecting_save_requests_diagnostic_refresh() {
    let mut context = setup_with(&[], pull_capabilities(true)).await;
    let uri = context.open("R/dep.R", "x <- 1\n");
    context.save(&uri);
    context.recv_refresh().await;
    context.shutdown().await;
}

#[tokio::test]
async fn workspace_root_comes_from_the_client_not_the_process_cwd() {
    let mut context = setup_test(&[("R/lib.R", "make_count <- function() 1L\n")]).await;
    let uri = context.open("R/use.R", "total <- make_count()\n");
    let published = messages(context.recv_diagnostics(&uri).await);
    assert!(
        published.is_empty(),
        "cross-file resolution must succeed: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn malformed_config_falls_back_to_defaults_and_reports() {
    let mut context = setup_test(&[("ry.toml", "[check]\ntyping = 1\n")]).await;
    let message = context.recv_message().await;
    assert!(
        message.message.contains("invalid config"),
        "{}",
        message.message
    );

    let config = context.workspace_uri("ry.toml");
    let published = context.recv_first_diagnostics(&config).await.diagnostics;
    assert_eq!(published.len(), 1, "{published:?}");
    assert_eq!(
        published[0].code,
        Some(NumberOrString::String("config".to_owned()))
    );
    assert!(
        published[0].message.contains("invalid config"),
        "{}",
        published[0].message
    );

    let uri = context.open("R/clean.R", "x <- 1\n");
    let published = messages(context.recv_diagnostics(&uri).await);
    assert!(published.is_empty(), "{published:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn ancestor_config_governs_a_workspace_without_its_own() {
    let mut context = setup_test(&[("../ry.toml", "[format]\nindent-width = 7\n")]).await;
    let uri = context.open("R/indent.R", "f <- function(x) {\nx + 1\n}\n");
    context.drain_diagnostics();
    let formatted = context.formatted(&uri).await;
    assert!(
        formatted.contains("       x + 1"),
        "expected 7-space indentation from the ancestor config, got:\n{formatted}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn config_reloads_and_a_failed_reload_keeps_the_previous_config() {
    let mut context = setup_test(&[("ry.toml", "[format]\nindent-width = 4\n")]).await;
    let uri = context.open("R/indent.R", "f <- function(x) {\nx + 1\n}\n");
    context.drain_diagnostics();
    let indented = |width: usize| format!("f <- function(x) {{\n{}x + 1\n}}\n", " ".repeat(width));
    assert_eq!(
        context.formatted(&uri).await,
        indented(4),
        "the initial config governs"
    );

    fs::write(
        context.workspace_dir.join("ry.toml"),
        "[format]\nindent-width = 3\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);
    assert_eq!(
        context.formatted(&uri).await,
        indented(3),
        "the reloaded config governs"
    );

    fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\ntyping = 1\n",
    )
    .expect("write broken config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);
    let message = context.recv_message().await;
    assert!(
        message
            .message
            .contains("keeping the previous configuration"),
        "{}",
        message.message
    );
    assert_eq!(
        context.formatted(&uri).await,
        indented(3),
        "the previous config keeps governing"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_refreshes_push_diagnostics() {
    let mut context = setup_test(&[("ry.toml", "[check]\nunused = false\n")]).await;
    let uri = context.open("R/dead.R", "f <- function() {\n  dead <- 1\n  2\n}\n");
    let settled = messages(context.recv_diagnostics(&uri).await);
    assert!(settled.is_empty(), "unused is off initially: {settled:?}");

    fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\nunused = true\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);

    let refreshed = messages(context.recv_diagnostics(&uri).await);
    assert!(
        mentions(&refreshed, "never used"),
        "the config toggle re-publishes with the unused finding: {refreshed:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn config_reload_requests_refresh_for_pull_clients() {
    let mut context = setup_with(
        &[("ry.toml", "[check]\nunused = false\n")],
        pull_capabilities(true),
    )
    .await;
    context.open("R/dead.R", "f <- function() 1\n");
    fs::write(
        context.workspace_dir.join("ry.toml"),
        "[check]\nunused = true\n",
    )
    .expect("update config");
    context.notify_watched_file_changed("ry.toml", FileChangeType::CHANGED);
    context.recv_refresh().await;
    context.shutdown().await;
}

#[tokio::test]
async fn deeply_nested_documents_do_not_kill_the_server() {
    let mut context = setup_test(&[]).await;

    let deep = format!("value <- {}1{}\n", "f(".repeat(900), ")".repeat(900));
    let deep_uri = context.open("R/deep_valid.R", &deep);
    context.recv_diagnostics(&deep_uri).await;

    let malformed = format!("value <- {}1{}\n", "f(".repeat(2000), ")".repeat(1000));
    let malformed_uri = context.open("R/deep_malformed.R", &malformed);
    context.recv_diagnostics(&malformed_uri).await;

    // Formatting recurses over the tree too; it must answer or refuse without dying.
    let formatted = context.formatting(&deep_uri).await;
    assert!(
        matches!(formatted, Ok(_) | Err(async_lsp::Error::Response(_))),
        "{formatted:?}"
    );

    let probe_uri = context.open("R/probe.R", "x <- 1\n");
    let published = messages(context.recv_diagnostics(&probe_uri).await);
    assert!(
        published.is_empty(),
        "the server still answers cleanly after the deep documents: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn namespace_buffer_publishes_import_validation() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "NAMESPACE",
        "import(stats)\nimportFrom(stats, sd, medain)\nimportFrom(dplyr, mutate)\n",
    );
    let published = context.recv_first_diagnostics(&uri).await.diagnostics;
    assert_eq!(
        published.len(),
        1,
        "only the known-namespace typo is reported: {published:?}"
    );
    let diagnostic = &published[0];
    assert!(
        diagnostic
            .message
            .contains("`medain` is not exported by `stats`, so this package will not load."),
        "unexpected message: {diagnostic:?}"
    );
    assert_eq!(
        diagnostic.severity,
        Some(DiagnosticSeverity::ERROR),
        "R refuses to load such a package: {diagnostic:?}"
    );
    assert_eq!(
        diagnostic.range.start.line, 1,
        "reports on the importFrom line"
    );

    context.change(
        &uri,
        2,
        Some(Range::new(Position::new(1, 22), Position::new(1, 28))),
        "median",
    );
    let published = context.recv_first_diagnostics(&uri).await.diagnostics;
    assert!(
        published.is_empty(),
        "the corrected NAMESPACE is clean: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn initialize_without_r_directory() {
    let mut context = launch(Launch {
        without_r_directory: true,
        ..Launch::default()
    })
    .await;
    assert!(
        context
            .init_result
            .capabilities
            .text_document_sync
            .is_some(),
        "expected initialize to succeed without an R directory"
    );

    fs::create_dir_all(context.workspace_dir.join("R")).expect("create R directory");
    fs::write(context.workspace_dir.join("R/created_later.R"), "x <- T\n")
        .expect("write test file");
    context.notify_watched_file_changed("R/created_later.R", FileChangeType::CREATED);

    let uri = context.open("R/created_later.R", "x <- T\n");
    let published = messages(context.recv_diagnostics(&uri).await);
    assert!(
        mentions(&published, "TRUE"),
        "expected diagnostics after creating R directory and file, got: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn requests_for_unopened_non_file_uris_answer_gracefully() {
    let mut context = setup_test(&[("R/main.R", "x <- 1L")]).await;
    let untitled = Url::parse("untitled:Untitled-9").expect("untitled uri should parse");

    let hover = context.hover(&untitled, 0, 0).await;
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

    let uri = context.open("R/main.R", "x <- 1L");
    context.recv_diagnostics(&uri).await;
    context.shutdown().await;
}

#[tokio::test]
async fn untitled_document_is_served_as_a_standalone_script() {
    let mut context = setup_test(&[]).await;
    let untitled = Url::parse("untitled:Untitled-1").expect("untitled uri should parse");
    context.open_uri(&untitled, "answer <- 42L\nfinal = answer\n");

    let published = messages(context.recv_diagnostics(&untitled).await);
    assert!(
        mentions(&published, "<-"),
        "expected the `=` lint on the untitled buffer, got: {published:?}"
    );

    let markdown = context.hover_markdown(&untitled, 1, 10).await;
    assert!(markdown.contains("integer"), "{markdown}");
    context.shutdown().await;
}

#[tokio::test]
async fn full_text_did_change_replaces_the_document() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/full_sync.R", "x <- T\n");
    let initial = messages(context.recv_diagnostics(&uri).await);
    assert!(
        mentions(&initial, "TRUE"),
        "expected the T-vs-TRUE lint before the replacement, got: {initial:?}"
    );

    context.change(&uri, 2, None, "y = 1\n");
    let after = messages(context.recv_diagnostics(&uri).await);
    assert!(
        mentions(&after, "<-"),
        "expected the `=` lint from the replacement text, got: {after:?}"
    );
    assert!(
        !mentions(&after, "TRUE"),
        "the old text must be fully replaced, not merged, got: {after:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn did_close_survives_a_failing_disk_reread() {
    let mut context = setup_test(&[]).await;
    fs::create_dir_all(context.workspace_dir.join("R/casualty.R"))
        .expect("create directory posing as a source file");

    let casualty = context.open("R/casualty.R", "x <- 1\n");
    context.drain_diagnostics();
    context.close(&casualty);

    let uri = context.open("R/still_alive.R", "x <- T\n");
    let published = messages(context.recv_diagnostics(&uri).await);
    assert!(
        mentions(&published, "TRUE"),
        "expected the server to survive the failing close-time re-read, got: {published:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn watched_file_create_and_delete_change_cross_file_resolution() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    let consumer = context.open("R/consumer.R", "value <- helper()\n");
    let before = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(mentions(&before, "`helper`"), "{before:?}");

    let helper = context.workspace_dir.join("R/helper.R");
    fs::write(&helper, "helper <- function() 1L\n").expect("write helper");
    context.notify_watched_file_changed("R/helper.R", FileChangeType::CREATED);
    let created = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(!mentions(&created, "`helper`"), "{created:?}");

    fs::remove_file(&helper).expect("delete helper");
    context.notify_watched_file_changed("R/helper.R", FileChangeType::DELETED);
    let deleted = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(mentions(&deleted, "`helper`"), "{deleted:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn an_open_buffer_shadows_the_disk_until_closed() {
    let mut context = setup_with(
        &[
            ("R/helper.R", "helper <- function() 1L\n"),
            ("R/consumer.R", "value <- helper()\n"),
        ],
        pull_capabilities(false),
    )
    .await;
    let helper = context.open("R/helper.R", "helper <- function() 1L\n");
    let consumer = context.open("R/consumer.R", "value <- helper()\n");

    fs::write(
        context.workspace_dir.join("R/helper.R"),
        "renamed_on_disk <- 1L\n",
    )
    .expect("rewrite helper on disk");
    context.notify_watched_file_changed("R/helper.R", FileChangeType::CHANGED);
    let disk_changed = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(!mentions(&disk_changed, "`helper`"), "{disk_changed:?}");

    fs::write(
        context.workspace_dir.join("R/helper.R"),
        "helper <- function() 1L\n",
    )
    .expect("restore helper on disk");
    context.change(&helper, 2, None, "renamed_in_buffer <- 1L\n");
    let edited = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(mentions(&edited, "`helper`"), "{edited:?}");

    context.close(&helper);
    let closed = report_messages(context.document_diagnostic(&consumer, None).await);
    assert!(!mentions(&closed, "`helper`"), "{closed:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn a_malformed_edit_suppresses_then_restores_semantic_diagnostics() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    let uri = context.open("R/main.R", "value <- helper()\n");
    let clean = report_messages(context.document_diagnostic(&uri, None).await);
    assert!(mentions(&clean, "`helper`"), "{clean:?}");

    context.change(&uri, 2, None, "value <- helper(\n");
    let malformed = report_messages(context.document_diagnostic(&uri, None).await);
    assert!(!mentions(&malformed, "`helper`"), "{malformed:?}");
    assert!(!malformed.is_empty(), "the syntax error is reported");

    context.change(&uri, 3, None, "value <- helper()\n");
    let restored = report_messages(context.document_diagnostic(&uri, None).await);
    assert!(mentions(&restored, "`helper`"), "{restored:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn pull_diagnostics_match_pushed_across_files() {
    let mut context = setup_test(&[]).await;
    let file_a = context.open("R/match_a.R", "x <- T\n");
    let file_b = context.open("R/match_b.R", "y = 1\n");

    let pushed_a = messages(context.recv_diagnostics(&file_a).await);
    let pushed_b = messages(context.recv_diagnostics(&file_b).await);
    let pulled_a = report_messages(context.document_diagnostic(&file_a, None).await);
    let pulled_b = report_messages(context.document_diagnostic(&file_b, None).await);

    assert_eq!(pulled_a, pushed_a);
    assert_eq!(pulled_b, pushed_b);
    assert!(!pulled_a.is_empty() && !pulled_b.is_empty());
    context.shutdown().await;
}

#[tokio::test]
async fn breaking_one_file_leaves_its_dependents_untouched() {
    let mut context = setup_test(&[]).await;
    let library = context.open("R/library.R", "shared_helper <- function() 1L\n");
    let consumer = context.open("R/consumer.R", "value <- shared_helper()\n");
    let settled = messages(context.recv_diagnostics(&consumer).await);
    assert!(settled.is_empty(), "the consumer starts clean: {settled:?}");

    context.change(
        &library,
        2,
        Some(Range::new(Position::new(1, 0), Position::new(1, 0))),
        "broken <- function() {\n",
    );
    let broken = loop {
        let settled = context.recv_diagnostics(&library).await;
        if !settled.diagnostics.is_empty() {
            break settled;
        }
    };
    assert!(
        broken
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code
                == Some(NumberOrString::String("syntax-error".to_owned()))),
        "the broken file reports its syntax error and nothing else: {:?}",
        broken.diagnostics
    );

    // Saving while broken refreshes every open document; `shared_helper` still resolves.
    context.save(&consumer);
    let after_break = messages(context.recv_diagnostics(&consumer).await);
    assert!(
        after_break.is_empty(),
        "the consumer is untouched while its dependency is mid-edit: {after_break:?}"
    );
    context.shutdown().await;
}

/// The edit is sent only after the held pull announces itself through the marker
/// file, so its cancellation provably lands in flight; the retry must succeed.
#[tokio::test]
async fn cancelled_pull_is_retryable_and_recovers() {
    let marker_dir = tempfile::tempdir().expect("marker dir");
    let marker = marker_dir.path().join("pull.marker");
    let marker_text = marker.to_str().expect("utf-8 marker path");
    let mut context = launch(Launch {
        capabilities: pull_capabilities(false),
        env: &[
            ("RY_TEST_DELAY_PULL_MS", "1000"),
            ("RY_TEST_PULL_MARKER", marker_text),
        ],
        ..Launch::default()
    })
    .await;
    let uri = context.open("R/cancel.R", "x = 1\n");

    // Socket sends are eager, so the request is written before the edit.
    let pending = context
        .server
        .document_diagnostic(diagnostic_params(&uri, None));
    timeout(TIMEOUT, async {
        while !marker.exists() {
            time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the server never started holding the pull");
    context.change(&uri, 2, None, "y = 2\n");

    let result = pending.await;
    let Err(async_lsp::Error::Response(error)) = result else {
        panic!("expected the cancelled-pull error, got {result:?}");
    };
    assert_eq!(error.code, ErrorCode::SERVER_CANCELLED, "{error:?}");
    let data = error.data.as_ref().expect("cancellation data");
    assert_eq!(
        data.get("retriggerRequest")
            .and_then(|value| value.as_bool()),
        Some(true),
        "{data:?}"
    );

    let retried = report_messages(context.document_diagnostic(&uri, None).await);
    assert!(
        mentions(&retried, "<-"),
        "expected the assignment-operator lint on the edited text: {retried:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn completion() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/comp.R", "my_function <- function(x) x\nmy_f\n");
    context.drain_diagnostics();

    let response = context
        .completion(&uri, 1, 4)
        .await
        .expect("completion request failed")
        .expect("expected completions");
    let CompletionResponse::List(list) = response else {
        panic!("expected a CompletionList response carrying isIncomplete");
    };
    assert!(
        !list.is_incomplete,
        "small candidate set should not be marked incomplete"
    );
    let my_function = list
        .items
        .iter()
        .find(|item| item.label == "my_function")
        .unwrap_or_else(|| {
            panic!(
                "expected 'my_function' in completions, got: {:?}",
                list.items
            )
        });
    assert!(
        my_function.insert_text.is_none() && my_function.insert_text_format.is_none(),
        "a non-snippet client must get plain-label insertion"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn completion_inserts_call_snippets_for_functions() {
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
    let mut context = setup_with(&[], capabilities).await;
    let uri = context.open(
        "R/snip.R",
        "snip_args <- function(x) x\nsnip_none <- function() 1L\nsn\n",
    );
    context.drain_diagnostics();

    let response = context
        .completion(&uri, 2, 2)
        .await
        .expect("completion request failed")
        .expect("expected completions");
    let CompletionResponse::List(list) = response else {
        panic!("expected a list response");
    };
    let item = |label: &str| {
        list.items
            .iter()
            .find(|item| item.label == label)
            .unwrap_or_else(|| panic!("expected a {label} item"))
    };
    let snip_args = item("snip_args");
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
    assert_eq!(
        item("snip_none").insert_text.as_deref(),
        Some("snip_none()$0"),
        "a zero-argument function drops the cursor past the parens"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn signature_help_labels_parameters_by_offset_or_substring() {
    let offset_capabilities = ClientCapabilities {
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
    for (capabilities, offsets) in [
        (offset_capabilities, true),
        (ClientCapabilities::default(), false),
    ] {
        let mut context = setup_with(&[], capabilities).await;
        let uri = context.open("R/sig.R", "result <- lapply()\n");
        let help = context
            .signature_help(&uri, 0, 17)
            .await
            .expect("signature_help request failed")
            .expect("signature help expected inside the call");
        // `lapply` is an overload set, so the list carries every declared
        // candidate; the labels under test are the active one's.
        let active = help.active_signature.expect("an active signature") as usize;
        let signature = &help.signatures[active];
        let parameter_texts: Vec<&str> = signature
            .parameters
            .as_ref()
            .expect("signature parameters expected")
            .iter()
            .map(|parameter| match &parameter.label {
                ParameterLabel::LabelOffsets([start, end]) if offsets => signature
                    .label
                    .get(*start as usize..*end as usize)
                    .expect("parameter offsets must slice the label"),
                ParameterLabel::Simple(text) if !offsets => text.as_str(),
                label => panic!("label offset support is {offsets}, but got {label:?}"),
            })
            .collect();
        assert_eq!(
            parameter_texts,
            ["X: list[named: T]", "FUN: fn(T) -> U", "...: Any"]
        );
        assert_eq!(help.active_parameter, Some(0));
        context.shutdown().await;
    }
}

#[tokio::test]
async fn inlay_hints_respect_requested_viewport() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "R/hints.R",
        "count <- 1L\nlabel <- \"hello\"\nratio <- 2L\n",
    );
    context.drain_diagnostics();

    let hints = context
        .inlay_hints(&uri, Range::new(Position::new(1, 0), Position::new(1, 16)))
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
async fn rename_and_references_use_analysis_across_files() {
    let mut context =
        setup_test(&[("R/a.R", "value <- 1L\n"), ("R/b.R", "result <- value\n")]).await;
    let file_a = context.open("R/a.R", "value <- 1L\n");
    let file_b = context.workspace_uri("R/b.R");
    context.drain_diagnostics();

    let changes = context
        .rename(&file_a, 0, 1, "renamed")
        .await
        .expect("rename request failed")
        .expect("expected rename edit")
        .changes
        .expect("expected rename changes");
    let file_a_edits = changes.get(&file_a).expect("missing file A edits");
    assert_eq!(file_a_edits.len(), 1);
    assert_eq!(file_a_edits[0].new_text, "renamed");
    assert_eq!(file_a_edits[0].range.start, Position::new(0, 0));
    let file_b_edits = changes.get(&file_b).expect("missing file B edits");
    assert_eq!(file_b_edits.len(), 1);
    assert_eq!(file_b_edits[0].range.start, Position::new(0, 10));

    for refused in ["..1", "..12", "if", "_x", "1x"] {
        let result = context.rename(&file_a, 0, 1, refused).await;
        let Err(async_lsp::Error::Response(error)) = result else {
            panic!("expected `{refused}` to be refused, got {result:?}");
        };
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "{error:?}");
    }
    for accepted in ["...", "..x", ".x", "x_1.y"] {
        let edit = context
            .rename(&file_a, 0, 1, accepted)
            .await
            .expect("rename request failed");
        assert!(edit.is_some(), "expected `{accepted}` to be accepted");
    }

    let locations = context
        .references(&file_a, 0, 1, true)
        .await
        .expect("references request failed")
        .expect("expected reference locations");
    assert!(
        locations
            .iter()
            .any(|location| location.uri == file_a && location.range.start.line == 0),
        "expected the declaration in file A, got: {locations:?}"
    );
    assert!(
        locations
            .iter()
            .any(|location| location.uri == file_b && location.range.start.character == 10),
        "expected the cross-file use in file B, got: {locations:?}"
    );

    let without_declaration = context
        .references(&file_a, 0, 1, false)
        .await
        .expect("references request failed")
        .expect("expected reference locations");
    assert!(
        without_declaration
            .iter()
            .all(|location| location.uri != file_a),
        "expected the declaration to be excluded, got: {without_declaration:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn document_symbols_cover_functions_type_definitions_and_classes() {
    let mut context = setup_test(&[]).await;
    let functions = context.open(
        "R/syms.R",
        "add <- function(x, y) x + y\nmultiply <- function(a, b) a * b\n",
    );
    let types = context.open(
        "R/typed_syms.R",
        "#: @type point {list{x: double, y: double}}\n\n#: @alias points {list[point]}\nmake <- function() 1L\n",
    );
    let classes = context.open(
        "R/classes.R",
        "setClass(\"Person\", representation(name = \"character\"))\n\
         Account <- R6Class(\"Account\",\n\
         \x20 public = list(\n\
         \x20   balance = 0,\n\
         \x20   deposit = function(amount) invisible(self)\n\
         \x20 )\n\
         )\n",
    );
    context.drain_diagnostics();
    let find = |symbols: &[DocumentSymbol], name: &str| -> DocumentSymbol {
        symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("expected `{name}` in the outline: {symbols:?}"))
    };

    let symbols = context.document_symbols(&functions).await;
    assert_eq!(find(&symbols, "add").kind, SymbolKind::FUNCTION);
    find(&symbols, "multiply");

    let symbols = context.document_symbols(&types).await;
    let point = find(&symbols, "point");
    assert_eq!(point.kind, SymbolKind::STRUCT);
    assert_eq!(point.detail.as_deref(), Some("@type"));
    assert_eq!(find(&symbols, "points").kind, SymbolKind::INTERFACE);
    // Item bindings still appear alongside type definitions.
    find(&symbols, "make");

    let symbols = context.document_symbols(&classes).await;
    assert_eq!(find(&symbols, "Person").kind, SymbolKind::CLASS);
    let account = find(&symbols, "Account");
    assert_eq!(account.kind, SymbolKind::CLASS);
    let members = account.children.as_deref().expect("R6 members nest");
    assert_eq!(find(members, "balance").kind, SymbolKind::FIELD);
    let deposit = find(members, "deposit");
    assert_eq!(deposit.kind, SymbolKind::METHOD);
    assert_eq!(deposit.detail.as_deref(), Some("fn(amount)"));
    context.shutdown().await;
}

#[tokio::test]
async fn out_of_bounds_positions_are_safe_on_every_entry_point() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/oob.R", "alpha <- 1\n\nbeta <- 2\n");
    context.drain_diagnostics();

    // A character past the end of the empty line 1, and a line past the end.
    for (line, character) in [(1, 50), (50, 0)] {
        context
            .hover(&uri, line, character)
            .await
            .expect("hover must not panic the server");
        context
            .definition(&uri, line, character)
            .await
            .expect("definition must not panic the server");
        context
            .references(&uri, line, character, true)
            .await
            .expect("references must not panic the server");
        context
            .completion(&uri, line, character)
            .await
            .expect("completion must not panic the server");
        let signature = context
            .signature_help(&uri, line, character)
            .await
            .expect("signature help must not panic the server");
        assert!(signature.is_none(), "OOB signature help should be None");
        let rename = context
            .rename(&uri, line, character, "renamed")
            .await
            .expect("rename must not panic the server");
        assert!(rename.is_none(), "OOB rename should be None");
    }

    // Inlay hints over an oversized viewport are clamped, not fatal.
    context
        .inlay_hints(&uri, Range::new(Position::new(0, 0), Position::new(99, 99)))
        .await
        .expect("inlay hints must not panic the server");
    context.shutdown().await;
}

#[tokio::test]
async fn annotation_bodies_get_semantic_tokens() {
    let mut context = setup_test(&[]).await;
    let uri = context.open("R/tokens.R", "#: <T> fn(x: T) -> T\nid <- function(x) x\n");
    context.drain_diagnostics();
    let tokens = context.semantic_tokens(&uri).await;
    assert!(
        !tokens.data.is_empty(),
        "the #: annotation body must produce semantic tokens"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn ry_syntax_gets_semantic_tokens() {
    let mut context = setup_test(&[]).await;
    let source = "type Shape<T> = :Circle(radius: T) | :Empty\n\
                  area <- function(s: Shape<double>) {\n\
                  \x20 match (s) {\n\
                  \x20   :Circle(radius = r) => r\n\
                  \x20   :Empty => 0\n\
                  \x20 }\n\
                  }\n";
    let uri = context.open("R/tokens.R", source);
    context.drain_diagnostics();
    let tokens = context.semantic_tokens(&uri).await;
    let lines: Vec<&str> = source.lines().collect();
    let (mut line, mut column) = (0usize, 0usize);
    let mut classified = Vec::new();
    for token in &tokens.data {
        line += token.delta_line as usize;
        column = if token.delta_line == 0 {
            column + token.delta_start as usize
        } else {
            token.delta_start as usize
        };
        let text = &lines[line][column..column + token.length as usize];
        classified.push((text.to_owned(), token.token_type));
    }
    // Legend indices: type 0, typeParameter 1, parameter 2, operator 3, decorator 4,
    // keyword 5, enumMember 6, function 7, namespace 8.
    for expected in [
        ("area", 7),
        ("s", 2),
        ("type", 5),
        ("Shape", 0),
        ("T", 1),
        ("Circle", 6),
        ("radius", 2),
        ("Empty", 6),
        ("match", 5),
        ("=>", 3),
    ] {
        assert!(
            classified
                .iter()
                .any(|(text, class)| text == expected.0 && *class == expected.1),
            "{expected:?} missing from {classified:?}"
        );
    }
    assert!(
        !classified.iter().any(|(text, _)| text == "r"),
        "a local that is neither a function nor a parameter stays with the R grammar: {classified:?}"
    );
    context.shutdown().await;
}

#[tokio::test]
async fn stub_documents_are_served_with_parse_diagnostics() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "stubs/project.Rtypes",
        "good_helper : fn(x: double) -> double\nsize : Frobnicate\n",
    );
    let published = context.recv_first_diagnostics(&uri).await.diagnostics;
    assert_eq!(published.len(), 1, "{published:?}");
    let diagnostic = &published[0];
    assert!(
        diagnostic.message.contains("does not load") && diagnostic.message.contains("Frobnicate"),
        "{diagnostic:?}"
    );
    assert_eq!(diagnostic.range.start.line, 1, "whole-line on the bad line");

    context.change(&uri, 2, None, "good_helper : fn(x: double) -> double\n");
    let published = context.recv_first_diagnostics(&uri).await.diagnostics;
    assert!(published.is_empty(), "{published:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn stub_documents_answer_pull_diagnostics() {
    let mut context = setup_with(&[], pull_capabilities(false)).await;
    let uri = context.open("stubs/project.Rtypes", "size : Frobnicate\n");
    let items = report_messages(context.document_diagnostic(&uri, None).await);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].contains("does not load"), "{items:?}");
    context.shutdown().await;
}

#[tokio::test]
async fn stub_documents_get_semantic_tokens_and_type_definitions() {
    let mut context = setup_test(&[]).await;
    let uri = context.open(
        "stubs/project.Rtypes",
        "@type frame\nmake_frame : fn(n: integer) -> frame\n",
    );
    context.drain_diagnostics();

    let tokens = context.semantic_tokens(&uri).await;
    assert!(
        !tokens.data.is_empty(),
        "a stub buffer must produce semantic tokens"
    );

    // The cursor sits on the `frame` return-type reference (line 1, col 32).
    let location = context.definition_location(&uri, 1, 32).await;
    assert_eq!(location.uri, uri);
    assert_eq!(
        location.range,
        Range::new(Position::new(0, 6), Position::new(0, 11))
    );
    context.shutdown().await;
}
