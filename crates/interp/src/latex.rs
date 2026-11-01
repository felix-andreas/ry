//! `tools::parseLatex`: the LaTeX subset that R's documentation tools read, such as the
//! bibliography fields that `tools::latexToUtf8` and `tools::deparseLatex` work on.
//!
//! The parser builds the same tagged list tree as GNU R. Leaves are `TEXT`, `MACRO`,
//! `COMMENT` and `VERB` strings, and groups are `BLOCK` (`{...}`), `MATH` (`$...$`) and
//! `ENVIRONMENT` (`\begin{name}...\end{name}`, a list of the name and the contents). Every
//! node carries a `latex_tag` attribute and a srcref into the srcfile the R wrapper made.
//!
//! The srcrefs follow GNU R's lexer exactly, quirks included, because `deparseLatex` and
//! user code read them back: a letter macro swallows one following space into its srcref
//! but not its text, and a verbatim environment's contents end on the backslash of `\end`.

use crate::{
    builtin_libraries::NativeCall,
    conditions,
    eval::find_var,
    unwind::{R, current_call, error},
    values::{self, alloc, install, integer_vector, list_vector, scalar_string, unbound},
};
use heap::object::{Sexp, SexpType};

/// `parseLatex(text, srcfile, verbose, verbatim)`: the LaTeX tree of `text`, one string.
pub unsafe fn parse_latex(call: &NativeCall<'_>) -> R<Sexp> {
    let text = values::first_string(call.arg(0)).map_or_else(String::new, |text| text.into_owned());
    let srcfile = call.arg(1);
    let verbatim: Vec<String> = values::strings(call.arg(3)).into_iter().flatten().collect();
    let mut parser = Parser {
        input: Input::new(&text),
        verbatim,
        last_value: String::new(),
    };
    let parsed = parser.items(Close::EndOfInput);
    let builder = Builder::new(srcfile)?;
    let nodes = match parsed {
        Ok(nodes) => nodes,
        Err(syntax) => {
            let filename = if srcfile.kind() == SexpType::Environment {
                let value = find_var(install("filename")?, srcfile)?;
                if value == unbound() {
                    String::new()
                } else {
                    values::first_string(value).map_or_else(String::new, |name| name.into_owned())
                }
            } else {
                String::new()
            };
            conditions::warning_call(
                current_call(),
                format!("{filename}:{}: {}", syntax.line, syntax.message),
            )?;
            return Err(error(format!(" in \"{}\"", syntax.context)));
        }
    };
    let end = parser.input.next;
    let whole = [1, 1, end.line, end.byte, 1, end.column];
    if nodes.is_empty() {
        // GNU R represents empty input as one empty text leaf without a srcref, and leaves the
        // result unclassed.
        let leaf = scalar_string("")?;
        values::set_attr(leaf, builder.latex_tag, scalar_string("TEXT")?)?;
        let result = list_vector(&[leaf])?;
        values::set_attr(
            result,
            values::symbols().srcref,
            builder.srcref_vector(whole)?,
        )?;
        return Ok(result);
    }
    let result = builder.list(&nodes)?;
    values::set_attr(result, values::symbols().class, scalar_string("LaTeX")?)?;
    values::set_attr(
        result,
        values::symbols().srcref,
        builder.srcref_vector(whole)?,
    )?;
    Ok(result)
}

struct Node {
    kind: NodeKind,
    first: Position,
    last: Position,
}

enum NodeKind {
    Leaf {
        tag: &'static str,
        text: String,
    },
    Group {
        tag: &'static str,
        children: Vec<Node>,
    },
    Environment {
        name: String,
        children: Vec<Node>,
    },
}

/// Where one character sits: its line, the first and last of its bytes within the line, and
/// its column, with tabs advancing to the next multiple of eight as in R's srcrefs.
#[derive(Clone, Copy)]
struct Position {
    line: i32,
    byte: i32,
    last_byte: i32,
    column: i32,
}

struct Input {
    characters: Vec<char>,
    index: usize,
    /// Where the next character starts.
    next: Position,
    /// The character read most recently.
    last: Position,
}

impl Input {
    fn new(text: &str) -> Input {
        let start = Position {
            line: 1,
            byte: 1,
            last_byte: 1,
            column: 1,
        };
        Input {
            characters: text.chars().collect(),
            index: 0,
            next: start,
            last: start,
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.characters.get(self.index + offset).copied()
    }

    fn next(&mut self) -> Option<char> {
        let character = *self.characters.get(self.index)?;
        self.index += 1;
        let width = character.len_utf8() as i32;
        self.last = Position {
            last_byte: self.next.byte + width - 1,
            ..self.next
        };
        self.next = match character {
            '\n' => Position {
                line: self.next.line + 1,
                byte: 1,
                last_byte: 1,
                column: 1,
            },
            '\t' => Position {
                byte: self.next.byte + 1,
                last_byte: self.next.byte + 1,
                column: ((self.next.column - 1 + 8) / 8) * 8 + 1,
                ..self.next
            },
            _ => Position {
                byte: self.next.byte + width,
                last_byte: self.next.byte + width,
                column: self.next.column + 1,
                ..self.next
            },
        };
        Some(character)
    }

    fn consumed(&self, from: usize) -> String {
        self.characters
            .get(from..self.index)
            .map_or_else(String::new, |characters| characters.iter().collect())
    }
}

/// What ends a sequence of items.
#[derive(Clone, Copy, PartialEq)]
enum Close {
    EndOfInput,
    Brace,
    Dollar,
    End,
}

/// A syntax error: GNU R warns with the message and its line, then fails quoting the input
/// read so far.
struct SyntaxError {
    line: i32,
    message: String,
    context: String,
}

struct Parser {
    input: Input,
    verbatim: Vec<String>,
    /// The most recent text value, which an unexpected end of input quotes.
    last_value: String,
}

impl Parser {
    fn items(&mut self, close: Close) -> Result<Vec<Node>, SyntaxError> {
        let mut nodes = Vec::new();
        loop {
            let Some(character) = self.input.peek(0) else {
                if close == Close::EndOfInput {
                    return Ok(nodes);
                }
                return Err(self.end_of_input(&nodes));
            };
            let first = self.input.next;
            match character {
                '}' => {
                    self.input.next();
                    if close == Close::Brace {
                        return Ok(nodes);
                    }
                    return Err(self.unexpected("'}'"));
                }
                '$' => {
                    self.input.next();
                    if close == Close::Dollar {
                        return Ok(nodes);
                    }
                    if self.input.peek(0) == Some('$') {
                        self.input.next();
                        return Err(self.unexpected("'$'"));
                    }
                    let children = self.items(Close::Dollar)?;
                    nodes.push(self.node(
                        NodeKind::Group {
                            tag: "MATH",
                            children,
                        },
                        first,
                    ));
                }
                '{' => {
                    self.input.next();
                    let children = self.items(Close::Brace)?;
                    nodes.push(self.node(
                        NodeKind::Group {
                            tag: "BLOCK",
                            children,
                        },
                        first,
                    ));
                }
                '%' => {
                    let mut text = String::new();
                    while let Some(character) = self.input.next() {
                        text.push(character);
                        if character == '\n' {
                            break;
                        }
                    }
                    nodes.push(self.node(
                        NodeKind::Leaf {
                            tag: "COMMENT",
                            text,
                        },
                        first,
                    ));
                }
                '\\' => {
                    self.input.next();
                    let mut name = String::from('\\');
                    while let Some(letter) = self.input.peek(0).filter(char::is_ascii_alphabetic) {
                        self.input.next();
                        name.push(letter);
                    }
                    match name.as_str() {
                        "\\" => {
                            if let Some(other) = self.input.next() {
                                name.push(other);
                            }
                            nodes.push(self.node(
                                NodeKind::Leaf {
                                    tag: "MACRO",
                                    text: name,
                                },
                                first,
                            ));
                        }
                        "\\begin" => nodes.push(self.environment(first)?),
                        "\\end" => {
                            if close == Close::End && !nodes.is_empty() {
                                return Ok(nodes);
                            }
                            return Err(self.unexpected("END '\\end'"));
                        }
                        "\\verb" => {
                            let mut text = name;
                            if self.input.peek(0) == Some('*') {
                                self.input.next();
                                text.push('*');
                            }
                            if let Some(delimiter) = self.input.next() {
                                text.push(delimiter);
                                while let Some(character) = self.input.next() {
                                    text.push(character);
                                    if character == delimiter {
                                        break;
                                    }
                                }
                            }
                            nodes.push(self.node(NodeKind::Leaf { tag: "VERB", text }, first));
                        }
                        _ => {
                            if self.input.peek(0) == Some(' ') {
                                self.input.next();
                            }
                            nodes.push(self.node(
                                NodeKind::Leaf {
                                    tag: "MACRO",
                                    text: name,
                                },
                                first,
                            ));
                        }
                    }
                }
                _ => {
                    let mut text = String::new();
                    while let Some(character) = self
                        .input
                        .peek(0)
                        .filter(|character| !matches!(character, '\\' | '{' | '}' | '$' | '%'))
                    {
                        self.input.next();
                        text.push(character);
                    }
                    self.last_value.clone_from(&text);
                    nodes.push(self.node(NodeKind::Leaf { tag: "TEXT", text }, first));
                }
            }
        }
    }

    /// `\begin{name}` has been read up to `name`'s brace; reads the rest of the environment.
    fn environment(&mut self, first: Position) -> Result<Node, SyntaxError> {
        let name = self.braced_name()?;
        self.last_value.clone_from(&name);
        let children = if self.verbatim.contains(&name) {
            let contents_first = self.input.next;
            let closing: Vec<char> = format!("\\end{{{name}}}").chars().collect();
            let mut text = String::new();
            loop {
                if self.input.peek(0).is_none() {
                    return Err(self.end_of_input(&[]));
                }
                let at_end = closing
                    .iter()
                    .enumerate()
                    .all(|(offset, character)| self.input.peek(offset) == Some(*character));
                if at_end {
                    break;
                }
                if let Some(character) = self.input.next() {
                    text.push(character);
                }
            }
            let backslash = self.input.next;
            for _ in 0..closing.len() {
                self.input.next();
            }
            // GNU R's verbatim lexer reads one character past `\end{name}` and drops it, or
            // counts a phantom one at the end of the input.
            if self.input.next().is_none() {
                let next = self.input.next;
                self.input.last = next;
                self.input.next = Position {
                    byte: next.byte + 1,
                    last_byte: next.byte + 1,
                    column: next.column + 1,
                    ..next
                };
            }
            vec![Node {
                kind: NodeKind::Leaf { tag: "VERB", text },
                first: contents_first,
                last: backslash,
            }]
        } else {
            let children = self.items(Close::End)?;
            self.braced_name()?;
            children
        };
        Ok(self.node(NodeKind::Environment { name, children }, first))
    }

    /// The `{name}` after `\begin` or `\end`, which may follow spaces.
    fn braced_name(&mut self) -> Result<String, SyntaxError> {
        while matches!(self.input.peek(0), Some(' ' | '\t')) {
            self.input.next();
        }
        match self.input.next() {
            Some('{') => {}
            Some(other) => return Err(self.unexpected(&format!("'{other}'"))),
            None => return Err(self.end_of_input(&[])),
        }
        let mut name = String::new();
        loop {
            match self.input.next() {
                Some('}') => return Ok(name),
                Some(character) => name.push(character),
                None => return Err(self.end_of_input(&[])),
            }
        }
    }

    fn node(&self, kind: NodeKind, first: Position) -> Node {
        Node {
            kind,
            first,
            last: self.input.last,
        }
    }

    fn unexpected(&self, token: &str) -> SyntaxError {
        SyntaxError {
            line: self.input.next.line,
            message: format!("unexpected {token}"),
            context: self.input.consumed(0),
        }
    }

    /// GNU R's lexer has not yet delivered a trailing text run when it meets the end of the
    /// input, so the quoted context stops before that run.
    fn end_of_input(&self, nodes: &[Node]) -> SyntaxError {
        let mut context = self.input.consumed(0);
        if let Some(Node {
            kind: NodeKind::Leaf { tag: "TEXT", text },
            ..
        }) = nodes.last()
        {
            let keep = context.chars().count().saturating_sub(text.chars().count());
            context = context.chars().take(keep).collect();
        }
        SyntaxError {
            line: self.input.next.line,
            message: format!("unexpected END_OF_INPUT '{}'", self.last_value),
            context,
        }
    }
}

struct Builder {
    srcfile: Sexp,
    latex_tag: Sexp,
    srcfile_symbol: Sexp,
    srcref_class: Sexp,
}

impl Builder {
    unsafe fn new(srcfile: Sexp) -> R<Builder> {
        Ok(Builder {
            srcfile,
            latex_tag: install("latex_tag")?,
            srcfile_symbol: install("srcfile")?,
            srcref_class: scalar_string("srcref")?,
        })
    }

    unsafe fn list(&self, nodes: &[Node]) -> R<Sexp> {
        let list = alloc(SexpType::List, nodes.len())?;
        for (index, node) in nodes.iter().enumerate() {
            values::set_elt(list, index, self.build(node)?);
        }
        Ok(list)
    }

    /// Leaves carry their tag before their srcref and groups the other way round, matching
    /// the attribute order of GNU R's trees.
    unsafe fn build(&self, node: &Node) -> R<Sexp> {
        let srcref = self.srcref_vector([
            node.first.line,
            node.first.byte,
            node.last.line,
            node.last.last_byte,
            node.first.column,
            node.last.column,
        ])?;
        let symbols = values::symbols();
        match &node.kind {
            NodeKind::Leaf { tag, text } => {
                let object = scalar_string(text)?;
                values::set_attr(object, self.latex_tag, scalar_string(tag)?)?;
                values::set_attr(object, symbols.srcref, srcref)?;
                Ok(object)
            }
            NodeKind::Group { tag, children } => {
                let object = self.list(children)?;
                values::set_attr(object, symbols.srcref, srcref)?;
                values::set_attr(object, self.latex_tag, scalar_string(tag)?)?;
                Ok(object)
            }
            NodeKind::Environment { name, children } => {
                let object = list_vector(&[scalar_string(name)?, self.list(children)?])?;
                values::set_attr(object, symbols.srcref, srcref)?;
                values::set_attr(object, self.latex_tag, scalar_string("ENVIRONMENT")?)?;
                Ok(object)
            }
        }
    }

    unsafe fn srcref_vector(&self, fields: [i32; 6]) -> R<Sexp> {
        let srcref = integer_vector(&fields)?;
        values::set_attr(srcref, self.srcfile_symbol, self.srcfile)?;
        values::set_attr(srcref, values::symbols().class, self.srcref_class)?;
        Ok(srcref)
    }
}
