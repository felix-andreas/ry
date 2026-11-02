// The data viewer and the link to a running `ry run` console.
//
// A console listens on a Unix socket and announces it with a registry file
// `$TMPDIR/ry-<uid>/console-<pid>.json` that names its working directory and socket. The debugger
// attaches over that socket, and a second connection subscribes to `View()` calls. A viewer
// never holds the data: it asks for one page of rows at a time with `ryTable`, so a data frame
// with millions of rows costs one page, and it closes the view with `ryRelease` when the panel
// closes.

import * as crypto from "crypto"
import * as fs from "fs"
import * as net from "net"
import * as os from "os"
import * as path from "path"

import { window, Disposable, ViewColumn, WebviewPanel } from "vscode"

export interface DataSource {
  request(command: string, args: object): Promise<any>
}

export interface View {
  viewId: number
  title: string
  rows: number
  columns: { name: string, type: string }[]
  class?: string
}

export interface ConsoleEntry {
  pid: number
  cwd: string
  socket: string
  modified: number
}

const PAGE_SIZE = 100

// The consoles whose working directory is `folder` or inside it, newest first. A console that
// exited without removing its entry is skipped when connecting, not here.
export function findConsoles(folder: string | undefined): ConsoleEntry[] {
  const uid = process.getuid?.()
  if (uid === undefined) {
    return []
  }
  const directory = path.join(os.tmpdir(), `ry-${uid}`)
  let names: string[]
  try {
    const stat = fs.lstatSync(directory)
    // Only a directory this user owns and no one else can write is trusted to name sockets.
    if (!stat.isDirectory() || stat.uid !== uid || (stat.mode & 0o077) !== 0) {
      return []
    }
    names = fs.readdirSync(directory)
  } catch {
    return []
  }
  const root = folder ? realPath(folder) : undefined
  const consoles: ConsoleEntry[] = []
  for (const name of names) {
    if (!name.startsWith("console-") || !name.endsWith(".json")) {
      continue
    }
    const file = path.join(directory, name)
    try {
      const entry = JSON.parse(fs.readFileSync(file, "utf8"))
      const cwd = realPath(entry.cwd)
      if (root && cwd !== root && !cwd.startsWith(root + path.sep)) {
        continue
      }
      consoles.push({ pid: entry.pid, cwd, socket: entry.socket, modified: fs.statSync(file).mtimeMs })
    } catch {
      continue
    }
  }
  return consoles.sort((left, right) => right.modified - left.modified)
}

function realPath(file: string): string {
  try {
    return fs.realpathSync(file)
  } catch {
    return file
  }
}

// A connection to a console that is subscribed to `View()`, and doubles as the data source of the
// viewers it opens.
export class ConsoleConnection implements DataSource, Disposable {
  private socket: net.Socket
  private buffer = Buffer.alloc(0)
  private seq = 0
  private pending = new Map<number, { resolve: (body: any) => void, reject: (error: Error) => void }>()
  closed = false

  constructor(readonly entry: ConsoleEntry, private onClose: () => void) {
    this.socket = net.createConnection(entry.socket)
    this.socket.on("data", (chunk) => this.receive(chunk))
    this.socket.on("error", () => this.socket.destroy())
    this.socket.on("close", () => {
      this.closed = true
      for (const { reject } of this.pending.values()) {
        reject(new Error("the console exited"))
      }
      this.pending.clear()
      this.onClose()
    })
  }

  async subscribe(): Promise<void> {
    await this.request("rySubscribe", {})
  }

  request(command: string, args: object): Promise<any> {
    if (this.closed) {
      return Promise.reject(new Error("the console exited"))
    }
    const seq = ++this.seq
    const body = JSON.stringify({ type: "request", seq, command, arguments: args })
    this.socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`)
    return new Promise((resolve, reject) => this.pending.set(seq, { resolve, reject }))
  }

  dispose(): void {
    this.socket.destroy()
  }

  private receive(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk])
    for (; ;) {
      const end = this.buffer.indexOf("\r\n\r\n")
      if (end < 0) {
        return
      }
      const header = this.buffer.subarray(0, end).toString("ascii")
      const length = Number(/content-length:\s*(\d+)/i.exec(header)?.[1])
      if (!Number.isFinite(length)) {
        this.socket.destroy()
        return
      }
      if (this.buffer.length < end + 4 + length) {
        return
      }
      let message
      try {
        message = JSON.parse(this.buffer.subarray(end + 4, end + 4 + length).toString("utf8"))
      } catch {
        this.socket.destroy()
        return
      }
      this.buffer = this.buffer.subarray(end + 4 + length)
      this.dispatch(message)
    }
  }

  private dispatch(message: any): void {
    if (message.type === "event" && message.event === "ryView") {
      openViewer(this, message.body)
    } else if (message.type === "response") {
      const waiting = this.pending.get(message.request_seq)
      this.pending.delete(message.request_seq)
      if (message.success) {
        waiting?.resolve(message.body)
      } else {
        waiting?.reject(new Error(message.message ?? "the request failed"))
      }
    }
  }
}

export function openViewer(source: DataSource, view: View): WebviewPanel {
  const panel = window.createWebviewPanel("ry.dataViewer", view.title, ViewColumn.Active, {
    enableScripts: true,
    retainContextWhenHidden: false,
  })
  const nonce = crypto.randomBytes(16).toString("base64")
  panel.webview.html = viewerHtml(view, nonce, panel.webview.cspSource)
  panel.webview.onDidReceiveMessage(async (message) => {
    if (message.type !== "page") {
      return
    }
    try {
      const page = await source.request("ryTable", { viewId: view.viewId, start: message.start, count: PAGE_SIZE })
      void panel.webview.postMessage({ type: "page", ...page })
    } catch (error) {
      void panel.webview.postMessage({ type: "error", message: String((error as Error).message ?? error) })
    }
  })
  panel.onDidDispose(() => {
    source.request("ryRelease", { viewId: view.viewId }).catch(() => undefined)
  })
  return panel
}

function viewerHtml(view: View, nonce: string, cspSource: string): string {
  const data = JSON.stringify({ rows: view.rows, columns: view.columns, pageSize: PAGE_SIZE })
    .replace(/</g, "\\u003c")
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<style>
  body { padding: 0; font-family: var(--vscode-editor-font-family); font-size: var(--vscode-editor-font-size); }
  header { position: sticky; top: 0; display: flex; gap: 8px; align-items: center; padding: 6px 8px;
    background: var(--vscode-editor-background); border-bottom: 1px solid var(--vscode-panel-border); z-index: 2; }
  button { background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground);
    border: none; padding: 2px 8px; cursor: pointer; }
  button:disabled { opacity: 0.5; cursor: default; }
  table { border-collapse: collapse; }
  th, td { padding: 2px 8px; border: 1px solid var(--vscode-panel-border); white-space: pre; text-align: right; }
  th { position: sticky; top: 33px; background: var(--vscode-editorWidget-background); }
  th small { display: block; font-weight: normal; opacity: 0.7; }
  td.name { opacity: 0.7; }
  #error { color: var(--vscode-errorForeground); }
</style>
</head>
<body>
<header>
  <button id="first">«</button><button id="previous">‹</button>
  <span id="range"></span>
  <button id="next">›</button><button id="last">»</button>
  <span id="error"></span>
</header>
<table><thead><tr id="head"></tr></thead><tbody id="body"></tbody></table>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi()
  const view = ${data}
  let start = 0
  const element = (id) => document.getElementById(id)
  const cell = (tag, text, className) => {
    const node = document.createElement(tag)
    node.textContent = text
    if (className) node.className = className
    return node
  }
  const head = element("head")
  head.appendChild(cell("th", ""))
  for (const column of view.columns) {
    const node = cell("th", column.name)
    node.appendChild(cell("small", column.type))
    head.appendChild(node)
  }
  const lastStart = Math.max(0, Math.floor((view.rows - 1) / view.pageSize) * view.pageSize)
  const go = (next) => {
    start = Math.min(Math.max(0, next), lastStart)
    vscode.postMessage({ type: "page", start })
  }
  element("first").onclick = () => go(0)
  element("previous").onclick = () => go(start - view.pageSize)
  element("next").onclick = () => go(start + view.pageSize)
  element("last").onclick = () => go(lastStart)
  window.addEventListener("message", ({ data: message }) => {
    // After quick clicks, a slow answer for a page the user already left must not replace the
    // page they are on.
    if (message.type === "page" && message.start !== start) return
    if (message.type === "error") {
      element("error").textContent = message.message
      return
    }
    element("error").textContent = ""
    const body = element("body")
    body.replaceChildren()
    message.rows.forEach((row, index) => {
      const line = document.createElement("tr")
      line.appendChild(cell("td", message.rowNames?.[index] ?? String(message.start + index + 1), "name"))
      for (const value of row) line.appendChild(cell("td", value))
      body.appendChild(line)
    })
    const end = message.start + message.rows.length
    element("range").textContent = view.rows === 0
      ? "no rows"
      : (message.start + 1) + "–" + end + " of " + view.rows + " rows, " + view.columns.length + " columns"
    element("first").disabled = element("previous").disabled = message.start === 0
    element("next").disabled = element("last").disabled = end >= view.rows
  })
  go(0)
</script>
</body>
</html>`
}
