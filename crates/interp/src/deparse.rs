//! Turning R values back into R source: `deparse`, `dput`, and the text error messages and
//! `print` show for calls and functions. The layout follows GNU R's deparser: operators by
//! precedence with the parentheses they need, a line break after a separator once a line is
//! longer than the cutoff, and four spaces per level of indentation.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    format, records, standard_library,
    unwind::R,
    values::{self, char_str, get_attr, length, nil, symbol_name, symbols},
};
use heap::{
    object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp, is_na_real},
    records::NativeKind,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("deparse", 5, do_deparse),
    internal("dput", 3, do_dput).invisible(),
    internal("dump", 5, do_dump).invisible(),
];

/// Deparse options, GNU R's bit values.
pub mod options {
    pub const KEEP_INTEGER: u32 = 1;
    pub const QUOTE_EXPRESSIONS: u32 = 2;
    pub const SHOW_ATTRIBUTES: u32 = 4;
    pub const USE_SOURCE: u32 = 8;
    pub const KEEP_NA: u32 = 64;
    pub const HEX_NUMERIC: u32 = 256;
    pub const DIGITS17: u32 = 512;
    pub const NICE_NAMES: u32 = 1024;
    /// `c("keepNA", "keepInteger", "niceNames", "showAttributes")`, the default of `deparse`.
    pub const DEFAULT: u32 = KEEP_NA | KEEP_INTEGER | NICE_NAMES | SHOW_ATTRIBUTES;
    /// What error messages and `print` of a call use.
    pub const SIMPLE: u32 = KEEP_INTEGER | KEEP_NA | NICE_NAMES;
}

/// Deparses to lines at the default cutoff.
pub unsafe fn deparse_lines(object: Sexp, cutoff: usize, opts: u32) -> Vec<String> {
    deparse_lines_with(object, cutoff, opts, true)
}

/// [`deparse_lines`], where `backtick` says whether a non-syntactic name is written in backticks
/// (`deparse(backtick = )`) or, for names and symbols outside calls, in double quotes.
pub unsafe fn deparse_lines_with(object: Sexp, cutoff: usize, opts: u32, backtick: bool) -> Vec<String> {
    let mut deparser = Deparser::new(cutoff, opts);
    deparser.backtick = backtick;
    deparser.deparse(object);
    deparser.finish()
}

/// The first line of a deparse, as GNU R's `deparse1s` gives it to error and warning messages.
pub unsafe fn deparse_first_line(object: Sexp) -> String {
    deparse_lines(object, 60, options::SIMPLE).into_iter().next().unwrap_or_default()
}

/// A deparse joined into one line, as GNU R's `deparse1line` gives it to coercion.
pub unsafe fn deparse_one_line(object: Sexp) -> String {
    let lines = deparse_lines(object, 60, options::SIMPLE);
    if lines.len() <= 1 {
        return lines.into_iter().next().unwrap_or_default();
    }
    lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Binary,
    Binary2,
    Unary,
    Assign,
    Dollar,
    Subset,
    Other,
}

/// GNU R's precedences.
mod precedence {
    pub const FUNCTION: u8 = 0;
    pub const EQ: u8 = 1;
    pub const LEFT: u8 = 2;
    pub const TILDE: u8 = 4;
    pub const OR: u8 = 5;
    pub const AND: u8 = 6;
    pub const NOT: u8 = 7;
    pub const COMPARE: u8 = 8;
    pub const SUM: u8 = 9;
    pub const PRODUCT: u8 = 10;
    pub const PERCENT: u8 = 11;
    pub const COLON: u8 = 12;
    pub const SIGN: u8 = 13;
    pub const POWER: u8 = 14;
    pub const SUBSET: u8 = 15;
    pub const DOLLAR: u8 = 15;
    pub const NAMESPACE: u8 = 16;
}

#[derive(Clone, Copy, Debug)]
struct OperatorInfo {
    kind: Kind,
    precedence: u8,
    right_associative: bool,
}

fn operator_info(name: &[u8]) -> Option<OperatorInfo> {
    use precedence::*;
    let (kind, precedence, right_associative) = match name {
        b"+" | b"-" => (Kind::Binary, SUM, false),
        b"*" => (Kind::Binary, PRODUCT, false),
        b"/" => (Kind::Binary2, PRODUCT, false),
        b"^" => (Kind::Binary2, POWER, true),
        b"%%" | b"%/%" => (Kind::Binary2, PERCENT, false),
        b"%*%" => (Kind::Binary, PERCENT, false),
        b":" => (Kind::Binary2, COLON, false),
        b"==" | b"!=" | b"<" | b">" | b"<=" | b">=" => (Kind::Binary, COMPARE, false),
        b"!" => (Kind::Unary, NOT, false),
        b"&" | b"&&" => (Kind::Binary, AND, false),
        b"|" | b"||" => (Kind::Binary, OR, false),
        b"~" => (Kind::Binary, TILDE, false),
        b"<-" | b"<<-" => (Kind::Assign, LEFT, true),
        b"=" => (Kind::Assign, EQ, true),
        b"$" | b"@" => (Kind::Dollar, DOLLAR, false),
        b"[" | b"[[" => (Kind::Subset, SUBSET, false),
        b"::" | b":::" => (Kind::Binary2, NAMESPACE, false),
        b"?" => (Kind::Binary, 3, false),
        _ => return None,
    };
    Some(OperatorInfo {
        kind,
        precedence,
        right_associative,
    })
}

fn is_user_binop(name: &[u8]) -> bool {
    name.len() >= 2 && name[0] == b'%' && name[name.len() - 1] == b'%'
}

/// Whether `name` can be written without backticks.
pub fn is_syntactic(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if syntax::kind::SyntaxKind::from_keyword(name).is_some() {
        return false;
    }
    if name == "..." {
        return true;
    }
    if let Some(digits) = name.strip_prefix("..")
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return true;
    }
    let mut characters = name.chars();
    let first = characters.next().unwrap_or(' ');
    if !(first.is_alphabetic() || first == '.') {
        return false;
    }
    if first == '.' {
        if let Some(second) = name.chars().nth(1)
            && second.is_ascii_digit()
        {
            return false;
        }
    }
    name.chars()
        .all(|character| character.is_alphanumeric() || character == '.' || character == '_')
}

/// A symbol's name, with backticks when it is not syntactic.
pub fn quote_name(name: &str) -> String {
    if is_syntactic(name) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
    }
}

struct Deparser {
    lines: Vec<String>,
    line: String,
    /// Characters on the current line, which the cutoff is measured against.
    length: usize,
    indent: usize,
    start_of_line: bool,
    cutoff: usize,
    opts: u32,
    in_curly: usize,
    in_list: usize,
    depth: usize,
    backtick: bool,
}

impl Deparser {
    fn new(cutoff: usize, opts: u32) -> Deparser {
        Deparser {
            lines: Vec::new(),
            line: String::new(),
            length: 0,
            indent: 0,
            start_of_line: true,
            cutoff,
            opts,
            in_curly: 0,
            in_list: 0,
            backtick: true,
            depth: 0,
        }
    }

    fn finish(mut self) -> Vec<String> {
        if !self.line.is_empty() || self.lines.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
        }
        self.lines
    }

    fn print(&mut self, text: &str) {
        if self.start_of_line {
            self.start_of_line = false;
            for level in 1..=self.indent {
                let tab = if level <= 4 { "    " } else { "  " };
                self.line.push_str(tab);
                self.length += tab.len();
            }
        }
        self.line.push_str(text);
        self.length += text.chars().count();
    }

    fn write_line(&mut self) {
        self.lines.push(std::mem::take(&mut self.line));
        self.length = 0;
        self.start_of_line = true;
    }

    /// GNU R's `linebreak`: breaks the line once it is past the cutoff, indenting the rest of
    /// the construct one level.
    fn line_break(&mut self, broken: &mut bool) {
        if self.length > self.cutoff {
            if !*broken {
                *broken = true;
                self.indent += 1;
            }
            self.write_line();
        }
    }

    unsafe fn deparse(&mut self, object: Sexp) {
        self.depth += 1;
        if self.depth > 500 {
            self.print("...");
            self.depth -= 1;
            return;
        }
        if object.has_gp(gp::S4_OBJECT)
            && let Some(parts) = s4_parts(object)
        {
            self.s4(parts);
            self.depth -= 1;
            return;
        }
        // An expression vector written from its source text shows none of its attributes.
        let attributes = self.opts & options::SHOW_ATTRIBUTES != 0
            && has_shown_attributes(object, self.opts & options::NICE_NAMES != 0)
            && self.expression_srcrefs(object).is_none();
        if attributes {
            self.print("structure(");
        }
        self.deparse_inner(object);
        if attributes {
            self.attributes(object);
            self.print(")");
        }
        self.depth -= 1;
    }

    unsafe fn deparse_inner(&mut self, object: Sexp) {
        match object.kind() {
            SexpType::Nil => self.print("NULL"),
            SexpType::Symbol => {
                let name = symbol_name(object);
                if self.opts & options::QUOTE_EXPRESSIONS != 0 {
                    self.print("quote(");
                    self.print(&quote_name(&name));
                    self.print(")");
                } else if name.is_empty() {
                    // The empty symbol, `R_MissingArg`.
                } else if self.backtick {
                    self.print(&quote_name(&name));
                } else {
                    // `backtick = FALSE`, the default for a symbol, prints a non-syntactic name bare.
                    self.print(&name);
                }
            }
            SexpType::Closure => self.closure(object),
            SexpType::Builtin | SexpType::Special => {
                let name = values::primitive_name(object);
                self.print(&format!(".Primitive(\"{name}\")"));
            }
            SexpType::Environment => self.print("<environment>"),
            SexpType::Promise => self.print("<promise>"),
            SexpType::ExternalPtr => self.print("<pointer: 0x0>"),
            SexpType::WeakRef => self.print("<weak reference>"),
            SexpType::Bytecode => self.print("<bytecode>"),
            SexpType::Char => self.print(&format::encode_element(object, Some('"'))),
            SexpType::Language => self.language(object),
            SexpType::Pairlist | SexpType::Dots => {
                self.print("pairlist(");
                self.in_list += 1;
                self.arguments(object, false);
                self.in_list -= 1;
                self.print(")");
            }
            SexpType::List => self.list(object, "list("),
            SexpType::Expression => match self.expression_srcrefs(object) {
                Some(srcrefs) => self.expression_from_source(object, srcrefs),
                None => {
                    // The elements of `expression()` are code, which its call does not evaluate.
                    let saved = self.opts;
                    self.opts &= !options::QUOTE_EXPRESSIONS;
                    self.list(object, "expression(");
                    self.opts = saved;
                }
            },
            SexpType::Object => self.print("<S4 object of class structure>"),
            SexpType::Record => self.native(object),
            kind if kind.is_atomic() => self.vector(object),
            _ => self.print("<unknown>"),
        }
    }

    /// An S4 object as the `new()` call that recreates it: its slots by name, then the data
    /// part of a class that extends a basic type without a `.Data` slot.
    unsafe fn s4(&mut self, parts: S4Parts) {
        self.print("new(");
        self.print(&format::encode_string(&parts.class, Some('"')));
        for (name, value) in parts.slots {
            self.print(", ");
            self.print(&quote_name(&name));
            self.print(" = ");
            self.deparse(value);
        }
        if let Some(data) = parts.data {
            self.print(", ");
            self.deparse(data);
        }
        self.print(")");
    }

    /// The `srcref` list an expression vector is written from under `useSource`: one srcref per
    /// element.
    unsafe fn expression_srcrefs(&self, object: Sexp) -> Option<Sexp> {
        if self.opts & options::USE_SOURCE == 0 || object.kind() != SexpType::Expression {
            return None;
        }
        let srcrefs = get_attr(object, symbols().srcref);
        (srcrefs.kind() == SexpType::List && length(srcrefs) == length(object)).then_some(srcrefs)
    }

    unsafe fn expression_from_source(&mut self, object: Sexp, srcrefs: Sexp) {
        self.print("expression(");
        for index in 0..length(object) {
            if index > 0 {
                self.print(", ");
            }
            match source_lines(srcrefs.element(index)) {
                Some(lines) => self.source(&lines),
                None => self.deparse(object.element(index)),
            }
        }
        self.print(")");
    }

    /// Writes source text as it is, line by line.
    fn source(&mut self, lines: &[String]) {
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                self.write_line();
            }
            self.print(line);
        }
    }

    unsafe fn closure(&mut self, function: Sexp) {
        if self.opts & options::USE_SOURCE != 0 {
            if let Some(lines) = source_lines(get_attr(function, symbols().srcref)) {
                self.source(&lines);
                return;
            }
        }
        let saved = self.opts;
        self.opts &= !options::QUOTE_EXPRESSIONS;
        self.print("function (");
        self.formals(function.car());
        self.print(") ");
        self.write_line();
        self.deparse(crate::bytecode::closure_body(function));
        self.opts = saved;
    }

    unsafe fn formals(&mut self, formals: Sexp) {
        let mut broken = false;
        let mut cell = formals;
        while cell.kind() == SexpType::Pairlist {
            let name = symbol_name(cell.tag());
            self.print(&quote_name(&name));
            let default = cell.car();
            if default != values::missing_arg() {
                self.print(" = ");
                self.deparse(default);
            }
            cell = cell.cdr();
            if cell.kind() == SexpType::Pairlist {
                self.print(", ");
                self.line_break(&mut broken);
            }
        }
        if broken {
            self.indent -= 1;
        }
    }

    /// The arguments of a call: `tag = value`, with GNU R's line breaks.
    unsafe fn arguments(&mut self, mut list: Sexp, formals: bool) {
        let mut broken = false;
        while matches!(list.kind(), SexpType::Pairlist | SexpType::Language | SexpType::Dots) {
            let tag = list.tag();
            if tag != nil() && tag.kind() == SexpType::Symbol {
                let name = symbol_name(tag);
                if name.is_empty() {
                    self.print("``");
                } else {
                    self.print(&quote_name(&name));
                }
                if formals {
                    if list.car() != values::missing_arg() {
                        self.print(" = ");
                        self.deparse(list.car());
                    }
                } else {
                    self.print(" = ");
                    if list.car() != values::missing_arg() {
                        self.deparse(list.car());
                    }
                }
            } else {
                self.deparse(list.car());
            }
            list = list.cdr();
            if matches!(list.kind(), SexpType::Pairlist | SexpType::Language | SexpType::Dots) {
                self.print(", ");
                self.line_break(&mut broken);
            }
        }
        if broken {
            self.indent -= 1;
        }
    }

    unsafe fn language(&mut self, call: Sexp) {
        // `quote()` wraps the outermost call once, and a formula evaluates to itself, so it needs
        // no wrapping at all.
        if self.opts & options::QUOTE_EXPRESSIONS != 0 {
            self.opts &= !options::QUOTE_EXPRESSIONS;
            let formula = call.car() == symbols().tilde;
            if !formula {
                self.print("quote(");
            }
            self.language(call);
            if !formula {
                self.print(")");
            }
            self.opts |= options::QUOTE_EXPRESSIONS;
            return;
        }
        let head = call.car();
        let arguments = call.cdr();
        let argument_count = values::pairlist_length(arguments);
        if head.kind() == SexpType::Symbol {
            let name_text = symbol_name(head).into_owned();
            let name = name_text.as_bytes();
            let symbols = symbols();
            if head == symbols.brace {
                self.curly(arguments);
                return;
            }
            if head == symbols.paren && argument_count == 1 {
                self.print("(");
                self.deparse(arguments.car());
                self.print(")");
                return;
            }
            if head == symbols.if_ && (2..=3).contains(&argument_count) {
                self.if_expression(arguments);
                return;
            }
            if head == symbols.for_ && argument_count == 3 {
                self.print("for (");
                self.deparse(arguments.car());
                self.print(" in ");
                self.deparse(arguments.cdr().car());
                self.print(") ");
                self.deparse(arguments.cdr().cdr().car());
                return;
            }
            if head == symbols.while_ && argument_count == 2 {
                self.print("while (");
                self.deparse(arguments.car());
                self.print(") ");
                self.deparse(arguments.cdr().car());
                return;
            }
            if head == symbols.repeat && argument_count == 1 {
                self.print("repeat ");
                self.deparse(arguments.car());
                return;
            }
            if let Some(form) = ry_form(name, arguments) {
                self.ry_call(form, arguments);
                return;
            }
            if head == symbols.break_ && argument_count == 0 {
                self.print("break");
                return;
            }
            if head == symbols.next && argument_count == 0 {
                self.print("next");
                return;
            }
            if head == symbols.function && argument_count >= 2 {
                self.print("function(");
                let formals = arguments.car();
                self.formals(formals);
                self.print(") ");
                self.deparse(arguments.cdr().car());
                return;
            }
            if let Some(info) = operator_info(name) {
                match (info.kind, argument_count) {
                    (Kind::Subset, count) if count >= 1 => {
                        let (open, close) = if name == b"[" { ("[", "]") } else { ("[[", "]]") };
                        let target = arguments.car();
                        let parens = needs_parens(info, target, true);
                        if parens {
                            self.print("(");
                        }
                        self.deparse(target);
                        if parens {
                            self.print(")");
                        }
                        self.print(open);
                        self.arguments(arguments.cdr(), false);
                        self.print(close);
                        return;
                    }
                    (Kind::Dollar, 2) => {
                        let target = arguments.car();
                        let parens = needs_parens(info, target, true);
                        if parens {
                            self.print("(");
                        }
                        self.deparse(target);
                        if parens {
                            self.print(")");
                        }
                        self.print(&name_text);
                        let member = arguments.cdr().car();
                        if member.kind() == SexpType::String && length(member) == 1 {
                            self.vector(member);
                        } else {
                            self.deparse(member);
                        }
                        return;
                    }
                    (Kind::Binary | Kind::Binary2 | Kind::Assign, 2) => {
                        let left = arguments.car();
                        let right = arguments.cdr().car();
                        let left_parens = needs_parens(info, left, true);
                        if left_parens {
                            self.print("(");
                        }
                        self.deparse(left);
                        if left_parens {
                            self.print(")");
                        }
                        if info.kind == Kind::Binary2 {
                            self.print(&name_text);
                        } else {
                            self.print(" ");
                            self.print(&name_text);
                            self.print(" ");
                        }
                        let mut broken = false;
                        if info.kind == Kind::Binary {
                            self.line_break(&mut broken);
                        }
                        let right_parens = needs_parens(info, right, false);
                        if right_parens {
                            self.print("(");
                        }
                        self.deparse(right);
                        if right_parens {
                            self.print(")");
                        }
                        if broken {
                            self.indent -= 1;
                        }
                        return;
                    }
                    (Kind::Binary | Kind::Binary2 | Kind::Unary, 1)
                        if matches!(name, b"+" | b"-" | b"!" | b"~" | b"?") =>
                    {
                        let operand = arguments.car();
                        let info = if info.precedence == precedence::SUM {
                            OperatorInfo {
                                precedence: precedence::SIGN,
                                ..info
                            }
                        } else {
                            info
                        };
                        self.print(&name_text);
                        let parens = needs_parens(info, operand, false);
                        if parens {
                            self.print("(");
                        }
                        self.deparse(operand);
                        if parens {
                            self.print(")");
                        }
                        return;
                    }
                    _ => {}
                }
            }
            if is_user_binop(name) && argument_count == 2 {
                let info = OperatorInfo {
                    kind: Kind::Binary,
                    precedence: precedence::PERCENT,
                    right_associative: false,
                };
                let left = arguments.car();
                let right = arguments.cdr().car();
                let left_parens = needs_parens(info, left, true);
                if left_parens {
                    self.print("(");
                }
                self.deparse(left);
                if left_parens {
                    self.print(")");
                }
                self.print(" ");
                self.print(&name_text);
                self.print(" ");
                let mut broken = false;
                self.line_break(&mut broken);
                let right_parens = needs_parens(info, right, false);
                if right_parens {
                    self.print("(");
                }
                self.deparse(right);
                if right_parens {
                    self.print(")");
                }
                if broken {
                    self.indent -= 1;
                }
                return;
            }
            self.plain_call(&quote_name(&name_text), arguments);
            return;
        }
        // A call whose function is itself an expression.
        let needs = matches!(head.kind(), SexpType::Language)
            && (head.car() == symbols().function
                || head.car().kind() == SexpType::Symbol && operator_info(head.car().car().bytes()).is_some_and(|info| info.kind != Kind::Subset && info.kind != Kind::Dollar && !(info.kind == Kind::Binary2 && info.precedence == precedence::NAMESPACE)));
        if needs || head.kind() == SexpType::Closure {
            self.print("(");
            self.deparse(head);
            self.print(")");
        } else {
            self.deparse(head);
        }
        self.print("(");
        self.in_list += 1;
        self.arguments(arguments, false);
        self.in_list -= 1;
        self.print(")");
    }

    /// A record, tuple or tag value, in the syntax that builds it: `(x = 1, y = 2)`, `(1L,)`,
    /// `()`, `:None` or `:Circle(radius = 2)`.
    unsafe fn native(&mut self, object: Sexp) {
        let layout = records::layout(object);
        if matches!(layout.kind(), NativeKind::Dict | NativeKind::Set) {
            self.collection(object, layout.kind() == NativeKind::Dict);
            return;
        }
        let count = layout.slot_count();
        if let Some(tag) = layout.tag {
            self.print(":");
            self.print(&quote_name(&records::tag_name(tag)));
            if count == 0 {
                return;
            }
        }
        self.print("(");
        self.in_list += 1;
        let mut broken = false;
        for index in 0..count {
            if let Some(name) = layout.names().get(index) {
                self.print(&quote_name(&symbol_name(*name)));
                self.print(" = ");
            }
            match records::slot_value(object, index) {
                Ok(value) => self.deparse(value),
                Err(_) => self.print("<unavailable>"),
            }
            if index + 1 < count {
                self.print(", ");
                self.line_break(&mut broken);
            }
        }
        if broken {
            self.indent -= 1;
        }
        self.in_list -= 1;
        // `(x = 1)` is an assignment and `(1L)` a parenthesized number, so a one-item record or
        // tuple keeps a trailing comma; a tag's payload parentheses are never ambiguous.
        if count == 1 && layout.tag.is_none() {
            self.print(",");
        }
        self.print(")");
    }

    /// A dictionary or set, as the call that rebuilds it: `Dict.from_list(list(("a", 1)))` or
    /// `Set.empty()`.
    unsafe fn collection(&mut self, object: Sexp, dict: bool) {
        let module = if dict { "Dict" } else { "Set" };
        // The deparser cannot fail, and reading entries fails only on a malformed collection or
        // an exhausted heap, so printing a placeholder would hide a corrupted value.
        let entries = match standard_library::entries(object) {
            Ok(entries) => entries,
            Err(failure) => panic!("a {module} value could not be read for deparsing: {failure:?}"),
        };
        if entries.is_empty() {
            self.print(module);
            self.print(".empty()");
            return;
        }
        self.print(module);
        self.print(".from_list(list(");
        self.in_list += 1;
        let mut broken = false;
        let count = entries.len();
        for (index, (key, value)) in entries.into_iter().enumerate() {
            match value {
                Some(value) => {
                    self.print("(");
                    self.deparse(key);
                    self.print(", ");
                    self.deparse(value);
                    self.print(")");
                }
                None => self.deparse(key),
            }
            if index + 1 < count {
                self.print(", ");
                self.line_break(&mut broken);
            }
        }
        if broken {
            self.indent -= 1;
        }
        self.in_list -= 1;
        self.print("))");
    }

    /// The calls ry's own syntax lowers to, printed back in that syntax.
    unsafe fn ry_call(&mut self, form: RyForm, arguments: Sexp) {
        match form {
            RyForm::Record | RyForm::Tuple => {
                self.print("(");
                self.in_list += 1;
                self.arguments(arguments, false);
                self.in_list -= 1;
                if values::pairlist_length(arguments) == 1 {
                    self.print(",");
                }
                self.print(")");
            }
            RyForm::Tag => {
                self.print(":");
                self.print(&quote_name(&char_str(arguments.car().element(0))));
                let payload = arguments.cdr();
                if payload != nil() {
                    self.print("(");
                    self.in_list += 1;
                    self.arguments(payload, false);
                    self.in_list -= 1;
                    self.print(")");
                }
            }
            RyForm::Match => {
                let scrutinee = arguments.car();
                self.print("match ");
                // `match (a, b) {` puts a tuple scrutinee in the parentheses themselves.
                if matches!(ry_call_form(scrutinee), Some(RyForm::Record | RyForm::Tuple))
                    && values::pairlist_length(scrutinee.cdr()) > 1
                {
                    self.deparse(scrutinee);
                } else {
                    self.print("(");
                    self.deparse(scrutinee);
                    self.print(")");
                }
                self.print(" {");
                self.in_curly += 1;
                self.indent += 1;
                self.write_line();
                let mut arms = arguments.cdr();
                while arms.kind() == SexpType::Pairlist {
                    self.arm(arms.car());
                    self.write_line();
                    arms = arms.cdr();
                }
                self.indent -= 1;
                self.print("}");
                self.in_curly -= 1;
            }
        }
    }

    /// One `match` arm, `pattern => body` or `pattern if (guard) => body`.
    unsafe fn arm(&mut self, arm: Sexp) {
        let parts: Vec<Sexp> = values::pairlist_items(arm.cdr()).into_iter().map(|(_, part)| part).collect();
        let (pattern, guard, body) = match parts.as_slice() {
            [pattern, body] => (*pattern, None, *body),
            [pattern, guard, body] => (*pattern, Some(*guard), *body),
            _ => {
                self.deparse(arm);
                return;
            }
        };
        self.pattern(pattern);
        if let Some(guard) = guard {
            self.print(" if (");
            self.deparse(guard);
            self.print(")");
        }
        self.print(" => ");
        // A brace-less `if` in a block breaks across lines, and a line break ends the arm.
        let flat = !is_curly(body);
        if flat {
            self.in_list += 1;
        }
        self.deparse(body);
        if flat {
            self.in_list -= 1;
        }
    }

    /// A pattern, which shares the call encoding of the values it matches but prints `_` and
    /// `..` bare.
    unsafe fn pattern(&mut self, pattern: Sexp) {
        match pattern.kind() {
            SexpType::Symbol => {
                let name = symbol_name(pattern);
                if name == "_" || name == ".." {
                    self.print(&name);
                } else {
                    self.print(&quote_name(&name));
                }
            }
            SexpType::Language => {
                let head = pattern.car();
                let arguments = pattern.cdr();
                let head_name = if head.kind() == SexpType::Symbol {
                    symbol_name(head).into_owned()
                } else {
                    String::new()
                };
                match (head_name.as_str(), ry_call_form(pattern)) {
                    (".ry_record" | ".ry_tuple", _) => {
                        self.print("(");
                        self.pattern_items(arguments);
                        let items = values::pairlist_items(arguments);
                        let rest = items
                            .last()
                            .is_some_and(|(_, item)| item.kind() == SexpType::Symbol && item.car().bytes() == b"..");
                        if items.len() == 1 && !rest {
                            self.print(",");
                        }
                        self.print(")");
                    }
                    (_, Some(RyForm::Tag)) => {
                        self.print(":");
                        self.print(&quote_name(&char_str(arguments.car().element(0))));
                        if arguments.cdr() != nil() {
                            self.print("(");
                            self.pattern_items(arguments.cdr());
                            self.print(")");
                        }
                    }
                    ("|", _) if values::pairlist_length(arguments) == 2 => {
                        self.pattern(arguments.car());
                        self.print(" | ");
                        self.pattern(arguments.cdr().car());
                    }
                    ("(", _) if values::pairlist_length(arguments) == 1 => {
                        self.print("(");
                        self.pattern(arguments.car());
                        self.print(")");
                    }
                    _ => self.deparse(pattern),
                }
            }
            _ => self.deparse(pattern),
        }
    }

    unsafe fn pattern_items(&mut self, mut items: Sexp) {
        while items.kind() == SexpType::Pairlist {
            let tag = items.tag();
            if tag != nil() && tag.kind() == SexpType::Symbol {
                self.print(&quote_name(&symbol_name(tag)));
                self.print(" = ");
            }
            self.pattern(items.car());
            items = items.cdr();
            if items.kind() == SexpType::Pairlist {
                self.print(", ");
            }
        }
    }

    unsafe fn plain_call(&mut self, name: &str, arguments: Sexp) {
        self.print(name);
        self.print("(");
        self.in_list += 1;
        self.arguments(arguments, false);
        self.in_list -= 1;
        self.print(")");
    }

    unsafe fn curly(&mut self, mut statements: Sexp) {
        self.print("{");
        self.in_curly += 1;
        self.indent += 1;
        self.write_line();
        while statements.kind() == SexpType::Pairlist {
            self.deparse(statements.car());
            self.write_line();
            statements = statements.cdr();
        }
        self.indent -= 1;
        self.print("}");
        self.in_curly -= 1;
    }

    unsafe fn if_expression(&mut self, arguments: Sexp) {
        let condition = arguments.car();
        let consequent = arguments.cdr().car();
        let alternative = arguments.cdr().cdr();
        self.print("if (");
        self.deparse(condition);
        self.print(") ");
        if self.in_curly > 0 && self.in_list == 0 {
            let curly_ahead = is_curly(consequent);
            if !curly_ahead {
                self.write_line();
                self.indent += 1;
            }
            if alternative.kind() == SexpType::Pairlist {
                self.deparse(consequent);
                if !curly_ahead {
                    self.write_line();
                    self.indent -= 1;
                } else {
                    self.print(" ");
                }
                self.print("else ");
                self.deparse(alternative.car());
            } else {
                self.deparse(consequent);
                if !curly_ahead {
                    self.indent -= 1;
                }
            }
        } else {
            self.deparse(consequent);
            if alternative.kind() == SexpType::Pairlist {
                self.print(" else ");
                self.deparse(alternative.car());
            }
        }
    }

    unsafe fn list(&mut self, list: Sexp, opener: &str) {
        let count = length(list);
        let names = get_attr(list, symbols().names);
        let nice = self.opts & options::NICE_NAMES != 0 && names != nil();
        self.print(opener);
        self.in_list += 1;
        let mut broken = false;
        for index in 0..count {
            // GNU R's vector deparse breaks a line that is past the cutoff before the next element
            // (and before the closing parenthesis), without indenting the continuation.
            if index > 0 && self.length > self.cutoff {
                self.write_line();
            }
            if nice {
                self.name_prefix(names, index);
            }
            self.deparse(list.element(index));
            if index + 1 < count {
                self.print(", ");
                self.line_break(&mut broken);
            }
        }
        if broken {
            self.indent -= 1;
        }
        self.in_list -= 1;
        self.print(")");
    }

    unsafe fn name_prefix(&mut self, names: Sexp, index: usize) {
        let name = names.element(index);
        if name == values::na_string() {
            self.print("`NA` = ");
            return;
        }
        let text = char_str(name);
        if !text.is_empty() {
            if is_syntactic(&text) {
                self.print(&text);
            } else if self.backtick {
                self.print(&quote_name(&text));
            } else {
                self.print(&format::encode_string(&text, Some('"')));
            }
            self.print(" = ");
        }
    }

    unsafe fn vector(&mut self, vector: Sexp) {
        let vector = match values::materialize(vector) {
            Ok(vector) => vector,
            Err(_) => {
                self.print("<ALTREP>");
                return;
            }
        };
        let count = length(vector);
        let kind = vector.kind();
        if count == 0 {
            self.print(match kind {
                SexpType::Logical => "logical(0)",
                SexpType::Integer => "integer(0)",
                SexpType::Real => "numeric(0)",
                SexpType::Complex => "complex(0)",
                SexpType::String => "character(0)",
                SexpType::Raw => "raw(0)",
                _ => "NULL",
            });
            return;
        }
        let names = get_attr(vector, symbols().names);
        let nice = self.opts & options::NICE_NAMES != 0 && nice_names_ok(vector);
        let keep_integer = self.opts & options::KEEP_INTEGER != 0;
        let keep_na = self.opts & options::KEEP_NA != 0;
        if is_integer_run(vector) {
            let data = values::ints(vector);
            self.print(&format!("{}:{}", data[0], data[count - 1]));
            return;
        }
        if kind == SexpType::Raw {
            self.print("as.raw(c(");
            let items: Vec<String> = values::raws(vector).iter().map(|byte| format!("0x{byte:02x}")).collect();
            self.print(&items.join(", "));
            self.print("))");
            return;
        }
        let all_na = match kind {
            SexpType::Logical | SexpType::Integer => values::ints(vector).iter().all(|&value| value == NA_INTEGER),
            SexpType::Real => values::reals(vector).iter().all(|&value| is_na_real(value)),
            SexpType::String => (0..count).all(|index| vector.element(index) == values::na_string()),
            SexpType::Complex => values::complexes(vector).iter().all(|value| is_na_real(value.r)),
            _ => false,
        };
        // Whole doubles keep their type through `as.double` only when some element shows it.
        let surround_double = kind == SexpType::Real
            && keep_integer
            && !all_na
            && values::reals(vector).iter().all(|&value| {
                !value.is_finite() || (value.fract() == 0.0 && value.abs() < 1e15)
            })
            && false;
        let wrap = count > 1 || nice;
        if surround_double {
            self.print("as.double(");
        }
        if wrap {
            self.print("c(");
        }
        for index in 0..count {
            // GNU R's vector deparse breaks a line that is past the cutoff before the next element
            // (and before the closing parenthesis), without indenting the continuation.
            if index > 0 && self.length > self.cutoff {
                self.write_line();
            }
            if nice {
                self.name_prefix(names, index);
            }
            let text = match kind {
                SexpType::Logical => match values::ints(vector)[index] {
                    NA_LOGICAL => "NA".to_owned(),
                    0 => "FALSE".to_owned(),
                    _ => "TRUE".to_owned(),
                },
                SexpType::Integer => {
                    let value = values::ints(vector)[index];
                    if value == NA_INTEGER {
                        if keep_na && all_na { "NA_integer_".to_owned() } else { "NA".to_owned() }
                    } else if keep_integer {
                        format!("{value}L")
                    } else {
                        value.to_string()
                    }
                }
                SexpType::Real => {
                    let value = values::reals(vector)[index];
                    if is_na_real(value) {
                        if keep_na && all_na { "NA_real_".to_owned() } else { "NA".to_owned() }
                    } else {
                        let digits = if self.opts & options::DIGITS17 != 0 { 17 } else { 15 };
                        let text = format::format_real_alone(value, digits);
                        if keep_integer && kind == SexpType::Real && false {
                            text
                        } else {
                            text
                        }
                    }
                }
                SexpType::Complex => {
                    let value = values::complexes(vector)[index];
                    if is_na_real(value.r) && is_na_real(value.i) {
                        if keep_na && all_na { "NA_complex_".to_owned() } else { "NA".to_owned() }
                    } else if !value.r.is_finite() || !value.i.is_finite() {
                        // `Inf+Infi` would not parse back, so a non-finite part needs the call.
                        let part = |part: f64| {
                            if is_na_real(part) { "NA".to_owned() } else { format::format_real_alone(part, 15) }
                        };
                        format!("complex(real={}, imaginary={})", part(value.r), part(value.i))
                    } else {
                        format::format_complex_alone(value, 15)
                    }
                }
                SexpType::String => {
                    let element = vector.element(index);
                    if element == values::na_string() {
                        if keep_na && all_na { "NA_character_".to_owned() } else { "NA".to_owned() }
                    } else {
                        format::encode_element(element, Some('"'))
                    }
                }
                _ => String::new(),
            };
            self.print(&text);
            if index + 1 < count {
                self.print(", ");
            }
        }
        if wrap {
            if self.length > self.cutoff {
                self.write_line();
            }
            self.print(")");
        }
        if surround_double {
            self.print(")");
        }
    }

    unsafe fn attributes(&mut self, object: Sexp) {
        let mut cell = object.attrib();
        while cell.kind() == SexpType::Pairlist {
            let tag = cell.tag();
            let skip_names = tag == symbols().names
                && self.opts & options::NICE_NAMES != 0
                && !matches!(object.kind(), SexpType::Language)
                && nice_names_ok(object);
            if !skip_names && tag != symbols().srcref {
                self.print(", ");
                let name = symbol_name(tag);
                let shown = match name.as_ref() {
                    "dim" => ".Dim".to_owned(),
                    "dimnames" => ".Dimnames".to_owned(),
                    "names" => ".Names".to_owned(),
                    "tsp" => ".Tsp".to_owned(),
                    "levels" => ".Label".to_owned(),
                    other => {
                        if is_syntactic(other) {
                            other.to_owned()
                        } else if self.backtick {
                            // GNU R quotes the backticked name: `structure(1, "`a b`" = 2)`.
                            format!("\"{}\"", quote_name(other))
                        } else {
                            format::encode_string(other, Some('"'))
                        }
                    }
                };
                // GNU R 4 writes the plain names for these, not the old dotted ones.
                let shown = match shown.as_str() {
                    ".Dim" => "dim".to_owned(),
                    ".Dimnames" => "dimnames".to_owned(),
                    ".Names" => "names".to_owned(),
                    ".Tsp" => "tsp".to_owned(),
                    ".Label" => "levels".to_owned(),
                    _ => shown,
                };
                self.print(&shown);
                self.print(" = ");
                self.deparse(cell.car());
            }
            cell = cell.cdr();
        }
    }
}

/// The source text a srcref points at, as `as.character(srcref)` reads it from its srcfile, or
/// `None` when `srcref` is not a srcref or the text cannot be read. GNU R's deparser falls back
/// to deparsing the code in that case too.
pub unsafe fn source_lines(srcref: Sexp) -> Option<Vec<String>> {
    if srcref.kind() != SexpType::Integer || length(srcref) < 6 {
        return None;
    }
    let lines = crate::scope(|| {
        let call = values::call(values::install("as.character")?, &[srcref])?;
        crate::eval::eval(call, values::base_env())
    })
    .ok()?;
    if lines.kind() != SexpType::String {
        return None;
    }
    Some(values::strings(lines).into_iter().map(Option::unwrap_or_default).collect())
}

/// Whether names can be written inline (`c(a = 1)`) rather than as an attribute.
unsafe fn nice_names_ok(object: Sexp) -> bool {
    let names = get_attr(object, symbols().names);
    if names.kind() != SexpType::String {
        return false;
    }
    if !object.kind().is_vector() || length(names) != length(object) {
        return false;
    }
    // All-empty names, or an `NA` name, need the attribute form.
    let any_named = (0..length(names)).any(|index| !values::is_blank(names.element(index)));
    let any_na = (0..length(names)).any(|index| names.element(index) == values::na_string());
    // In `c(...)` these names would match `c()`'s own arguments instead of naming elements.
    let names_an_argument = object.kind().is_atomic()
        && (0..length(names)).any(|index| {
            matches!(values::string_at(names, index).as_deref(), Some("recursive" | "use.names"))
        });
    // A run deparses as `from:to`, which has no room for names.
    any_named && !any_na && !names_an_argument && !is_integer_run(object)
}

/// A run of at least two consecutive integers, increasing or decreasing, which deparses as
/// `from:to`.
unsafe fn is_integer_run(object: Sexp) -> bool {
    if object.kind() != SexpType::Integer || length(object) < 2 {
        return false;
    }
    let data = values::ints(object);
    let step = data[1] as i64 - data[0] as i64;
    data[0] != NA_INTEGER
        && (step == 1 || step == -1)
        && data
            .windows(2)
            .all(|pair| pair[1] != NA_INTEGER && pair[1] as i64 == pair[0] as i64 + step)
}

/// The parts of an S4 object that its `new()` call names.
struct S4Parts {
    class: String,
    slots: Vec<(String, Sexp)>,
    data: Option<Sexp>,
}

/// The class, slots and data part of an S4 object, read through the methods package, which
/// owns class definitions. Without it, or for an object whose class it does not know, the
/// object deparses as its plain value.
unsafe fn s4_parts(object: Sexp) -> Option<S4Parts> {
    let symbols = symbols();
    let class = get_attr(object, symbols.class);
    if class.kind() != SexpType::String || length(class) != 1 {
        return None;
    }
    let class_name = values::string_at(class, 0)?.into_owned();
    let methods_function = |name: &str| -> R<Sexp> {
        values::call(symbols.double_colon, &[values::install("methods")?, values::install(name)?])
    };
    let quoted = values::call(symbols.quote, &[object]).ok()?;
    let slot_names_call = values::call(methods_function("slotNames").ok()?, &[quoted]).ok()?;
    let slot_names = crate::eval::eval(slot_names_call, values::base_namespace()).ok()?;
    if slot_names.kind() != SexpType::String {
        return None;
    }
    crate::heap().root(slot_names);
    let mut slots = Vec::new();
    let mut has_data_slot = false;
    for index in 0..length(slot_names) {
        let name = values::string_at(slot_names, index)?.into_owned();
        has_data_slot |= name == ".Data";
        let name_value = values::scalar_string(&name).ok()?;
        let slot_call = values::call(methods_function("slot").ok()?, &[quoted, name_value]).ok()?;
        let value = crate::eval::eval(slot_call, values::base_namespace()).ok()?;
        crate::heap().root(value);
        slots.push((name, value));
    }
    let data = if has_data_slot || object.kind() == SexpType::Object {
        None
    } else {
        let data = values::shallow_duplicate(object).ok()?;
        values::remove_attr(data, symbols.class);
        for (name, _) in &slots {
            values::remove_attr(data, values::install(name).ok()?);
        }
        data.set_gp_bits(gp::S4_OBJECT, false);
        crate::heap().root(data);
        Some(data)
    };
    Some(S4Parts { class: class_name, slots, data })
}

unsafe fn has_shown_attributes(object: Sexp, nice_names: bool) -> bool {
    if matches!(object.kind(), SexpType::Language | SexpType::Symbol | SexpType::Environment | SexpType::Closure) {
        return false;
    }
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        let tag = cell.tag();
        if tag == symbols().srcref {
            cell = cell.cdr();
            continue;
        }
        if tag != symbols().names || !nice_names || !nice_names_ok(object) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// The calls ry's own syntax lowers to.
#[derive(Clone, Copy)]
enum RyForm {
    Record,
    Tuple,
    Tag,
    Match,
}

/// Which ry form a call of `name` with `arguments` is, when its arguments have that form's
/// shape; any other call of the same name prints as an ordinary call.
unsafe fn ry_form(name: &[u8], arguments: Sexp) -> Option<RyForm> {
    let items = values::pairlist_items(arguments);
    let named = items.iter().filter(|(tag, _)| *tag != nil()).count();
    match name {
        b".ry_record" if !items.is_empty() && named == items.len() => Some(RyForm::Record),
        b".ry_tuple" if named == 0 => Some(RyForm::Tuple),
        b".ry_tag" => {
            let (first_tag, first) = items.first()?;
            (*first_tag == nil() && first.kind() == SexpType::String && length(*first) == 1)
                .then_some(RyForm::Tag)
        }
        b".ry_match" if !items.is_empty() => {
            let arms_ok = items[1..].iter().all(|(_, arm)| {
                arm.kind() == SexpType::Language
                    && arm.car().kind() == SexpType::Symbol
                    && arm.car().car().bytes() == b"=>"
                    && (2..=3).contains(&values::pairlist_length(arm.cdr()))
            });
            arms_ok.then_some(RyForm::Match)
        }
        _ => None,
    }
}

unsafe fn ry_call_form(expression: Sexp) -> Option<RyForm> {
    if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
        return None;
    }
    ry_form(expression.car().car().bytes(), expression.cdr())
}

unsafe fn is_curly(expression: Sexp) -> bool {
    expression.kind() == SexpType::Language && expression.car() == symbols().brace
}

/// GNU R's `needsparens`: whether an operand of `main` must be parenthesized.
unsafe fn needs_parens(main: OperatorInfo, argument: Sexp, left: bool) -> bool {
    if argument.kind() == SexpType::Language {
        let head = argument.car();
        if head.kind() == SexpType::Symbol {
            let name = head.car().bytes();
            if let Some(mut info) = operator_info(name) {
                let count = values::pairlist_length(argument.cdr());
                match info.kind {
                    Kind::Binary | Kind::Binary2 => {
                        match count {
                            1 => {
                                if !left {
                                    return false;
                                }
                                if info.precedence == precedence::SUM {
                                    info.precedence = precedence::SIGN;
                                }
                            }
                            2 => {
                                if main.precedence == precedence::COMPARE
                                    && info.precedence == precedence::COMPARE
                                {
                                    return true;
                                }
                            }
                            _ => return false,
                        }
                        return main.precedence > info.precedence
                            || (main.precedence == info.precedence && left == main.right_associative);
                    }
                    Kind::Subset => {
                        if main.kind == Kind::Dollar {
                            return false;
                        }
                        return main.precedence > info.precedence
                            || (main.precedence == info.precedence && left == main.right_associative);
                    }
                    Kind::Assign | Kind::Unary | Kind::Dollar => {
                        return main.precedence > info.precedence
                            || (main.precedence == info.precedence && left == main.right_associative);
                    }
                    Kind::Other => return false,
                }
            }
            if matches!(name, b"for" | b"if" | b"while" | b"repeat") {
                return left;
            }
            if name == b"function" {
                return left || main.precedence > precedence::FUNCTION && false;
            }
            if is_user_binop(name) {
                return main.precedence > precedence::PERCENT
                    || (main.precedence == precedence::PERCENT && left == main.right_associative);
            }
        }
    } else if argument.kind() == SexpType::Complex && length(argument) == 1 {
        return main.precedence > precedence::SUM
            || (main.precedence == precedence::SUM && left == main.right_associative);
    }
    false
}

// The internals.

unsafe fn do_deparse(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let cutoff = values::as_integer_scalar(call.arg(1));
    let cutoff = if cutoff == NA_INTEGER || !(20..=500).contains(&cutoff) {
        if cutoff != NA_INTEGER {
            crate::conditions::warning("invalid 'cutoff' value for 'deparse', using default")?;
        }
        60
    } else {
        cutoff as usize
    };
    let opts = values::as_integer_scalar(call.arg(3)).max(0) as u32;
    let nlines = values::as_integer_scalar(call.arg(4));
    let backtick = values::as_logical_scalar(call.arg(2)) == 1;
    let mut lines = deparse_lines_with(object, cutoff, opts, backtick);
    if nlines > 0 && (nlines as usize) < lines.len() {
        lines.truncate(nlines as usize);
    }
    values::string_vector(&lines)
}

/// `.Internal(dump(list, file, envir, opts, evaluate))`: writes `name <-` and the deparsed value
/// of each object that exists, and returns the names written.
unsafe fn do_dump(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let file = call.arg(1);
    let env = call.arg(2);
    let opts = values::as_integer_scalar(call.arg(3)).max(0) as u32;
    let evaluate = values::as_logical_scalar(call.arg(4)) != 0;
    if names.kind() != SexpType::String {
        return Err(call.error("character arguments expected"));
    }
    let mut written = Vec::new();
    let mut text = String::new();
    for index in 0..length(names) {
        let Some(name) = values::string_at(names, index).map(|name| name.into_owned()) else {
            continue;
        };
        let symbol = values::install(&name)?;
        let mut value = crate::eval::find_var(symbol, env)?;
        if value == values::unbound() {
            continue;
        }
        if value.kind() == SexpType::Promise && evaluate {
            value = crate::eval::force_promise(value)?;
        }
        text.push_str(&quote_name(&name));
        text.push_str(" <-\n");
        for line in deparse_lines(value, 60, opts) {
            text.push_str(&line);
            text.push('\n');
        }
        written.push(name);
    }
    crate::connections::write_text(file, &text)?;
    values::string_vector(&written)
}

unsafe fn do_dput(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let file = call.arg(1);
    let opts = values::as_integer_scalar(call.arg(2)).max(0) as u32;
    let lines = deparse_lines(object, 60, opts);
    let mut text = lines.join("\n");
    text.push('\n');
    crate::connections::write_text(file, &text)?;
    Ok(object)
}
