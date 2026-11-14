//! The boundary-call recorder (`RY_CAPTURE_PACKAGE`): it measures where a workload's user code
//! calls into package code and keeps a few of those calls, with their inputs, to replay later.
//!
//! The user's code is named by its top-level environments: namespace names and `R_GlobalEnv`.
//! Every closure call passes through [`enter`], which keeps a stack of layer records. A user
//! closure called from outside user code pushes a user record, and a package closure called from
//! user code pushes a package record: that call is a boundary call, keyed by the callee's owner
//! and the name its call used. Calls inside one layer push nothing.
//!
//! A boundary call is capturable when `eval(call, env)` in a fresh process without the user's
//! code would redo it, which [`inspect`] decides from the names the call mentions. A capturable
//! call owns its whole inclusive time and covers everything below it. Any other boundary call
//! owns only its exclusive time, and the user code it calls back into is measured as usual.
//! Each key keeps a reservoir of sampled calls; a sample's inputs are serialized at entry,
//! because the environments it reads may change while the call runs, and its value at exit.
//!
//! Recording runs from the end of start-up to the end of the session. The work it does for a
//! call (forcing promises, serializing) runs suspended, so the calls that work makes are not
//! recorded themselves.

use {
    crate::{
        dispatch, envir,
        eval::{self, Reify},
        interp,
        unwind::{R, Unwind, error},
        values::{self, base_env, base_namespace, global_env, missing_arg, nil, symbol_name},
    },
    heap::{
        activity,
        object::{Sexp, SexpType, gp},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    std::{
        cell::{Cell, RefCell},
        path::PathBuf,
        rc::Rc,
        time::{Duration, Instant},
    },
};

/// Comma-separated names of the user's top-level environments: namespace names and
/// `R_GlobalEnv`.
pub const PACKAGE_VARIABLE: &str = "RY_CAPTURE_PACKAGE";
/// The directory the recorder writes to.
pub const DIRECTORY_VARIABLE: &str = "RY_CAPTURE_DIR";
/// How many samples each key keeps (default 8).
pub const SAMPLES_VARIABLE: &str = "RY_CAPTURE_SAMPLES";
/// The largest serialized sample input or value kept, in bytes (default 65536).
pub const MAX_BYTES_VARIABLE: &str = "RY_CAPTURE_MAX_BYTES";

const GLOBAL_LABEL: &str = "R_GlobalEnv";

/// A recording in progress, from [`mark_ready`] to [`finish`].
pub struct Scope {
    /// One more than the index of the record the call pushed, or 0 when it pushed none.
    record: usize,
}

impl Scope {
    #[inline(always)]
    pub fn none() -> Scope {
        Scope { record: 0 }
    }

    /// Ends the call's record with the call's outcome. A scope that drops without this records
    /// the call as ended by an unwind.
    #[inline(always)]
    pub unsafe fn returned(&mut self, result: &R<Sexp>) {
        if self.record != 0 {
            let index = self.record - 1;
            self.record = 0;
            leave(index, Some(result));
        }
    }
}

impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        if self.record != 0 {
            // SAFETY: the scope lives on the R thread, inside the call it records.
            unsafe { leave(self.record - 1, None) };
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<Capture>> = const { RefCell::new(None) };
    /// Set while the recorder itself runs R code, so the calls that code makes are not recorded.
    static SUSPENDED: Cell<bool> = const { Cell::new(false) };
}

struct Capture {
    /// The process that read the configuration. A forked child inherits the state but writes
    /// nothing.
    process: u32,
    directory: PathBuf,
    users: Vec<String>,
    sample_slots: usize,
    max_bytes: usize,
    /// When recording started; `None` until [`mark_ready`].
    started: Option<Instant>,
    stack: Vec<Record>,
    /// The index of the capturable boundary call whose region the top of the stack is in.
    covering: Option<usize>,
    keys: Vec<KeyStats>,
    key_index: FxHashMap<(Rc<str>, Rc<str>), usize>,
    /// Owner label and whether it is user code, by top-level environment address. Only
    /// namespaces and the global environment are kept: they live for the whole session, so an
    /// address names one environment.
    owners: FxHashMap<usize, (Rc<str>, bool)>,
    /// The names each call expression mentions, by the call's address. Every call in the table
    /// is a global root, so its address is never reused.
    free_names: FxHashMap<usize, Rc<FreeNames>>,
    files: Vec<SampleFile>,
    random: u64,
    user: Duration,
    covered: Duration,
    uncapturable: Duration,
    boundary_calls: u64,
    /// The first failure to write a sample, reported when the session ends.
    failure: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    User,
    Package,
}

struct Record {
    layer: Layer,
    role: Role,
    started: Instant,
    /// The inclusive time of the records pushed directly above this one.
    nested: Duration,
}

enum Role {
    User,
    /// Below a capturable boundary call, which owns this record's time.
    Covered,
    Capturable {
        key: usize,
        sample: Option<Pending>,
        /// Whether user code ran below the call.
        reentered: bool,
    },
    Uncapturable {
        key: usize,
        reason: Reason,
    },
}

/// A sampled call's inputs, serialized at entry, waiting for its outcome.
struct Pending {
    slot: usize,
    inputs: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Reason {
    Dots,
    LocalClosure,
    UserBinding,
    /// The callee reads its caller's frame (`match.arg`, `match.call`, `sys.function`): a
    /// replay calls it from a different frame.
    CallerFrame,
}

struct KeyStats {
    package: Rc<str>,
    name: Rc<str>,
    calls: u64,
    seconds: Duration,
    uncapturable_calls: u64,
    uncapturable_seconds: Duration,
    dots: u64,
    local_closure: u64,
    user_binding: u64,
    caller_frame: u64,
    oversize: u64,
    /// Capturable calls that ran user code below them: a method the user defined, or a closure
    /// reached through a value. Their replay without the user's code may differ.
    reentered: u64,
    /// Capturable calls seen, the reservoir's denominator.
    seen: u64,
    /// The sample file of each reservoir slot.
    slots: Vec<Option<usize>>,
}

struct SampleFile {
    key: usize,
    seconds: f64,
    threw: bool,
    bytes: usize,
}

struct FreeNames {
    symbols: Vec<Sexp>,
    dots: bool,
}

/// Reads the recorder's configuration, if [`PACKAGE_VARIABLE`] is set. Recording starts at
/// [`mark_ready`].
pub fn start_from_environment() {
    let Some(packages) = std::env::var_os(PACKAGE_VARIABLE).filter(|value| !value.is_empty())
    else {
        return;
    };
    let directory = std::env::var_os(DIRECTORY_VARIABLE)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("ry-capture"), PathBuf::from);
    let number = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(default)
    };
    let sample_slots = number(SAMPLES_VARIABLE, 8);
    let max_bytes = number(MAX_BYTES_VARIABLE, 65536);
    // Child processes (callr, parallel's workers) would write over this process's outputs.
    // SAFETY: the session starts before R code can start threads that read the environment.
    unsafe {
        for name in [
            PACKAGE_VARIABLE,
            DIRECTORY_VARIABLE,
            SAMPLES_VARIABLE,
            MAX_BYTES_VARIABLE,
        ] {
            std::env::remove_var(name);
        }
    }
    // R code may change the working directory before the outputs are written.
    let directory = std::path::absolute(&directory).unwrap_or(directory);
    let users = packages
        .to_string_lossy()
        .split(',')
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    STATE.with_borrow_mut(|state| {
        *state = Some(Capture {
            process: std::process::id(),
            directory,
            users,
            sample_slots,
            max_bytes,
            started: None,
            stack: Vec::new(),
            covering: None,
            keys: Vec::new(),
            key_index: FxHashMap::default(),
            owners: FxHashMap::default(),
            free_names: FxHashMap::default(),
            files: Vec::new(),
            random: 0x9e37_79b9_7f4a_7c15,
            user: Duration::ZERO,
            covered: Duration::ZERO,
            uncapturable: Duration::ZERO,
            boundary_calls: 0,
            failure: None,
        });
    });
}

/// Starts recording, at the end of start-up.
pub fn mark_ready() {
    STATE.with_borrow_mut(|state| {
        let Some(state) = state.as_mut().filter(|state| state.started.is_none()) else {
            return;
        };
        let now = Instant::now();
        state.started = Some(now);
        if let Err(failure) = std::fs::create_dir_all(state.directory.join("samples")) {
            state.failure = Some(format!(
                "cannot create '{}': {failure}",
                state.directory.display()
            ));
        }
        // Top-level code of a script is user code when the global environment is.
        if state.users.iter().any(|name| name == GLOBAL_LABEL) {
            state.stack.push(Record {
                layer: Layer::User,
                role: Role::User,
                started: now,
                nested: Duration::ZERO,
            });
        }
        activity::set_hook(activity::CAPTURE, true);
        activity::set_enabled(true);
    });
}

/// Records the entry of a call of the closure `function` through `call`. `sys_parent` is the
/// environment the call was made from, or `NULL` when it was made from the slots of the frame
/// [`crate::Interp::slot_owner`] names.
pub unsafe fn enter(call: Sexp, function: Sexp, sys_parent: Sexp) -> R<Scope> {
    if SUSPENDED.get() {
        return Ok(Scope::none());
    }
    let top = dispatch::top_env(function.tag());
    let known = STATE.with_borrow(|state| {
        state
            .as_ref()
            .and_then(|state| state.owners.get(&(top.as_ptr() as usize)).cloned())
    });
    let (owner, user) = match known {
        Some(owner) => owner,
        None => owner_of(top),
    };
    enum Step {
        Nothing,
        User,
        Boundary,
    }
    let step = STATE.with_borrow(|state| {
        let Some(state) = state.as_ref().filter(|state| state.started.is_some()) else {
            return Step::Nothing;
        };
        let current = state.stack.last().map(|record| record.layer);
        match (user, current) {
            (true, Some(Layer::User)) | (false, None | Some(Layer::Package)) => Step::Nothing,
            (true, _) => Step::User,
            (false, Some(Layer::User)) => Step::Boundary,
        }
    });
    match step {
        Step::Nothing => Ok(Scope::none()),
        Step::User => Ok(push(Layer::User, |state| {
            if let Some(covering) = state.covering {
                if let Some(Record {
                    role: Role::Capturable { reentered, .. },
                    ..
                }) = state.stack.get_mut(covering)
                {
                    *reentered = true;
                }
                Role::Covered
            } else {
                Role::User
            }
        })),
        Step::Boundary => boundary(call, sys_parent, owner),
    }
}

unsafe fn boundary(call: Sexp, sys_parent: Sexp, owner: Rc<str>) -> R<Scope> {
    let name: Rc<str> = match head_symbol(call) {
        Some(symbol) => symbol_name(symbol).as_ref().into(),
        None => "<anonymous>".into(),
    };
    let covered = STATE.with_borrow(|state| state.as_ref().is_some_and(|s| s.covering.is_some()));
    if covered {
        return Ok(push(Layer::Package, |_| Role::Covered));
    }
    let free = free_names(call);
    let _suspended = Suspend::start();
    let mark = crate::heap().handle_mark();
    let inspected = if free.dots {
        Inspection::Uncapturable(Reason::Dots)
    } else if reads_caller_frame(&owner, &name, call) {
        Inspection::Uncapturable(Reason::CallerFrame)
    } else {
        match inspect(&free, sys_parent) {
            Ok(inspected) => inspected,
            Err(unwind) => {
                crate::heap().release_handles(mark);
                return Err(unwind);
            }
        }
    };
    let role = match inspected {
        Inspection::Uncapturable(reason) => {
            let key = key_of(&owner, &name);
            Role::Uncapturable { key, reason }
        }
        Inspection::Capturable {
            variables,
            imports,
            complete,
        } => {
            let key = key_of(&owner, &name);
            let slot = STATE.with_borrow_mut(|state| {
                let state = state.as_mut()?;
                let slots = state.sample_slots;
                let stats = state.keys.get_mut(key)?;
                stats.seen += 1;
                let seen = stats.seen;
                if seen as usize <= slots {
                    return Some(seen as usize - 1);
                }
                let draw = next_random(&mut state.random) % seen;
                (draw < slots as u64).then_some(draw as usize)
            });
            let sample = match slot {
                Some(slot) if complete => {
                    let inputs = sample_inputs(&owner, &name, call, &variables, &imports)
                        .and_then(|inputs| serialize(inputs));
                    match inputs {
                        Ok(bytes) if bytes.len() <= max_bytes() => Some(Pending {
                            slot,
                            inputs: bytes,
                        }),
                        Ok(_) => {
                            with_key(key, |stats| stats.oversize += 1);
                            None
                        }
                        Err(unwind) => {
                            note_failure(&unwind);
                            None
                        }
                    }
                }
                _ => None,
            };
            Role::Capturable {
                key,
                sample,
                reentered: false,
            }
        }
    };
    crate::heap().release_handles(mark);
    drop(_suspended);
    let capturable = matches!(role, Role::Capturable { .. });
    let scope = push(Layer::Package, |state| {
        state.boundary_calls += 1;
        role
    });
    if capturable {
        STATE.with_borrow_mut(|state| {
            if let Some(state) = state.as_mut() {
                state.covering = Some(scope.record - 1);
            }
        });
    }
    Ok(scope)
}

/// Pushes a record whose role `role` decides, starting its clock now.
fn push(layer: Layer, role: impl FnOnce(&mut Capture) -> Role) -> Scope {
    STATE.with_borrow_mut(|state| {
        let Some(state) = state.as_mut() else {
            return Scope::none();
        };
        let role = role(state);
        state.stack.push(Record {
            layer,
            role,
            started: Instant::now(),
            nested: Duration::ZERO,
        });
        Scope {
            record: state.stack.len(),
        }
    })
}

/// Pops the record at `index`, and any a non-local exit left above it, and attributes its time.
unsafe fn leave(index: usize, result: Option<&R<Sexp>>) {
    let finished = STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        if state.stack.len() <= index {
            return None;
        }
        state.stack.truncate(index + 1);
        let record = state.stack.pop()?;
        if state.covering.is_some_and(|covering| covering >= index) {
            state.covering = None;
        }
        let inclusive = record.started.elapsed();
        if let Some(parent) = state.stack.last_mut() {
            parent.nested += inclusive;
        }
        let exclusive = inclusive.saturating_sub(record.nested);
        match record.role {
            Role::User => state.user += exclusive,
            Role::Covered => {}
            Role::Capturable {
                key,
                sample,
                reentered,
            } => {
                state.covered += inclusive;
                let stats = state.keys.get_mut(key)?;
                stats.calls += 1;
                stats.reentered += u64::from(reentered);
                stats.seconds += inclusive;
                return sample.map(|sample| (key, sample, inclusive));
            }
            Role::Uncapturable { key, reason } => {
                state.uncapturable += exclusive;
                let stats = state.keys.get_mut(key)?;
                stats.uncapturable_calls += 1;
                stats.uncapturable_seconds += exclusive;
                match reason {
                    Reason::Dots => stats.dots += 1,
                    Reason::LocalClosure => stats.local_closure += 1,
                    Reason::UserBinding => stats.user_binding += 1,
                    Reason::CallerFrame => stats.caller_frame += 1,
                }
            }
        }
        None
    });
    if let Some((key, sample, seconds)) = finished {
        let _suspended = Suspend::start();
        let mark = crate::heap().handle_mark();
        let value = match result {
            Some(Ok(value)) => {
                crate::heap().root(*value);
                Some(*value)
            }
            _ => None,
        };
        if let Err(unwind) = write_sample(key, sample, seconds, value) {
            note_failure(&unwind);
        }
        crate::heap().release_handles(mark);
    }
}

/// Runs `work` unrecorded and leaves its time out of every total, as if the recording had
/// paused. The host uses it for the R code it runs itself after start-up, such as attaching the
/// package loader, which is neither the user's code nor a call the user's code made.
pub fn without_recording<T>(work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let suspended = Suspend::start();
    let result = work();
    drop(suspended);
    let paused = started.elapsed();
    STATE.with_borrow_mut(|state| {
        if let Some(state) = state.as_mut()
            && let Some(recording) = state.started.as_mut()
        {
            *recording += paused;
            for record in &mut state.stack {
                record.started += paused;
            }
        }
    });
    result
}

/// Restores [`SUSPENDED`] and the visibility of the last value when dropped, however the
/// recorder's own work ends: the R code it runs (`serialize`, forcing promises) sets the
/// visibility, and the call being recorded may have returned invisibly.
struct Suspend {
    suspended: bool,
    visible: bool,
}

impl Suspend {
    fn start() -> Suspend {
        Suspend {
            suspended: SUSPENDED.replace(true),
            visible: interp().visible,
        }
    }
}

impl Drop for Suspend {
    fn drop(&mut self) {
        SUSPENDED.set(self.suspended);
        interp().visible = self.visible;
    }
}

/// The owner label of the top-level environment `top`, and whether it is user code.
unsafe fn owner_of(top: Sexp) -> (Rc<str>, bool) {
    let (name, stable) = if top == global_env() {
        (GLOBAL_LABEL.to_owned(), true)
    } else if envir::is_namespace(top) {
        (
            envir::namespace_name(top).unwrap_or_else(|| "other".to_owned()),
            true,
        )
    } else {
        ("other".to_owned(), false)
    };
    STATE.with_borrow_mut(|state| {
        let Some(state) = state.as_mut() else {
            return (Rc::from(name.as_str()), false);
        };
        let user = stable && state.users.contains(&name);
        let owner: (Rc<str>, bool) = (Rc::from(name.as_str()), user);
        if stable {
            state.owners.insert(top.as_ptr() as usize, owner.clone());
        }
        owner
    })
}

unsafe fn head_symbol(call: Sexp) -> Option<Sexp> {
    if call.kind() != SexpType::Language {
        return None;
    }
    let mut head = call.car();
    if head.kind() == SexpType::Language
        && head.car().kind() == SexpType::Symbol
        && matches!(&*symbol_name(head.car()), "::" | ":::")
    {
        head = head.cdr().cdr().car();
    }
    (head.kind() == SexpType::Symbol).then_some(head)
}

/// Base functions whose result depends on the frame that called them, not only on their
/// arguments. A replay evaluates the recorded call in a fresh environment with no calling
/// closure, so these would answer about the wrong frame. `match.arg(arg)` with no `choices`
/// reads the caller's formals the same way; with explicit choices it is an ordinary call.
unsafe fn reads_caller_frame(owner: &str, name: &str, call: Sexp) -> bool {
    if owner != "base" {
        return false;
    }
    match name {
        "match.call" | "sys.call" | "sys.function" | "sys.frame" | "sys.nframe" | "sys.calls"
        | "sys.frames" | "sys.parent" | "sys.parents" | "sys.on.exit" | "parent.frame"
        | "environment" | "Recall" | "missing" | "nargs" => true,
        "match.arg" => {
            let mut arguments = 0;
            let mut rest = call.cdr();
            while rest.kind() == SexpType::Pairlist {
                arguments += 1;
                rest = rest.cdr();
            }
            arguments <= 1
        }
        _ => false,
    }
}

fn key_of(owner: &Rc<str>, name: &Rc<str>) -> usize {
    STATE.with_borrow_mut(|state| {
        let Some(state) = state.as_mut() else {
            return 0;
        };
        if let Some(&key) = state.key_index.get(&(owner.clone(), name.clone())) {
            return key;
        }
        let key = state.keys.len();
        let slots = state.sample_slots;
        state.keys.push(KeyStats {
            package: owner.clone(),
            name: name.clone(),
            calls: 0,
            seconds: Duration::ZERO,
            uncapturable_calls: 0,
            uncapturable_seconds: Duration::ZERO,
            dots: 0,
            local_closure: 0,
            user_binding: 0,
            caller_frame: 0,
            oversize: 0,
            reentered: 0,
            seen: 0,
            slots: vec![None; slots],
        });
        state.key_index.insert((owner.clone(), name.clone()), key);
        key
    })
}

fn with_key(key: usize, update: impl FnOnce(&mut KeyStats)) {
    STATE.with_borrow_mut(|state| {
        if let Some(stats) = state.as_mut().and_then(|state| state.keys.get_mut(key)) {
            update(stats);
        }
    });
}

fn max_bytes() -> usize {
    STATE.with_borrow(|state| state.as_ref().map_or(0, |state| state.max_bytes))
}

fn next_random(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

fn note_failure(unwind: &Unwind) {
    let message = match unwind {
        Unwind::Error(failure) => failure.message.clone(),
        Unwind::Interrupt => "interrupted".to_owned(),
        Unwind::Jump(_) => "a non-local exit".to_owned(),
    };
    STATE.with_borrow_mut(|state| {
        if let Some(state) = state.as_mut()
            && state.failure.is_none()
        {
            state.failure = Some(format!("cannot record a sample: {message}"));
        }
    });
}

/// Every symbol `call` mentions, function positions and the bodies of inline functions
/// included, once each.
unsafe fn free_names(call: Sexp) -> Rc<FreeNames> {
    let address = call.as_ptr() as usize;
    if let Some(names) = STATE.with_borrow(|state| {
        state
            .as_ref()
            .and_then(|state| state.free_names.get(&address).cloned())
    }) {
        return names;
    }
    let mut seen = FxHashSet::default();
    let mut symbols = Vec::new();
    let mut pending = vec![call];
    while let Some(object) = pending.pop() {
        match object.kind() {
            SexpType::Symbol => {
                if object != missing_arg() && seen.insert(object.as_ptr() as usize) {
                    symbols.push(object);
                }
            }
            // A cell's tag is an argument or formal name, not a variable the call reads.
            SexpType::Language | SexpType::Pairlist => {
                pending.push(object.cdr());
                pending.push(object.car());
            }
            SexpType::Expression => {
                for index in 0..values::length(object) {
                    pending.push(values::list_elt(object, index));
                }
            }
            _ => {}
        }
    }
    let dots = symbols.iter().any(|&symbol| {
        let name = symbol_name(symbol);
        name == "..."
            || name.strip_prefix("..").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
    });
    let names = Rc::new(FreeNames { symbols, dots });
    crate::heap().add_global_root(call);
    STATE.with_borrow_mut(|state| {
        if let Some(state) = state.as_mut() {
            state.free_names.insert(address, names.clone());
        }
    });
    names
}

enum Inspection {
    Uncapturable(Reason),
    Capturable {
        variables: Vec<(Sexp, Sexp)>,
        imports: Vec<(Sexp, String)>,
        /// False when a variable's value could not be read (forcing its promise failed): the
        /// call is still capturable, but it is not sampled.
        complete: bool,
    },
}

/// Decides whether the call whose names are `free` can be replayed without the user's code,
/// and collects what a replay needs: the values of the variables it reads from user code and
/// the namespaces of the functions it reads from the user namespace's imports. The values it
/// collects are rooted in the current handle scope.
unsafe fn inspect(free: &FreeNames, sys_parent: Sexp) -> R<Inspection> {
    let env = caller_env(sys_parent)?;
    let top = dispatch::top_env(env);
    let user_top = is_user_top(top);
    let heap = crate::heap();
    let mut variables = Vec::new();
    let mut imports = Vec::new();
    let mut complete = true;
    for &symbol in &free.symbols {
        let mut scope = env;
        let mut found = None;
        while scope != top && scope != values::empty_env() {
            let frame = eval::resolved_env(scope)?;
            if let Some(cell) = heap.frame_cell(frame, symbol) {
                found = Some(cell);
                break;
            }
            scope = frame.enclosure();
        }
        if let Some(cell) = found {
            let Some(value) = binding_value(cell)? else {
                complete = false;
                continue;
            };
            if value == missing_arg() {
                continue;
            }
            if value.kind() == SexpType::Closure {
                let closure_env = value.tag();
                if !(closure_env == global_env()
                    || closure_env == base_env()
                    || closure_env == base_namespace()
                    || envir::is_namespace(closure_env))
                {
                    return Ok(Inspection::Uncapturable(Reason::LocalClosure));
                }
                // A function of the user's code reached through a variable is user code like
                // one named directly.
                if is_user_top(closure_env) {
                    return Ok(Inspection::Uncapturable(Reason::UserBinding));
                }
            }
            heap.root(value);
            variables.push((symbol, value));
            continue;
        }
        if !user_top {
            continue;
        }
        if let Some(cell) = heap.frame_cell(top, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return Ok(Inspection::Uncapturable(Reason::UserBinding));
            }
            let Some(value) = binding_value(cell)? else {
                complete = false;
                continue;
            };
            if value.kind() == SexpType::Closure {
                return Ok(Inspection::Uncapturable(Reason::UserBinding));
            }
            heap.root(value);
            variables.push((symbol, value));
            continue;
        }
        if top == global_env() {
            continue;
        }
        let imports_env = top.enclosure();
        if let Some(cell) = heap.frame_cell(imports_env, symbol) {
            let Some(value) = binding_value(cell)? else {
                complete = false;
                continue;
            };
            if value.kind() == SexpType::Closure {
                let owner = dispatch::top_env(value.tag());
                let namespace = if owner == global_env() {
                    GLOBAL_LABEL.to_owned()
                } else {
                    envir::namespace_name(owner).unwrap_or_else(|| "other".to_owned())
                };
                imports.push((symbol, namespace));
            } else {
                heap.root(value);
                variables.push((symbol, value));
            }
        }
    }
    Ok(Inspection::Capturable {
        variables,
        imports,
        complete,
    })
}

/// Whether `top` is a top-level environment of the user's code.
unsafe fn is_user_top(top: Sexp) -> bool {
    let known = STATE.with_borrow(|state| {
        state.as_ref().and_then(|state| {
            state
                .owners
                .get(&(top.as_ptr() as usize))
                .map(|owner| owner.1)
        })
    });
    match known {
        Some(user) => user,
        None => owner_of(top).1,
    }
}

/// The environment a call's arguments are evaluated in.
unsafe fn caller_env(sys_parent: Sexp) -> R<Sexp> {
    if sys_parent != nil() {
        return eval::resolved_env(sys_parent);
    }
    let owner = interp().slot_owner;
    if owner == crate::NO_SLOTS {
        return Ok(global_env());
    }
    let env = eval::frame_env(owner as usize, Reify::Site("boundary capture"))?;
    Ok(if env == nil() { global_env() } else { env })
}

/// The value bound in `cell`, its promise forced, or `None` when reading it raised an error.
/// A jump (an exiting handler, a restart) passes through: the code that established it is
/// waiting for it.
unsafe fn binding_value(cell: Sexp) -> R<Option<Sexp>> {
    let value = match eval::cell_value(cell) {
        Ok(value) => value,
        Err(Unwind::Error(_)) => return Ok(None),
        Err(unwind) => return Err(unwind),
    };
    if value.kind() != SexpType::Promise {
        return Ok(Some(value));
    }
    match eval::force_promise(value) {
        Ok(value) => Ok(Some(value)),
        Err(Unwind::Error(_)) => Ok(None),
        Err(unwind) => Err(unwind),
    }
}

/// The list a sample file holds before the call ran: `package`, `name`, `call`, `variables`,
/// `imports` and `seed`.
unsafe fn sample_inputs(
    owner: &str,
    name: &str,
    call: Sexp,
    variables: &[(Sexp, Sexp)],
    imports: &[(Sexp, String)],
) -> R<Sexp> {
    let names: Vec<String> = variables
        .iter()
        .map(|&(symbol, _)| symbol_name(symbol).into_owned())
        .collect();
    let values_list: Vec<Sexp> = variables.iter().map(|&(_, value)| value).collect();
    let variables = values::list_vector(&values_list)?;
    let names = values::string_vector(&names)?;
    values::set_attr(variables, values::symbols().names, names)?;
    let import_names: Vec<String> = imports
        .iter()
        .map(|(symbol, _)| symbol_name(*symbol).into_owned())
        .collect();
    let import_namespaces: Vec<&str> = imports
        .iter()
        .map(|(_, namespace)| namespace.as_str())
        .collect();
    let imports = values::string_vector(&import_namespaces)?;
    let import_names = values::string_vector(&import_names)?;
    values::set_attr(imports, values::symbols().names, import_names)?;
    let seed = eval::find_var_in_frame(values::symbols().dot_random_seed, global_env())?;
    let seed = if seed == values::unbound() || seed.kind() == SexpType::Promise {
        nil()
    } else {
        seed
    };
    values::named_list(&[
        ("package", values::scalar_string(owner)?),
        ("name", values::scalar_string(name)?),
        ("call", call),
        ("variables", variables),
        ("imports", imports),
        ("seed", seed),
    ])
}

/// `object` serialized as `saveRDS` writes it without compression: XDR, version 3.
unsafe fn serialize(object: Sexp) -> R<Vec<u8>> {
    // The object sits in the call as a constant: a list evaluates to itself.
    let inner = values::call(
        values::install("serialize")?,
        &[object, nil(), values::scalar_integer(0)?, nil(), nil()],
    )?;
    let outer = values::call(values::install(".Internal")?, &[inner])?;
    let bytes = eval::eval(outer, base_env())?;
    if bytes.kind() != SexpType::Raw {
        return Err(error("serialize did not return a raw vector"));
    }
    Ok(bytes.slice::<u8>().to_vec())
}

/// Completes the sample of a call that took `seconds` and returned `value` (`None` when it
/// ended in an unwind), and writes it to its reservoir slot's file.
unsafe fn write_sample(
    key: usize,
    sample: Pending,
    seconds: Duration,
    value: Option<Sexp>,
) -> R<()> {
    let max = max_bytes();
    let inputs = crate::serialize::unserialize(&sample.inputs, nil())?;
    crate::heap().root(inputs);
    let kept = match value {
        Some(value) => {
            let wrapped = values::list_vector(&[value])?;
            serialize(wrapped)?.len() <= max
        }
        None => false,
    };
    let names = [
        "package",
        "name",
        "call",
        "variables",
        "imports",
        "seed",
        "seconds",
        "threw",
        "value",
        "value_kept",
    ];
    let list = values::alloc(SexpType::List, names.len())?;
    for index in 0..6 {
        values::set_elt(list, index, values::list_elt(inputs, index));
    }
    values::set_elt(list, 6, values::scalar_real(seconds.as_secs_f64())?);
    values::set_elt(list, 7, values::scalar_bool(value.is_none())?);
    values::set_elt(
        list,
        8,
        match value {
            Some(value) if kept => value,
            _ => nil(),
        },
    );
    values::set_elt(list, 9, values::scalar_bool(kept)?);
    let names = values::string_vector(&names)?;
    values::set_attr(list, values::symbols().names, names)?;
    let bytes = serialize(list)?;
    let (directory, file) = STATE
        .with_borrow_mut(|state| {
            let state = state.as_mut()?;
            let file = match state.keys.get(key)?.slots.get(sample.slot)? {
                Some(file) => *file,
                None => {
                    let file = state.files.len();
                    state.files.push(SampleFile {
                        key,
                        seconds: 0.0,
                        threw: false,
                        bytes: 0,
                    });
                    if let Some(slot) = state.keys.get_mut(key)?.slots.get_mut(sample.slot) {
                        *slot = Some(file);
                    }
                    file
                }
            };
            if let Some(record) = state.files.get_mut(file) {
                record.seconds = seconds.as_secs_f64();
                record.threw = value.is_none();
                record.bytes = bytes.len();
            }
            Some((state.directory.clone(), file))
        })
        .ok_or_else(|| error("the sample's key is gone"))?;
    let path = directory.join("samples").join(format!("{file}.rds"));
    std::fs::write(&path, &bytes)
        .map_err(|failure| error(format!("cannot write '{}': {failure}", path.display())))
}

/// Stops recording and writes `entries.tsv`, `samples.tsv` and `summary.tsv`.
pub fn finish() -> R<()> {
    let Some(mut state) = STATE.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    activity::set_hook(activity::CAPTURE, false);
    if !crate::profile_summary::sampling() {
        activity::set_enabled(false);
    }
    let Some(started) = state.started else {
        return Ok(());
    };
    if state.process != std::process::id() {
        return Ok(());
    }
    let now = Instant::now();
    // The records still open are the root user record and whatever an exit to top level left.
    while let Some(record) = state.stack.pop() {
        let inclusive = now.duration_since(record.started);
        if let Some(parent) = state.stack.last_mut() {
            parent.nested += inclusive;
        }
        if let Role::User = record.role {
            state.user += inclusive.saturating_sub(record.nested);
        }
    }
    let total = now.duration_since(started);
    let written = write_outputs(&state, total);
    if let Some(failure) = state.failure.take() {
        return Err(error(format!("boundary capture: {failure}")));
    }
    written.map_err(|failure| {
        error(format!(
            "cannot write the boundary capture to '{}': {failure}",
            state.directory.display()
        ))
    })
}

fn write_outputs(state: &Capture, total: Duration) -> std::io::Result<()> {
    std::fs::create_dir_all(&state.directory)?;
    let mut keys: Vec<&KeyStats> = state.keys.iter().collect();
    keys.sort_by(|left, right| {
        let weight = |stats: &KeyStats| stats.seconds + stats.uncapturable_seconds;
        weight(right)
            .cmp(&weight(left))
            .then_with(|| left.package.cmp(&right.package))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut entries = String::from(
        "package\tname\tcalls\tseconds\tsamples\tuncapturable_calls\tuncapturable_seconds\tdots\tlocal_closure\tuser_binding\toversize\treentered\tcaller_frame\n",
    );
    for stats in keys {
        let samples = stats.slots.iter().filter(|slot| slot.is_some()).count();
        entries.push_str(&format!(
            "{}\t{}\t{}\t{:.6}\t{samples}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            stats.package,
            stats.name,
            stats.calls,
            stats.seconds.as_secs_f64(),
            stats.uncapturable_calls,
            stats.uncapturable_seconds.as_secs_f64(),
            stats.dots,
            stats.local_closure,
            stats.user_binding,
            stats.oversize,
            stats.reentered,
            stats.caller_frame,
        ));
    }
    std::fs::write(state.directory.join("entries.tsv"), entries)?;
    let mut samples = String::from("file\tpackage\tname\tseconds\tthrew\tbytes\n");
    for (file, record) in state.files.iter().enumerate() {
        let Some(stats) = state.keys.get(record.key) else {
            continue;
        };
        samples.push_str(&format!(
            "samples/{file}.rds\t{}\t{}\t{:.6}\t{}\t{}\n",
            stats.package,
            stats.name,
            record.seconds,
            if record.threw { "TRUE" } else { "FALSE" },
            record.bytes,
        ));
    }
    std::fs::write(state.directory.join("samples.tsv"), samples)?;
    let seconds = |duration: Duration| duration.as_secs_f64();
    let outside = total
        .saturating_sub(state.user)
        .saturating_sub(state.covered)
        .saturating_sub(state.uncapturable);
    let summary = format!(
        "key\tvalue\ntotal_seconds\t{:.6}\nuser_seconds\t{:.6}\ncovered_seconds\t{:.6}\nuncapturable_seconds\t{:.6}\noutside_seconds\t{:.6}\nboundary_calls\t{}\nkeys\t{}\n",
        seconds(total),
        seconds(state.user),
        seconds(state.covered),
        seconds(state.uncapturable),
        seconds(outside),
        state.boundary_calls,
        state.keys.len(),
    );
    std::fs::write(state.directory.join("summary.tsv"), summary)
}
