---
title: REPL design
description: "How the R console loads R at runtime with no build-time linking"
---

The R console has shipped. It lives in `crates/repl`, behind `ry repl`, and has been verified end to
end against a real R. This page records the architecture that lets ry embed R without linking
against it at build time, and what is still open.

## What ships

All of the following ship: discovering R, the typed runtime-binding layer, the console hosted inside
R's ReadConsole hook, lexer-based highlighting, conservative checking of whether input is complete,
routing of SIGINT interrupts, the headless runner behind `ry run` and `ry repl --file`, vi
keybindings, and embedding on Windows. The pseudo-terminal end-to-end suite runs green against a real
R, both on a Unix pty and on Windows ConPTY, covering interactive evaluation, multiline input, the
error stream, Ctrl-C interruption, and both batch directions. R installs in an agent container
through apt and the CRAN repository, so the suite can run anywhere, not only on a developer machine.

Tab completion is backed by the analysis stack. The `repl` crate itself stays syntax-only and exposes
a `SessionCompleter` seam with two methods, one to accept a line and one to complete. The console
feeds every accepted line back through it, and an `Arc<Mutex>` shares the completer between
reedline's menu and the accept path. The host implements it as `AnalysisCompleter` in
`crates/ry/src/repl_completer.rs`, which runs `ide::completion` over the session treated as a script:
one salsa `SourceFile`, updated per request, with the stubs and export manifests installed. The menu
therefore shows typed signatures for standard-library names, session bindings, `pkg::` exports, and
manifest names. Tab is bound in both emacs mode and vi insert mode.

Two facts about the test harness must survive any rewrite. The pty driver must answer reedline's
cursor-position query, replying `ESC[1;1R` to `ESC[6n`, or the editor blocks before its first prompt.
And interactive sessions must run one at a time, through the in-session `SESSION_LOCK`, because
concurrent R sessions on a loaded machine are flaky while serial runs are stable.

Four things are not built: merging live-session facts (the listing of R's global environment) into
completion, diagnostics before evaluation, hover on the input line, and graphics devices.

## Why linking at build time was a dead end

The predecessor embedded R through `extendr` and libR-sys: bindgen ran at build time against a local
R's headers, and the binary carried a load-time dependency on libR. That had three consequences. The
build machine needed a matching R, which is why that crate was excluded from CI and from every gate.
The artifact was bound to the R it was built against. And a missing symbol was a loader failure
rather than something the program could handle. Deleting that crate removed the exclusion from every
gate command and dropped `extendr` from the workspace.

## The core technique: bind R at runtime, one symbol at a time

Do not link. Open R's shared library with `dlopen` at startup, and resolve every needed symbol by
name, each one individually optional.

The declarations here are original, written from R's public headers. The design was checked against
the source of a production-grade Rust R kernel, which served as study material, not something to
copy.

- **The binding surface is written by hand, not generated.** One declaration list covers the C API
  functions, variadic functions such as `Rf_error` and `Rprintf`, the mutable globals, and the
  constants whose values are snapshotted. A variadic is stored with its real `...` type and exposed
  at a fixed arity. The mutable globals are `R_interrupts_pending`, `R_Interactive`, the
  `ptr_R_ReadConsole` and `ptr_R_WriteConsoleEx` hook pointers, `R_PolledEvents`, and
  `R_SignalHandlers`; the constants are `R_GlobalEnv`, `R_NilValue`, and the like. Declarative
  macros expand each declaration into a `static Option<fn ptr>` plus a passthrough wrapper.
  Resolution happens eagerly, in one batched dlsym sweep at init, rather than lazily per call.
- **A missing symbol is `None`, not a crash.** Every binding gets a `has::name()` probe, and a call
  site branches on it to fall back on an older R, using a newer accessor when it is present and the
  classic macro equivalent otherwise. That is the entire version-compatibility story: resolve
  optimistically, and degrade one symbol at a time. A hard version floor keeps the matrix of
  fallbacks small. It is read by parsing `{R_HOME}/library/base/DESCRIPTION`, and R 4.2 matches
  current ecosystem practice.
- **Init has two phases, and their order matters.** Functions and mutable globals are bound before
  `Rf_initialize_R`. Constant globals are copied by value only after `setup_Rmainloop()`, because
  that is where R initializes them. The library handle is leaked and held for the life of the
  process.
- **The loader flags matter.** On Unix, open the library with `RTLD_LAZY | RTLD_GLOBAL`, so that a
  compiled package whose shared object links libR resolves R's symbols as if the host had linked R
  itself. Also set `LD_LIBRARY_PATH` (or `DYLD_LIBRARY_PATH`) to `{R_HOME}/lib`, so that such a
  package can find a libR when it is itself opened; the `RTLD_GLOBAL` symbols then shadow it. macOS
  needs the dyld-environment entitlement on the binary. On Windows, open `R.dll` together with its
  sibling DLLs (`Rblas`, `Rlapack`, `Riconv`, and `Rgraphapp`), so that the list of loaded modules
  satisfies a package's imports. Windows looks symbols up per module, so it needs no equivalent of
  `RTLD_GLOBAL`.
- **Keep structs whose layout drifts off the surface.** R's `DevDesc` and `Rstart` change layout
  between versions. The reference approach mirrors them per engine version and casts at runtime. The
  console avoids these surfaces entirely, since it has no custom graphics device, and the technique
  is recorded here for when plots arrive.

## Discovery

1. Use the `R_HOME` environment variable when it is set, so an editor or CI can pin the version.
2. Otherwise, run `R RHOME` from the `PATH` (`R.exe` or `R.bat` on Windows) and read its output.
   Re-export the result as `R_HOME`, so that R's own `R_HomeDir()` agrees.
3. Find the shared library at `{R_HOME}/lib/libR.so` or `{R_HOME}/lib/libR.dylib`, or at
   `{R_HOME}/bin/x64/R.dll` on Windows. On macOS, the framework's `R_HOME` already points into
   `Resources/`. When the shared library is missing, fail with a message that names
   `--enable-R-shlib`.
4. Recover the secondary variables `R_SHARE_DIR`, `R_INCLUDE_DIR`, and `R_DOC_DIR`. R's shell
   wrapper at `{R_HOME}/bin/R` exports them, and embedding bypasses the wrapper, so on a layout that
   relocates those directories, as Fedora and RHEL do, `R.home("share")` would otherwise point at a
   path under `{R_HOME}` that does not exist. The console parses the wrapper's plain `VAR=value` lines
   instead: R substitutes those values literally at install time, and asking a separate R process
   would add a few hundred milliseconds to startup. This is Unix-only, because Windows R derives the
   variables from `R_HOME` internally.

## Process shape and the console loop

- **R owns the process's main thread**, because its stack checks and its expectations about signals
  assume it does. Everything else (the protocol threads, the analysis engine, output capture) runs on
  background threads of the same process. R initializes once per process, so tests run one process
  per test.
- **Init runs in this order.** Suppress R's signal handlers by setting `R_SignalHandlers = 0` and
  installing our own. Call `Rf_initialize_R` with `--interactive --no-save --no-restore-data`. Hook
  `ptr_R_ReadConsole`, `ptr_R_WriteConsoleEx`, `ptr_R_Busy`, and `ptr_R_Suicide`. Set
  `R_PolledEvents` and `R_wait_usec`, so that long-running R code polls us. Then call
  `setup_Rmainloop()` and `run_Rmainloop()`. This drives R's *real* REPL through the console hooks,
  rather than through `R_ReplDLLdo1`, which keeps `browser()` prompts, `readline()` calls, and nested
  REPLs honest.
- **The ReadConsole callback is the scheduler.** When R asks for input, the process is at a safe idle
  point. Classify the prompt as top-level, a `browser()` prompt, or a `readline()` request, then park
  in a channel select over user input, evaluation requests, and idle work. A periodic tick runs R's
  input handlers, so background R machinery such as the help server or an event-loop package stays
  alive. R is fed one expression per read, which we parse and split first.
- **Exactly one thread touches R, by construction.** The editor runs inside the ReadConsole hook, so
  the console and R share the main thread, and there is no layer for marshaling work across threads.
  Analysis keeps it that way: it may run on a background thread, but a live-session fact, such as a
  loaded namespace, the `ls()` of the global environment, or a data frame's columns, is fetched only
  while R is parked at a prompt, on the main thread, between reads. A background thread never calls
  into R.
- **The terminal width is ours to set.** R's `width` option defaults to 80 columns whatever the
  terminal is, and R exports no setter for it, so a table that had room on screen still wrapped. The
  console measures the width once and applies it with `R_ParseEvalString`, in the window between
  `setup_Rmainloop()` and `run_Rmainloop()`. The profiles have been sourced by then, so a profile that
  chose its own width is left alone and only R's untouched default is replaced, and nothing the
  console does can be disturbed, because no prompt has run yet. The first attempt fed
  `options(width = …)` through the ReadConsole hook instead, which is wrong twice over: it costs a
  round trip through the main loop, and a round trip taken before the editor's first prompt leaves
  the terminal in whatever state R found it, which desynchronizes the next read. Resizing is not
  tracked. A stock terminal session handles `SIGWINCH` by calling `R_SetOptionWidth`, which R does not
  export, and the console is the only other way in.
- **Interrupts.** SIGINT is blocked everywhere except on the R thread. An interrupt sets
  `R_interrupts_pending` (through the signal on Unix, through `UserBreak` on Windows), and R honors it
  at its next check. While waiting for input, the console polls the flag and long-jumps through
  `Rf_onintr` itself.
- **An R error never crosses a Rust frame.** Every callback from C into Rust has a plain frame,
  guarded by `R_ToplevelExec`, uses `R_withCallingErrorHandler` to capture structured conditions, and
  is declared `extern "C-unwind"`. Output is captured in two layers: the WriteConsoleEx hook captures
  R-level output, and a dup of the file descriptor into a pipe captures C-level `printf` output that
  bypasses R's console.

## The Windows implementation

This has been verified on real Windows against R 4.5.2. The full pty end-to-end suite runs green over
ConPTY, and so do `ry run`'s exit codes, `system()`, `~` expansion, and `.Platform$GUI`.

Embedding R on Windows does not use the Unix `ptr_R_ReadConsole` globals; the callbacks are wired
through the `Rstart` struct instead. This is the recipe that works:

- **Load.** Open `{R_HOME}\bin\x64\R.dll` (or the plain `bin\` path on ARM64) with `LoadLibrary`,
  after preloading the sibling DLLs so that a compiled package's imports resolve. `Rblas`, `Rlapack`,
  and `Riconv` are preloaded on a best-effort basis, but `Rgraphapp.dll` is required, because it
  exports `GA_initapp` and `R.dll` does not. Skipping that call leaves graphapp uninitialized, and
  `readconsolecfg()` then crashes with an access violation. That exact miss caused the original
  Windows crash: `GA_initapp` was looked up in `R.dll`, not found, and treated as optional.
- **Discovery.** Use the `R_HOME` environment variable, or else `R.exe RHOME` from the `PATH`. A
  registry lookup can come later.
- **Init order, which matters.** Call `cmdlineoptions(1, [name])`. Call
  `R_DefParamsEx(&rstart, RSTART_VERSION)`; the version handshake makes R validate the struct's
  layout, which saves mirroring the struct for each R version. Fill in the callbacks: `ReadConsole`,
  `WriteConsoleEx` (with plain `WriteConsole` set to NULL), `ShowMessage`, `YesNoCancel`,
  `CallBack`, `Busy`, and `Suicide`. Set `R_Interactive = 1`. Set `rhome` from discovery, and `home`
  from R's own `getRUser()`, *not* from `USERPROFILE`: R's `~` is the Documents folder, the default
  `R_LIBS_USER` hangs off it, and the wrong `home` silently loses the user's installed packages. Set
  `CharacterMode` to `RGui`, so that `R_SetParams` wires up the callbacks. Call `GA_initapp(0, NULL)`
  from `Rgraphapp.dll`, then `readconsolecfg()`. Only then switch `CharacterMode` to `LinkDLL`, before
  `setup_Rmainloop`. That keeps the RGui callback wiring while avoiding the `SetStdHandle`
  invalidation in `do_system`, which otherwise makes a `system()` call hang; the fixed behavior is
  verified. Finally, call `setup_Rmainloop()` and `run_Rmainloop()`.
- **`.Platform$GUI`.** Initializing in RGui mode stamps it as `"Rgui"`, and switching to `LinkDLL`
  does not update it after the fact. Packages take `"Rgui"` as permission to call Rgui-only GUI
  functions, such as menus and dialogs, which fail here. So before any user input, the console feeds
  a hidden first line that rebinds it to `"ry"` in `baseenv()`, unlocking and relocking `.Platform`.
  One gap is known: R sources the startup profiles before the first console read, so profile code
  still sees `"Rgui"`. Fixing that would mean suppressing R's own profile loading and sourcing the
  profiles by hand after init, which is deliberately not taken on.
- **Encoding.** `ry.exe` embeds a Windows application manifest that declares UTF-8 as the active code
  page. `crates/ry/build.rs` writes it, and the MSVC linker takes it through `/MANIFESTINPUT`, so it
  adds no build dependency. An embedded R 4.2 or newer, on UCRT, takes its native encoding from the
  host process's code page, and `R.exe` declares UTF-8 the same way. Without the manifest, R runs in
  the system's ANSI code page on any machine that has not enabled UTF-8 system-wide, and text handling
  silently diverges from stock R. A machine with the system-wide UTF-8 option hides the problem, so
  verify any encoding claim on a machine with the default locale, where `l10n_info()` must report
  code page 65001.
- **Line endings.** Every path into the console normalizes CRLF, and a lone CR, to `\n`, because R's
  parser reports a raw `\r` as "unexpected invalid token". Two sources really produce them: a script
  written on Windows, and the editor's multiline buffer, which joins continuation lines with `\r\n`
  there. Single-line interactive input never contains one.
- **Interrupts.** A `SetConsoleCtrlHandler` handler sets both `UserBreak`, the front end's break
  flag, and `R_interrupts_pending`, the deferred flag, and both are cleared when one is handled.
  Over ConPTY, Ctrl-C reaches the handler only while R is evaluating, because the editor's raw mode
  disables `ENABLE_PROCESSED_INPUT`. That is exactly what is wanted, and the end-to-end interrupt test
  passes.
- **Editor.** reedline runs fine in a Windows terminal. The ecosystem carries a crossterm patch for
  handling VT input, so expect that caveat at the editor layer.
- **End-to-end tests.** The pty suite drives the same harness through ConPTY, using `portable-pty`'s
  native pty, so a change that touches the console can be verified on a Windows machine with R exactly
  as on Unix.

## Console backlog

These parity items come from production Rust R consoles. All of them fit this architecture, and none
blocks the analysis work.

- **Upgrade the line editor.** The console pins an old reedline. A newer version adds an idle
  callback, the natural seam for running analysis between keystrokes, and brings vi mode and
  configurable keybindings for free. The editor's chrono dependency is unwanted baggage, since only
  its default prompt clock and the timestamps in its sqlite history use it; the decision record on
  Apple frameworks explains why that matters at release time.
- **History.** Add the sqlite backend, an editor feature flag, and import the `.Rhistory` and radian
  history formats. That is cheap, and removes a migration step.
- **End-to-end assertions.** Parse the pty output through a vt100 screen model instead of grepping a
  raw transcript, which is robust against redraws and cursor movement.
- **Help.** Add a fuzzy help browser over the installed packages, as comparable consoles have. It
  should come from the analysis stack, where hover documentation already exists, not from a separate
  Rd pipeline.
- **A reprex mode**, rendered through ry's own formatter.
- **Auto-matching brackets, smart quotes, and a TOML configuration for colors and prompts**, once the
  console has a configuration story.

## No kernel protocol: settled

The reference architecture this design was checked against is a notebook kernel. Its front end lives
in another process, so it needs a wire protocol (message sockets, serialization, signing, ordering,
heartbeats), comm channels for its UI surfaces, and, as a structural consequence, a layer that ships
work from a protocol thread onto the R thread at a safe point.

None of that applies here, and dropping it is a settled decision, not a gap. The front end is in the
same process, since the editor runs inside the ReadConsole hook, so exactly one thread ever touches R
and the "protocol" is a function call. If a remote or GUI front end is ever wanted, it will be a
second front end over the runtime layer in `libr.rs`, with its own process shape, rather than IPC
threaded through the console. Editor integration is already the language server's job.

## The headless runner

`ry run script.R` executes a file through the embedded runtime and exits at its end. `ry repl --file
script.R` (or `-f`) feeds the same script and then hands over to the interactive prompt.

The ReadConsole front end feeds the script's bytes exactly as it feeds accepted console input, and in
batch mode it answers "end of input" once they are used up. There is no second driver, so parsing,
evaluation, and autoprinting behave exactly as in the console. Propagating the exit code needs no new
C surface: batch mode prepends `options(error = function() q(status = 1, save = "no"))`, so a
top-level error halts the script and the process exits with status 1. End-to-end tests that run the
plain command pin exit 0 with output and exit 1 with a halt, and skip when no R is installed, like
the rest.

Vi keybindings shipped alongside the runner. `ry repl --keybindings vi` selects the editor's built-in
vi mode, with emacs as the default, so the console needs no configuration story yet.

One thing is still ahead for the runner: running typed `.ry` sources directly, which means
type-checking them, compiling them in memory, and executing the result (see
[inline type syntax](/contributing/design/inline-type-syntax/)).

## What makes this better than the previous integration

The REPL is not a goal in itself. The point is a console with the analyzer in the same process:

- **Our parser drives the input.** Whether the input is complete is decided by `crates/syntax`, not
  by feeding R and watching its parse state. That also lets the console highlight the input line, and
  underline an error in it while you type.
- **Completions are typed.** `crates/ide` completes over the script so far. The plan is to combine
  that with live-session facts (loaded namespaces, the `ls()` of the global environment, and the
  column names of a data frame in memory), fetched through the idle-task seam, which would make the
  session one more resolution layer on top of the stub corpus.
- **Diagnostics can run before evaluation.** Run the checker on the pending input against the session
  so far. The REPL history is a script document, and the engine already models script scoping from
  the top down.
- **A runtime type bridge, later.** The class or type actually observed for a session value could
  seed or validate a stub, giving the idea of introspecting CRAN packages an interactive on-ramp.
- **The rest of the toolchain comes along**: the formatter can run on history, hover can work on the
  input line, and `#:` annotations become usable interactively.

## Testing

R initializes once per process, so a test that embeds R needs a process of its own and a one-shot
init fixture, and a test that runs R off the main thread must raise `R_CStackLimit`. CI has no R, so
the embedded tests stay out of the workspace gates. The binding layer's declaration list and the
discovery logic are plain Rust that can be tested anywhere, so keep the surface that needs R as thin
as possible.

## Constraints and costs, accepted with eyes open

- There is no subprocess isolation, so an R crash kills the REPL process. Mitigate that with a trap
  handler and a front-end restart, not with in-process recovery.
- The dyld entitlement on macOS, the `LD_LIBRARY_PATH` arrangement, and the set of preloaded DLLs on
  Windows are distribution obligations that come with loading R at runtime.
- On Windows, once R is up, environment variables must be changed through R, with `Sys.setenv`,
  because the C environment and the Win32 environment diverge.
- The binding list is maintained by hand, and the `has::` probes and the version floor keep it honest.
