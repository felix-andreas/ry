import * as fs from "fs"
import * as path from "path"

import {
  commands,
  debug,
  languages,
  window,
  workspace,
  CancellationToken,
  DebugAdapterDescriptor,
  DebugAdapterExecutable,
  DebugAdapterNamedPipeServer,
  DebugConfiguration,
  DebugSession,
  DebugStackFrame,
  DocumentSymbol,
  ExtensionContext,
  MarkdownString,
  Position,
  Range,
  Selection,
  StatusBarAlignment,
  StatusBarItem,
  SymbolKind,
  Terminal,
  TextEditor,
  ThemeColor,
  Uri,
  WorkspaceFolder,
} from 'vscode'

import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node'
import * as lsp from 'vscode-languageclient'

import { findConsoles, openViewer, ConsoleConnection, ConsoleEntry } from "./viewer"

const logger = window.createOutputChannel("ry Extension", { log: true })
const outputChannel = window.createOutputChannel("ry")

const EXTENSION_ROOT_DIR = path.dirname(__dirname)
const RY_BINARY_NAME = process.platform === "win32" ? "ry.exe" : "ry"
const BUNDLED_RY_EXECUTABLE = path.join(EXTENSION_ROOT_DIR, "bin", RY_BINARY_NAME)

let client: LanguageClient | null = null
let statusBar: StatusBarItem
let status = { health: "started" }
let version: string | undefined

export async function activate({ subscriptions, extension, }: ExtensionContext): Promise<void> {
  version = extension.packageJSON.version ?? "<unknown>"

  subscriptions.push(
    workspace.onDidChangeConfiguration(async (change) => {
      if (
        change.affectsConfiguration("ry.path")
        || change.affectsConfiguration("ry.args")
        || change.affectsConfiguration("ry.experimentalFeatures")
        || change.affectsConfiguration("roughly.path")
        || change.affectsConfiguration("roughly.args")
        || change.affectsConfiguration("roughly.experimentalFeatures")
      ) {
        const choice = await window.showWarningMessage(
          "Configuration change requires restarting the language server",
          "Restart",
        )
        if (choice === "Restart") {
          await restartClient()
          setTimeout(() => {
            client?.outputChannel.show()
          }, 1500)
        }
      }
    }),
    commands.registerCommand(
      "ry.restartServer",
      async () => {
        await restartClient()
      }
    ),
    commands.registerCommand(
      "ry.startServer",
      async () => {
        await restartClient()
      }
    ),
    commands.registerCommand(
      "ry.stopServer",
      async () => {
        await stopClient()
      }
    ),
    commands.registerCommand(
      "ry.openLogs",
      async () => {
        if (client?.outputChannel) {
          client.outputChannel.show()
        }
      }
    ),
    commands.registerCommand("ry.runFile", runFile),
    commands.registerCommand("ry.runTest", runTest),
    commands.registerCommand("ry.debugTest", debugTest),
    commands.registerCommand("ry.showReferences", showReferences),
    commands.registerCommand("ry.openConsole", () => openConsole(window.activeTextEditor?.document.uri)),
    commands.registerCommand("ry.sendSelection", sendSelection),
    commands.registerCommand("ry.sendFunction", sendFunction),
    commands.registerCommand("ry.sourceFile", sourceFile),
    commands.registerCommand("ry.attachConsole", attachConsole),
    commands.registerCommand("ry.viewData", viewData),
    { dispose: () => consoles.forEach((connection) => connection.dispose()) },
    debug.registerDebugAdapterDescriptorFactory("ry", { createDebugAdapterDescriptor }),
    debug.registerDebugConfigurationProvider("ry", { resolveDebugConfiguration }),
    window.onDidCloseTerminal((closed) => {
      if (closed === consoleTerminal) {
        consoleTerminal = undefined
      }
      if (closed === runTerminal) {
        runTerminal = undefined
      }
    }),
  )

  statusBar = window.createStatusBarItem(StatusBarAlignment.Left)
  updateStatusBarItem()

  updateStatusBarVisibility(window.activeTextEditor)
  window.onDidChangeActiveTextEditor((editor) => updateStatusBarVisibility(editor))

  restartClient()
}

export async function deactivate(): Promise<void> {
  await stopClient()
}

async function restartClient(): Promise<void> {
  const newClient = createClient()
  try {
    await newClient.start()
    void stopClient()
    client = newClient
    setServerStatus({ health: "started" })
  } catch (reason) {
    void window.showWarningMessage("Failed to start ry language server.")
    setServerStatus({ health: "stopped" })
  }
}

// Falls back to the settings of the project's former name, `roughly`.
function setting<T>(key: string): T | null {
  return workspace.getConfiguration("ry").get<T | null>(key)
    ?? workspace.getConfiguration("roughly").get<T | null>(key)
    ?? null
}

function ryExecutable(): string {
  return process.env.SERVER_PATH
    ?? setting<string>("path")
    ?? (
      fs.existsSync(BUNDLED_RY_EXECUTABLE)
        ? BUNDLED_RY_EXECUTABLE
        : "ry"
    )
}

function createClient(): LanguageClient {
  const command = ryExecutable()

  const commandArgs = setting<string[]>("args") ?? ["server"]
  const experimentalFeatures = setting<string[]>("experimentalFeatures")
  const experimentalArgs = experimentalFeatures
    ? ["--experimental-features", experimentalFeatures.join(" ")]
    : []
  const args = [...experimentalArgs, ...commandArgs]

  logger.info("using server command:", [command, ...args].join(" "))

  const serverOptions: ServerOptions = {
    command,
    args,
    transport: TransportKind.stdio,
    options: {
      env: {
        ...process.env,
        RUST_LOG: "debug",
      },
    },
  }

  const clientOptions: LanguageClientOptions = {
    // Literate documents are matched by name because their language id depends on which
    // extension (vscode-R, Quarto, or none) the user has installed.
    documentSelector: [
      { scheme: "file", language: "r" },
      { scheme: "file", pattern: "**/*.{Rmd,rmd,qmd,Rnw,rnw,Snw,snw}" },
    ],
    outputChannel,
  }

  const client = new LanguageClient(
    'ry',
    'ry',
    serverOptions,
    clientOptions
  )

  return client
}


async function stopClient(): Promise<void> {
  if (!client) {
    return
  }

  logger.info("stopping server ...")

  const oldClient = client
  client = null

  // `stop` sends "shutdown"; only `dispose`'s "exit" makes the server process exit.
  await oldClient.stop()
  await oldClient.dispose()
}

type ServerStatus = {
  health: "started" | "stopped"
}

function setServerStatus(newStatus: ServerStatus) {
  status = newStatus
  updateStatusBarItem()
}

function updateStatusBarVisibility(editor: TextEditor | undefined) {

  if (editor != null && languages.match({ language: 'r' }, editor.document) > 0) {
    statusBar.show()
  } else {
    statusBar.hide()
  }
}

function updateStatusBarItem() {
  let icon = ""
  statusBar.tooltip = new MarkdownString("", true)
  statusBar.tooltip.isTrusted = true
  switch (status.health) {
    case "started":
      statusBar.color = undefined
      statusBar.backgroundColor = undefined
      statusBar.command = "ry.openLogs"
      break
    case "stopped":
      statusBar.tooltip.appendText("Server is stopped")
      statusBar.tooltip.appendMarkdown("\n\n[Start server](command:ry.startServer)")
      statusBar.color = new ThemeColor("statusBarItem.warningForeground")
      statusBar.backgroundColor = new ThemeColor("statusBarItem.warningBackground")
      statusBar.command = "ry.startServer"
      statusBar.text = "$(stop-circle) ry"
      return
  }
  if (statusBar.tooltip.value) {
    statusBar.tooltip.appendMarkdown("\n\n---\n\n")
  }

  const serverVersion = "unknown"
  statusBar.tooltip.appendMarkdown([
    `[Extension Info](command:ry.serverVersion "Show version and server binary info"): Version ${version}, Server Version ${serverVersion}\n\n---`,
    '[$(terminal) Open Logs](command:ry.openLogs "Open the server logs")',
    '[$(debug-restart) Restart server](command:ry.restartServer "Restart the server")',
    '[$(stop-circle) Stop server](command:ry.stopServer "Stop the server")',
  ].join("\n\n"))

  statusBar.text = `${icon}ry`
}

// Running, testing and debugging. Tests run through `ry load`, which loads the package lazily
// before testthat runs the file, and each run gets a fresh terminal so its output stands alone.

let runTerminal: Terminal | undefined
let consoleTerminal: Terminal | undefined

// The package a file belongs to: the nearest directory above it with a DESCRIPTION.
function packageRoot(file: string): string | undefined {
  let directory = path.dirname(file)
  for (;;) {
    if (fs.existsSync(path.join(directory, "DESCRIPTION"))) {
      return directory
    }
    const parent = path.dirname(directory)
    if (parent === directory) {
      return undefined
    }
    directory = parent
  }
}

function isTestFile(file: string): boolean {
  return file.split(path.sep).includes("testthat")
}

// JSON's string syntax is also valid R string syntax, quotes and escapes included.
function rString(text: string): string {
  return JSON.stringify(text)
}

function folderOf(uri: Uri | undefined): WorkspaceFolder | undefined {
  return uri ? workspace.getWorkspaceFolder(uri) : workspace.workspaceFolders?.[0]
}

function runInTerminal(name: string, args: string[], cwd: string | undefined): void {
  runTerminal?.dispose()
  runTerminal = window.createTerminal({ name, shellPath: ryExecutable(), shellArgs: args, cwd })
  runTerminal.show(true)
}

async function fileArgument(uri: string | Uri | undefined): Promise<string | undefined> {
  const target = typeof uri === "string" ? Uri.parse(uri) : uri ?? window.activeTextEditor?.document.uri
  if (!target || target.scheme !== "file") {
    void window.showWarningMessage("ry can only run a file that is saved on disk.")
    return undefined
  }
  const document = workspace.textDocuments.find((document) => document.uri.toString() === target.toString())
  if (document?.isDirty) {
    await document.save()
  }
  return target.fsPath
}

async function runFile(uri?: string | Uri): Promise<void> {
  const file = await fileArgument(uri)
  if (!file) {
    return
  }
  const root = packageRoot(file)
  if (root && isTestFile(file)) {
    runInTerminal(`ry: ${path.basename(file)}`, ["load", root, "-e", `testthat::test_file(${rString(file)})`], root)
  } else {
    runInTerminal(`ry: ${path.basename(file)}`, ["run", file], folderOf(Uri.file(file))?.uri.fsPath ?? path.dirname(file))
  }
}

async function runTest(uri: string, name: string): Promise<void> {
  const file = await fileArgument(uri)
  if (!file) {
    return
  }
  const root = packageRoot(file) ?? path.dirname(file)
  const expression = `testthat::test_file(${rString(file)}, desc = ${rString(name)})`
  runInTerminal(`ry: ${name}`, ["load", root, "-e", expression], root)
}

async function debugTest(uri: string, name: string): Promise<void> {
  const file = await fileArgument(uri)
  if (!file) {
    return
  }
  const root = packageRoot(file)
  const prelude = root ? `load_all(${rString(root)}); library(testthat)` : "library(testthat)"
  await debug.startDebugging(folderOf(Uri.file(file)), {
    type: "ry",
    request: "launch",
    name: `Debug test: ${name}`,
    program: file,
    cwd: root ?? path.dirname(file),
    prelude,
    testName: name,
  })
}

// The server sends plain LSP locations, and the editor's peek view takes editor types.
async function showReferences(uri: string, position: lsp.Position, locations: lsp.Location[]): Promise<void> {
  if (!client) {
    return
  }
  const converter = client.protocol2CodeConverter
  await commands.executeCommand(
    "editor.action.showReferences",
    Uri.parse(uri),
    converter.asPosition(position),
    locations.map((location) => converter.asLocation(location)),
  )
}

function createDebugAdapterDescriptor(session: DebugSession): DebugAdapterDescriptor {
  if (session.configuration.request === "attach") {
    return new DebugAdapterNamedPipeServer(session.configuration.socket)
  }
  const cwd = session.configuration.cwd ?? session.workspaceFolder?.uri.fsPath
  return new DebugAdapterExecutable(ryExecutable(), ["debug-adapter"], cwd ? { cwd } : {})
}

// Pressing F5 without a launch configuration debugs the active R file.
function resolveDebugConfiguration(
  _folder: WorkspaceFolder | undefined,
  configuration: DebugConfiguration,
  _token?: CancellationToken,
): DebugConfiguration | undefined {
  if (!configuration.type && !configuration.request && !configuration.name) {
    const document = window.activeTextEditor?.document
    if (document?.languageId !== "r") {
      return undefined
    }
    return { type: "ry", request: "launch", name: "Debug R file", program: document.uri.fsPath }
  }
  if (configuration.request === "attach" && !configuration.socket) {
    const entry = findConsoles(_folder?.uri.fsPath)[0]
    if (!entry) {
      void window.showErrorMessage("No ry console runs in this workspace. Start one with \"ry: Open Console\".")
      return undefined
    }
    subscribe(entry)
    return { ...configuration, socket: entry.socket }
  }
  return configuration
}

// Attaches the debugger to the workspace console, opening one first if none runs.
async function attachConsole(): Promise<void> {
  const folder = folderOf(window.activeTextEditor?.document.uri) ?? workspace.workspaceFolders?.[0]
  let entry: ConsoleEntry | undefined = findConsoles(folder?.uri.fsPath)[0]
  if (!entry) {
    openConsole(window.activeTextEditor?.document.uri)
    entry = await waitForConsole(folder?.uri.fsPath, Date.now() - 1000)
  }
  if (!entry) {
    void window.showErrorMessage("The ry console did not start.")
    return
  }
  subscribe(entry)
  await debug.startDebugging(folder, { type: "ry", request: "attach", name: "Attach to Console", socket: entry.socket })
}

// "View Data" in the debugger's variables view: the variable's expression is evaluated in the
// selected frame, so a local of the stopped function opens, not a global of the same name.
async function viewData(context: { variable?: { name: string, evaluateName?: string } }): Promise<void> {
  const session = debug.activeDebugSession
  const expression = context.variable?.evaluateName ?? context.variable?.name
  if (!session || !expression) {
    return
  }
  const item = debug.activeStackItem
  const frameId = item instanceof DebugStackFrame && item.session.id === session.id ? item.frameId : undefined
  try {
    const view = await session.customRequest("ryView", { expression, frameId })
    openViewer({ request: (command, args) => Promise.resolve(session.customRequest(command, args)) }, view)
  } catch (error) {
    void window.showErrorMessage(`Cannot view ${expression}: ${(error as Error).message}`)
  }
}

// The consoles subscribed to `View()`, by socket.
const consoles = new Map<string, ConsoleConnection>()

function subscribe(entry: ConsoleEntry): void {
  if (consoles.has(entry.socket)) {
    return
  }
  const connection = new ConsoleConnection(entry, () => consoles.delete(entry.socket))
  consoles.set(entry.socket, connection)
  connection.subscribe().catch(() => connection.dispose())
}

// Polls for a console in `folder` that announced itself after `since`, for up to ten seconds.
async function waitForConsole(folder: string | undefined, since: number): Promise<ConsoleEntry | undefined> {
  for (let attempt = 0; attempt < 50; attempt++) {
    const entry = findConsoles(folder).find((entry) => entry.modified >= since)
    if (entry) {
      return entry
    }
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  return undefined
}

// The console is `ry run` in a terminal, so it is the same console as on the command line and
// loads the package of its working directory. Text sent while a command runs waits for the next
// prompt.

function openConsole(uri: Uri | undefined): Terminal {
  if (!consoleTerminal) {
    const file = uri?.scheme === "file" ? uri.fsPath : undefined
    const cwd = (file && packageRoot(file)) ?? folderOf(uri)?.uri.fsPath
    consoleTerminal = window.createTerminal({ name: "ry console", shellPath: ryExecutable(), shellArgs: ["run"], cwd })
    // Subscribing right away makes `View()` in the new console open the data viewer.
    void waitForConsole(cwd, Date.now() - 1000).then((entry) => entry && subscribe(entry))
  }
  consoleTerminal.show(true)
  return consoleTerminal
}

// Sends the selection, or with nothing selected the current line and moves to the next line
// with code, so repeated presses step through a script.
function sendSelection(): void {
  const editor = window.activeTextEditor
  if (!editor) {
    return
  }
  const document = editor.document
  const terminal = openConsole(document.uri)
  if (!editor.selection.isEmpty) {
    terminal.sendText(document.getText(editor.selection), true)
    return
  }
  const line = editor.selection.active.line
  terminal.sendText(document.lineAt(line).text, true)
  let next = line + 1
  while (next < document.lineCount && document.lineAt(next).isEmptyOrWhitespace) {
    next += 1
  }
  if (next < document.lineCount) {
    const position = new Position(next, 0)
    editor.selection = new Selection(position, position)
    editor.revealRange(new Range(position, position))
  }
}

// Sends the innermost function definition around the cursor, as the outline names it.
async function sendFunction(): Promise<void> {
  const editor = window.activeTextEditor
  if (!editor) {
    return
  }
  const symbols = await commands.executeCommand<DocumentSymbol[] | undefined>(
    "vscode.executeDocumentSymbolProvider",
    editor.document.uri,
  )
  const cursor = editor.selection.active
  let found: DocumentSymbol | undefined
  let level = symbols ?? []
  for (;;) {
    const container = level.find((symbol) => symbol.range.contains(cursor))
    if (!container) {
      break
    }
    if (container.kind === SymbolKind.Function) {
      found = container
    }
    level = container.children
  }
  if (!found) {
    void window.showInformationMessage("The cursor is not inside a function definition.")
    return
  }
  openConsole(editor.document.uri).sendText(editor.document.getText(found.range), true)
}

async function sourceFile(): Promise<void> {
  const file = await fileArgument(undefined)
  if (file) {
    openConsole(Uri.file(file)).sendText(`source(${rString(file)})`, true)
  }
}
