//! `ry.toml` discovery and parsing. Both the language server (from the
//! client's workspace root) and the CLI (from each target argument) resolve
//! configuration through the one nearest-ancestor search here.
//!
//! `roughly.toml` is still accepted under the project's former name. Both are
//! looked for in the SAME directory before walking to the parent, so a
//! repository that has adopted the new name is never overridden by an old file
//! left behind further up the tree.

use miette::{Diagnostic, LabeledSpan, NamedSource, SourceCode, SourceSpan};
use semantics::lints::{LintConfig, NameStyle};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::{fmt, io};
use thiserror::Error;

pub const CONFIG_FILE_NAME: &str = "ry.toml";

/// The pre-rename spelling, still honoured so existing projects keep working.
/// Checked after [`CONFIG_FILE_NAME`] within each directory, so the current
/// name wins a tie.
pub const LEGACY_CONFIG_FILE_NAME: &str = "roughly.toml";

/// Every file name that marks a project root and carries configuration, in
/// precedence order.
pub const CONFIG_FILE_NAMES: [&str; 2] = [CONFIG_FILE_NAME, LEGACY_CONFIG_FILE_NAME];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub format: format::Config,
    pub lint: LintConfig,
    pub check: CheckConfig,
    /// The directory containing the loaded configuration file. It anchors a
    /// relative pattern such as `[check] exclude`. It is `None` for the
    /// built-in default configuration.
    pub source_directory: Option<PathBuf>,
    /// Keys the loaded file set that this version does not know, in file
    /// order, such as `check.excluded`. The configuration still loads, which
    /// keeps a file written against a newer version usable, and a host surfaces
    /// these as visible warnings. A wrong TYPE on a known key remains a hard
    /// error.
    pub unknown_keys: Vec<String>,
}

/// Which diagnostic classes are published. Every class is computed on demand
/// for IDE features regardless; these gate only what is reported.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct CheckConfig {
    /// Surface unused-local-binding warnings (on by default; `unused = false`
    /// under `[check]` opts out).
    pub unused: bool,
    /// Surface reads a path can reach with no prior write (off by default).
    /// Correct by flow but noisy on real code, because two conditions that
    /// always agree at run time are two independent branches to the analysis.
    pub maybe_undefined: bool,
    /// Surface type-error diagnostics.
    pub typing: bool,
    /// Surface strict-mode diagnostics (each site originating a genuine
    /// `Unknown`), and escalate unresolved-name findings to errors.
    pub strict: bool,
    /// Paths `check`'s directory walk skips: gitignore-style patterns
    /// (`scripts/`, `**/generated`, `!scripts/keep.R`) anchored at the config
    /// file's directory. Files named explicitly on the command line are
    /// always checked.
    pub exclude: Vec<String>,
}

impl Default for CheckConfig {
    fn default() -> CheckConfig {
        CheckConfig {
            unused: true,
            maybe_undefined: false,
            typing: false,
            strict: false,
            exclude: Vec::new(),
        }
    }
}

impl Config {
    /// Loads the config file governing `target`: the nearest one found in
    /// the target's directory (its parent, when `target` is a file) or the
    /// directory's ancestors, falling back to the default configuration when
    /// none exists.
    pub fn discover(target: impl AsRef<Path>) -> Result<Config, ConfigError> {
        match find_config_file(target.as_ref())? {
            Some(path) => Config::from_path(path),
            None => Ok(Config::default()),
        }
    }

    /// The path [`discover`](Config::discover) would load for `target`, when
    /// one exists. This is the file the language server must watch for a live
    /// reload, and it may sit in an ancestor *above* the workspace root. A
    /// resolution failure degrades to `None`, because the caller is deciding
    /// what to watch and `discover` itself reports the error.
    pub fn discover_path(target: impl AsRef<Path>) -> Option<PathBuf> {
        find_config_file(target.as_ref()).ok().flatten()
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, Some(path)).map(|mut config| {
                config.source_directory = path.parent().map(Path::to_path_buf);
                config
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source: error,
            }),
        }
    }

    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, None)
    }

    /// The one parse. `path` names the file a failure is reported against. It
    /// is `None` for text that came from somewhere other than a file on
    /// disk.
    fn parse(text: &str, path: Option<&Path>) -> Result<Config, ConfigError> {
        let deserializer = toml::de::Deserializer::new(text);
        let mut unknown_keys = Vec::new();
        match serde_ignored::deserialize::<_, _, ConfigToml>(deserializer, |key| {
            unknown_keys.push(key.to_string())
        }) {
            Ok(config) => {
                let mut config = config.to_config();
                config.unknown_keys = unknown_keys;
                Ok(config)
            }
            Err(error) => Err(ConfigError::Invalid(ConfigParseError::new(
                &error, text, path,
            ))),
        }
    }
}

/// The nearest-ancestor search both `discover` and `discover_path` walk: the
/// target's own directory (its parent when the target is a file), then each
/// ancestor, for a `ry.toml`, or for a `roughly.toml` under the former
/// name.
fn find_config_file(target: &Path) -> Result<Option<PathBuf>, ConfigError> {
    // A relative path is made absolute first: its lexical parent chain ends
    // at the empty path, so walking it directly would never reach the real
    // filesystem ancestors.
    let target = std::path::absolute(target).map_err(|error| ConfigError::Resolve {
        path: target.to_path_buf(),
        source: error,
    })?;
    // `std::path::absolute` keeps a `..` component, and `parent()` strips it
    // lexically. A target like `/a/b/../c.R` would then walk `/a/b/..` and back
    // INTO `/a/b`, which is not an ancestor of the target at all.
    let target = normalize_lexically(&target);

    let mut directory = if target.is_dir() {
        Some(target.as_path())
    } else {
        target.parent()
    };
    while let Some(current) = directory {
        for name in CONFIG_FILE_NAMES {
            let candidate = current.join(name);
            if candidate.is_file() {
                return Ok(Some(candidate));
            }
        }
        directory = current.parent();
    }
    Ok(None)
}

/// The underlying I/O failures render as the report's cause rather than as
/// part of the message, so the reason a file could not be read is not welded
/// into the sentence that says which file it was.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to read config file {path}", path = .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("failed to resolve {path} while searching for a config file", path = .path.display())]
    Resolve { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Invalid(ConfigParseError),
}

impl ConfigError {
    /// The 1-based line and column of a parse or deserialize failure inside
    /// the config text, when the underlying toml error carries a span. What
    /// the language server anchors its published diagnostic at; the CLI draws
    /// the same span as a snippet instead.
    pub fn parse_location(&self) -> Option<(usize, usize)> {
        match self {
            ConfigError::Invalid(error) => error
                .span
                .map(|span| line_and_column(error.source.inner(), span.offset())),
            ConfigError::Io { .. } | ConfigError::Resolve { .. } => None,
        }
    }

    /// The config file a failure sits in, as it should be shown to a reader.
    /// The I/O variants already name their path in the message; a parse
    /// failure keeps it on the source its snippet is drawn from.
    pub fn file(&self) -> Option<&str> {
        match self {
            ConfigError::Invalid(error) => Some(error.source.name()),
            ConfigError::Io { .. } | ConfigError::Resolve { .. } => None,
        }
    }
}

impl Diagnostic for ConfigError {
    fn code(&self) -> Option<Box<dyn fmt::Display + '_>> {
        Some(Box::new("config"))
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        match self {
            ConfigError::Invalid(error) => Some(&error.source),
            ConfigError::Io { .. } | ConfigError::Resolve { .. } => None,
        }
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let ConfigError::Invalid(error) = self else {
            return None;
        };
        // A failure toml reports no span for is still shown against the file:
        // an empty label at its start names it in the snippet header.
        let span = error.span.unwrap_or_else(|| SourceSpan::from(0..0));
        Some(Box::new(std::iter::once(
            LabeledSpan::new_primary_with_span(None, span),
        )))
    }
}

/// A malformed config file: the toml parse or deserialize failure, kept with
/// the text it happened in so a host can point at the offending key or value
/// rather than describe where it was.
#[derive(Debug)]
pub struct ConfigParseError {
    source: NamedSource<String>,
    span: Option<SourceSpan>,
    message: String,
}

impl ConfigParseError {
    fn new(error: &toml::de::Error, text: &str, path: Option<&Path>) -> ConfigParseError {
        let name = path.map_or_else(
            || CONFIG_FILE_NAME.to_owned(),
            |path| path.display().to_string(),
        );
        ConfigParseError {
            source: NamedSource::new(name, text.to_owned()),
            span: error.span().map(SourceSpan::from),
            message: error.message().to_owned(),
        }
    }
}

/// The dotted key whose value sits at `offset`. It is the `[table]` header
/// above the offset, joined with the `key =` on its own line.
fn offending_key(text: &str, offset: usize) -> Option<String> {
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    // The `=` is what makes it a key: a failure inside a table header or a
    // bare value has no setting to name, and the snippet points at it anyway.
    let (key, _) = before.get(line_start..)?.split_once('=')?;
    let key = key.trim().trim_matches('"');
    if key.is_empty() {
        return None;
    }
    let table = before[..line_start]
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix('[')?.strip_suffix(']')
        })
        .filter(|table| !table.is_empty());
    Some(match table {
        Some(table) => format!("{table}.{key}"),
        None => key.to_owned(),
    })
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid config")?;
        // The dotted key is what the documentation calls the setting, so it
        // belongs in the sentence; where it sits is the snippet's job.
        if let Some(key) = self
            .span
            .and_then(|span| offending_key(self.source.inner(), span.offset()))
        {
            write!(formatter, " for `{key}`")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for ConfigParseError {}

/// The table a bare top-level key belongs under, when its name is a key of a
/// known table. `typing = true` written outside `[check]` is the easy mistake
/// to make and by far the most dangerous one: the file still loads, so the run
/// reports nothing and a project that is not being type-checked is
/// indistinguishable from one that is clean. A warning that only says "unknown
/// key" leaves the reader looking for a typo they did not make, so it has to
/// name the placement instead.
pub fn suggested_table(unknown_key: &str) -> Option<&'static str> {
    // A dotted path is already inside a table, so it is a misspelling rather
    // than a misplacement, and the plain wording is the right answer for it.
    if unknown_key.contains('.') {
        return None;
    }
    TABLE_KEYS
        .iter()
        .find(|(_, keys)| keys.contains(&unknown_key))
        .map(|(table, _)| *table)
}

/// Which keys each config table accepts. Serde offers no way to enumerate a
/// struct's field names at run time, so this list is written out. It
/// duplicates the config structs, and only one direction of the duplication is
/// tested: `table_keys_match_the_config_structs` catches a key listed here
/// that the struct does not have. A field ADDED to a struct and not added here
/// is not caught, and silently costs that key its "belongs under `[check]`"
/// hint.
const TABLE_KEYS: [(&str, &[&str]); 3] = [
    ("format", &["indent-width", "line-ending"]),
    (
        "lint",
        &[
            "naming-style",
            "assignment-operator",
            "boolean-shorthand",
            "missing-comma",
            "trailing-comma",
            "unused-parameter",
            "unused-import",
            "shadows-builtin",
            "shadows-namespace",
        ],
    ),
    (
        "check",
        &["unused", "maybe-undefined", "typing", "strict", "exclude"],
    ),
];

/// Resolves `.` and `..` components lexically (without touching the
/// filesystem), so an ancestor walk over the result visits only true
/// ancestors. Lexical resolution can differ from the filesystem view when
/// `..` crosses a symlink; the walk prefers the path as the user spelled it.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                // The root's parent is the root itself, so a `..` dissolves.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                // A leading `..` on a relative path has nothing to cancel.
                _ => normalized.push(component),
            },
            other => normalized.push(other),
        }
    }
    normalized
}

/// The 1-based line and column of a byte offset within `text`, for rendering
/// a toml error span.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_start = text[..offset].rfind('\n').map_or(0, |newline| newline + 1);
    let line = text[..line_start].matches('\n').count() + 1;
    let column = text[line_start..offset].chars().count() + 1;
    (line, column)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ConfigToml {
    pub case: Option<NameStyle>, // kept for backwards compatibility
    pub spaces: Option<usize>,   // kept for backwards compatibility
    pub format: format::Config,
    pub lint: LintConfig,
    pub check: CheckConfig,
}

impl ConfigToml {
    pub fn to_config(mut self) -> Config {
        if let Some(spaces) = self.spaces {
            self.format.indent_width = spaces;
        }
        if let Some(case) = self.case {
            self.lint.naming_style = Some(case);
        }
        Config {
            format: self.format,
            lint: self.lint,
            check: self.check,
            source_directory: None,
            unknown_keys: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalFeatures {
    pub range_formatting: bool,
}

/// One experimental feature: its flag name, a one-line description, and the
/// setter that turns it on. The CLI help, the `--experimental-features`
/// parser, and `"all"` all derive from [`ExperimentalFeatures::KNOWN`], so a
/// new feature added there is automatically listed and parseable.
pub struct ExperimentalFeature {
    pub name: &'static str,
    pub description: &'static str,
    enable: fn(&mut ExperimentalFeatures),
}

impl ExperimentalFeatures {
    pub const KNOWN: &'static [ExperimentalFeature] = &[ExperimentalFeature {
        name: "range_formatting",
        description: "format only the selected range in the editor instead of the whole file",
        enable: |features| features.range_formatting = true,
    }];

    /// Enables the named feature; false when the name is unknown.
    pub fn enable(&mut self, name: &str) -> bool {
        match Self::KNOWN.iter().find(|feature| feature.name == name) {
            Some(feature) => {
                (feature.enable)(self);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use format::LineEnding;
    use indoc::indoc;
    use semantics::lints::LintLevel;

    fn parse(text: &str) -> Config {
        Config::from_toml_str(text).unwrap()
    }

    fn parse_error(text: &str) -> String {
        Config::from_toml_str(text)
            .expect_err("expected the config to be rejected")
            .to_string()
    }

    #[test]
    fn per_lint_levels() {
        let config = parse(indoc! {r#"
            [lint]
            assignment-operator = "off"
            boolean-shorthand = "error"
        "#});
        assert_eq!(config.lint.assignment_operator, LintLevel::Off);
        assert_eq!(config.lint.boolean_shorthand, LintLevel::Error);
        assert_eq!(config.lint.trailing_comma, LintLevel::Default);
    }

    #[test]
    fn all_fields() {
        let config = parse(indoc! {r#"
            [format]
            indent-width = 4
            line-ending = "auto"

            [lint]
            naming-style = "snake_case"
        "#});
        assert_eq!(config.format.indent_width, 4);
        assert_eq!(config.format.line_ending, LineEnding::Auto);
        assert_eq!(config.lint.naming_style, Some(NameStyle::Snake));
    }

    #[test]
    fn backwards_compatability() {
        let config = parse(indoc! {r#"
            case = "snake_case" # should override lint.naming-style
            spaces = 6          # should override format.indent-width

            [format]
            indent-width = 4
            line-ending = "cr-lf"

            [lint]
            naming-style = "camelCase"
            missing-comma = "off"

            [check]
            unused = true
        "#});
        assert_eq!(config.format.indent_width, 6);
        assert_eq!(config.format.line_ending, LineEnding::CrLf);
        assert_eq!(config.lint.naming_style, Some(NameStyle::Snake));
        assert!(config.check.unused);
    }

    #[test]
    fn defaults() {
        let config = parse("[format]\n[lint]\n");
        assert_eq!(config.format.indent_width, 2);
        assert_eq!(config.format.line_ending, LineEnding::Auto);
        assert_eq!(config.lint.naming_style, None);
        assert!(config.check.unused, "unused warnings are on by default");
        assert!(!config.check.typing);
    }

    #[test]
    fn unused_can_be_opted_out() {
        let config = parse("[check]\nunused = false\n");
        assert!(!config.check.unused);
    }

    #[test]
    fn unknown_keys_warn_but_load() {
        let config = parse("strict = true\n[check]\ntyping = true\nstric = true\n");
        assert_eq!(config.unknown_keys, ["strict", "check.stric"]);
        assert!(
            config.check.typing,
            "the known keys around an unknown one still apply"
        );
    }

    #[test]
    fn a_table_key_written_at_the_top_level_names_its_table() {
        assert_eq!(suggested_table("typing"), Some("check"));
        assert_eq!(suggested_table("indent-width"), Some("format"));
        assert_eq!(suggested_table("naming-style"), Some("lint"));
        // A genuine misspelling has no placement to suggest, and neither does
        // a key already inside a table.
        assert_eq!(suggested_table("typng"), None);
        assert_eq!(suggested_table("check.typing"), None);
    }

    #[test]
    fn every_check_field_has_a_placement_hint() {
        // Guards the direction `table_keys_match_the_config_structs` cannot
        // see: a field added to `CheckConfig` but not to TABLE_KEYS. Written
        // out because serde cannot enumerate the struct's fields.
        for key in ["unused", "maybe-undefined", "typing", "strict", "exclude"] {
            assert_eq!(
                suggested_table(key),
                Some("check"),
                "`{key}` is a `[check]` field with no placement hint"
            );
        }
    }

    #[test]
    fn table_keys_match_the_config_structs() {
        for (table, keys) in TABLE_KEYS {
            for key in keys {
                // The value is deliberately arbitrary: a key the struct has
                // rejects a wrong type as a hard error, while a key it does
                // not have is skipped and recorded. Only the second outcome
                // means this list has drifted.
                let text = format!("[{table}]\n{key} = 0\n");
                if let Ok(config) = Config::from_toml_str(&text) {
                    assert!(
                        config.unknown_keys.is_empty(),
                        "`{table}.{key}` is listed in TABLE_KEYS but the struct does not have it"
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_keys_in_each_section_are_recorded() {
        let config = parse("[format]\nindent = 4\n[lint]\nstyle = \"x\"\n[check]\nstric = true\n");
        assert_eq!(
            config.unknown_keys,
            ["format.indent", "lint.style", "check.stric"]
        );
    }

    #[test]
    fn wrong_type_on_known_key_stays_a_hard_error() {
        let message = parse_error("[check]\ntyping = \"yes\"\n");
        assert!(message.contains("expected a boolean"), "{message}");
    }

    #[test]
    fn check_exclude_patterns_parse() {
        let config = parse("[check]\nexclude = [\"scripts/\", \"!scripts/keep.R\"]\n");
        assert_eq!(config.check.exclude, ["scripts/", "!scripts/keep.R"]);
    }

    #[test]
    fn from_path_records_the_source_directory() {
        let directory = tempfile::tempdir().expect("temp dir");
        let config_path = directory.path().join(CONFIG_FILE_NAME);
        std::fs::write(&config_path, "[check]\nunused = false\n").expect("write config");
        let config = Config::from_path(&config_path).expect("config parses");
        assert_eq!(config.source_directory.as_deref(), Some(directory.path()));
    }

    // The message names the setting; where it sits is carried as a span, for
    // the CLI to draw as a snippet and the language server to publish.
    #[test]
    fn wrong_value_type_is_rejected_with_location() {
        let error = Config::from_toml_str("[check]\nstrict = \"yes\"\n")
            .expect_err("expected the config to be rejected");
        let message = error.to_string();
        assert!(message.contains("expected a boolean"), "{message}");
        assert!(message.contains("`check.strict`"), "{message}");
        assert_eq!(error.parse_location(), Some((2, 10)));
    }

    #[test]
    fn from_path_names_the_file_in_errors() {
        let directory = tempfile::tempdir().expect("temp dir");
        let config_path = directory.path().join(CONFIG_FILE_NAME);
        std::fs::write(&config_path, "[check]\ntyping = 1\n").expect("write config");
        let error =
            Config::from_path(&config_path).expect_err("expected the config to be rejected");
        assert_eq!(error.parse_location(), Some((2, 10)));

        let mut rendered = String::new();
        miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::none())
            .render_report(&mut rendered, &error)
            .expect("the report renders");
        assert!(rendered.contains("ry.toml:2:10"), "{rendered}");
        assert!(rendered.contains("typing = 1"), "{rendered}");
    }

    #[test]
    fn discover_walks_ancestors_from_a_file_target() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        std::fs::write(root.join(CONFIG_FILE_NAME), "[format]\nindent-width = 7\n")
            .expect("write config");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested dirs");
        std::fs::write(nested.join("script.R"), "x <- 1\n").expect("write script");
        let config = Config::discover(nested.join("script.R")).expect("discover config");
        assert_eq!(config.format.indent_width, 7);
    }

    #[test]
    fn discover_prefers_the_nearest_config() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        std::fs::write(root.join(CONFIG_FILE_NAME), "[format]\nindent-width = 7\n")
            .expect("write outer config");
        let nested = root.join("inner");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::write(
            nested.join(CONFIG_FILE_NAME),
            "[format]\nindent-width = 3\n",
        )
        .expect("write inner config");
        let config = Config::discover(&nested).expect("discover from directory");
        assert_eq!(config.format.indent_width, 3);
    }

    #[test]
    fn discover_defaults_when_no_config_exists() {
        let directory = tempfile::tempdir().expect("temp dir");
        let config = Config::discover(directory.path()).expect("discover config");
        assert_eq!(config, Config::default());
    }

    // A `..` in the target must not let the walk wander back into a sibling
    // directory: a config in `project/` does not govern `project/../outside.R`.
    #[test]
    fn discover_ignores_configs_behind_dot_dot_components() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let project = root.join("project");
        std::fs::create_dir_all(&project).expect("create project dir");
        std::fs::write(
            project.join(CONFIG_FILE_NAME),
            "[format]\nindent-width = 7\n",
        )
        .expect("write project config");
        std::fs::write(root.join("outside.R"), "x <- 1\n").expect("write outside script");
        let config = Config::discover(project.join("..").join("outside.R"))
            .expect("discover through dot-dot");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn obsolete_missing_comma_key_is_still_accepted() {
        let config = parse("[lint]\nmissing-comma = \"off\"\n");
        assert_eq!(config.lint.missing_comma, LintLevel::Off);
    }
}
