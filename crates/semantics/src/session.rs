//! A console session as a sequence of command files. Each accepted command is a script file of
//! its own that sees the bindings, type definitions and `# typing:` directive of the commands
//! before it, so checking a command costs time in proportion to the command, not the session.
//!
//! What a command contributes is recorded in a log outside salsa's tracking. A command asks the
//! log only about commands with a smaller index, and the log only grows past a command or drops
//! commands after it, so every answer a command reads stays fixed for as long as that command is
//! in the session. The one exception is a read inside a function body, which runs when the
//! function is called and so sees the session's latest binder of the name, as R's global
//! environment does: those reads go through a salsa input per name ([`SessionBinder`]), which a
//! later command that rebinds the name sets, invalidating exactly the functions that read it.

use {
    crate::{
        Db, DocumentKind, Item, RootDatabase, SiteRole, SourceFile, TypingMode, annotations,
        diagnostics::{file_global_variable_declarations, super_globals},
        file_binders, file_syntax_sites, file_type_definitions, file_typing_directives,
        hir::Symbol,
        item_naming, item_tree, types,
    },
    rustc_hash::FxHashMap,
    salsa::Setter,
    std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

/// The log of the session's commands, shared by every handle of the database.
#[salsa::input(singleton)]
pub struct ConsoleSession {
    #[returns(ref)]
    log: Arc<RwLock<Log>>,
}

/// The index of the session's latest command that binds one name, for the reads of that name
/// inside function bodies.
#[salsa::input]
struct SessionBinder {
    latest: Option<u32>,
}

#[derive(Default)]
pub struct Log {
    commands: Vec<Command>,
    names: FxHashMap<Symbol, NameHistory>,
    /// The commands that define each type name, in order.
    types: FxHashMap<Symbol, Vec<u32>>,
    /// The first command that declares each name a global without binding it at the top level:
    /// a function's `<<-` or a `globalVariables()` call.
    declared: FxHashMap<Symbol, u32>,
    /// Each command with a `# typing:` directive, and the mode it sets.
    typing: Vec<(u32, TypingMode)>,
    /// The first command written with ry syntax.
    ry_values_from: Option<u32>,
}

struct Command {
    file: SourceFile,
    contributions: Contributions,
}

#[derive(Default)]
struct NameHistory {
    /// The commands that bind the name at the top level, in order.
    binders: Vec<u32>,
    /// The commands that read the name inside a function body, in order.
    deferred_readers: Vec<u32>,
    /// Exists once some command reads the name inside a function body.
    latest: Option<SessionBinder>,
}

/// What one command adds to the session, read from its analysis.
#[derive(Debug, Default, Clone)]
pub struct Contributions {
    binds: Vec<Symbol>,
    deferred_reads: Vec<Symbol>,
    types: Vec<Symbol>,
    declared: Vec<Symbol>,
    typing: Option<TypingMode>,
    ry_syntax: bool,
}

/// A new command file, next in the session. It joins the session only through [`commit`], so a
/// command strict mode refuses is checked against the session and then forgotten.
pub fn command_file(db: &RootDatabase, text: String) -> SourceFile {
    let index = match ConsoleSession::try_get(db) {
        Some(session) => read(session.log(db)).commands.len(),
        None => {
            ConsoleSession::new(db, Arc::default());
            0
        }
    };
    let index = u32::try_from(index).expect("a session holds fewer than u32::MAX commands");
    SourceFile::builder(text, DocumentKind::Script)
        .command(Some(index))
        .new(db)
}

/// How many commands the session holds.
pub fn len(db: &dyn Db) -> usize {
    ConsoleSession::try_get(db).map_or(0, |session| read(session.log(db)).commands.len())
}

/// The session's command files, in order.
pub fn command_files(db: &dyn Db) -> Vec<SourceFile> {
    ConsoleSession::try_get(db).map_or_else(Vec::new, |session| {
        read(session.log(db))
            .commands
            .iter()
            .map(|command| command.file)
            .collect()
    })
}

/// What `file` would add to the session. Before the file's own check, so that its function
/// bodies' reads of a name later commands rebind depend on that name's [`SessionBinder`],
/// which this creates for every name they read.
pub fn prepare(db: &dyn Db, file: SourceFile) -> Contributions {
    let mut binds: Vec<Symbol> = file_binders(db, file)
        .iter()
        .map(|&(name, _, _)| name)
        .collect();
    binds.dedup();
    let mut deferred_reads = Vec::new();
    for &item in item_tree(db, file) {
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        let extras = naming.extras();
        deferred_reads.extend(
            naming
                .non_locals
                .iter()
                .filter(|(expression, _)| naming.deferred_non_locals.contains(expression))
                .map(|(_, &name)| name),
        );
        deferred_reads.extend(
            naming
                .quiet_reads
                .iter()
                .filter(|(expression, _)| extras.deferred_quiet_reads.contains(expression))
                .map(|(_, &name)| name),
        );
        deferred_reads.extend(extras.quiet_operator_reads.iter().copied());
    }
    deferred_reads.sort_unstable();
    deferred_reads.dedup();
    let mut types: Vec<Symbol> = file_type_definitions(db, file)
        .iter()
        .map(|definition| definition.name.symbol())
        .collect();
    types.sort_unstable();
    types.dedup();
    let mut declared: Vec<Symbol> = super_globals(db, file)
        .iter()
        .copied()
        .chain(
            file_global_variable_declarations(db, file)
                .iter()
                .map(|name| Symbol::new(name)),
        )
        .collect();
    declared.sort_unstable();
    declared.dedup();
    let sites = file_syntax_sites(db, file);
    let contributions = Contributions {
        binds,
        deferred_reads,
        types,
        declared,
        typing: file_typing_directives(db, file).0,
        ry_syntax: sites.any(SiteRole::RySyntax)
            || sites.any(SiteRole::InlineSite)
            || sites.any(SiteRole::InlineAnnotation),
    };
    if let Some(session) = ConsoleSession::try_get(db) {
        let mut log = write(session.log(db));
        for &name in &contributions.deferred_reads {
            let history = log.names.entry(name).or_default();
            if history.latest.is_none() {
                history.latest = Some(SessionBinder::new(db, history.binders.last().copied()));
            }
        }
    }
    contributions
}

/// Adds `file`, which [`command_file`] made last, to the session, and returns the earlier
/// commands whose function bodies read a name it binds: their checks may change.
pub fn commit(
    db: &mut RootDatabase,
    file: SourceFile,
    contributions: Contributions,
) -> Vec<SourceFile> {
    let Some(session) = ConsoleSession::try_get(db) else {
        return Vec::new();
    };
    let log = session.log(db).clone();
    let mut log = write(&log);
    let index =
        u32::try_from(log.commands.len()).expect("a session holds fewer than u32::MAX commands");
    debug_assert_eq!(*file.command(db), Some(index));
    let mut readers: Vec<u32> = Vec::new();
    let mut changed = Vec::new();
    for &name in &contributions.binds {
        let history = log.names.entry(name).or_default();
        history.binders.push(index);
        readers.extend(&history.deferred_readers);
        if let Some(latest) = history.latest {
            changed.push(latest);
        }
    }
    for &name in &contributions.deferred_reads {
        let history = log.names.entry(name).or_default();
        history.deferred_readers.push(index);
    }
    for &name in &contributions.types {
        log.types.entry(name).or_default().push(index);
    }
    for &name in &contributions.declared {
        log.declared.entry(name).or_insert(index);
    }
    if let Some(mode) = contributions.typing {
        log.typing.push((index, mode));
    }
    if contributions.ry_syntax && log.ry_values_from.is_none() {
        log.ry_values_from = Some(index);
    }
    log.commands.push(Command {
        file,
        contributions,
    });
    readers.sort_unstable();
    readers.dedup();
    let readers = readers
        .into_iter()
        .filter_map(|reader| {
            log.commands
                .get(reader as usize)
                .map(|command| command.file)
        })
        .collect();
    drop(log);
    for latest in changed {
        latest.set_latest(db).to(Some(index));
    }
    readers
}

/// Forgets the commands from `len` on, as if they had never been typed.
pub fn truncate(db: &mut RootDatabase, len: usize) {
    let Some(session) = ConsoleSession::try_get(db) else {
        return;
    };
    let log = session.log(db).clone();
    let mut log = write(&log);
    let mut changed: Vec<(SessionBinder, Option<u32>)> = Vec::new();
    while log.commands.len() > len {
        let Some(command) = log.commands.pop() else {
            break;
        };
        let index = u32::try_from(log.commands.len())
            .expect("a session holds fewer than u32::MAX commands");
        let contributions = command.contributions;
        for name in &contributions.binds {
            if let Some(history) = log.names.get_mut(name) {
                history.binders.pop_if(|&mut binder| binder == index);
                if let Some(latest) = history.latest {
                    changed.push((latest, history.binders.last().copied()));
                }
            }
        }
        for name in &contributions.deferred_reads {
            if let Some(history) = log.names.get_mut(name) {
                history
                    .deferred_readers
                    .pop_if(|&mut reader| reader == index);
            }
        }
        for name in &contributions.types {
            if let Some(commands) = log.types.get_mut(name) {
                commands.pop_if(|&mut command| command == index);
            }
        }
        for name in &contributions.declared {
            if log.declared.get(name) == Some(&index) {
                log.declared.remove(name);
            }
        }
        log.typing.pop_if(|&mut (command, _)| command == index);
        if log.ry_values_from == Some(index) {
            log.ry_values_from = None;
        }
    }
    drop(log);
    // A name several dropped commands bind is listed once per command; the last entry is the
    // binder that remains.
    let mut latest: FxHashMap<SessionBinder, Option<u32>> = FxHashMap::default();
    for (binder, remaining) in changed {
        latest.insert(binder, remaining);
    }
    for (binder, remaining) in latest {
        if *binder.latest(db) != remaining {
            binder.set_latest(db).to(remaining);
        }
    }
}

/// The binder of `name` that a read in command `command` sees past its own file's binders.
/// `own` is the binder the command's own file gives, if any. A read inside a function body
/// (`deferred`) sees a later command's binder over its own file's.
pub(crate) fn binder<'db>(
    db: &'db dyn Db,
    command: u32,
    name: Symbol,
    deferred: bool,
    own: Option<Item<'db>>,
) -> Option<Item<'db>> {
    let Some(session) = ConsoleSession::try_get(db) else {
        return own;
    };
    let log = read(session.log(db));
    let Some(history) = log.names.get(&name) else {
        return own;
    };
    // Only a deferred read depends on the latest binder: an immediate one sees the commands
    // before it, which never change, so a later rebinding leaves it valid.
    let later = history
        .latest
        .filter(|_| deferred)
        .and_then(|latest| *latest.latest(db));
    let at = match later {
        Some(at) if at > command => at,
        _ if own.is_some() => return own,
        _ => {
            let before = history.binders.partition_point(|&at| at < command);
            *history.binders[..before].last()?
        }
    };
    let file = log.commands.get(at as usize)?.file;
    drop(log);
    last_binder(db, file, name)
}

/// Whether a read of `name` in command `command` that its own file does not bind resolves in
/// the session.
pub(crate) fn resolves(db: &dyn Db, file: SourceFile, name: Symbol, deferred: bool) -> bool {
    let Some(command) = *file.command(db) else {
        return false;
    };
    let Some(session) = ConsoleSession::try_get(db) else {
        return false;
    };
    let log = read(session.log(db));
    if log.declared.get(&name).is_some_and(|&at| at < command) {
        return true;
    }
    let Some(history) = log.names.get(&name) else {
        return false;
    };
    history.binders.first().is_some_and(|&at| at < command)
        || (deferred
            && history
                .latest
                .is_some_and(|latest| latest.latest(db).is_some()))
}

/// The definition of the type `name` that command `command` sees from earlier commands.
pub(crate) fn type_definition<'db>(
    db: &'db dyn Db,
    command: u32,
    name: types::Name<'db>,
) -> Option<&'db annotations::NamedDefinition<'db>> {
    let session = ConsoleSession::try_get(db)?;
    let log = read(session.log(db));
    let commands = log.types.get(&name.symbol())?;
    let before = commands.partition_point(|&at| at < command);
    let file = log.commands.get(*commands[..before].last()? as usize)?.file;
    drop(log);
    file_type_definitions(db, file)
        .iter()
        .rev()
        .find(|definition| definition.name == name)
}

/// The `# typing:` mode a command without a directive of its own inherits from earlier ones.
pub(crate) fn typing_mode(db: &dyn Db, file: SourceFile) -> Option<TypingMode> {
    let command = (*file.command(db))?;
    let session = ConsoleSession::try_get(db)?;
    let log = read(session.log(db));
    let before = log.typing.partition_point(|&(at, _)| at < command);
    log.typing[..before].last().map(|&(_, mode)| mode)
}

/// Whether an earlier command was written with ry syntax, which makes a literal bound outside
/// any function a ry value for the rest of the session.
pub(crate) fn holds_ry_values(db: &dyn Db, file: SourceFile) -> bool {
    let Some(command) = *file.command(db) else {
        return false;
    };
    ConsoleSession::try_get(db).is_some_and(|session| {
        read(session.log(db))
            .ry_values_from
            .is_some_and(|at| at < command)
    })
}

/// The last binder of `name` in `file`, a command that ran whole before any later read.
fn last_binder<'db>(db: &'db dyn Db, file: SourceFile, name: Symbol) -> Option<Item<'db>> {
    let binders = file_binders(db, file);
    // Sorted by name and then position, so the name's last entry is its last binder.
    let end = binders.partition_point(|&(binder, _, _)| binder.index() <= name.index());
    binders[..end]
        .last()
        .filter(|&&(binder, _, _)| binder == name)
        .map(|&(_, _, item)| item)
}

fn read(log: &RwLock<Log>) -> RwLockReadGuard<'_, Log> {
    log.read().unwrap_or_else(PoisonError::into_inner)
}

fn write(log: &RwLock<Log>) -> RwLockWriteGuard<'_, Log> {
    log.write().unwrap_or_else(PoisonError::into_inner)
}
