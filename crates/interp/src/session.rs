//! Starting an R session and running scripts. Startup follows GNU R's: the system `Renviron`,
//! the C-level options, base R's own loader (`library/base/R/base`, which lazy-loads base from
//! its database), the system profile, `methods`, then `.First.sys` for the default packages.
//! Base R's R code is read from the user's GNU R installation, never vendored (see
//! `decisions/docs-process.md`, "Licensing").

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        conditions, eval, interp, print, scope,
        unwind::{Jump, R, Unwind, error},
        values::{self, base_env, base_namespace, global_env, length, nil},
        write_console,
    },
    heap::object::{Sexp, SexpType, gp},
    std::{cell::RefCell, rc::Rc},
};

pub const BUILTINS: &[BuiltinDef] = &[internal("mkUnbound", 1, do_mk_unbound).invisible()];

/// How a session starts.
pub struct StartOptions {
    /// `commandArgs()`.
    pub command_args: Vec<String>,
    /// Sources the site profile (`R_PROFILE`, else `R_HOME/etc/Rprofile.site`), which
    /// `--vanilla` and `--no-site-file` turn off.
    pub site_profile: bool,
    /// Sources the user profile (`R_PROFILE_USER`, else `./.Rprofile`, else `~/.Rprofile`) into
    /// the global environment, which `--vanilla` and `--no-init-file` turn off.
    pub user_profile: bool,
    /// Reads the site and user environment files (`R_ENVIRON`, else
    /// `R_HOME/etc/Renviron.site`; `R_ENVIRON_USER`, else `./.Renviron`, else `~/.Renviron`),
    /// which `--vanilla` and `--no-environ` turn off.
    pub environ: bool,
    /// `interactive()`, which also makes `keep.source` default to `TRUE` (base R's system
    /// profile sets it from `interactive()`).
    pub interactive: bool,
    /// Lets a start that [`crate::image::applies`] to load the start-up image of its R home;
    /// `None` always starts the slow way.
    pub image: Option<crate::image::ImageHost>,
    /// The runtime build's identity, which stamps the code images an install writes and picks
    /// the ones a load reads (`crate::code_image`); empty turns code images off.
    pub build: &'static str,
}

/// Starts the session. Errors here are fatal: base R did not load.
pub unsafe fn start(options: StartOptions) -> R<()> {
    crate::profile_summary::start_from_environment();
    crate::capture::start_from_environment();
    interp().command_args = options.command_args;
    interp().interactive = options.interactive;
    interp().code_image.build = options.build;
    let locale_failures = prepare_process(options.environ);
    let home = crate::system::r_home();
    let site_profile = options.site_profile.then(site_profile_path).flatten();
    let user_profile = options.user_profile.then(user_profile_path).flatten();
    if let Some(host) = &options.image
        && crate::image::applies(options.interactive)
    {
        match crate::image::load(host, &home) {
            Ok(()) => {
                let started = start_from_image(&home, site_profile, user_profile, &locale_failures);
                crate::image::touch::finish_start_up();
                return started;
            }
            Err(crate::image::LoadError::Absent(reason)) => crate::image::report_unused(&reason),
            Err(crate::image::LoadError::Untouched(message)) => {
                write_console(
                    format!("ry: starting without the start-up image: {message}\n").as_bytes(),
                    true,
                );
                crate::image::report_unused(&message);
            }
            Err(crate::image::LoadError::Partial(message)) => {
                return Err(error(format!(
                    "the start-up image did not load: {message}; set RY_IMAGE=0 to start \
                     without it"
                )));
            }
        }
    }
    start_slowly(&home, site_profile, user_profile, &locale_failures)?;
    Ok(())
}

/// Starts a session the slow way in the process's environment and writes its start-up image to
/// `path`, as the base tree's build does. Fails when start-up warned, since a start from the
/// image would not print the warning. `layout` is the directory of the touch profile that lays
/// the image out ([`crate::image::write`]).
pub unsafe fn write_start_image(
    host: crate::image::ImageHost,
    path: &std::path::Path,
    layout: Option<&std::path::Path>,
) -> R<()> {
    interp().interactive = false;
    let locale_failures = prepare_process(false);
    let home = crate::system::r_home();
    let before = crate::image::before(&host).map_err(error)?;
    if start_slowly(&home, None, None, &locale_failures)? {
        return Err(error("start-up warned, so it cannot be imaged"));
    }
    interp().globals.host_environment = (host.environment)()?;
    crate::image::write(&host, &home, path, layout, &before).map_err(error)
}

/// GNU R's start-up greeting, which its `R` prints before the profiles run unless `--quiet` or
/// `--no-echo` is given.
pub fn greeting() -> String {
    let version = crate::system::r_version();
    format!(
        "\nR version {} ({}) -- \"{}\"\nCopyright (C) {} The R Foundation for Statistical \
         Computing\nPlatform: {}\n\n\
         R is free software and comes with ABSOLUTELY NO WARRANTY.\n\
         You are welcome to redistribute it under certain conditions.\n\
         Type 'license()' or 'licence()' for distribution details.\n\n\
         R is a collaborative project with many contributors.\n\
         Type 'contributors()' for more information and\n\
         'citation()' on how to cite R or R packages in publications.\n\n\
         Type 'demo()' for some demos, 'help()' for on-line help, or\n\
         'help.start()' for an HTML browser interface to help.\n\
         Type 'q()' to quit R.\n\n",
        version.number(),
        version.date,
        version.nickname,
        version.date.get(0..4).unwrap_or(""),
        crate::system::platform_triple(),
    )
}

/// The process state every start sets before R code runs: the system `Renviron`, with
/// `environ` the site and user environment files, and the locale, which those files may set.
/// Returns the locale categories that fell back to "C", which start-up warns about after every
/// other start-up warning, as GNU R does.
unsafe fn prepare_process(environ: bool) -> Vec<&'static str> {
    read_renviron();
    if environ {
        read_site_and_user_renviron();
    }
    crate::system::init_locale()
}

/// The start-up steps that read the process environment, run after the image loads: the
/// C-level options and the variables that describe this machine, base R's profile, the
/// `.onLoad` hooks of the default packages that set options from the environment, and the site
/// and user profiles. Every option is removed first, so that the hooks, which set only options
/// not yet set, see the order GNU R's start-up gives them.
unsafe fn start_from_image(
    home: &str,
    site_profile: Option<String>,
    user_profile: Option<String>,
    locale_failures: &[&str],
) -> R<()> {
    // The image was written by a start that had locked them.
    lock_base_bindings(false)?;
    crate::base_fast_paths::register_native_entries(base_namespace())?;
    crate::base_fast_paths::register_routine_entries()?;
    mark_base_closures()?;
    crate::envir::clear_options()?;
    define_process_variables()?;
    source_file(&format!("{home}/library/base/R/Rprofile"), base_env())?;
    // The profile attaches a new `Autoloads` at position 2, in front of the default packages;
    // the image's own sits behind them, where the slow start put it.
    eval_text(
        "detach(pos = 2L); .AutoloadEnv <- as.environment(\"Autoloads\")",
        base_env(),
    )?;
    lock_base_bindings(true)?;
    // utils' hook also assigns `osVersion`, this machine's, into its sealed namespace.
    eval_text(
        "local(for (name in c(\"utils\", \"grDevices\", \"stats\")) { \
         namespace <- asNamespace(name); \
         if (name == \"utils\") unlockBinding(\"osVersion\", namespace); \
         namespace$.onLoad(dirname(getNamespaceInfo(name, \"path\")), name); \
         if (name == \"utils\") lockBinding(\"osVersion\", namespace) })",
        base_env(),
    )?;
    if site_profile.is_none() && user_profile.is_none() {
        remove_library_site()?;
    } else {
        // GNU R's order: the profiles and `.First()` run with only `methods`, `Autoloads` and base
        // attached, then `.First.sys()` attaches the default packages in front of what they
        // attached (`decisions/heap-gc.md`, "Profiles run over the start-up image").
        let methods = crate::envir::as_environment(values::scalar_string("package:methods")?)?;
        let held: Vec<Sexp> = crate::envir::search_path()
            .skip(1)
            .take_while(|&env| env != methods)
            .collect();
        // `attach()` would copy a package environment, so an entry goes back by relinking.
        let insert_after_global = |env: Sexp| {
            crate::heap().set_enclosure(env, global_env().enclosure());
            crate::heap().set_enclosure(global_env(), env);
            if !interp().globals.keep.contains(&env) {
                interp().globals.keep.push(env);
            }
        };
        let image_packages = option_strings("defaultPackages");
        let default_package = |env: Sexp| {
            crate::envir::search_name(env)
                .strip_prefix("package:")
                .is_some_and(|package| image_packages.iter().any(|held| held == package))
        };
        let own: Vec<Sexp> = held
            .iter()
            .copied()
            .filter(|&env| !default_package(env))
            .collect();
        // `package:ry` stays reachable in front of base, where a slow start has it, because a
        // test worker loads its package from the profile.
        let autoloads = methods.enclosure();
        if let Some(&loader) = own
            .iter()
            .find(|&&env| crate::envir::search_name(env) == "package:ry")
        {
            crate::heap().set_enclosure(loader, base_env());
            crate::heap().set_enclosure(autoloads, loader);
        }
        crate::heap().set_enclosure(global_env(), methods);
        if let Some(site) = site_profile {
            source_file(&site, base_env())?;
        }
        remove_library_site()?;
        if let Some(user) = user_profile {
            source_file(&user, global_env())?;
        }
        eval_text(CALL_FIRST, global_env())?;
        let mut restored = vec![false; held.len()];
        for package in &option_strings("defaultPackages") {
            let name = format!("package:{package}");
            if crate::envir::search_path().any(|env| crate::envir::search_name(env) == name) {
                continue;
            }
            scope(|| -> R<()> {
                let pkg_scope = values::new_env(base_env())?;
                crate::heap().root(pkg_scope);
                eval::define(
                    values::install("pkg")?,
                    values::scalar_string(package)?,
                    pkg_scope,
                )?;
                let held_entry = held
                    .iter()
                    .position(|&env| crate::envir::search_name(env) == name);
                // An attach hook a profile set runs only through `library()`.
                let hooked = values::is_true(eval_text(
                    "length(getHook(packageEvent(pkg, \"attach\"))) > 0L",
                    pkg_scope,
                )?);
                match held_entry {
                    Some(index) if !hooked => {
                        insert_after_global(held[index]);
                        restored[index] = true;
                    }
                    _ => {
                        eval_text(FIRST_SYS_PACKAGE, pkg_scope)?;
                    }
                }
                Ok(())
            })?;
        }
        // ry's own entries are attached after `.First.sys()`, so they move to the front.
        for &env in own.iter().rev() {
            let mut previous = global_env();
            while previous != base_env() && previous.enclosure() != env {
                previous = previous.enclosure();
            }
            if previous != base_env() {
                crate::heap().set_enclosure(previous, env.enclosure());
            }
            insert_after_global(env);
        }
        for (index, &env) in held.iter().enumerate() {
            if own.contains(&env) {
                restored[index] = true;
            }
        }
        let dropped: Vec<Sexp> = held
            .iter()
            .zip(&restored)
            .filter(|&(_, &restored)| !restored)
            .map(|(&env, _)| env)
            .collect();
        interp().globals.keep.retain(|kept| !dropped.contains(kept));
    }
    crate::profile_summary::mark_ready();
    crate::capture::mark_ready();
    for name in locale_failures {
        conditions::warning_call(nil(), format!("Setting {name} failed, using \"C\""))?;
    }
    conditions::print_warnings(conditions::DURING_STARTUP)
}

/// The packages ry attaches itself rather than from a library.
const OWN_PACKAGES: [&str; 2] = ["std", "ry"];

/// The strings of the option `name`, or none when it is not a character vector.
unsafe fn option_strings(name: &str) -> Vec<String> {
    let value = crate::envir::get_option(name);
    if value.kind() != SexpType::String {
        return Vec::new();
    }
    (0..length(value))
        .filter_map(|index| values::string_at(value, index).map(|text| text.into_owned()))
        .collect()
}

/// The body of base R's `.First.sys()` for the one package `pkg`.
const FIRST_SYS_PACKAGE: &str = "\
if (!require(pkg, quietly = !getOption(\"verbose\", FALSE), warn.conflicts = FALSE, \
             character.only = TRUE)) \
    warning(gettextf(\"package %s in options(\\\"defaultPackages\\\") was not found\", sQuote(pkg)), \
            call. = FALSE, domain = NA)";

/// Calls `.First()` when the global environment or the search path behind it binds a closure
/// of that name, as GNU R does after the user profile.
const CALL_FIRST: &str =
    "if (typeof(get0(\".First\", envir = globalenv())) == \"closure\") .First()";

/// The options and base variables that depend on this process: the C-level options,
/// `.Machine`, `.Platform` and the graphics device variables.
unsafe fn define_process_variables() -> R<()> {
    crate::envir::install_default_options()?;
    let machine = crate::system::machine()?;
    eval::define(values::install(".Machine")?, machine, base_env())?;
    let platform = crate::system::platform()?;
    eval::define(values::install(".Platform")?, platform, base_env())?;
    (interp().extensions.graphics.define_device_variables)()
}

/// Locks, or with `false` unlocks, every binding of the base environment but the graphics
/// device variables, as GNU R does once base's profile has run: `T <<- 0` in a function fails
/// with "cannot change value of locked binding for 'T'".
unsafe fn lock_base_bindings(lock: bool) -> R<()> {
    let heap = crate::heap();
    let open = [values::install(".Device")?, values::install(".Devices")?];
    for symbol in heap.bound_symbols(base_env()) {
        if let Some(cell) = heap.frame_cell(base_env(), symbol) {
            cell.set_gp_bits(gp::BINDING_LOCKED, lock && !open.contains(&symbol));
        }
    }
    Ok(())
}

/// The base profile binds `.Library.site` in the global environment too, so that a site profile
/// can assign it directly; GNU R's global environment no longer holds it afterwards.
unsafe fn remove_library_site() -> R<()> {
    eval_text(
        "if (exists(\".Library.site\", envir = globalenv(), inherits = FALSE)) \
         rm(\".Library.site\", envir = globalenv())",
        global_env(),
    )?;
    Ok(())
}

/// The site profile to source: `R_PROFILE`, else `R_HOME/etc/Rprofile.site`, if it exists.
fn site_profile_path() -> Option<String> {
    let site = std::env::var("R_PROFILE")
        .ok()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| format!("{}/etc/Rprofile.site", crate::system::r_home()));
    std::path::Path::new(&site).exists().then_some(site)
}

/// The user profile to source: `R_PROFILE_USER`, else `./.Rprofile`, else `~/.Rprofile`, if it
/// exists. A set `R_PROFILE_USER` is the only candidate, even when the file is missing (callr
/// points it at its own profile).
fn user_profile_path() -> Option<String> {
    let user = match std::env::var("R_PROFILE_USER")
        .ok()
        .filter(|path| !path.is_empty())
    {
        Some(path) => Some(crate::connections::expand_path(&path)),
        None => [
            ".Rprofile".to_owned(),
            crate::connections::expand_path("~/.Rprofile"),
        ]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists()),
    };
    user.filter(|path| std::path::Path::new(path).exists())
}

/// Start-up from the C-level options to the default packages, as GNU R runs it. Returns whether
/// start-up warned.
unsafe fn start_slowly(
    home: &str,
    site_profile: Option<String>,
    user_profile: Option<String>,
    locale_failures: &[&str],
) -> R<bool> {
    define_process_variables()?;
    eval::define(
        values::install(".BaseNamespaceEnv")?,
        base_namespace(),
        base_env(),
    )?;
    eval::define(
        values::install("base")?,
        base_namespace(),
        crate::heap().known().namespace_registry,
    )?;
    let version = crate::system::r_version();
    let (major, minor) = crate::system::OLDEST_SUPPORTED_R;
    if (version.major, version.minor) < (major, minor) {
        return Err(crate::unwind::error(format!(
            "ry's runtime needs base R {major}.{minor} or later, but {home} holds R {}; \
             install a newer GNU R, or set R_HOME to one",
            version.number()
        )));
    }
    source_base(home)?;
    mark_base_closures()?;
    install_register_s3_methods()?;
    source_file(&format!("{home}/library/base/R/Rprofile"), base_env())?;
    lock_base_bindings(true)?;
    eval_text(".OptRequireMethods()", global_env())?;
    // GNU R's order: the site profile, then the user profile, both after `methods` loads.
    if let Some(site) = site_profile {
        source_file(&site, base_env())?;
    }
    remove_library_site()?;
    if let Some(user) = user_profile {
        source_file(&user, global_env())?;
    }
    eval_text(CALL_FIRST, global_env())?;
    // `.First.sys()` for each default package, except ry's own, which are attached next: a
    // start from the image finds them on the search path, so neither start warns for them.
    for package in option_strings("defaultPackages") {
        if !OWN_PACKAGES.contains(&package.as_str()) {
            scope(|| -> R<()> {
                let pkg_scope = values::new_env(base_env())?;
                crate::heap().root(pkg_scope);
                eval::define(
                    values::install("pkg")?,
                    values::scalar_string(&package)?,
                    pkg_scope,
                )?;
                eval_text(FIRST_SYS_PACKAGE, pkg_scope)?;
                Ok(())
            })?;
        }
    }
    install_os_version()?;
    crate::standard_library::attach_package()?;
    for name in locale_failures {
        conditions::warning_call(nil(), format!("Setting {name} failed, using \"C\""))?;
    }
    let warned = !interp().warnings.is_empty();
    conditions::print_warnings(conditions::DURING_STARTUP)?;
    crate::profile_summary::mark_ready();
    crate::capture::mark_ready();
    Ok(warned)
}

/// Records the base R closures the interpreter recognizes by identity: `loadNamespace`, so the
/// collector knows when a namespace load runs (a load allocates mostly what the session keeps,
/// so collecting during it frees little), and `do.call`, which call sites run without its body.
unsafe fn mark_base_closures() -> R<()> {
    let loader = eval::find_fun(values::install("loadNamespace")?, base_namespace())?;
    if loader.kind() == SexpType::Closure {
        crate::heap().preserve(loader);
        interp().namespace_loader = loader;
    }
    let do_call = eval::find_fun(values::install("do.call")?, base_namespace())?;
    if do_call.kind() == SexpType::Closure {
        crate::heap().preserve(do_call);
        interp().do_call = do_call;
    }
    Ok(())
}

/// GNU R 4.6.2's `registerS3methods()`, as [`stable_hash`] of its formals and body. Setting
/// `RY_PRINT_FAST_PATH_HASHES` prints the hash of the one base R defines, for updating this
/// together with `register_s3_methods.R`.
const GNU_REGISTER_S3_METHODS: u64 = 0x54a3_acc7_9c94_1f6c;

/// Replaces base R's `registerS3methods()` with `register_s3_methods.R`, which runs its two
/// per-method loops natively: they cost every namespace load about a millisecond per hundred S3
/// methods in R, which is most of the time `stats`, `utils` and the packages a script imports
/// take to load. The copy follows one GNU R release, so it replaces base R's function only when
/// that is exactly the code it was made from, recognised by its hash; any other keeps its own.
/// ry's bundled base R is that release, so there a mismatch is an error rather than a silent
/// loss of speed.
unsafe fn install_register_s3_methods() -> R<()> {
    let symbol = values::install("registerS3methods")?;
    let original = eval::value_of(eval::find_var_in_frame(symbol, base_namespace())?)?;
    if original.kind() != SexpType::Closure {
        return Ok(());
    }
    let mut hash = FNV_OFFSET;
    stable_hash(original.car(), &mut hash);
    stable_hash(crate::bytecode::closure_body(original), &mut hash);
    if std::env::var_os("RY_PRINT_FAST_PATH_HASHES").is_some() {
        eprintln!("registerS3methods {hash:#018x}");
    }
    let replacement = eval_text(include_str!("register_s3_methods.R"), base_namespace())?;
    // ry builds its bundled base R itself, so the base namespace it saved holds the replacement
    // already, while a build from the source release defines GNU R's own function.
    let mut replacement_hash = FNV_OFFSET;
    stable_hash(replacement.car(), &mut replacement_hash);
    stable_hash(
        crate::bytecode::closure_body(replacement),
        &mut replacement_hash,
    );
    if hash == replacement_hash {
        return Ok(());
    }
    if hash != GNU_REGISTER_S3_METHODS {
        if crate::system::r_version().number() == crate::methods_core::BUNDLED_VERSION.trim() {
            return Err(crate::unwind::error(format!(
                "base R's registerS3methods() hashes to {hash:#018x}, not the GNU R {} \
                 function register_s3_methods.R copies",
                crate::methods_core::BUNDLED_VERSION.trim()
            )));
        }
        return Ok(());
    }
    eval::define(symbol, replacement, base_namespace())
}

/// GNU R 4.6.2's `utils:::.osVersion()`, as [`stable_hash`] of its formals and body. Setting
/// `RY_PRINT_FAST_PATH_HASHES` prints the hash of the loaded one.
const GNU_OS_VERSION: u64 = 0x1e6d_407c_5956_7ebd;

/// Puts the native `ryOsVersion` in front of `utils:::.osVersion()`, which utils' `.onLoad` calls
/// on every start (the start-up image runs that hook again) and which otherwise starts two shells.
/// The image holds the patched closure, so only a start from it gains.
unsafe fn install_os_version() -> R<()> {
    let registry = crate::heap().known().namespace_registry;
    let utils = eval::find_var_in_frame(values::install("utils")?, registry)?;
    if utils.kind() != SexpType::Environment {
        return Ok(());
    }
    let original = eval::value_of(eval::find_var_in_frame(
        values::install(".osVersion")?,
        utils,
    )?)?;
    if original.kind() != SexpType::Closure {
        return Ok(());
    }
    let body = crate::bytecode::closure_body(original);
    let mut hash = FNV_OFFSET;
    stable_hash(original.car(), &mut hash);
    stable_hash(body, &mut hash);
    if std::env::var_os("RY_PRINT_FAST_PATH_HASHES").is_some() {
        eprintln!(".osVersion {hash:#018x}");
    }
    if hash != GNU_OS_VERSION {
        if crate::system::r_version().number() == crate::methods_core::BUNDLED_VERSION.trim() {
            return Err(crate::unwind::error(format!(
                "utils' .osVersion() hashes to {hash:#018x}, not the GNU R {} function \
                 install_os_version patches",
                crate::methods_core::BUNDLED_VERSION.trim()
            )));
        }
        return Ok(());
    }
    let prologue = eval_text(
        "quote(if (!is.null(version <- .Internal(ryOsVersion()))) return(version))",
        base_env(),
    )?;
    let block = values::lcons(
        values::install("{")?,
        values::cons(prologue, values::cons(body, nil())?)?,
    )?;
    crate::heap().set_cdr(original, block);
    Ok(())
}

pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// An FNV-1a hash of code that is the same in every process, unlike a hash of object addresses:
/// symbols and strings contribute their text, and attributes such as srcrefs are left out.
pub(crate) unsafe fn stable_hash(object: Sexp, hash: &mut u64) {
    let mut mix = |bytes: &[u8]| {
        for &byte in bytes {
            *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&object.kind().code().to_le_bytes());
    match object.kind() {
        SexpType::Symbol => mix(object.car().bytes()),
        SexpType::String => {
            for index in 0..length(object) {
                let element = object.element(index);
                if element == values::na_string() {
                    mix(&[0xff]);
                } else {
                    mix(element.bytes());
                    mix(&[0]);
                }
            }
        }
        SexpType::Logical | SexpType::Integer => {
            for value in values::ints(object) {
                mix(&value.to_le_bytes());
            }
        }
        SexpType::Real => {
            for value in values::reals(object) {
                mix(&value.to_bits().to_le_bytes());
            }
        }
        SexpType::List | SexpType::Expression => {
            for index in 0..length(object) {
                stable_hash(object.element(index), hash);
            }
        }
        SexpType::Pairlist | SexpType::Language => {
            // A `function` call's fourth element is its srcref, which a parse with source
            // references fills with line numbers and a saved namespace may not hold at all.
            let elements = if object.kind() == SexpType::Language
                && object.car().kind() == SexpType::Symbol
                && object.car().car().bytes() == b"function"
            {
                3
            } else {
                usize::MAX
            };
            let mut cell = object;
            let mut position = 0;
            while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language)
                && position < elements
            {
                stable_hash(cell.tag(), hash);
                stable_hash(cell.car(), hash);
                cell = cell.cdr();
                position += 1;
            }
        }
        _ => {}
    }
}

/// Runs the base package's `R/base` file and puts the fast paths in front of base's functions.
/// A built tree's file is the lazy loader, whose first expression binds every base function, and
/// the fast paths go in right after it, before any of those functions runs: the tree keeps the
/// prologues of the build that wrote it, and this build may lack the builtin one of them names.
/// While base R is being built the file is base's whole source, which defines the functions as
/// it runs, so the fast paths go in after it.
unsafe fn source_base(home: &str) -> R<()> {
    let path = format!("{home}/library/base/R/base");
    let text = std::fs::read_to_string(&path)
        .map_err(|failure| error(format!("cannot open file '{path}': {failure}")))?;
    let expressions = crate::lower::parse_text(&text, &path)?;
    crate::heap().root(expressions);
    let namespace = base_namespace();
    let mut installed = false;
    for index in 0..length(expressions) {
        scope(|| eval::eval(expressions.element(index), namespace))?;
        if index == 0 && crate::base_fast_paths::all_bound(namespace)? {
            crate::base_fast_paths::install_all(namespace)?;
            installed = true;
        }
    }
    if !installed {
        crate::base_fast_paths::install_all(namespace)?;
    }
    Ok(())
}

/// Evaluates every expression of a file in `env`, without printing, as GNU R sources its startup
/// files.
pub unsafe fn source_file(path: &str, env: Sexp) -> R<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|failure| error(format!("cannot open file '{path}': {failure}")))?;
    let expressions = crate::lower::parse_text(&text, path)?;
    crate::heap().root(expressions);
    for index in 0..length(expressions) {
        scope(|| eval::eval(expressions.element(index), env))?;
        // GNU R's `R_ReplFile` prints a profile's warnings after each expression, unheaded.
        conditions::print_warnings("")?;
    }
    Ok(())
}

pub(crate) unsafe fn eval_text(text: &str, env: Sexp) -> R<Sexp> {
    let expressions = crate::lower::parse_text(text, "<startup>")?;
    let mut value = nil();
    for index in 0..length(expressions) {
        value = eval::eval(expressions.element(index), env)?;
    }
    Ok(value)
}

/// Reads `R_HOME/etc/Renviron` the way GNU R does: `NAME=value` lines, where `${VAR-default}`
/// and `${VAR:-default}` expand, and a variable already set wins.
fn read_renviron() {
    let home = crate::system::r_home();
    set_start_variable("R_HOME", &home);
    export_wrapper_directories(&home);
    let user_set = std::env::var_os("R_LIBS_USER").is_some();
    let site_set = std::env::var_os("R_LIBS_SITE").is_some();
    read_renviron_file(&home);
    // ry is its own R home, so the defaults would hide every package installed for GNU R.
    // Its libraries go after ry's own, which keeps a package installed for ry in front.
    #[cfg(unix)]
    for (name, set, extra) in [
        ("R_LIBS_USER", user_set, gnu_r_user_libraries()),
        ("R_LIBS_SITE", site_set, gnu_r_site_libraries()),
    ] {
        if set || extra.is_empty() {
            continue;
        }
        let mut value = std::env::var(name).unwrap_or_default();
        for path in extra {
            if !value.split(':').any(|entry| entry == path) {
                if !value.is_empty() {
                    value.push(':');
                }
                value.push_str(&path);
            }
        }
        set_start_variable(name, value);
    }
}

/// Sets the environment variable `name` while the session starts.
fn set_start_variable(name: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: the session starts before any other thread exists.
    unsafe { std::env::set_var(name, value) };
}

/// GNU R's user libraries for this major version and an older minor one, newest first:
/// `~/R/<platform>-library/<x.y>` on Linux, `~/Library/R/<arch>/<x.y>/library` on macOS. The
/// current version's is `R_LIBS_USER`'s `%U` already. GNU R's platform vendor differs by
/// architecture (`x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`), so any vendor matches,
/// and macOS names Apple silicon `arm64`.
#[cfg(unix)]
fn gnu_r_user_libraries() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };
    let version = crate::system::r_version();
    let arch = std::env::consts::ARCH;
    let platforms: Vec<std::path::PathBuf> = if cfg!(target_os = "macos") {
        let arch = if arch == "aarch64" { "arm64" } else { arch };
        vec![home.join("Library/R").join(arch)]
    } else {
        let mut platforms: Vec<std::path::PathBuf> = std::fs::read_dir(home.join("R"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(&format!("{arch}-")) && name.ends_with("-linux-gnu-library")
                })
            })
            .collect();
        platforms.sort();
        platforms
    };
    let mut libraries = Vec::new();
    for minor in (0..version.minor).rev() {
        let x_y = format!("{}.{minor}", version.major);
        for platform in &platforms {
            let library = if cfg!(target_os = "macos") {
                platform.join(&x_y).join("library")
            } else {
                platform.join(&x_y)
            };
            if library.is_dir() {
                libraries.push(library.to_string_lossy().into_owned());
            }
        }
    }
    libraries
}

/// The site libraries of GNU R: the `site-library` of the `R` on `PATH`, and the ones Debian,
/// Ubuntu and Fedora install packages into. A directory holding `base` is a GNU R's own
/// library, whose base packages must not shadow ry's, so it is left out.
#[cfg(unix)]
fn gnu_r_site_libraries() -> Vec<String> {
    let path_home = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            let wrapper = std::fs::read_to_string(directory.join("R")).ok()?;
            wrapper.lines().find_map(|line| {
                let value = line.trim().strip_prefix("R_HOME_DIR=")?;
                let value = value.trim_matches(|c| c == '\'' || c == '"');
                (!value.is_empty() && !value.contains('$'))
                    .then(|| std::path::Path::new(value).join("site-library"))
            })
        })
    });
    path_home
        .into_iter()
        .chain(
            [
                "/usr/local/lib/R/site-library",
                "/usr/lib/R/site-library",
                "/usr/local/lib/R/library",
            ]
            .map(std::path::PathBuf::from),
        )
        .filter(|path| path.is_dir() && !path.join("base").is_dir())
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Sets the variables of the first system `Renviron` that exists.
fn read_renviron_file(home: &str) {
    for path in [format!("{home}/etc/Renviron"), "/etc/R/Renviron".to_owned()] {
        if read_environment_file(&path, false) {
            break;
        }
    }
}

/// GNU R's `process_site_Renviron` and `process_user_Renviron`: `R_ENVIRON` names the site
/// file and `R_ENVIRON_USER` the user's, and either set to the empty string reads none. Their
/// variables replace the process's own, so a user's `.Renviron` can override `R_LIBS_USER`.
fn read_site_and_user_renviron() {
    match std::env::var("R_ENVIRON") {
        Ok(path) => {
            if !path.is_empty() {
                read_environment_file(&path, true);
            }
        }
        Err(_) => {
            let home = crate::system::r_home();
            read_environment_file(&format!("{home}/etc/Renviron.site"), true);
        }
    }
    match std::env::var("R_ENVIRON_USER") {
        Ok(path) => {
            if !path.is_empty() {
                read_environment_file(&crate::connections::expand_path(&path), true);
            }
        }
        Err(_) => {
            if !read_environment_file(".Renviron", true)
                && let Some(home) = std::env::var_os("HOME")
            {
                let path = std::path::Path::new(&home).join(".Renviron");
                read_environment_file(&path.to_string_lossy(), true);
            }
        }
    }
}

/// Sets the `NAME=value` variables of the environment file at `path`, expanding `${NAME}`
/// references, and returns whether the file could be read. Without `replace`, a variable the
/// process has keeps its value. An empty value sets nothing, as in GNU R.
fn read_environment_file(path: &str, replace: bool) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || (!replace && std::env::var_os(name).is_some()) {
            continue;
        }
        let value = expand_environment(value.trim().trim_matches(|c| c == '\'' || c == '"'));
        if !value.is_empty() {
            set_start_variable(name, value);
        }
    }
    true
}

/// GNU R's `bin/R` shell wrapper exports `R_SHARE_DIR`, `R_INCLUDE_DIR` and `R_DOC_DIR`
/// before R starts, and `R.home("share")` and `R.home("doc")` read them. Distributions that
/// keep those directories outside `R_HOME` (Debian puts them under `/usr/share/R`) need them
/// recovered from the wrapper's plain `NAME=value` lines; a value that is still a shell
/// expression is left alone.
fn export_wrapper_directories(home: &str) {
    let Ok(wrapper) = std::fs::read_to_string(format!("{home}/bin/R")) else {
        return;
    };
    for name in ["R_SHARE_DIR", "R_INCLUDE_DIR", "R_DOC_DIR"] {
        if std::env::var_os(name).is_some() {
            continue;
        }
        let value = wrapper.lines().find_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            (key == name).then(|| value.trim_matches(|c| c == '\'' || c == '"'))
        });
        if let Some(value) = value
            && !value.is_empty()
            && !value.contains('$')
        {
            set_start_variable(name, value);
        }
    }
}

fn expand_environment(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        // Find the matching brace, allowing nesting.
        let mut depth = 1;
        let mut end = None;
        for (index, character) in after.char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            result.push_str(&rest[start..]);
            return result;
        };
        let inner = &after[..end];
        let (name, default) = match inner
            .find(":-")
            .map(|position| (position, 2))
            .or_else(|| inner.find('-').map(|position| (position, 1)))
        {
            Some((position, width)) => (&inner[..position], Some(&inner[position + width..])),
            None => (inner, None),
        };
        match std::env::var(name) {
            Ok(found) if !found.is_empty() => result.push_str(&found),
            _ => {
                if let Some(default) = default {
                    result.push_str(&expand_environment(
                        default.trim_matches(|c| c == '\'' || c == '"'),
                    ));
                }
            }
        }
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    result
}

/// How a script run ended.
pub enum Outcome {
    Completed,
    Failed,
}

/// Runs a script the way `Rscript` does: each top-level expression is evaluated in the global
/// environment, a visible value is printed, and the first error stops the run.
///
/// Like `Rscript`, which reads the script as console input, each expression is lowered just
/// before it runs, so `options(keep.source = TRUE)` keeps source references for the expressions
/// after it.
pub unsafe fn run_text(text: &str, source_name: &str) -> Outcome {
    // GNU R reads a script a line at a time into a C string, so a NUL ends its line, the
    // newline included.
    let text: Rc<str> = if text.contains('\0') {
        text.split_inclusive('\n')
            .map(|line| line.find('\0').map_or(line, |nul| &line[..nul]))
            .collect::<String>()
            .into()
    } else {
        Rc::from(text)
    };
    let previous = SCRIPT_INPUT.with(|input| {
        input.replace(Some(ScriptInput {
            text: Rc::clone(&text),
            position: 0,
        }))
    });
    let outcome = run_script(&text, source_name);
    crate::ahead::finish();
    SCRIPT_INPUT.with(|input| input.replace(previous));
    outcome
}

/// A script being run, which is also what `stdin()` reads: GNU R reads a script as its console
/// input, so `readLines(stdin())` in a script returns the script's next lines and the script
/// carries on after them.
struct ScriptInput {
    text: Rc<str>,
    /// Where the console reads next: the start of the line after the expression running now.
    position: usize,
}

thread_local! {
    static SCRIPT_INPUT: RefCell<Option<ScriptInput>> = const { RefCell::new(None) };
}

/// Reads the running script's next bytes into `buffer` for `stdin()`, or returns `None` when no
/// script is running and `stdin()` is the process's standard input.
pub fn read_script_input(buffer: &mut [u8]) -> Option<usize> {
    SCRIPT_INPUT.with(|input| {
        let mut input = input.borrow_mut();
        let input = input.as_mut()?;
        let mut count = 0;
        while count < buffer.len() {
            let text = &input.text;
            let escaped = text
                .get(input.position..)
                .and_then(|rest| rest.chars().next())
                .and_then(|character| {
                    values::escaped_byte(character).map(|byte| (byte, character.len_utf8()))
                });
            let (byte, width) = match escaped {
                Some(escaped) => escaped,
                None => match text.as_bytes().get(input.position) {
                    Some(&byte) => (byte, 1),
                    None => break,
                },
            };
            buffer[count] = byte;
            count += 1;
            input.position += width;
        }
        Some(count)
    })
}

unsafe fn run_script(text: &str, source_name: &str) -> Outcome {
    let mut start = 0;
    'parse: loop {
        let rest = &text[start..];
        let (script, failure) = crate::lower::parse_script_prefix(rest);
        // Only the script as first parsed compiles ahead: text parsed again after an expression
        // read some of it as data is run as a console would run it.
        let prepared = if start == 0 && interp().ahead.enabled {
            match crate::ahead::prepare(&script, keep_source()) {
                Ok(prepared) => Some(prepared),
                Err(unwind) => {
                    report_toplevel(unwind);
                    write_console(b"Execution halted\n", true);
                    return Outcome::Failed;
                }
            }
        } else {
            None
        };
        for (index, node) in script.expressions.iter().enumerate() {
            let end = usize::from(node.text_range().end()).min(rest.len());
            let next_line = rest[end..]
                .find('\n')
                .map_or(rest.len(), |newline| end + newline + 1);
            let cursor = start + next_line;
            SCRIPT_INPUT.with(|input| {
                if let Some(input) = input.borrow_mut().as_mut() {
                    input.position = cursor;
                }
            });
            let mark = crate::heap().handle_mark();
            let keep_source = keep_source();
            let lowered = match prepared
                .as_ref()
                .and_then(|prepared| crate::ahead::lowered(prepared, index, keep_source))
            {
                Some(expression) => Ok(expression),
                None => crate::lower::lower_script_expression(&script, node, keep_source),
            };
            let succeeded = match lowered {
                Ok(expression) => run_toplevel(expression),
                Err(unwind) => {
                    report_toplevel(unwind);
                    false
                }
            };
            crate::heap().release_handles(mark);
            // A script stops at its first error unless `options(error =)` holds a handler, which
            // GNU R runs and then carries on with the next expression.
            if !succeeded && crate::envir::get_option("error") == nil() {
                write_console(b"Execution halted\n", true);
                return Outcome::Failed;
            }
            let position = SCRIPT_INPUT
                .with(|input| input.borrow().as_ref().map(|input| input.position))
                .unwrap_or(cursor);
            if position != cursor {
                // The expression read the script's next lines as data, so parsing resumes after
                // them.
                let mut resumed = position.min(text.len());
                while !text.is_char_boundary(resumed) {
                    resumed += 1;
                }
                start = resumed;
                continue 'parse;
            }
        }
        if let Some(failure) = failure {
            if matches!(failure.report, crate::lower::FailureReport::Condition(_)) {
                report_toplevel(crate::lower::syntax_failure_unwind(
                    rest,
                    &failure,
                    source_name,
                    "<input>",
                    Some(&script),
                ));
                write_console(b"Execution halted\n", true);
                return Outcome::Failed;
            }
            let message = crate::lower::script_syntax_failure_message(&script, &failure);
            write_console(
                format!("Error: {message}\nExecution halted\n").as_bytes(),
                true,
            );
            return Outcome::Failed;
        }
        return Outcome::Completed;
    }
}

/// `getOption("keep.source")`: whether console input keeps its source references.
pub unsafe fn keep_source() -> bool {
    values::as_logical_scalar(crate::envir::get_option("keep.source")) == 1
}

/// Runs a read-eval-print loop: `read` returns the next chunk of input after showing a prompt,
/// or `None` at the end of input. Input that ends inside an unfinished expression is
/// continued on the next chunk, and each complete expression runs as its own top-level call.
/// With `halt_on_error`, the console is GNU R's non-interactive one (`R < file`, or callr
/// driving `R` over a pipe): the first error ends the session with `Execution halted`, unless
/// `options(error =)` holds a handler.
pub unsafe fn run_console(
    mut read: impl FnMut(&str) -> Option<String>,
    halt_on_error: bool,
) -> Outcome {
    let mut buffer = String::new();
    loop {
        let prompt = if buffer.is_empty() { "> " } else { "+ " };
        let Some(chunk) = read(prompt) else {
            break;
        };
        buffer.push_str(&chunk);
        if !buffer.ends_with('\n') {
            buffer.push('\n');
        }
        if syntax::input_is_unfinished(&buffer) {
            continue;
        }
        let text = std::mem::take(&mut buffer);
        let mark = crate::heap().handle_mark();
        let (script, failure) = crate::lower::parse_script_prefix(&text);
        let mut succeeded = true;
        for node in &script.expressions {
            succeeded = match crate::lower::lower_script_expression(&script, node, keep_source()) {
                Ok(expression) => run_toplevel(expression),
                Err(unwind) => {
                    report_toplevel(unwind);
                    false
                }
            };
            if !succeeded {
                break;
            }
        }
        if succeeded && let Some(failure) = failure {
            if matches!(failure.report, crate::lower::FailureReport::Condition(_)) {
                report_toplevel(crate::lower::syntax_failure_unwind(
                    &text,
                    &failure,
                    "<text>",
                    "<input>",
                    Some(&script),
                ));
            } else {
                let message = crate::lower::script_syntax_failure_message(&script, &failure);
                write_console(format!("Error: {message}\n").as_bytes(), true);
            }
            // GNU R's non-interactive console halts on a syntax error even with an error
            // handler, which only runs for evaluation errors.
            if halt_on_error {
                crate::heap().release_handles(mark);
                write_console(b"Execution halted\n", true);
                return Outcome::Failed;
            }
        }
        if !succeeded && halt_on_error && crate::envir::get_option("error") == nil() {
            crate::heap().release_handles(mark);
            write_console(b"Execution halted\n", true);
            return Outcome::Failed;
        }
        crate::heap().release_handles(mark);
    }
    Outcome::Completed
}

/// Ends a session as GNU R does before it exits: closes every graphics device, which writes
/// the files they were drawing, stops the profilers, runs the exit finalizers and removes the
/// temporary directory. The directory goes last because a finalizer may still write there.
pub unsafe fn finish() {
    if let Err(unwind) = (interp().extensions.graphics.kill_all_devices)() {
        report_toplevel(unwind);
    }
    if let Err(unwind) = crate::utils::stop_profiler() {
        report_toplevel(unwind);
    }
    if let Err(unwind) = crate::utils::stop_memory_profiler() {
        report_toplevel(unwind);
    }
    if let Err(unwind) = crate::capture::finish() {
        report_toplevel(unwind);
    }
    if let Err(unwind) = crate::profile_summary::finish() {
        report_toplevel(unwind);
    }
    (interp().hooks.run_exit_finalizers)();
    crate::system::remove_session_temp_dir();
}

/// Evaluates one top-level expression, prints it if visible, and reports deferred warnings.
/// Returns whether it succeeded.
pub unsafe fn run_toplevel(expression: Sexp) -> bool {
    let mark = crate::heap().handle_mark();
    interp().visible = true;
    crate::time_limits::reset();
    let mut value = nil();
    let result = eval::eval(expression, global_env()).and_then(|result| {
        // An unwind parked after the last poll belongs to this command, not the next.
        crate::unwind::poll_interrupt()?;
        value = result;
        crate::heap().root(value);
        let last = values::install(".Last.value")?;
        crate::heap().define_base_variable(last, value)?;
        // GNU R's `.Last.value` does not count as a reference, so a value the user just
        // bound stays unshared and `.Internal(refcnt())` reports what the user's own
        // bindings hold.
        if let Some(cell) = crate::heap().frame_cell(base_env(), last) {
            crate::heap().untrack_references(cell);
        }
        if interp().visible {
            print::print_value_top(value, global_env())?;
        }
        Ok(())
    });
    let succeeded = match result {
        Ok(()) => true,
        Err(unwind) => {
            report_toplevel(unwind);
            false
        }
    };
    crate::truncate_frames(interp(), 0);
    interp().foreign_calls.clear();
    interp().handlers = interp().global_handlers.clone();
    interp().restarts = None;
    interp().srcref = nil();
    if let Err(unwind) = conditions::print_warnings("") {
        report_toplevel(unwind);
    }
    // The task's warnings print first, as in GNU R, and the callbacks' own warnings after them.
    let visible = interp().visible;
    if let Err(unwind) = (interp().hooks.run_task_callbacks)(expression, value, succeeded, visible)
    {
        report_toplevel(unwind);
        crate::truncate_frames(interp(), 0);
        interp().foreign_calls.clear();
        interp().handlers = interp().global_handlers.clone();
        interp().restarts = None;
    }
    if let Err(unwind) = conditions::print_warnings("") {
        report_toplevel(unwind);
    }
    crate::heap().release_handles(mark);
    succeeded
}

/// Reports an unwind that reached top level.
pub unsafe fn report_toplevel(unwind: Unwind) {
    match unwind {
        Unwind::Error(failure) => {
            // An error that was never signalled, such as one raised by native code outside any
            // R call.
            let call = crate::unwind::resolve_call(failure.call);
            let text = conditions::format_error(call, &failure.message);
            write_console(text.as_bytes(), true);
        }
        Unwind::Interrupt => write_console(b"\n", false),
        Unwind::Jump(jump) => match jump {
            Jump::TopLevel => {}
            Jump::Return { .. } => {
                write_console(
                    b"Error: no function to return from, jumping to top level\n",
                    true,
                );
            }
            Jump::Break { .. } | Jump::Next { .. } => {
                write_console(
                    b"Error: no loop for break/next, jumping to top level\n",
                    true,
                );
            }
        },
    }
}

unsafe fn do_mk_unbound(call: &Call<'_>) -> R<Sexp> {
    let symbol = call.arg(0);
    if symbol.kind() != SexpType::Symbol {
        return Err(call.error("not a symbol"));
    }
    crate::heap().remove_binding(base_env(), symbol)?;
    Ok(nil())
}

#[cfg(test)]
mod tests {
    use {super::*, crate::testing::with_interpreter};

    /// A function parsed from source holds srcrefs in its nested `function` calls, while the copy
    /// a namespace saves may not, and the guards on base R's functions must hash both alike.
    #[test]
    fn stable_hash_ignores_nested_function_srcrefs() {
        let text = "function(x) {\n  inner <- function(y) y + 1\n  inner(x)\n}";
        with_interpreter(|| {
            // SAFETY: the interpreter is installed.
            unsafe {
                let hash_of = |srcfile: Sexp| -> R<u64> {
                    let parsed = crate::lower::parse_with_srcfile(text, "<test>", srcfile)?;
                    let mut hash = FNV_OFFSET;
                    stable_hash(parsed.element(0), &mut hash);
                    Ok(hash)
                };
                let with_source = hash_of(values::new_env(base_env()).expect("environment"))
                    .expect("parse with source");
                let without_source = hash_of(nil()).expect("parse without source");
                assert_eq!(with_source, without_source);
            }
        });
    }
}
