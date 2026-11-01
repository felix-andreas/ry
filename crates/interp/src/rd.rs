//! Rd, R's documentation markup: `parseRd` turns an Rd file into the tagged list tree that
//! `tools::parse_Rd` returns, and `deparseRd` escapes one leaf of that tree back into Rd source.
//!
//! An Rd file mixes three lexical modes. LaTeX-like text (`TEXT`) treats braces as groups and
//! backslash words as macros. R-like code (`RCODE`) keeps balanced braces as literal text and
//! follows R's string quotes, so a brace inside a string never closes the argument. Verbatim
//! text (`VERB`) keeps everything but comments and escapes. Each macro fixes the mode of its
//! arguments, and leaves of every mode end after a newline, so a multi-line argument becomes one
//! leaf per line.
//!
//! User macros from `\newcommand` expand by pushing their substituted definition back into the
//! input, so the expansion is read in whatever mode the macro appeared in and its text merges
//! with the surrounding input exactly as if it had been written there. Characters read back from
//! an expansion do not advance the source position, which is why expanded leaves carry the
//! zero-width srcref just after the macro call.
//!
//! Syntax errors are warnings, as in GNU R: the parser reports them and keeps whatever tree it
//! can still build.

use std::collections::HashMap;

use crate::{
    builtin_libraries::NativeCall,
    conditions, connections,
    eval::{define, find_var},
    unwind::{R, current_call, error, error_in},
    values::{
        self, alloc, install, integer_vector, list_vector, nil, scalar_integer, scalar_string,
        string_vector, unbound,
    },
};
use heap::object::{Sexp, SexpType};

/// `parseRd(con, srcfile, encoding, verbose, basename, fragment, warningCalls, macros,
/// warndups)`: the Rd tree of the text on `con`.
pub unsafe fn parse_rd(call: &NativeCall<'_>) -> R<Sexp> {
    let text = connections::read_all_text(call.arg(0))?;
    let srcfile = call.arg(1);
    let basename =
        values::first_string(call.arg(4)).map_or_else(String::new, |name| name.into_owned());
    let fragment = values::as_logical_scalar(call.arg(5)) == 1;
    let warning_calls = values::as_logical_scalar(call.arg(6)) != 0;
    let macros = call.arg(7);
    // Without a macro environment GNU R starts from a fresh empty one, which then sits
    // between the result's macros and the empty environment.
    let parent = if macros.kind() == SexpType::Environment {
        macros
    } else {
        values::new_env(values::empty_env())?
    };
    let mut parser = Parser {
        input: Input::new(&text),
        fragment,
        basename,
        parent_macros: parent,
        macros: HashMap::new(),
        item: ItemContext::None,
        ifdef_depth: 0,
        last_text: String::new(),
        expansions: 0,
        warnings: Vec::new(),
    };
    let parsed = parser.content(Mode::Latex, Close::EndOfInput, true);
    let end = parser.input.next_position;
    let warning_call = if warning_calls { current_call() } else { nil() };
    for message in std::mem::take(&mut parser.warnings) {
        conditions::warning_call(warning_call, message)?;
    }
    let nodes = parsed?;
    let env = values::new_env(parent)?;
    let builder = Builder::new(srcfile, env)?;
    let mut flag = 0;
    let mut children = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let (child, child_flag) = builder.build(node)?;
        flag |= child_flag;
        children.push(child);
    }
    let result = list_vector(&children)?;
    values::set_attr(result, values::symbols().class, scalar_string("Rd")?)?;
    let whole = [0, 0, end.line, end.byte, 0, end.column];
    values::set_attr(result, builder.srcref, builder.srcref_vector(whole)?)?;
    if flag != 0 {
        values::set_attr(result, builder.dynamic_flag, scalar_integer(flag)?)?;
    }
    values::set_attr(result, install("macros")?, env)?;
    Ok(result)
}

/// `deparseRd(element, state)`: `element` escaped for Rd source, and the state after it.
///
/// The state is `c(braceDepth, inRString, mode, inEqn, quoteBraces)`, threaded through the
/// leaves of one macro by `as.character.Rd(deparse = TRUE)`. Braces are escaped whenever
/// `quoteBraces` is set or the leaf is LaTeX-like; in R-like and verbatim leaves outside an
/// equation, balanced braces stay bare and only an unmatched closing brace is escaped. R-like
/// leaves follow string quotes, without honoring backslash escapes, the same way the Rd
/// lexer did when it read them.
pub unsafe fn deparse_rd(call: &NativeCall<'_>) -> R<Sexp> {
    const R_LIKE: i32 = 1;
    const LATEX_LIKE: i32 = 2;
    const VERBATIM: i32 = 3;
    const COMMENT: i32 = 5;
    let element =
        values::first_string(call.arg(0)).map_or_else(String::new, |text| text.into_owned());
    let state_object = crate::coerce::coerce_vector(call.arg(1), SexpType::Integer)?;
    let mut state = [0i32; 5];
    for (index, slot) in state
        .iter_mut()
        .enumerate()
        .take(values::length(state_object))
    {
        *slot = values::ints(state_object)[index];
    }
    let [mut depth, mut in_string, mode, in_equation, quote_braces] = state;
    let in_equation = in_equation != 0;
    let escape_braces = quote_braces != 0 || mode == LATEX_LIKE;
    let mut output = String::with_capacity(element.len() + 8);
    for character in element.chars() {
        match character {
            '{' | '}' if escape_braces => {
                output.push('\\');
                output.push(character);
            }
            '{' | '}'
                if !in_equation && (mode == VERBATIM || (mode == R_LIKE && in_string == 0)) =>
            {
                if character == '{' {
                    depth += 1;
                    output.push('{');
                } else if depth > 0 {
                    depth -= 1;
                    output.push('}');
                } else {
                    output.push_str("\\}");
                }
            }
            '%' if !in_equation && mode != COMMENT => output.push_str("\\%"),
            '\\' if mode != COMMENT && ((mode == R_LIKE && in_string != 0) || !in_equation) => {
                output.push_str("\\\\");
            }
            '"' | '\'' | '`' if mode == R_LIKE => {
                if in_string == 0 {
                    in_string = character as i32;
                } else if in_string == character as i32 {
                    in_string = 0;
                }
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    state[0] = depth;
    state[1] = in_string;
    list_vector(&[scalar_string(&output)?, integer_vector(&state)?])
}

/// One element of the parse, before it becomes R objects. A list without a tag and without a
/// span is the argument list of a multi-argument macro.
struct Node {
    span: Option<Span>,
    kind: NodeKind,
}

enum NodeKind {
    Leaf {
        tag: &'static str,
        text: String,
    },
    List {
        tag: Option<String>,
        option: Option<Box<Node>>,
        children: Vec<Node>,
    },
    /// A `\newcommand` or `\renewcommand` definition.
    Command {
        tag: String,
        name: String,
        arguments: usize,
        definition: String,
    },
    /// A user macro call: its definition followed by the arguments it was given.
    UserMacro {
        name: String,
        values: Vec<String>,
    },
}

#[derive(Clone, Copy, Default)]
struct Position {
    line: i32,
    byte: i32,
    column: i32,
}

#[derive(Clone, Copy)]
struct Span {
    first: Position,
    last: Position,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Latex,
    RCode,
    Verbatim,
    /// The first argument of `\eqn` and `\deqn`: LaTeX source where `%` and backslash escapes
    /// mean what LaTeX says, so nothing is stripped.
    RawVerbatim,
}

impl Mode {
    fn leaf_tag(self) -> &'static str {
        match self {
            Mode::Latex => "TEXT",
            Mode::RCode => "RCODE",
            Mode::Verbatim | Mode::RawVerbatim => "VERB",
        }
    }
}

/// What ends a run of content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    Brace,
    EndOfInput,
    /// The body of a `#ifdef` block, which ends at `#endif`.
    EndIf,
}

/// Which form `\item` takes inside the current list macro.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemContext {
    None,
    /// `\itemize` and `\enumerate`: `\item` takes no argument.
    Bare,
    /// `\describe`, `\arguments` and `\value`: `\item{label}{text}`.
    Pair,
}

/// How a built-in macro takes its arguments.
#[derive(Clone, Copy)]
enum Shape {
    Zero,
    One(Mode),
    /// An optional `[option]` before the one argument.
    OptionThenOne(Mode),
    Several(&'static [Mode]),
    /// A required first argument and an optional second one.
    OptionalSecond(Mode, Mode),
}

fn builtin_shape(name: &str) -> Option<Shape> {
    use Mode::{Latex, RCode, RawVerbatim, Verbatim};
    Some(match name {
        "cr" | "dots" | "ldots" | "R" | "tab" => Shape::Zero,
        "acronym" | "abbr" | "bold" | "cite" | "command" | "dfn" | "dQuote" | "email" | "emph"
        | "file" | "pkg" | "sQuote" | "strong" | "var" | "linkS4class" | "itemize"
        | "enumerate" | "describe" => Shape::One(Latex),
        "link" => Shape::OptionThenOne(Latex),
        "code" | "donttest" | "dontshow" | "testonly" | "special" => Shape::One(RCode),
        "Sexpr" => Shape::OptionThenOne(RCode),
        "env" | "kbd" | "option" | "preformatted" | "samp" | "url" | "verb" | "dontrun" | "out" => {
            Shape::One(Verbatim)
        }
        "enc" | "method" | "S3method" | "S4method" | "tabular" | "subsection" | "if" => {
            Shape::Several(&[Latex, Latex])
        }
        "href" => Shape::Several(&[Verbatim, Latex]),
        "ifelse" => Shape::Several(&[Latex, Latex, Latex]),
        "eqn" | "deqn" => Shape::OptionalSecond(RawVerbatim, Verbatim),
        "figure" => Shape::OptionalSecond(Verbatim, Verbatim),
        _ => return None,
    })
}

/// The sections of a complete Rd file, which only appear at its top level.
fn section_shape(name: &str) -> Option<Shape> {
    use Mode::{Latex, RCode, Verbatim};
    Some(match name {
        "name" | "alias" | "Rdversion" => Shape::One(Verbatim),
        "usage" | "examples" | "synopsis" => Shape::One(RCode),
        "title" | "description" | "details" | "value" | "references" | "note" | "author"
        | "seealso" | "format" | "source" | "arguments" | "keyword" | "concept" | "docType"
        | "encoding" => Shape::One(Latex),
        "section" => Shape::Several(&[Latex, Latex]),
        _ => return None,
    })
}

/// The text being parsed, with a stack of macro expansions read before it.
struct Input {
    characters: Vec<char>,
    index: usize,
    /// Expanded macro text, last character first.
    pushback: Vec<char>,
    next_position: Position,
    last_position: Position,
    at_line_start: bool,
}

impl Input {
    fn new(text: &str) -> Input {
        Input {
            characters: text.chars().collect(),
            index: 0,
            pushback: Vec::new(),
            next_position: Position {
                line: 1,
                byte: 1,
                column: 1,
            },
            last_position: Position {
                line: 1,
                byte: 0,
                column: 0,
            },
            at_line_start: true,
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        let pending = self.pushback.len();
        if offset < pending {
            return self.pushback.get(pending - 1 - offset).copied();
        }
        self.characters.get(self.index + offset - pending).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(offset, character)| self.peek(offset) == Some(character))
    }

    fn next(&mut self) -> Option<char> {
        let character = match self.pushback.pop() {
            Some(character) => character,
            None => {
                let character = *self.characters.get(self.index)?;
                self.index += 1;
                self.last_position = self.next_position;
                self.next_position = if character == '\n' {
                    Position {
                        line: self.next_position.line + 1,
                        byte: 1,
                        column: 1,
                    }
                } else {
                    Position {
                        line: self.next_position.line,
                        byte: self.next_position.byte + character.len_utf8() as i32,
                        column: self.next_position.column + 1,
                    }
                };
                character
            }
        };
        self.at_line_start = character == '\n';
        Some(character)
    }

    fn push_back(&mut self, text: &str) {
        self.pushback.extend(text.chars().rev());
        self.at_line_start = false;
    }
}

/// Characters accumulating into one leaf.
#[derive(Default)]
struct Run {
    text: String,
    first: Option<Position>,
}

struct Parser {
    input: Input,
    fragment: bool,
    basename: String,
    parent_macros: Sexp,
    /// User macros by name, with their argument count and definition: those defined in this
    /// file and those already looked up in the macro environment.
    macros: HashMap<String, (usize, String)>,
    item: ItemContext,
    ifdef_depth: usize,
    /// The most recent leaf, which a syntax error quotes.
    last_text: String,
    expansions: usize,
    warnings: Vec<String>,
}

/// Expansions beyond this many mean a macro that expands to itself.
const MAX_EXPANSIONS: usize = 100_000;

impl Parser {
    fn warn(&mut self, message: impl std::fmt::Display) {
        let line = self.input.next_position.line;
        self.warnings
            .push(format!("{}:{line}: {message}", self.basename));
    }

    fn begin(&self, run: &mut Run) {
        if run.first.is_none() {
            run.first = Some(self.input.next_position);
        }
    }

    fn flush(&mut self, run: &mut Run, mode: Mode, nodes: &mut Vec<Node>) {
        let Some(first) = run.first.take() else {
            return;
        };
        let text = std::mem::take(&mut run.text);
        self.last_text.clone_from(&text);
        nodes.push(Node {
            span: Some(Span {
                first,
                last: self.input.last_position,
            }),
            kind: NodeKind::Leaf {
                tag: mode.leaf_tag(),
                text,
            },
        });
    }

    /// Content in `mode` up to `close`, which it consumes when it is a brace. `top` marks the
    /// top level, where a complete file has its sections.
    unsafe fn content(&mut self, mode: Mode, close: Close, top: bool) -> R<Vec<Node>> {
        let mut nodes = Vec::new();
        let mut run = Run::default();
        let mut quote: Option<char> = None;
        // Set while the text so far ends in a backslash that R would read as escaping the next
        // character of the string, so a quote there does not close the string.
        let mut escaped = false;
        let mut quote_start = self.input.next_position;
        // Inside an R comment `%` and macros are plain text and quotes do not open strings,
        // though braces still nest.
        let mut in_comment = false;
        let mut depth = 0usize;
        let verbatim = matches!(mode, Mode::Verbatim | Mode::RawVerbatim);
        loop {
            if self.input.at_line_start && !verbatim {
                if self.input.starts_with("#ifdef") || self.input.starts_with("#ifndef") {
                    self.flush(&mut run, mode, &mut nodes);
                    let conditional = self.conditional(mode)?;
                    nodes.push(conditional);
                    continue;
                }
                if self.input.starts_with("#endif") {
                    if close == Close::EndIf {
                        self.flush(&mut run, mode, &mut nodes);
                        return Ok(nodes);
                    }
                    self.flush(&mut run, mode, &mut nodes);
                    self.warn("unexpected ENDIF '#endif'");
                    for _ in 0.."#endif".len() {
                        self.input.next();
                    }
                    continue;
                }
            }
            let Some(character) = self.input.peek(0) else {
                self.flush(&mut run, mode, &mut nodes);
                if let Some(quote) = quote {
                    // An R string still open at the end of the input is the one syntax error
                    // GNU R does not recover from.
                    let line = self.input.last_position.line;
                    self.warnings.push(format!(
                        "newline within quoted string at {}:{line}",
                        self.basename
                    ));
                    return Err(error_in(
                        current_call(),
                        format!(
                            "Unexpected end of input (in {quote} quoted string opened at {}:{}:{})",
                            self.basename,
                            quote_start.line,
                            quote_start.column + 1
                        ),
                    ));
                }
                if close != Close::EndOfInput {
                    let text = self.last_text.clone();
                    self.warn(format!("unexpected END_OF_INPUT '{text}'"));
                }
                return Ok(nodes);
            };
            match character {
                '%' if mode != Mode::RawVerbatim && !in_comment => {
                    self.flush(&mut run, mode, &mut nodes);
                    if quote.is_some() {
                        let line = self.input.next_position.line;
                        self.warnings.push(format!(
                            "newline within quoted string at {}:{line}",
                            self.basename
                        ));
                    }
                    let comment = self.comment();
                    nodes.push(comment);
                }
                '\\' => {
                    let next = self.input.peek(1);
                    match mode {
                        Mode::Latex | Mode::RCode
                            if quote.is_none()
                                && !in_comment
                                && next.is_some_and(|next| next.is_ascii_alphabetic()) =>
                        {
                            self.flush(&mut run, mode, &mut nodes);
                            if let Some(node) = self.macro_call(top)? {
                                nodes.push(node);
                            }
                        }
                        Mode::RawVerbatim => {
                            self.begin(&mut run);
                            self.input.next();
                            run.text.push('\\');
                            if let Some(next) = next.filter(|next| *next != '\n') {
                                self.input.next();
                                run.text.push(next);
                            }
                        }
                        _ => {
                            self.begin(&mut run);
                            self.input.next();
                            match next {
                                Some('\\') => {
                                    self.input.next();
                                    run.text.push('\\');
                                    escaped = quote.is_some() && !escaped;
                                    continue;
                                }
                                Some('%') => {
                                    self.input.next();
                                    run.text.push('%');
                                }
                                Some(brace @ ('{' | '}')) => {
                                    self.input.next();
                                    if quote.is_some() {
                                        run.text.push('\\');
                                    }
                                    run.text.push(brace);
                                }
                                // After an escaping backslash, an escaped closing quote drops
                                // its own backslash, which is what GNU R produces.
                                Some(other) if escaped && quote == Some(other) => {
                                    self.input.next();
                                    run.text.push(other);
                                }
                                Some(other) if quote.is_some() && other != '\n' => {
                                    // Inside an R string a backslash escapes the next
                                    // character for R, so an escaped quote never ends it.
                                    self.input.next();
                                    run.text.push('\\');
                                    run.text.push(other);
                                }
                                _ => run.text.push('\\'),
                            }
                            escaped = false;
                        }
                    }
                }
                '{' if mode == Mode::Latex => {
                    self.flush(&mut run, mode, &mut nodes);
                    let first = self.input.next_position;
                    self.input.next();
                    let children = self.content(Mode::Latex, Close::Brace, false)?;
                    nodes.push(Node {
                        span: Some(Span {
                            first,
                            last: self.input.last_position,
                        }),
                        kind: NodeKind::List {
                            tag: Some("LIST".to_string()),
                            option: None,
                            children,
                        },
                    });
                }
                '{' if quote.is_none() => {
                    self.begin(&mut run);
                    self.input.next();
                    depth += 1;
                    run.text.push('{');
                }
                '}' if quote.is_none() && depth > 0 => {
                    self.begin(&mut run);
                    self.input.next();
                    depth -= 1;
                    run.text.push('}');
                }
                '}' if quote.is_none() || mode == Mode::Latex => {
                    self.flush(&mut run, mode, &mut nodes);
                    if close == Close::Brace {
                        self.input.next();
                        return Ok(nodes);
                    }
                    self.warn("unexpected '}'");
                    self.input.next();
                }
                _ => {
                    self.begin(&mut run);
                    self.input.next();
                    run.text.push(character);
                    if mode == Mode::RCode && character == '#' && quote.is_none() {
                        in_comment = true;
                    }
                    if mode == Mode::RCode
                        && matches!(character, '"' | '\'' | '`')
                        && !escaped
                        && !in_comment
                    {
                        if quote.is_none() {
                            quote = Some(character);
                            quote_start = self.input.last_position;
                        } else if quote == Some(character) {
                            quote = None;
                        }
                    }
                    escaped = false;
                    if character == '\n' {
                        in_comment = false;
                        self.flush(&mut run, mode, &mut nodes);
                    }
                }
            }
        }
    }

    fn comment(&mut self) -> Node {
        let first = self.input.next_position;
        let mut text = String::new();
        while let Some(character) = self.input.peek(0).filter(|character| *character != '\n') {
            self.input.next();
            text.push(character);
        }
        self.last_text.clone_from(&text);
        Node {
            span: Some(Span {
                first,
                last: self.input.last_position,
            }),
            kind: NodeKind::Leaf {
                tag: "COMMENT",
                text,
            },
        }
    }

    /// A `#ifdef` or `#ifndef` block: the rest of its line as the condition, then the body up to
    /// `#endif`.
    unsafe fn conditional(&mut self, mode: Mode) -> R<Node> {
        let first = self.input.next_position;
        let tag = if self.input.starts_with("#ifdef") {
            "#ifdef"
        } else {
            "#ifndef"
        };
        for _ in 0..tag.len() {
            self.input.next();
        }
        // GNU R starts the condition's srcref on the directive's last character.
        let mut run = Run {
            text: String::new(),
            first: Some(Position {
                line: self.input.next_position.line,
                byte: self.input.next_position.byte - 1,
                column: self.input.next_position.column - 1,
            }),
        };
        let mut condition = Vec::new();
        while let Some(character) = self.input.peek(0) {
            self.input.next();
            run.text.push(character);
            if character == '\n' {
                break;
            }
        }
        self.flush(&mut run, Mode::Latex, &mut condition);
        self.ifdef_depth += 1;
        let body = self.content(mode, Close::EndIf, false)?;
        self.ifdef_depth -= 1;
        if self.input.starts_with("#endif") {
            for _ in 0.."#endif".len() {
                self.input.next();
            }
            if self.input.peek(0) == Some('\n') {
                self.input.next();
            }
        }
        Ok(Node {
            span: Some(Span {
                first,
                last: self.input.last_position,
            }),
            kind: NodeKind::List {
                tag: Some(tag.to_string()),
                option: None,
                children: vec![argument_list(condition), argument_list(body)],
            },
        })
    }

    /// A backslash macro at the input position. Returns `None` for a
    /// macro that leaves nothing in the tree.
    unsafe fn macro_call(&mut self, top: bool) -> R<Option<Node>> {
        let first = self.input.next_position;
        self.input.next();
        let mut name = String::new();
        while let Some(character) = self.input.peek(0).filter(char::is_ascii_alphanumeric) {
            self.input.next();
            name.push(character);
        }
        let tag = format!("\\{name}");
        if let Some((arguments, definition)) = self.user_macro(&tag)? {
            return Ok(Some(self.expand(first, tag, arguments, definition)?));
        }
        if name == "newcommand" || name == "renewcommand" {
            return Ok(Some(self.definition(first, tag)));
        }
        if name == "RdOpts" {
            if top && !self.fragment {
                return Ok(Some(self.macro_node(
                    first,
                    tag,
                    Shape::One(Mode::Verbatim),
                )?));
            }
            self.warn("unexpected RDOPTS '\\RdOpts'");
            return Ok(None);
        }
        if let Some(shape) = section_shape(&name) {
            if top && !self.fragment {
                return Ok(Some(self.macro_node(first, tag, shape)?));
            }
            self.warn(format!("unexpected section header '{tag}'"));
            return Ok(None);
        }
        let shape = match (name.as_str(), self.item) {
            ("item", ItemContext::Bare) => Some(Shape::Zero),
            ("item", ItemContext::Pair) => Some(Shape::Several(&[Mode::Latex, Mode::Latex])),
            ("item", ItemContext::None) => None,
            _ => builtin_shape(&name),
        };
        match shape {
            Some(shape) => Ok(Some(self.macro_node(first, tag, shape)?)),
            None => {
                if top && !self.fragment {
                    self.warn(format!("unexpected UNKNOWN '{tag}'"));
                } else {
                    self.warn(format!("unknown macro '{tag}'"));
                }
                self.last_text.clone_from(&tag);
                Ok(Some(Node {
                    span: Some(Span {
                        first,
                        last: self.input.last_position,
                    }),
                    kind: NodeKind::Leaf {
                        tag: "UNKNOWN",
                        text: tag,
                    },
                }))
            }
        }
    }

    unsafe fn macro_node(&mut self, first: Position, tag: String, shape: Shape) -> R<Node> {
        let saved_item = self.item;
        self.item = match tag.as_str() {
            "\\itemize" | "\\enumerate" => ItemContext::Bare,
            "\\describe" | "\\arguments" | "\\value" => ItemContext::Pair,
            _ => self.item,
        };
        let mut option = None;
        let children = match shape {
            Shape::Zero => Vec::new(),
            Shape::One(mode) => self.argument(mode, &tag)?.unwrap_or_default(),
            Shape::OptionThenOne(mode) => {
                if self.input.peek(0) == Some('[') {
                    option = Some(Box::new(self.option()));
                }
                self.argument(mode, &tag)?.unwrap_or_default()
            }
            Shape::Several(modes) => {
                let mut lists = Vec::with_capacity(modes.len());
                for &mode in modes {
                    match self.argument(mode, &tag)? {
                        Some(children) => lists.push(argument_list(children)),
                        None => break,
                    }
                }
                lists
            }
            Shape::OptionalSecond(first_mode, second_mode) => {
                let mut lists = Vec::with_capacity(2);
                if let Some(children) = self.argument(first_mode, &tag)? {
                    lists.push(argument_list(children));
                    if self.input.peek(0) == Some('{')
                        && let Some(children) = self.argument(second_mode, &tag)?
                    {
                        lists.push(argument_list(children));
                    }
                }
                lists
            }
        };
        self.item = saved_item;
        Ok(Node {
            span: Some(Span {
                first,
                last: self.input.last_position,
            }),
            kind: NodeKind::List {
                tag: Some(tag),
                option,
                children,
            },
        })
    }

    /// One braced argument in `mode`, or `None` (with a warning) when no brace follows.
    unsafe fn argument(&mut self, mode: Mode, tag: &str) -> R<Option<Vec<Node>>> {
        if self.input.peek(0) != Some('{') {
            self.warn(format!("unexpected macro '{tag}' without its argument"));
            return Ok(None);
        }
        self.input.next();
        Ok(Some(self.content(mode, Close::Brace, false)?))
    }

    /// The `[option]` of `\link` or `\Sexpr`, as one text leaf.
    fn option(&mut self) -> Node {
        self.input.next();
        let mut run = Run::default();
        let mut nodes = Vec::new();
        while let Some(character) = self.input.peek(0) {
            if character == ']' {
                break;
            }
            self.begin(&mut run);
            self.input.next();
            run.text.push(character);
        }
        self.flush(&mut run, Mode::Latex, &mut nodes);
        self.input.next();
        nodes.pop().unwrap_or(Node {
            span: None,
            kind: NodeKind::Leaf {
                tag: "TEXT",
                text: String::new(),
            },
        })
    }

    /// The definition and argument count of the user macro `tag`, from this file's definitions
    /// or the macro environment it was given.
    unsafe fn user_macro(&mut self, tag: &str) -> R<Option<(usize, String)>> {
        if let Some(found) = self.macros.get(tag) {
            return Ok(Some(found.clone()));
        }
        let value = find_var(install(tag)?, self.parent_macros)?;
        if value == unbound() || value.kind() != SexpType::Integer {
            return Ok(None);
        }
        let definition = values::first_string(values::get_attr(value, install("definition")?))
            .map_or_else(String::new, |text| text.into_owned());
        let arguments = (values::as_integer_scalar(value) - COMMAND_BASE).clamp(0, 9) as usize;
        self.macros
            .insert(tag.to_string(), (arguments, definition.clone()));
        Ok(Some((arguments, definition)))
    }

    /// A user macro call: reads its arguments, pushes the substituted definition back into the
    /// input, and returns the call's record.
    unsafe fn expand(
        &mut self,
        first: Position,
        name: String,
        arguments: usize,
        definition: String,
    ) -> R<Node> {
        self.expansions += 1;
        if self.expansions > MAX_EXPANSIONS {
            return Err(error(format!(
                "{}: macro '{name}' expands without end",
                self.basename
            )));
        }
        let mut values = vec![definition.clone()];
        for _ in 0..arguments {
            if self.input.peek(0) == Some('{') {
                values.push(self.raw_braced());
            } else {
                values.push(String::new());
            }
        }
        let mut expansion = String::with_capacity(definition.len());
        let mut characters = definition.chars().peekable();
        while let Some(character) = characters.next() {
            let index = characters.peek().and_then(|next| next.to_digit(10));
            match index {
                Some(index) if character == '#' && index >= 1 => {
                    characters.next();
                    if let Some(value) = values.get(index as usize) {
                        expansion.push_str(value);
                    }
                }
                _ => expansion.push(character),
            }
        }
        let span = Span {
            first,
            last: self.input.last_position,
        };
        self.input.push_back(&expansion);
        Ok(Node {
            span: Some(span),
            kind: NodeKind::UserMacro { name, values },
        })
    }

    /// `\newcommand{\name}{definition}` or `\renewcommand`.
    fn definition(&mut self, first: Position, tag: String) -> Node {
        let name = if self.input.peek(0) == Some('{') {
            self.raw_braced().trim().to_string()
        } else {
            String::new()
        };
        let definition = if self.input.peek(0) == Some('{') {
            self.raw_braced()
        } else {
            self.warn(format!("unexpected macro '{tag}' without its definition"));
            String::new()
        };
        let arguments = definition
            .as_bytes()
            .windows(2)
            .filter(|pair| pair[0] == b'#' && pair[1].is_ascii_digit())
            .map(|pair| usize::from(pair[1] - b'0'))
            .max()
            .unwrap_or(0);
        self.macros
            .insert(name.clone(), (arguments, definition.clone()));
        Node {
            span: Some(Span {
                first,
                last: self.input.last_position,
            }),
            kind: NodeKind::Command {
                tag,
                name,
                arguments,
                definition,
            },
        }
    }

    /// The raw text of a braced group, with nested braces balanced and comments removed, as
    /// user macro arguments and definitions are read.
    fn raw_braced(&mut self) -> String {
        self.input.next();
        let mut depth = 1usize;
        let mut text = String::new();
        while let Some(character) = self.input.next() {
            match character {
                '\\' => {
                    text.push('\\');
                    if let Some(next) = self.input.next() {
                        text.push(next);
                    }
                }
                '%' => {
                    while self.input.peek(0).is_some_and(|next| next != '\n') {
                        self.input.next();
                    }
                }
                '{' => {
                    depth += 1;
                    text.push('{');
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return text;
                    }
                    text.push('}');
                }
                _ => text.push(character),
            }
        }
        let leftover = text.clone();
        self.warn(format!("unexpected END_OF_INPUT '{leftover}'"));
        text
    }
}

fn argument_list(children: Vec<Node>) -> Node {
    Node {
        span: None,
        kind: NodeKind::List {
            tag: None,
            option: None,
            children,
        },
    }
}

/// GNU R stores a macro definition as this number plus the macro's argument count.
const COMMAND_BASE: i32 = 280;

/// Turns parse nodes into R objects, defining each user macro in the result's environment.
struct Builder {
    srcfile: Sexp,
    env: Sexp,
    rd_tag: Sexp,
    rd_option: Sexp,
    srcref: Sexp,
    dynamic_flag: Sexp,
    definition: Sexp,
    macro_symbol: Sexp,
    srcref_class: Sexp,
}

impl Builder {
    unsafe fn new(srcfile: Sexp, env: Sexp) -> R<Builder> {
        Ok(Builder {
            srcfile,
            env,
            rd_tag: install("Rd_tag")?,
            rd_option: install("Rd_option")?,
            srcref: values::symbols().srcref,
            dynamic_flag: install("dynamicFlag")?,
            definition: install("definition")?,
            macro_symbol: install("macro")?,
            srcref_class: scalar_string("srcref")?,
        })
    }

    /// The R object for `node` and its dynamic flags: 1 for a `#ifdef` inside it, 2 for a
    /// `\Sexpr`, which `tools` uses to find the parts it must process further.
    unsafe fn build(&self, node: &Node) -> R<(Sexp, i32)> {
        match &node.kind {
            NodeKind::Leaf { tag, text } => {
                let object = scalar_string(text)?;
                values::set_attr(object, self.rd_tag, scalar_string(tag)?)?;
                self.attach_srcref(object, node.span)?;
                Ok((object, 0))
            }
            NodeKind::List {
                tag,
                option,
                children,
            } => {
                let object = alloc(SexpType::List, children.len())?;
                let mut flag = match tag.as_deref() {
                    Some("#ifdef" | "#ifndef") => 1,
                    Some("\\Sexpr") => 2,
                    _ => 0,
                };
                for (index, child) in children.iter().enumerate() {
                    let (value, child_flag) = self.build(child)?;
                    values::set_elt(object, index, value);
                    flag |= child_flag;
                }
                if let Some(tag) = tag {
                    values::set_attr(object, self.rd_tag, scalar_string(tag)?)?;
                }
                if let Some(option) = option {
                    let (value, _) = self.build(option)?;
                    values::set_attr(object, self.rd_option, value)?;
                }
                self.attach_srcref(object, node.span)?;
                if flag != 0 {
                    values::set_attr(object, self.dynamic_flag, scalar_integer(flag)?)?;
                }
                Ok((object, flag))
            }
            NodeKind::Command {
                tag,
                name,
                arguments,
                definition,
            } => {
                let object = scalar_integer(COMMAND_BASE + *arguments as i32)?;
                values::set_attr(object, self.rd_tag, scalar_string(tag)?)?;
                values::set_attr(object, self.definition, scalar_string(definition)?)?;
                self.attach_srcref(object, node.span)?;
                define(install(name)?, object, self.env)?;
                Ok((object, 0))
            }
            NodeKind::UserMacro {
                name,
                values: macro_values,
            } => {
                let object = string_vector(macro_values)?;
                values::set_attr(object, self.rd_tag, scalar_string("USERMACRO")?)?;
                self.attach_srcref(object, node.span)?;
                values::set_attr(object, self.macro_symbol, scalar_string(name)?)?;
                Ok((object, 0))
            }
        }
    }

    unsafe fn attach_srcref(&self, object: Sexp, span: Option<Span>) -> R<()> {
        let Some(Span { first, last }) = span else {
            return Ok(());
        };
        let fields = [
            first.line,
            first.byte,
            last.line,
            last.byte,
            first.column,
            last.column,
        ];
        values::set_attr(object, self.srcref, self.srcref_vector(fields)?)
    }

    unsafe fn srcref_vector(&self, fields: [i32; 6]) -> R<Sexp> {
        let srcref = integer_vector(&fields)?;
        values::set_attr(srcref, install("srcfile")?, self.srcfile)?;
        values::set_attr(srcref, values::symbols().class, self.srcref_class)?;
        Ok(srcref)
    }
}
