---
title: REPL design
description: "How the R console loads R at runtime with no build-time linking"
---

The R console ships. It lives in `crates/repl`, behind `ry repl`, and it has
been verified end to end against a real R. This page records the architecture
that lets ry embed R without linking against it at build time, and it records
what is still open.

## What ships

Discovery, the typed runtime-binding layer, the console hosted inside R's
ReadConsole hook, lexer highlighting, conservative completeness checking,
SIGINT interrupt routing, the headless runner behind `ry run` and
`ry repl --file`, vi keybindings, and the Windows embedding all ship. The
pseudo-terminal end-to-end suite runs green against a real R, both on a Unix
pty and on Windows ConPTY. It covers interactive evaluation, multiline input,
the error stream, Ctrl-C interruption, and both batch directions. R installs
in an agent container through apt and the CRAN repository, so the suite runs
anywhere rather than only on a developer machine.

Tab completion is backed by the analysis stack. The `repl` crate stays
syntax-only and exposes a `SessionCompleter` seam, with an accept method and a
complete method. The console feeds each accepted line back through it, and an
`Arc<Mutex>` shares the object between reedline's menu and the accept path.
The host implements `AnalysisCompleter` in `crates/ry/src/repl_completer.rs`.
It runs `ide::completion` over the session treated as a script, using one
salsa `SourceFile` that is updated per request, with the stubs and the export
manifests installed. The menu therefore shows typed signatures for
standard-library names, session bindings, `pkg::` exports, and manifest names.
Tab is bound in both emacs mode and vi-insert mode.

Two facts about the test harness must survive any rewrite. The pty driver must
answer reedline's cursor-position query, replying `ESC[1;1R` to `ESC[6n`, or
the editor blocks before its first prompt. Interactive sessions must run
serialized, through the in-session `SESSION_LOCK`, because concurrent R
sessions on a loaded machine are flaky while serial runs are stable.

Four things are not built. The live-session facts are not unioned into
completion, which means the R environment listing. There are no pre-evaluation
diagnostics. There is no hover on the input line. There are no graphics
devices.

## Why build-time linking was a dead end

The predecessor embedded R through `extendr` and libR-sys. bindgen ran at
build time against a local R's headers, and the binary carried a load-time
dynamic dependency on libR. That had three consequences. The build machine
needed a matching R, which is why that crate was excluded from CI and from
every gate. The artifact was bound to the R it was built against. A missing
symbol was a loader failure rather than a recoverable fact. Deleting that
crate is what removed the exclusion from every gate command, and it dropped
`extendr` from the workspace.

## The core technique: bind R at runtime, one symbol at a time

Do not link. Open R's shared library with `dlopen` at startup, and resolve
every needed symbol by name, with per-symbol optionality.

The declarations here are original, written from R's public headers. This
design was verified against a production-grade Rust R kernel's source, and
that source is study material rather than something to copy.

- **The binding surface is hand-curated, not generated.** One declaration list
  covers the C-API functions, the variadic functions such as `Rf_error` and
  `Rprintf`, the mutable globals, and the value-snapshotted constants. A
  variadic is stored with its real `...` type and exposed at fixed arity. The
  mutable globals are `R_interrupts_pending`, `R_Interactive`, the
  `ptr_R_ReadConsole` and `ptr_R_WriteConsoleEx` hook pointers,
  `R_PolledEvents`, and `R_SignalHandlers`. The constants are `R_GlobalEnv`,
  `R_NilValue`, and others like them. Declarative macros expand each
  declaration into a `static Option<fn ptr>` plus a passthrough wrapper.
  Resolution is eager and batched at init, in one dlsym sweep, rather than
  lazy per call.
- **A missing symbol is `None`, not a crash.** Every binding gets a
  `has::name()` probe, and a call site branches on it to provide a fallback on
  an older R. A newer accessor is used when it is present, and the classic
  macro equivalent otherwise. That is the whole version-compatibility story:
  resolve optimistically, and degrade one symbol at a time. A hard version
  floor keeps the fallback matrix small. The floor is read by parsing
  `{R_HOME}/library/base/DESCRIPTION`, and R 4.2 mirrors current ecosystem
  practice.
- **Init has two phases, and the order is load-bearing.** The functions and
  the mutable globals bind before `Rf_initialize_R`. The constant globals are
  copied by value only after `setup_Rmainloop()`, because R initializes them
  there. The library handle is leaked and held for the process lifetime.
- **The loader flags matter.** On Unix, open with `RTLD_LAZY | RTLD_GLOBAL`,
  so that a compiled package's shared object that links libR resolves R's
  symbols as if the host had linked R itself. Also set `LD_LIBRARY_PATH`, or
  `DYLD_LIBRARY_PATH`, to `{R_HOME}/lib`, so such a package can find a libR at
  its own dlopen time. The `RTLD_GLOBAL` symbols then shadow it. macOS needs
  the dyld-environment entitlement on the binary. On Windows, open `R.dll`
  plus its sibling DLLs, which are `Rblas`, `Rlapack`, `Riconv`, and
  `Rgraphapp`, so the loaded-module list satisfies a package's imports.
  Windows looks a symbol up per module, so it needs no `RTLD_GLOBAL`
  equivalent.
- **Keep an ABI-drifting struct off the surface.** R's `DevDesc` and `Rstart`
  change layout across versions. The reference approach mirrors them per
  engine version and casts at runtime. The console avoids those surfaces,
  because it has no custom graphics device. The technique is recorded here for
  when plots come.

## Discovery

1. Use the `R_HOME` environment variable when it is set, so an editor or CI
   can pin the version.
2. Otherwise run `R RHOME` from `PATH`, which is `R.exe` or `R.bat` on
   Windows, and read its stdout. Re-export `R_HOME` so that R's own
   `R_HomeDir()` agrees.
3. Find the shared library at `{R_HOME}/lib/libR.so` or
   `{R_HOME}/lib/libR.dylib`, or at `{R_HOME}/bin/x64/R.dll` on Windows. The
   macOS framework's `R_HOME` already points into `Resources/`. When the
   shared library is absent, fail with a message that names
   `--enable-R-shlib`.
4. Recover the secondary variables `R_SHARE_DIR`, `R_INCLUDE_DIR`, and
   `R_DOC_DIR`. R's shell wrapper at `{R_HOME}/bin/R` exports them, and
   embedding bypasses that wrapper. On a layout that relocates those
   directories, as Fedora and RHEL do, `R.home("share")` would otherwise
   resolve to a nonexistent path under `{R_HOME}`. The console parses the
   wrapper's plain `VAR=value` lines instead. R substitutes those values
   literally at install time, and asking R directly in a subprocess would cost
   a few hundred milliseconds of startup. This is Unix-only, because Windows R
   derives them from `R_HOME` internally.

## Process shape and the console loop

- **R owns the process main thread**, because its stack checks and its signal
  expectations assume it. Everything else runs on a background thread of the
  same process, including the protocol threads, the analysis engine, and
  output capture. R initializes once per process, so tests run one process per
  test.
- Init runs in this order. Suppress R's signal handlers by setting
  `R_SignalHandlers = 0` and installing our own. Call `Rf_initialize_R` with
  `--interactive --no-save --no-restore-data`. Hook `ptr_R_ReadConsole`,
  `ptr_R_WriteConsoleEx`, `ptr_R_Busy`, and `ptr_R_Suicide`. Set
  `R_PolledEvents` and `R_wait_usec`, so long-running R code polls us. Then
  call `setup_Rmainloop()` and `run_Rmainloop()`. This drives R's real REPL
  through the console hooks rather than through `R_ReplDLLdo1`, which keeps a
  browser prompt, a `readline()` call, and a nested REPL honest.
- **The ReadConsole callback is the scheduler.** When R asks for input, the
  process is at a safe idle point. Classify the prompt as top-level, as a
  `browser()` prompt, or as a `readline()` input request. Then park in a
  channel-select over user input, evaluation requests, and idle work, with a
  periodic tick that runs R's input handlers so that background R machinery,
  such as the help server and an event-loop package, stays live. Feed R one
  expression per read, parsed and split by us first.
- **One thread touches R, by construction.** The editor runs inside the
  ReadConsole hook, so the console and R share the main thread and no
  cross-thread marshaling layer exists. Analysis keeps it that way. Analysis
  may run on a background thread, but a live-session fact, such as a loaded
  namespace, the `ls()` of the global environment, or a frame's columns, is
  fetched only while R is parked at a prompt, on the main thread, between
  reads. A background thread never calls into R.
- **The terminal width is ours to set.** R's `width` option defaults to 80
  columns whatever the terminal is, and R exports no setter, so a table with
  room on screen still wrapped. The width is measured once and applied with
  `R_ParseEvalString`, in the window between `setup_Rmainloop()` and
  `run_Rmainloop()`. The profiles have been sourced by then, so a profile that
  chose a width of its own is left alone and only R's untouched default is
  replaced. Nothing the console does can be perturbed, because no prompt has
  run yet. Feeding `options(width = …)` through the ReadConsole hook instead
  is what the first attempt did, and it is wrong twice over. It costs a
  main-loop round trip, and a round trip taken before the editor's first
  prompt leaves the terminal in the state R found it in, which desynchronizes
  the next read. A resize is not tracked. A stock terminal session handles
  `SIGWINCH` by calling `R_SetOptionWidth`, which R does not export, and the
  console is the only other way in.
- **Interrupts.** Block SIGINT everywhere except on the R thread. An interrupt
  request sets `R_interrupts_pending`, through the signal on Unix and through
  `UserBreak` on Windows, and R honors it at its next check. While waiting on
  input, poll the flag and long-jump through `Rf_onintr` ourselves.
- **An error never crosses a Rust frame.** Every C-to-Rust callback body is a
  plain frame guarded by `R_ToplevelExec`, with `R_withCallingErrorHandler`
  for structured condition capture, and everything is `extern "C-unwind"`.
  Output capture has two layers. The WriteConsoleEx hook captures R-level
  output, and a file-descriptor dup and pipe captures the C `printf` output
  that bypasses R's console.

## The Windows implementation

This is verified on real Windows against R 4.5.2. The full pty end-to-end
suite runs green over ConPTY, and so do `ry run` exit codes, `system()`, `~`
expansion, and `.Platform$GUI`.

Windows R embedding does not use the Unix `ptr_R_ReadConsole` globals. It
wires the callbacks through the `Rstart` struct instead. Here is the working
recipe.

- **Load.** Open `{R_HOME}\bin\x64\R.dll`, or the plain `bin\` path on ARM64,
  with `LoadLibrary`. Preload the sibling DLLs first, so a compiled package's
  imports resolve. `Rblas`, `Rlapack`, and `Riconv` are best-effort.
  `Rgraphapp.dll` is a hard requirement rather than a best-effort preload. It
  exports `GA_initapp`, and `R.dll` does not. Skipping that call leaves
  graphapp uninitialized, which crashes `readconsolecfg()` with an access
  violation. That exact miss was the original cause of the Windows crash:
  `GA_initapp` was resolved against `R.dll`, found nothing, and was treated as
  optional.
- **Discovery.** Use the `R_HOME` environment variable, else `R.exe RHOME`
  from `PATH`. A registry lookup can come later.
- **Init order, which is load-bearing.** Call `cmdlineoptions(1, [name])`.
  Call `R_DefParamsEx(&rstart, RSTART_VERSION)`, because the version handshake
  makes R validate the struct layout, which replaces mirroring the struct per
  R version. Fill in the callbacks, which are `ReadConsole`, `WriteConsoleEx`
  with plain `WriteConsole` set to NULL, `ShowMessage`, `YesNoCancel`,
  `CallBack`, `Busy`, and `Suicide`. Set `R_Interactive = 1`. Set `rhome` from
  discovery, and set `home` from R's own `getRUser()`. Do not use
  `USERPROFILE`: R's `~` is the Documents folder, the default `R_LIBS_USER`
  hangs off it, and the wrong `home` silently loses the user's installed
  packages. Set `CharacterMode` to `RGui`, so that `R_SetParams` wires the
  callback set. Call `GA_initapp(0, NULL)` from `Rgraphapp.dll`. Call
  `readconsolecfg()`. Only then switch `CharacterMode` to `LinkDLL`, and do it
  before `setup_Rmainloop`. That keeps the RGui callback wiring while avoiding
  the `SetStdHandle` invalidation in `do_system`, which hangs a `system()`
  call. The un-hung behavior is verified. Then call `setup_Rmainloop()` and
  `run_Rmainloop()`.
- **`.Platform$GUI`.** RGui-mode init stamps it `"Rgui"`, and the flip to
  `LinkDLL` does not update it retroactively. A package takes `"Rgui"` as
  license to call an Rgui-only GUI function, such as a menu or a dialog, which
  fails here. The console therefore feeds a first hidden line that rebinds it
  to `"ry"` in `baseenv()`, unlocking and relocking `.Platform`, before any
  user input. One gap is known. R sources the startup profiles before the
  first console read, so profile code still sees `"Rgui"`. Fixing that would
  mean suppressing native profile loading and sourcing the profiles manually
  after init, which is deliberately not taken on.
- **Encoding.** `ry.exe` embeds a Windows application manifest that declares
  UTF-8 as the active code page. `crates/ry/build.rs` writes it, and the MSVC
  linker takes it through `/MANIFESTINPUT`, so this adds no build dependency.
  An embedded R of 4.2 or newer, on UCRT, takes its native encoding from the
  host process's code page, and `R.exe` declares UTF-8 the same way. Without
  the manifest, R runs in the system ANSI code page on any machine that has
  not enabled UTF-8 system-wide, and text handling silently diverges from
  stock R. A machine with the system-wide UTF-8 option masks the gap, so
  verify an encoding claim on a machine with the default locale, where
  `l10n_info()` must report codepage 65001.
- **Line endings.** Every path into the console feed normalizes CRLF, and a
  lone CR, to `\n`. R's parser reports a raw `\r` as "unexpected invalid
  token". Two carriers are real: a script file written on Windows, and the
  editor's multiline buffer, which joins continuation lines with `\r\n` there.
  Single-line interactive input never carries one.
- **Interrupts.** A `SetConsoleCtrlHandler` handler sets both `UserBreak`,
  which is the front-end break flag, and `R_interrupts_pending`, which is the
  deferred flag. Clear both when handling one. Ctrl-C over ConPTY reaches the
  handler only while R evaluates, because raw editor mode disables
  `ENABLE_PROCESSED_INPUT`. That is exactly as intended, and the end-to-end
  interrupt test passes.
- **Editor.** reedline runs on a Windows terminal. The field carries a
  crossterm patch for VT input handling, so expect that caveat at the editor
  layer.
- **End-to-end tests.** The pty suite drives the same harness through ConPTY,
  using `portable-pty`'s native pty, so a change that touches the console is
  verifiable on a Windows machine with R exactly as it is on Unix.

## Console backlog

These parity items were observed in production Rust R consoles. All of them
are compatible with this architecture, and none of them blocks the analysis
work.

- **Upgrade the line editor.** The console pins an old reedline. A newer
  version adds an idle-callback hook, which is the natural seam for running
  analysis between keystrokes, and it brings vi mode and configurable
  keybindings for free. The editor's chrono dependency is unwanted baggage,
  because only its default prompt clock and its sqlite-history timestamps use
  it. The Apple-framework decision record says why that matters at release
  time.
- **History.** Add the sqlite backend, which is an editor feature flag, and
  import from the `.Rhistory` and radian history formats. That is low cost and
  removes a migration step.
- **End-to-end assertions.** Parse pty output through a vt100 screen model
  instead of grepping a raw transcript. That is robust against a redraw and
  against cursor movement.
- **Help.** Add a fuzzy help browser over the installed packages, which a
  comparable console provides. It should come from the analysis stack, where
  hover docs already exist, rather than from a parallel Rd pipeline.
- **Reprex mode**, rendered through our own formatter.
- Auto-matching brackets and smart quotes, and a TOML configuration for colors
  and prompts, once the console has a configuration story.

## No kernel protocol, settled

The reference architecture this design was verified against is a notebook
kernel. Its frontend lives in another process, so it carries a wire protocol,
with message sockets, serialization, signing, ordering, and heartbeats. It
also carries comm channels for its UI surfaces, and, as the structural
consequence, a marshaling layer that ships work from a protocol thread onto
the R thread at a safe point.

None of that applies here, and dropping it is a settled decision rather than a
gap. The frontend is in-process, because the editor runs inside the
ReadConsole hook, so exactly one thread ever touches R and the protocol is a
function call. If a remote or GUI frontend is ever wanted, it becomes a second
frontend over the runtime layer in `libr.rs`, with its own process shape. IPC
does not get threaded through the console. Editor integration is already the
LSP's job.

## The headless runner

`ry run script.R` executes a file through the embedded runtime and exits at
its end. `ry repl --file script.R`, also spelled `-f`, feeds the same script
and then hands over to the interactive prompt.

The mechanism is this. The ReadConsole frontend feeds the script bytes exactly
as it feeds accepted console input, and in batch mode it answers end-of-input
once they are consumed. There is no second driver, and the parse, evaluation,
and autoprint semantics are identical to the console's. Exit-code propagation
needs no new C surface. Batch mode prepends
`options(error = function() q(status = 1, save = "no"))`, so a top-level error
halts the script and the process exits 1. Plain-Command end-to-end tests pin
exit 0 with output, and exit 1 with a halt, and they skip when no R is
installed, like the rest.

Vi keybindings shipped alongside the runner. `ry repl --keybindings vi`
selects the editor's built-in vi mode, and emacs is the default, so the
console needs no configuration story yet.

One thing is still ahead for the runner. It should run TypedR files directly,
which means typecheck, compile in memory, and execute. See
[inline type syntax](/contributing/design/inline-type-syntax/).

## What makes this better than the previous integration

The REPL is not a goal in itself. The point is a console with the analyzer in
the same process.

- **Our parser drives the input.** The answer to "is this input complete?"
  comes from `crates/syntax` rather than from feeding R and watching its parse
  state. That also lets the console highlight the input line and squiggle an
  error in it as the user types.
- **Completions are typed.** `crates/ide` completes over the script so far.
  The plan is to union that with live-session facts, which are the loaded
  namespaces, the `ls()` of the global environment, and the column names of an
  in-memory frame, fetched through the idle-task seam. The session then
  becomes another resolution layer on top of the stub corpus.
- **Diagnostics can run before evaluation.** Run the checker on the pending
  input against the accumulated session document. The REPL history is a script
  document, and the engine already models script scoping top-down.
- **A runtime type bridge, later.** The observed class or type of a session
  value can seed or validate a stub. The idea of introspecting CRAN packages
  then gets an interactive on-ramp.
- The formatter can run on history, hover can work on the input line, and `#:`
  annotations become usable interactively.

## Testing

R initializes once per process, so an embedded-R test needs one process per
test and a one-shot init fixture. Raise `R_CStackLimit` when R runs off the
main thread in a test. CI has no R, so the embedded tests stay excluded from
the workspace gates. The binding layer's declaration list and the discovery
logic are plain Rust and are testable everywhere, so keep the surface that
requires R as thin as possible.

## Constraints and costs, accepted with eyes open

- There is no subprocess isolation, so an R crash kills the REPL process.
  Mitigate that with a trap handler and a frontend restart, not with
  in-process recovery.
- The dyld entitlement on macOS, the `LD_LIBRARY_PATH` arrangement, and the
  Windows DLL preload set are distribution obligations that come with runtime
  loading.
- Mutating an environment variable on Windows must go through R, with
  `Sys.setenv`, once R is up. The C environment space and the Win32
  environment space diverge.
- The binding list is hand-maintained. The `has::` probes and the version
  floor keep that honest.
