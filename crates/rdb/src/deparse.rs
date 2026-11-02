//! Turns values read from a serialized stream back into R source that parses and reads like R:
//! operators follow R's precedence with parentheses only where the tree needs them, and blocks
//! are laid out one statement per line with four-space indentation.

use crate::unserialize::{Argument, Value, is_na_real};

/// R source for `value`: the code of a call or function, or an expression that rebuilds a
/// constant. Values with no source form print as R prints them, such as `<environment>`.
pub fn deparse(value: &Value) -> String {
    let mut deparser = Deparser {
        output: String::new(),
        indent: 0,
    };
    deparser.expression(value, LOWEST, false);
    deparser.output
}

/// Whether `name` can be written as a bare symbol, without backticks.
pub fn is_syntactic_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if RESERVED.contains(&name) {
        return false;
    }
    if name == "..." || is_dot_dot_number(name) {
        return true;
    }
    let starts_well = first.is_alphabetic()
        || (first == '.' && !name[1..].starts_with(|next: char| next.is_ascii_digit()));
    starts_well
        && characters.all(|character| character.is_alphanumeric() || matches!(character, '.' | '_'))
}

/// A double as R prints it with 15 significant digits: in fixed notation unless scientific
/// notation is narrower.
pub fn format_double(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Inf" } else { "-Inf" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let scientific = format!("{:.14e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let significant = digits.len() as i32;
    let sign = if value < 0.0 { "-" } else { "" };

    let exponent_text = format!(
        "{}{:02}",
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    );
    let scientific_width =
        significant + i32::from(significant > 1) + 1 + exponent_text.len() as i32;
    let fixed_width = if exponent >= 0 {
        (exponent + 1).max(significant) + i32::from(significant > exponent + 1)
    } else {
        2 + (-exponent - 1) + significant
    };
    if fixed_width <= scientific_width {
        let fixed = if exponent >= 0 {
            let integer_length = (exponent + 1) as usize;
            if digits.len() <= integer_length {
                format!("{digits}{}", "0".repeat(integer_length - digits.len()))
            } else {
                format!(
                    "{}.{}",
                    &digits[..integer_length],
                    &digits[integer_length..]
                )
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        };
        format!("{sign}{fixed}")
    } else if digits.len() > 1 {
        format!("{sign}{}.{}e{exponent_text}", &digits[..1], &digits[1..])
    } else {
        format!("{sign}{digits}e{exponent_text}")
    }
}

/// A string literal with R's escapes.
pub fn quote_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\r' => quoted.push_str("\\r"),
            character if (character as u32) < 0x20 || character == '\u{7f}' => {
                quoted.push_str(&format!("\\{:03o}", character as u32));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

const RESERVED: &[&str] = &[
    "if",
    "else",
    "repeat",
    "while",
    "function",
    "for",
    "next",
    "break",
    "in",
    "TRUE",
    "FALSE",
    "NULL",
    "Inf",
    "NaN",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NA_character_",
    "NA_complex_",
];

// Binding strength, from loosest to tightest, after R's `?Syntax`. Control-flow constructs
// and `function` extend as far right as they can, so they bind loosest of all.
const LOWEST: u8 = 0;
const HELP: u8 = 1;
const EQUALS_ASSIGN: u8 = 2;
const LEFT_ASSIGN: u8 = 3;
const TILDE: u8 = 5;
const OR: u8 = 6;
const AND: u8 = 7;
const NOT: u8 = 8;
const COMPARISON: u8 = 9;
const SUM: u8 = 10;
const PRODUCT: u8 = 11;
const SPECIAL: u8 = 12;
const RANGE: u8 = 13;
const UNARY: u8 = 14;
const POWER: u8 = 15;
const POSTFIX: u8 = 16;
const NAMESPACE: u8 = 17;
const ATOM: u8 = 18;

const INDENT: &str = "    ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Associativity {
    Left,
    Right,
    None,
}

/// How an operator call is written.
enum Form<'a> {
    Binary {
        operator: &'a str,
        precedence: u8,
        associativity: Associativity,
        /// Set for `a -> b`, written as `b <- a`. R's parser already rewrites the arrow, so
        /// only a hand-built call has it.
        swapped: bool,
    },
    Unary {
        operator: &'a str,
        precedence: u8,
    },
    Dollar(&'a str),
    Namespace(&'a str),
    Index {
        open: &'static str,
        close: &'static str,
    },
    Block,
    Parenthesized,
    If,
    For,
    While,
    Repeat,
    Function,
    Keyword(&'a str),
    Call,
}

struct Deparser {
    output: String,
    indent: usize,
}

impl Deparser {
    /// Writes `value`, parenthesized when it binds looser than `minimum`. `followed` says
    /// whether more of the same expression follows on the line, in which case a construct
    /// like `if` must be closed off with parentheses so it does not swallow what follows.
    fn expression(&mut self, value: &Value, minimum: u8, followed: bool) {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
            let precedence = precedence_of(value);
            let open_construct = precedence == LOWEST && minimum > LOWEST;
            if precedence < minimum && (followed || !open_construct) {
                self.output.push('(');
                self.unparenthesized(value, false);
                self.output.push(')');
            } else {
                self.unparenthesized(value, followed);
            }
        });
    }

    /// An expression between delimiters such as a call's parentheses and commas, where
    /// anything may appear except an `=` call, which would read as a named argument.
    fn delimited(&mut self, value: &Value) {
        if is_call_to(value, "=") {
            self.output.push('(');
            self.unparenthesized(value, false);
            self.output.push(')');
        } else {
            self.expression(value, LOWEST, false);
        }
    }

    fn unparenthesized(&mut self, value: &Value, followed: bool) {
        match value {
            Value::Language(function, arguments) => self.call(function, arguments, followed),
            Value::Closure(closure) => {
                self.output.push_str("function(");
                self.formals(&closure.formals);
                self.output.push_str(") ");
                self.expression(&closure.body, LOWEST, followed);
            }
            Value::Symbol(name) => self.name(name),
            Value::Null => self.output.push_str("NULL"),
            Value::Missing => {}
            Value::Promise(inner) => self.expression(inner, LOWEST, followed),
            Value::Pairlist(arguments) => {
                self.output.push_str("pairlist(");
                self.arguments(arguments);
                self.output.push(')');
            }
            Value::Environment(_) => self.output.push_str("<environment>"),
            Value::Builtin(name) => {
                self.output.push_str(".Primitive(");
                self.output.push_str(&quote_string(name));
                self.output.push(')');
            }
            Value::Opaque(description) => {
                self.output.push('<');
                self.output.push_str(description);
                self.output.push('>');
            }
            Value::Logical(_)
            | Value::Integer(_)
            | Value::Double(_)
            | Value::Complex(_)
            | Value::Character(_)
            | Value::Raw(_)
            | Value::List(_)
            | Value::Expression(_) => self.constant(value),
        }
    }

    fn call(&mut self, function: &Value, arguments: &[Argument], followed: bool) {
        let form = match function {
            Value::Symbol(name) => form_of(name, arguments),
            _ => Form::Call,
        };
        match form {
            Form::Binary {
                operator,
                precedence,
                associativity,
                swapped,
            } => {
                let (left_minimum, right_minimum) = match associativity {
                    Associativity::Left => (precedence, precedence + 1),
                    Associativity::Right => (precedence + 1, precedence),
                    Associativity::None => (precedence + 1, precedence + 1),
                };
                let (left, right) = if swapped {
                    (&arguments[1].value, &arguments[0].value)
                } else {
                    (&arguments[0].value, &arguments[1].value)
                };
                self.expression(left, left_minimum, true);
                if matches!(operator, "^" | ":" | "/") {
                    self.output.push_str(operator);
                } else {
                    self.output.push(' ');
                    self.output.push_str(operator);
                    self.output.push(' ');
                }
                self.expression(right, right_minimum, followed);
            }
            Form::Unary {
                operator,
                precedence,
            } => {
                self.output.push_str(operator);
                self.expression(&arguments[0].value, precedence, followed);
            }
            Form::Dollar(operator) => {
                self.expression(&arguments[0].value, POSTFIX, true);
                self.output.push_str(operator);
                self.member(&arguments[1].value);
            }
            Form::Namespace(operator) => {
                self.member(&arguments[0].value);
                self.output.push_str(operator);
                self.member(&arguments[1].value);
            }
            Form::Index { open, close } => {
                self.expression(&arguments[0].value, POSTFIX, true);
                self.output.push_str(open);
                self.arguments(&arguments[1..]);
                self.output.push_str(close);
            }
            Form::Block => self.block(arguments),
            Form::Parenthesized => {
                self.output.push('(');
                self.expression(&arguments[0].value, LOWEST, false);
                self.output.push(')');
            }
            Form::If => {
                self.output.push_str("if (");
                self.delimited(&arguments[0].value);
                self.output.push_str(") ");
                let alternative = arguments.get(2);
                let consequent = &arguments[1].value;
                // An `else` after an `if` that has none would attach to that inner `if`.
                if alternative.is_some() && ends_with_open_if(consequent) {
                    self.output.push('(');
                    self.unparenthesized(consequent, false);
                    self.output.push(')');
                } else {
                    self.expression(consequent, LOWEST, followed && alternative.is_none());
                }
                if let Some(alternative) = alternative {
                    self.output.push_str(" else ");
                    self.expression(&alternative.value, LOWEST, followed);
                }
            }
            Form::For => {
                self.output.push_str("for (");
                self.expression(&arguments[0].value, ATOM, true);
                self.output.push_str(" in ");
                self.delimited(&arguments[1].value);
                self.output.push_str(") ");
                self.expression(&arguments[2].value, LOWEST, followed);
            }
            Form::While => {
                self.output.push_str("while (");
                self.delimited(&arguments[0].value);
                self.output.push_str(") ");
                self.expression(&arguments[1].value, LOWEST, followed);
            }
            Form::Repeat => {
                self.output.push_str("repeat ");
                self.expression(&arguments[0].value, LOWEST, followed);
            }
            Form::Function => {
                self.output.push_str("function(");
                if let Value::Pairlist(formals) = &arguments[0].value {
                    self.formals(formals);
                }
                self.output.push_str(") ");
                self.expression(&arguments[1].value, LOWEST, followed);
            }
            Form::Keyword(keyword) => self.output.push_str(keyword),
            Form::Call => {
                match function {
                    Value::Symbol(name) => self.name(name),
                    Value::Character(vector) if vector.elements.len() == 1 => {
                        self.name(vector.elements[0].as_deref().unwrap_or("NA"));
                    }
                    other => self.expression(other, POSTFIX, true),
                }
                self.output.push('(');
                self.arguments(arguments);
                self.output.push(')');
            }
        }
    }

    fn block(&mut self, statements: &[Argument]) {
        self.output.push('{');
        self.indent += 1;
        for statement in statements {
            self.newline();
            self.expression(&statement.value, LOWEST, false);
        }
        self.indent -= 1;
        self.newline();
        self.output.push('}');
    }

    fn newline(&mut self) {
        self.output.push('\n');
        for _ in 0..self.indent {
            self.output.push_str(INDENT);
        }
    }

    fn arguments(&mut self, arguments: &[Argument]) {
        for (index, argument) in arguments.iter().enumerate() {
            if index > 0 {
                self.output.push_str(", ");
            }
            if let Some(name) = argument.name.as_deref().filter(|name| !name.is_empty()) {
                self.name(name);
                self.output.push_str(" = ");
            }
            self.delimited(&argument.value);
        }
    }

    fn formals(&mut self, formals: &[Argument]) {
        for (index, formal) in formals.iter().enumerate() {
            if index > 0 {
                self.output.push_str(", ");
            }
            self.name(formal.name.as_deref().unwrap_or(""));
            if !is_missing(&formal.value) {
                self.output.push_str(" = ");
                self.delimited(&formal.value);
            }
        }
    }

    fn member(&mut self, value: &Value) {
        self.name(member_name(value).unwrap_or_default());
    }

    fn name(&mut self, name: &str) {
        if is_syntactic_name(name) {
            self.output.push_str(name);
        } else {
            self.output.push('`');
            for character in name.chars() {
                match character {
                    '`' => self.output.push_str("\\`"),
                    '\\' => self.output.push_str("\\\\"),
                    character => self.output.push(character),
                }
            }
            self.output.push('`');
        }
    }

    fn constant(&mut self, value: &Value) {
        let attributes: Vec<_> = value
            .attributes()
            .iter()
            .filter(|(name, _)| &**name != "names")
            .collect();
        if !attributes.is_empty() {
            self.output.push_str("structure(");
        }
        self.vector(value);
        if !attributes.is_empty() {
            for (name, attribute) in attributes {
                self.output.push_str(", ");
                self.name(name);
                self.output.push_str(" = ");
                self.delimited(attribute);
            }
            self.output.push(')');
        }
    }

    fn vector(&mut self, value: &Value) {
        let names = value.names();
        let (empty, length, wrapper) = match value {
            Value::Logical(vector) => ("logical(0)", vector.elements.len(), "c"),
            Value::Integer(vector) => ("integer(0)", vector.elements.len(), "c"),
            Value::Double(vector) => ("numeric(0)", vector.elements.len(), "c"),
            Value::Complex(vector) => ("complex(0)", vector.elements.len(), "c"),
            Value::Character(vector) => ("character(0)", vector.elements.len(), "c"),
            Value::Raw(vector) => ("raw(0)", vector.elements.len(), "as.raw(c"),
            Value::List(vector) => ("list()", vector.elements.len(), "list"),
            Value::Expression(vector) => ("expression()", vector.elements.len(), "expression"),
            _ => return,
        };
        if length == 0 {
            self.output.push_str(empty);
            return;
        }
        if let Value::Integer(vector) = value
            && names.is_none()
            && let Some((first, last)) = integer_range(&vector.elements)
        {
            self.output.push_str(&format!("{first}:{last}"));
            return;
        }
        let is_list = matches!(value, Value::List(_) | Value::Expression(_));
        let scalar = length == 1 && names.is_none() && !is_list;
        if !scalar {
            self.output.push_str(wrapper);
            self.output.push('(');
        }
        for index in 0..length {
            if index > 0 {
                self.output.push_str(", ");
            }
            if let Some(name) = names
                .and_then(|names| names.get(index))
                .and_then(|name| name.as_deref())
                .filter(|name| !name.is_empty())
            {
                self.name(name);
                self.output.push_str(" = ");
            }
            self.element(value, index, scalar);
        }
        if !scalar {
            self.output.push(')');
            if matches!(value, Value::Raw(_)) {
                self.output.push(')');
            }
        }
    }

    /// One element of a vector. A lone `NA` names its type, since a bare `NA` is logical.
    fn element(&mut self, value: &Value, index: usize, alone: bool) {
        let typed_na = |typed: &'static str| if alone { typed } else { "NA" };
        let text = match value {
            Value::Logical(vector) => match vector.elements.get(index).copied().flatten() {
                Some(true) => "TRUE".to_string(),
                Some(false) => "FALSE".to_string(),
                None => "NA".to_string(),
            },
            Value::Integer(vector) => match vector.elements.get(index).copied().flatten() {
                Some(integer) => format!("{integer}L"),
                None => typed_na("NA_integer_").to_string(),
            },
            Value::Double(vector) => match vector.elements.get(index) {
                Some(&double) if is_na_real(double) => typed_na("NA_real_").to_string(),
                Some(&double) => format_double(double),
                None => return,
            },
            Value::Complex(vector) => match vector.elements.get(index) {
                Some(&(real, _)) if is_na_real(real) => typed_na("NA_complex_").to_string(),
                Some(&(real, imaginary)) => {
                    let imaginary_text = format_double(imaginary.abs());
                    let sign = if imaginary < 0.0 { '-' } else { '+' };
                    format!("{}{sign}{imaginary_text}i", format_double(real))
                }
                None => return,
            },
            Value::Character(vector) => match vector.elements.get(index) {
                Some(Some(string)) => quote_string(string),
                Some(None) => typed_na("NA_character_").to_string(),
                None => return,
            },
            Value::Raw(vector) => match vector.elements.get(index) {
                Some(byte) => format!("0x{byte:02x}"),
                None => return,
            },
            Value::List(vector) | Value::Expression(vector) => {
                if let Some(element) = vector.elements.get(index) {
                    self.delimited(element);
                }
                return;
            }
            _ => return,
        };
        self.output.push_str(&text);
    }
}

fn form_of<'a>(name: &'a str, arguments: &[Argument]) -> Form<'a> {
    let unnamed = arguments.iter().all(|argument| argument.name.is_none());
    let count = arguments.len();
    let binary = |precedence, associativity| Form::Binary {
        operator: name,
        precedence,
        associativity,
        swapped: false,
    };
    let unary = |precedence| Form::Unary {
        operator: name,
        precedence,
    };
    match (name, count) {
        ("{", _) => Form::Block,
        ("(", 1) => Form::Parenthesized,
        ("if", 2 | 3) => Form::If,
        ("for", 3) => Form::For,
        ("while", 2) => Form::While,
        ("repeat", 1) => Form::Repeat,
        ("function", 2 | 3) => Form::Function,
        ("break" | "next", 0) => Form::Keyword(name),
        ("[" | "[[", 1..) if arguments[0].name.is_none() => {
            if name == "[" {
                Form::Index {
                    open: "[",
                    close: "]",
                }
            } else {
                Form::Index {
                    open: "[[",
                    close: "]]",
                }
            }
        }
        _ if !unnamed => Form::Call,
        // R's grammar takes only a name or a string after these operators, so a computed
        // member, as rlang builds with `!!`, needs the prefix form.
        ("$" | "@", 2) if member_name(&arguments[1].value).is_some() => Form::Dollar(name),
        ("::" | ":::", 2)
            if arguments
                .iter()
                .all(|argument| member_name(&argument.value).is_some()) =>
        {
            Form::Namespace(name)
        }
        ("-" | "+", 1) => unary(UNARY),
        ("!", 1) => unary(NOT),
        ("~", 1) => unary(TILDE),
        ("?", 1) => unary(HELP),
        (_, 2) => match name {
            "?" => binary(HELP, Associativity::Left),
            "=" => binary(EQUALS_ASSIGN, Associativity::Right),
            "<-" | "<<-" | ":=" => binary(LEFT_ASSIGN, Associativity::Right),
            "->" | "->>" => Form::Binary {
                operator: if name == "->" { "<-" } else { "<<-" },
                precedence: LEFT_ASSIGN,
                associativity: Associativity::Right,
                swapped: true,
            },
            "~" => binary(TILDE, Associativity::Left),
            "||" | "|" => binary(OR, Associativity::Left),
            "&&" | "&" => binary(AND, Associativity::Left),
            "==" | "!=" | "<" | ">" | "<=" | ">=" => binary(COMPARISON, Associativity::None),
            "+" | "-" => binary(SUM, Associativity::Left),
            "*" | "/" => binary(PRODUCT, Associativity::Left),
            "|>" => binary(SPECIAL, Associativity::Left),
            ":" => binary(RANGE, Associativity::Left),
            "^" => binary(POWER, Associativity::Right),
            _ if name.len() >= 2 && name.starts_with('%') && name.ends_with('%') => {
                binary(SPECIAL, Associativity::Left)
            }
            _ => Form::Call,
        },
        _ => Form::Call,
    }
}

fn precedence_of(value: &Value) -> u8 {
    match value {
        Value::Language(function, arguments) => {
            let Value::Symbol(name) = &**function else {
                return POSTFIX;
            };
            match form_of(name, arguments) {
                Form::Binary { precedence, .. } => precedence,
                Form::Unary { precedence, .. } => precedence,
                Form::Dollar(_) | Form::Index { .. } | Form::Call => POSTFIX,
                Form::Namespace(_) => NAMESPACE,
                Form::Block | Form::Parenthesized | Form::Keyword(_) => ATOM,
                Form::If | Form::For | Form::While | Form::Repeat | Form::Function => LOWEST,
            }
        }
        Value::Closure(_) => LOWEST,
        Value::Double(vector)
            if vector.attributes.is_empty()
                && vector.elements.len() == 1
                && vector.elements[0] < 0.0 =>
        {
            UNARY
        }
        Value::Integer(vector)
            if vector.attributes.is_empty()
                && matches!(vector.elements.as_slice(), [Some(integer)] if *integer < 0) =>
        {
            UNARY
        }
        Value::Integer(vector)
            if vector.attributes.is_empty() && integer_range(&vector.elements).is_some() =>
        {
            RANGE
        }
        Value::Complex(vector) if vector.attributes.is_empty() && vector.elements.len() == 1 => SUM,
        _ => ATOM,
    }
}

/// The bounds of an integer vector that counts up by one, which R writes as `first:last`.
fn integer_range(elements: &[Option<i32>]) -> Option<(i32, i32)> {
    if elements.len() < 2 {
        return None;
    }
    let first = elements.first().copied().flatten()?;
    let consecutive = elements.iter().enumerate().all(|(index, element)| {
        i32::try_from(index)
            .ok()
            .and_then(|offset| first.checked_add(offset))
            == *element
    });
    consecutive.then(|| (first, first + (elements.len() as i32 - 1)))
}

/// Whether the rightmost part of `value` is an `if` without `else`, which would capture an
/// `else` written after `value`.
fn ends_with_open_if(value: &Value) -> bool {
    let Value::Language(function, arguments) = value else {
        return false;
    };
    let Value::Symbol(name) = &**function else {
        return false;
    };
    let last = |arguments: &[Argument]| {
        arguments
            .last()
            .is_some_and(|argument| ends_with_open_if(&argument.value))
    };
    match form_of(name, arguments) {
        Form::If => arguments.len() == 2 || last(arguments),
        Form::Binary { swapped: false, .. }
        | Form::Unary { .. }
        | Form::For
        | Form::While
        | Form::Repeat => last(arguments),
        Form::Binary { swapped: true, .. } => arguments
            .first()
            .is_some_and(|argument| ends_with_open_if(&argument.value)),
        Form::Function => arguments
            .get(1)
            .is_some_and(|argument| ends_with_open_if(&argument.value)),
        _ => false,
    }
}

/// The name on the right of `$` or `@`, or on either side of `::`, when it is written as a
/// symbol or a string.
fn member_name(value: &Value) -> Option<&str> {
    match value {
        Value::Symbol(name) => Some(name),
        Value::Character(vector) => match vector.elements.as_slice() {
            [Some(name)] if vector.attributes.is_empty() => Some(name),
            _ => None,
        },
        _ => None,
    }
}

fn is_call_to(value: &Value, operator: &str) -> bool {
    matches!(value, Value::Language(function, arguments)
        if matches!(&**function, Value::Symbol(name) if &**name == operator) && arguments.len() == 2)
}

fn is_missing(value: &Value) -> bool {
    match value {
        Value::Missing => true,
        Value::Symbol(name) => name.is_empty(),
        _ => false,
    }
}

fn is_dot_dot_number(name: &str) -> bool {
    name.strip_prefix("..").is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::unserialize::{Closure, Vector},
        std::sync::Arc,
    };

    fn symbol(name: &str) -> Value {
        Value::Symbol(Arc::from(name))
    }

    fn call(function: &str, arguments: Vec<Value>) -> Value {
        Value::Language(
            Box::new(symbol(function)),
            arguments
                .into_iter()
                .map(|value| Argument { name: None, value })
                .collect(),
        )
    }

    fn named(name: &str, value: Value) -> Argument {
        Argument {
            name: Some(Arc::from(name)),
            value,
        }
    }

    fn double(value: f64) -> Value {
        Value::Double(Vector::new(vec![value]))
    }

    fn integer(value: i32) -> Value {
        Value::Integer(Vector::new(vec![Some(value)]))
    }

    fn string(value: &str) -> Value {
        Value::Character(Vector::new(vec![Some(value.to_string())]))
    }

    #[test]
    fn operators_get_parentheses_only_where_needed() {
        let a = || symbol("a");
        let b = || symbol("b");
        let c = || symbol("c");
        let cases = [
            (call("+", vec![call("*", vec![a(), b()]), c()]), "a * b + c"),
            (
                call("*", vec![call("+", vec![a(), b()]), c()]),
                "(a + b) * c",
            ),
            (
                call("-", vec![a(), call("-", vec![b(), c()])]),
                "a - (b - c)",
            ),
            (call("-", vec![call("-", vec![a(), b()]), c()]), "a - b - c"),
            (call("^", vec![a(), call("^", vec![b(), c()])]), "a^b^c"),
            (call("^", vec![call("^", vec![a(), b()]), c()]), "(a^b)^c"),
            (call("^", vec![call("-", vec![a()]), double(2.0)]), "(-a)^2"),
            (call("-", vec![call("^", vec![a(), double(2.0)])]), "-a^2"),
            (call("!", vec![call("==", vec![a(), b()])]), "!a == b"),
            (call("==", vec![call("!", vec![a()]), b()]), "(!a) == b"),
            (
                call(":", vec![integer(1), call("-", vec![a(), integer(1)])]),
                "1L:(a - 1L)",
            ),
            (
                call("%in%", vec![a(), call("c", vec![b(), c()])]),
                "a %in% c(b, c)",
            ),
            (
                call("<-", vec![a(), call("<-", vec![b(), c()])]),
                "a <- b <- c",
            ),
            (call("->", vec![a(), b()]), "b <- a"),
            (call("~", vec![a(), call("+", vec![b(), c()])]), "a ~ b + c"),
            (call("~", vec![b()]), "~b"),
            (
                call("$", vec![call("[[", vec![a(), integer(1)]), symbol("x y")]),
                "a[[1L]]$`x y`",
            ),
            (call("::", vec![symbol("pkg"), symbol("f")]), "pkg::f"),
            (call("@", vec![a(), call("!", vec![b()])]), "`@`(a, !b)"),
            (call("[", vec![a(), Value::Missing, double(1.0)]), "a[, 1]"),
            (
                call("&&", vec![call("||", vec![a(), b()]), c()]),
                "(a || b) && c",
            ),
            (call("(", vec![call("+", vec![a(), b()])]), "(a + b)"),
            (call("+", vec![a(), double(-1.0)]), "a + -1"),
            (call("^", vec![double(-2.0), a()]), "(-2)^a"),
            (call("+", vec![a(), b(), c()]), "`+`(a, b, c)"),
            (call("f", vec![call("=", vec![a(), b()])]), "f((a = b))"),
        ];
        for (tree, expected) in cases {
            assert_eq!(deparse(&tree), expected);
        }
    }

    #[test]
    fn control_flow_and_functions() {
        let function = Value::Closure(Box::new(Closure {
            formals: vec![
                named("x", Value::Missing),
                named("y", double(1.0)),
                named("...", Value::Missing),
            ],
            body: call(
                "{",
                vec![
                    call(
                        "if",
                        vec![
                            call("is.null", vec![symbol("x")]),
                            call("return", vec![Value::Null]),
                            call(
                                "{",
                                vec![call("<-", vec![symbol("x"), call("-", vec![symbol("x")])])],
                            ),
                        ],
                    ),
                    call(
                        "for",
                        vec![
                            symbol("i"),
                            call("seq_len", vec![symbol("y")]),
                            call("{", vec![call("next", vec![])]),
                        ],
                    ),
                    call(
                        "while",
                        vec![
                            Value::Logical(Vector::new(vec![Some(true)])),
                            call("break", vec![]),
                        ],
                    ),
                    call("repeat", vec![call("{", vec![])]),
                ],
            ),
            attributes: Vec::new(),
        }));
        assert_eq!(
            deparse(&function),
            "function(x, y = 1, ...) {\n    if (is.null(x)) return(NULL) else {\n        x <- -x\n    }\n    for (i in seq_len(y)) {\n        next\n    }\n    while (TRUE) break\n    repeat {\n    }\n}"
        );
    }

    #[test]
    fn open_constructs_are_closed_before_what_follows() {
        let open_if = call("if", vec![symbol("a"), symbol("b")]);
        let assigned = call("<-", vec![symbol("x"), open_if.clone()]);
        assert_eq!(deparse(&assigned), "x <- if (a) b");
        let nested = call("if", vec![symbol("c"), open_if.clone(), symbol("d")]);
        assert_eq!(deparse(&nested), "if (c) (if (a) b) else d");
        let sum = call("+", vec![open_if.clone(), double(1.0)]);
        assert_eq!(deparse(&sum), "(if (a) b) + 1");
        let lambda = call(
            "function",
            vec![
                Value::Pairlist(vec![named("x", Value::Missing)]),
                symbol("x"),
            ],
        );
        let applied = Value::Language(
            Box::new(lambda),
            vec![Argument {
                name: None,
                value: double(1.0),
            }],
        );
        assert_eq!(deparse(&applied), "(function(x) x)(1)");
    }

    #[test]
    fn constants_print_as_r_does() {
        let cases = [
            (double(1.0), "1"),
            (double(0.1), "0.1"),
            (double(100000.0), "1e+05"),
            (double(123456.0), "123456"),
            (double(1e-4), "1e-04"),
            (double(0.001), "0.001"),
            (double(1.0 / 3.0), "0.333333333333333"),
            (double(1.23456), "1.23456"),
            (double(1e300), "1e+300"),
            (double(f64::INFINITY), "Inf"),
            (double(f64::NAN), "NaN"),
            (double(f64::from_bits(0x7ff0_0000_0000_07a2)), "NA_real_"),
            (integer(3), "3L"),
            (Value::Integer(Vector::new(vec![None])), "NA_integer_"),
            (
                Value::Integer(Vector::new(vec![Some(1), Some(2), Some(3)])),
                "1:3",
            ),
            (
                Value::Integer(Vector::new(vec![Some(1), None])),
                "c(1L, NA)",
            ),
            (
                string("a \"quoted\"\n\\ line\u{1}"),
                "\"a \\\"quoted\\\"\\n\\\\ line\\001\"",
            ),
            (Value::Character(Vector::new(vec![None])), "NA_character_"),
            (Value::Character(Vector::new(Vec::new())), "character(0)"),
            (
                Value::Logical(Vector::new(vec![Some(true), None])),
                "c(TRUE, NA)",
            ),
            (Value::Complex(Vector::new(vec![(1.0, -2.0)])), "1-2i"),
            (
                Value::Raw(Vector::new(vec![0, 255])),
                "as.raw(c(0x00, 0xff))",
            ),
            (symbol("if"), "`if`"),
            (symbol("..1"), "..1"),
            (symbol(".2x"), "`.2x`"),
            (symbol("été"), "été"),
        ];
        for (value, expected) in cases {
            assert_eq!(deparse(&value), expected);
        }
    }

    #[test]
    fn names_and_attributes() {
        let names = Value::Character(Vector::new(vec![
            Some("a".to_string()),
            Some("".to_string()),
            Some("b c".to_string()),
        ]));
        let list = Value::List(Vector {
            elements: vec![double(1.0), string("x"), Value::Null],
            attributes: vec![(Arc::from("names"), names)],
        });
        assert_eq!(deparse(&list), "list(a = 1, \"x\", `b c` = NULL)");
        let classed = Value::Integer(Vector {
            elements: vec![Some(2)],
            attributes: vec![(Arc::from("class"), string("foo"))],
        });
        assert_eq!(deparse(&classed), "structure(2L, class = \"foo\")");
    }
}
