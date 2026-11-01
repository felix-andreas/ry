#!/usr/bin/env python3
"""Measure incremental re-check latency through the language server.

Opens one file of a generated code base in `ry server`, waits for its first diagnostics,
then repeats two edits and times each from the `didChange` notification to the diagnostics
the server publishes for that file:

  body       one literal changes inside a function body (no signature changes)
  signature  a defaulted parameter is appended to a function's formals

Each edit is followed by the revert, so the code base ends where it started. `first` is the
first publish after the edit (the fast wave plus whatever the server had ready) and `settled`
is the last publish before the server went quiet for `--quiet` seconds (the semantic wave).

It then times editor requests (hover, completion, definition, references and rename) at
identifiers sampled with a fixed seed from `--request-files` open files, twice: on a settled
server, and right after a body edit, while the semantic wave is still pending. Each reports
p50 and p99 from the request to its response.

Usage:
    python3 -I lsp_latency.py --ry target/release/ry --root DIR [--file R/x.R] [--runs 10]
"""

import argparse
import json
import os
import random
import re
import statistics
import subprocess
import sys
import threading
import time
from queue import Empty, Queue


class Client:
    def __init__(self, binary, root):
        self.process = subprocess.Popen(
            [binary, "server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        self.publishes = Queue()
        self.responses = {}
        self.condition = threading.Condition()
        self.next_id = 0
        threading.Thread(target=self.read_loop, daemon=True).start()
        uri = "file://" + root
        self.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": uri,
                "workspaceFolders": [{"uri": uri, "name": "bench"}],
                "capabilities": {"general": {"positionEncodings": ["utf-8", "utf-16"]}},
            },
        )
        self.notify("initialized", {})

    def read_loop(self):
        stream = self.process.stdout
        while True:
            length = None
            while True:
                line = stream.readline()
                if not line:
                    return
                line = line.strip()
                if not line:
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            message = json.loads(stream.read(length))
            received = time.perf_counter()
            if "method" in message and "id" in message:
                self.send({"jsonrpc": "2.0", "id": message["id"], "result": None})
            elif message.get("method") == "textDocument/publishDiagnostics":
                self.publishes.put((received, message["params"]))
            elif "id" in message:
                with self.condition:
                    self.responses[message["id"]] = message
                    self.condition.notify_all()

    def send(self, message):
        body = json.dumps(message).encode()
        self.process.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.process.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params):
        self.next_id += 1
        identifier = self.next_id
        self.send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
        with self.condition:
            while identifier not in self.responses:
                self.condition.wait()
            return self.responses.pop(identifier)

    def drain(self, uri, quiet, limit):
        """Publishes for `uri` until the server is quiet; returns their arrival times."""
        times = []
        deadline = time.perf_counter() + limit
        while time.perf_counter() < deadline:
            try:
                received, params = self.publishes.get(timeout=quiet)
            except Empty:
                if times:
                    break
                continue
            if uri is None or params["uri"] == uri:
                times.append(received)
        return times

    def close(self):
        self.request("shutdown", None)
        self.notify("exit", None)
        self.process.wait(timeout=10)


def pick_edits(text):
    lines = text.split("\n")
    body = signature = None
    for index, line in enumerate(lines):
        if signature is None:
            match = re.match(r"^(\w+) <- function\(([^)]*)\) \{$", line)
            if match and match.group(2):
                signature = (index, line.rindex(")"))
        if body is None and signature is not None and index > signature[0]:
            match = re.search(r"(?<![\w.\[(])(\d)(?![\w.\])])", line)
            if match and line.startswith("  ") and "#" not in line and '"' not in line:
                body = (index, match.start(1), match.group(1))
        if body and signature:
            break
    return body, signature


def edit(client, uri, version, line, start, end, replacement):
    client.notify(
        "textDocument/didChange",
        {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [
                {
                    "range": {
                        "start": {"line": line, "character": start},
                        "end": {"line": line, "character": end},
                    },
                    "text": replacement,
                }
            ],
        },
    )


REQUESTS = ("hover", "completion", "definition", "references", "rename")
IDENTIFIER = re.compile(r"(?<![\w.$@:])([A-Za-z][\w.]*[A-Za-z0-9_])(?![\w.(]*\s*=[^=])")


def sample_positions(text, count, generator):
    """Identifier occurrences outside comments and strings, as (line, start, end)."""
    found = []
    for index, line in enumerate(text.split("\n")):
        code = line.split("#", 1)[0]
        if '"' in code or "'" in code:
            continue
        for match in IDENTIFIER.finditer(code):
            found.append((index, match.start(1), match.end(1)))
    generator.shuffle(found)
    return found[:count]


def request_params(method, uri, line, start, end):
    document = {"uri": uri}
    if method == "completion":
        # Mid-identifier, as while typing: the prefix narrows the candidates.
        return {"textDocument": document, "position": {"line": line, "character": min(start + 2, end)}}
    position = {"line": line, "character": start + 1}
    if method == "references":
        return {"textDocument": document, "position": position, "context": {"includeDeclaration": True}}
    if method == "rename":
        return {"textDocument": document, "position": position, "newName": "bench_renamed"}
    return {"textDocument": document, "position": position}


def time_request(client, method, params):
    sent = time.perf_counter()
    response = client.request("textDocument/" + method, params)
    elapsed = time.perf_counter() - sent
    if "error" in response and response["error"].get("code") != -32800:
        sys.exit(f"{method} failed: {response['error']}")
    return elapsed


def percentiles(samples):
    if not samples:
        return {"p50_ms": None, "p99_ms": None, "max_ms": None, "n": 0}
    ordered = sorted(samples)
    p99 = ordered[min(len(ordered) - 1, int(len(ordered) * 0.99))]
    return {
        "p50_ms": statistics.median(ordered) * 1000,
        "p99_ms": p99 * 1000,
        "max_ms": ordered[-1] * 1000,
        "n": len(ordered),
    }


def summarize(label, samples):
    if not samples:
        return f"{label}: no samples"
    ordered = sorted(samples)
    p95 = ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))]
    return f"{label}: p50 {statistics.median(ordered) * 1000:.0f} ms, p95 {p95 * 1000:.0f} ms, max {ordered[-1] * 1000:.0f} ms"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ry", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--file", help="file relative to the root; default: the first R file with a usable function")
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--quiet", type=float, default=0.4)
    parser.add_argument("--requests", type=int, default=200, help="identifier positions sampled per request kind")
    parser.add_argument("--request-files", type=int, default=8)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    root = os.path.abspath(args.root)
    candidates = [args.file] if args.file else sorted(os.listdir(os.path.join(root, "R")))
    for candidate in candidates:
        relative = candidate if args.file else os.path.join("R", candidate)
        path = os.path.join(root, relative)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        body, signature = pick_edits(text)
        if body and signature:
            break
    else:
        sys.exit("no file with a function body and a signature to edit")

    uri = "file://" + path
    started = time.perf_counter()
    client = Client(args.ry, root)
    initialized = time.perf_counter() - started
    opened = time.perf_counter()
    client.notify(
        "textDocument/didOpen",
        {"textDocument": {"uri": uri, "languageId": "r", "version": 1, "text": text}},
    )
    initial = [received for received in client.drain(uri, args.quiet, 120) if received >= opened]
    if not initial:
        sys.exit("the server published no diagnostics for the opened file")
    cold = (initial[0] - opened, initial[-1] - opened)

    version = 1
    results = {"body": ([], []), "signature": ([], [])}
    body_line, body_column, digit = body
    sig_line, sig_column = signature
    changed = str((int(digit) + 1) % 10)
    extra = ", bench_extra = 1L"
    for _ in range(args.runs):
        for kind in ("body", "signature"):
            for forward in (True, False):
                version += 1
                if kind == "body":
                    new, old = (changed, digit) if forward else (digit, changed)
                    start, end, replacement = body_column, body_column + len(old), new
                    line = body_line
                elif forward:
                    line, start, end, replacement = sig_line, sig_column, sig_column, extra
                else:
                    line, start, end, replacement = sig_line, sig_column, sig_column + len(extra), ""
                sent = time.perf_counter()
                edit(client, uri, version, line, start, end, replacement)
                times = client.drain(uri, args.quiet, 30)
                if forward and times:
                    results[kind][0].append(times[0] - sent)
                    results[kind][1].append(times[-1] - sent)

    generator = random.Random(7)
    request_files = [os.path.join(root, "R", name) for name in sorted(os.listdir(os.path.join(root, "R")))]
    generator.shuffle(request_files)
    opened_files = []
    for request_path in request_files[: args.request_files]:
        if request_path == path:
            continue
        with open(request_path, encoding="utf-8") as handle:
            request_text = handle.read()
        request_uri = "file://" + request_path
        client.notify(
            "textDocument/didOpen",
            {"textDocument": {"uri": request_uri, "languageId": "r", "version": 1, "text": request_text}},
        )
        opened_files.append((request_uri, request_text))
    opened_files.append((uri, text))
    client.drain(None, args.quiet, 120)
    positions = [
        (request_uri, position)
        for request_uri, request_text in opened_files
        for position in sample_positions(request_text, max(1, args.requests // len(opened_files)), generator)
    ]

    settled_requests = {method: [] for method in REQUESTS}
    for method in REQUESTS:
        for request_uri, (line, start, end) in positions:
            settled_requests[method].append(time_request(client, method, request_params(method, request_uri, line, start, end)))

    # Each request follows its own body edit, so it races the semantic wave that edit started.
    dirty_requests = {method: [] for method in REQUESTS}
    for method in REQUESTS:
        for index, (request_uri, (line, start, end)) in enumerate(positions[: args.requests // 4]):
            new, old = (changed, digit) if index % 2 == 0 else (digit, changed)
            version += 1
            edit(client, uri, version, body_line, body_column, body_column + len(old), new)
            dirty_requests[method].append(time_request(client, method, request_params(method, request_uri, line, start, end)))
    if len(positions[: args.requests // 4]) % 2 == 1:
        version += 1
        edit(client, uri, version, body_line, body_column, body_column + 1, digit)
    client.drain(None, args.quiet, 60)
    client.close()

    report = {
        "file": os.path.relpath(path, root),
        "initialize_s": initialized,
        "cold_open_first_s": cold[0],
        "cold_open_settled_s": cold[1],
    }
    for kind, (first, settled) in results.items():
        report[kind] = {
            "first_p50_ms": statistics.median(first) * 1000 if first else None,
            "settled_p50_ms": statistics.median(settled) * 1000 if settled else None,
            "first_max_ms": max(first) * 1000 if first else None,
        }
    report["requests"] = {
        "settled": {method: percentiles(samples) for method, samples in settled_requests.items()},
        "after_edit": {method: percentiles(samples) for method, samples in dirty_requests.items()},
    }
    if args.json:
        print(json.dumps(report))
        return
    print(f"initialize {initialized * 1000:.0f} ms")
    print(f"file {report['file']}: open {cold[0] * 1000:.0f} ms first publish, {cold[1] * 1000:.0f} ms settled")
    for kind, (first, settled) in results.items():
        print(summarize(f"{kind} edit, first publish", first))
        print(summarize(f"{kind} edit, settled", settled))
    for state, rows in report["requests"].items():
        for method, row in rows.items():
            if row["n"]:
                print(f"{method} ({state}, n={row['n']}): p50 {row['p50_ms']:.1f} ms, p99 {row['p99_ms']:.1f} ms, max {row['max_ms']:.1f} ms")


if __name__ == "__main__":
    main()
