//! The checks R makes on text its grammar accepts: a parameter name a function declares
//! twice, a `|>` whose right side R's pipe cannot call, a pipe placeholder `_` that no pipe
//! replaces, and an empty string that names a function or an argument. R makes them in its
//! grammar actions and raises each as an error of the parse, so a program that fails one never
//! starts. They run on the finished tree, so the checker and the
//! runtime read one verdict.

use {
    crate::{
        Classed, PipeCheck, SyntaxError, Unexpected,
        kind::SyntaxKind::{self, *},
        lexer::{EscapePiece, raw_string_content, unescape_quoted},
        tree::{NodeOrToken, NodeRef, TokenRef, Tree},
    },
    std::borrow::Cow,
    text_size::TextRange,
};

pub(crate) fn validate(tree: &Tree, errors: &mut Vec<SyntaxError>) {
    let root = tree.root();
    let mut pipes = Vec::new();
    let mut placeholders = Vec::new();
    for token in root.tokens_outside(SyntaxKind::is_erased, |kind| {
        matches!(
            kind,
            FUNCTION_KW | BACKSLASH | PIPE_GREATER | UNDERSCORE | STRING | RAW_STRING
        )
    }) {
        let Some(parent) = token.parent() else {
            continue;
        };
        match (token.kind(), parent.kind()) {
            // R makes a symbol of a string that names a call's function or an argument, and
            // has no symbol for the empty string.
            (STRING | RAW_STRING, LITERAL) => {
                let empty = match token.kind() {
                    STRING => token.text().len() == 2,
                    _ => raw_string_content(token.text()).is_empty(),
                };
                let message = match parent.parent() {
                    _ if !empty => continue,
                    Some(call) if call.kind() == CALL_EXPR && callee(call) == Some(parent) => {
                        "a function's name cannot be empty: this call names its function with an empty string"
                    }
                    Some(argument)
                        if argument.kind() == ARGUMENT
                            && argument.child_nodes().next() == Some(parent)
                            && argument.child_tokens().any(|token| token.kind() == EQ) =>
                    {
                        "an argument's name cannot be empty: name the argument, or drop the empty string and its `=`"
                    }
                    _ => continue,
                };
                let mut error = SyntaxError::new(message, token.range());
                error.unexpected = Unexpected::Classed(Classed::EmptyName);
                errors.push(error);
            }
            (FUNCTION_KW | BACKSLASH, FUNCTION_DEF) => repeated_parameters(parent, errors),
            (PIPE_GREATER, BINARY_EXPR) => pipes.push(parent),
            (UNDERSCORE, NAME) => placeholders.push(parent),
            _ => {}
        }
    }
    if placeholders.is_empty() && pipes.is_empty() {
        return;
    }

    // R's grammar actions run innermost first, and a pipe nested in another one sits later
    // in preorder, so by the time a pipe is checked every pipe inside it has taken its
    // placeholder out of `placeholders`.
    pipes.sort_unstable_by_key(|pipe| std::cmp::Reverse(pipe.position()));
    for pipe in pipes {
        let mut sides = pipe.child_nodes().filter(|child| !child.kind().is_erased());
        let (Some(_), Some(right)) = (sides.next(), sides.next()) else {
            continue;
        };
        // A right side that did not parse has its own report, and its tree is not the call R
        // would have built.
        let checked = if right.any_kind(|kind| kind == ERROR) {
            Err(None)
        } else {
            pipe_target(right, &placeholders)
        };
        match checked {
            Ok(Some(replaced)) => {
                placeholders.retain(|placeholder| *placeholder != replaced);
            }
            Ok(None) => {}
            Err(refusal) => {
                if let Some((check, range)) = refusal {
                    let mut error = SyntaxError::new(pipe_message(check, right), range);
                    error.unexpected = Unexpected::Classed(Classed::Pipe {
                        check,
                        target: right.range(),
                    });
                    errors.push(error);
                }
                // One mistake, one finding: a placeholder of a refused pipe is not reported
                // again as one no pipe replaces.
                placeholders.retain(|placeholder| !is_free_in(*placeholder, right));
            }
        }
    }

    // R checks each top-level expression for a placeholder that is left, and reports the
    // expression once. It never looks into a function's parameters.
    let mut reported = None;
    for placeholder in placeholders {
        let mut statement = placeholder;
        let mut in_parameters = false;
        while let Some(parent) = statement.parent() {
            if parent.position() == root.position() {
                break;
            }
            in_parameters |= parent.kind() == PARAMETER_LIST;
            statement = parent;
        }
        if in_parameters || reported == Some(statement.position()) {
            continue;
        }
        reported = Some(statement.position());
        let mut error = SyntaxError::new(
            "`_` is the pipe placeholder: it can only be a named argument of the call on the right side of `|>`, as in `x |> f(data = _)`",
            placeholder.range(),
        );
        error.unexpected = Unexpected::Classed(Classed::StrayPlaceholder);
        errors.push(error);
    }
}

/// The `_` a pipe replaces with its left side, given the pipe's right side: the object an
/// extraction chain such as `_$a[[1]]` starts from, or else the first argument of the call
/// that is a `_` with a name. `None` when the left side becomes the call's first argument
/// instead.
///
/// This is the pipe's shape alone. Whether R accepts the pipe is in the parse's errors.
pub fn pipe_placeholder(right: NodeRef<'_>) -> Option<NodeRef<'_>> {
    if let Some((placeholder, _)) = extraction_chain(right) {
        return Some(placeholder);
    }
    let arguments = direct_arguments(right);
    let first = arguments
        .iter()
        .find(|argument| argument.value.is_some_and(is_placeholder))?;
    first.value.filter(|_| first.named)
}

/// Checks the right side of one pipe the way R's grammar action does, in R's order, and
/// gives the placeholder the pipe replaces. A refusal without a report is a mistake some
/// other error already names.
fn pipe_target<'a>(
    right: NodeRef<'a>,
    placeholders: &[NodeRef<'a>],
) -> Result<Option<NodeRef<'a>>, Option<(PipeCheck, TextRange)>> {
    if matches!(right.kind(), NAME | LITERAL) {
        return Err(Some((PipeCheck::NotCall, right.range())));
    }
    let free_in = |node: NodeRef<'a>| {
        placeholders
            .iter()
            .copied()
            .find(|placeholder| is_free_in(*placeholder, node))
    };
    if let Some((placeholder, levels)) = extraction_chain(right) {
        for level in levels {
            let repeated = direct_arguments(level)
                .iter()
                .skip(1)
                .filter_map(|argument| argument.value)
                .find_map(free_in);
            if let Some(repeated) = repeated {
                return Err(Some((PipeCheck::PlaceholderRepeated, repeated.range())));
            }
        }
        return Ok(Some(placeholder));
    }
    if right.kind() == CALL_EXPR
        && let Some(placeholder) = callee(right).and_then(free_in)
    {
        return Err(Some((
            PipeCheck::PlaceholderInFunction,
            placeholder.range(),
        )));
    }
    let arguments = direct_arguments(right);
    let mut passed = arguments
        .iter()
        .filter(|argument| argument.value.is_some_and(is_placeholder));
    if let Some(first) = passed.next() {
        let placeholder = first.value.ok_or(None)?;
        if !first.named {
            return Err(Some((PipeCheck::PlaceholderNotNamed, placeholder.range())));
        }
        if let Some(repeated) = passed.next().and_then(|argument| argument.value) {
            return Err(Some((PipeCheck::PlaceholderRepeated, repeated.range())));
        }
        return Ok(Some(placeholder));
    }
    if called_function(right).is_some_and(|name| SYNTACTIC_FUNCTIONS.contains(&&*name)) {
        return Err(Some((PipeCheck::Special, right.range())));
    }
    Ok(None)
}

fn pipe_message(check: PipeCheck, right: NodeRef<'_>) -> String {
    match check {
        PipeCheck::NotCall => {
            "the right side of `|>` must be a function call, such as `f()`".to_owned()
        }
        PipeCheck::Special => {
            let name = called_function(right).unwrap_or_default();
            format!(
                "R's pipe cannot call `{}`: the right side of `|>` must call an ordinary function, such as `f()`",
                String::from_utf8_lossy(&name)
            )
        }
        PipeCheck::PlaceholderInFunction => {
            "the pipe placeholder `_` cannot be part of the function a `|>` calls".to_owned()
        }
        PipeCheck::PlaceholderNotNamed => {
            "the pipe placeholder `_` must be passed as a named argument, as in `f(data = _)`"
                .to_owned()
        }
        PipeCheck::PlaceholderRepeated => {
            "the pipe placeholder `_` may appear only once in the right side of a `|>`".to_owned()
        }
    }
}

/// The functions R's parser gives a syntax of their own. A pipe's right side cannot call one
/// by name.
const SYNTACTIC_FUNCTIONS: &[&[u8]] = &[
    b"if",
    b"while",
    b"repeat",
    b"for",
    b"break",
    b"next",
    b"return",
    b"function",
    b"(",
    b"{",
    b"+",
    b"-",
    b"*",
    b"/",
    b"^",
    b"%%",
    b"%/%",
    b"%*%",
    b":",
    b"::",
    b":::",
    b"?",
    b"~",
    b"==",
    b"!=",
    b"<",
    b">",
    b"<=",
    b">=",
    b"&",
    b"|",
    b"&&",
    b"||",
    b"!",
    b"<-",
    b"<<-",
    b"=",
    b"$",
    b"[",
    b"[[",
    b"@",
    b"$<-",
    b"[<-",
    b"[[<-",
    b"@<-",
];

/// An extraction chain that starts from a placeholder: `right` and the objects below it, as
/// long as each is a `[`, `[[`, `$` or `@` call, when the innermost object is `_`. Gives that
/// placeholder and the chain's calls, outermost first.
fn extraction_chain(right: NodeRef<'_>) -> Option<(NodeRef<'_>, Vec<NodeRef<'_>>)> {
    let mut levels = Vec::new();
    let mut node = right;
    loop {
        let name = called_function(node)?;
        if !matches!(&*name, b"[" | b"[[" | b"$" | b"@") {
            return None;
        }
        levels.push(node);
        let object = direct_arguments(node).first()?.value?;
        if is_placeholder(object) {
            return Some((object, levels));
        }
        node = object;
    }
}

/// Whether `placeholder` lies in `node` where R's search for a placeholder reaches it: R
/// descends calls, and a function's parameters are not one.
fn is_free_in(placeholder: NodeRef<'_>, node: NodeRef<'_>) -> bool {
    if !node.range().contains_range(placeholder.range()) {
        return false;
    }
    let mut current = placeholder;
    while current.position() != node.position() {
        let Some(parent) = current.parent() else {
            return false;
        };
        if parent.kind() == PARAMETER_LIST {
            return false;
        }
        current = parent;
    }
    true
}

fn is_placeholder(node: NodeRef<'_>) -> bool {
    node.kind() == NAME
        && node
            .first_token()
            .is_some_and(|token| token.kind() == UNDERSCORE)
}

/// The name of the function `node` calls in the call R's parser builds for it: an operator
/// or a keyword, or a call's own function when that is written as a name or a string. `None`
/// for a name or a constant, which is no call, and for a call of an expression's value.
pub fn called_function(node: NodeRef<'_>) -> Option<Cow<'_, [u8]>> {
    let name: &[u8] = match node.kind() {
        CALL_EXPR => {
            let callee = callee(node)?;
            let token = callee.first_token()?;
            return match (callee.kind(), token.kind()) {
                (NAME, IDENT | DOTS | DOTDOTI) | (LITERAL, STRING | RAW_STRING) => {
                    Some(decoded_name(token))
                }
                _ => None,
            };
        }
        BINARY_EXPR | UNARY_EXPR => {
            let operator = node
                .child_tokens()
                .find(|token| !token.kind().is_trivia())?;
            match operator.kind() {
                MINUS_GREATER => b"<-",
                MINUS_GREATER2 => b"<<-",
                CARET => b"^",
                _ => return Some(Cow::Borrowed(operator.text().as_bytes())),
            }
        }
        SUBSET_EXPR => b"[",
        SUBSET2_EXPR => b"[[",
        DOLLAR_EXPR => b"$",
        AT_EXPR => b"@",
        NAMESPACE_EXPR => b"::",
        PAREN_EXPR => b"(",
        BRACE_EXPR => b"{",
        FUNCTION_DEF => b"function",
        IF_EXPR => b"if",
        FOR_EXPR => b"for",
        WHILE_EXPR => b"while",
        REPEAT_EXPR => b"repeat",
        BREAK_EXPR => b"break",
        NEXT_EXPR => b"next",
        _ => return None,
    };
    Some(Cow::Borrowed(name))
}

fn callee(call: NodeRef<'_>) -> Option<NodeRef<'_>> {
    call.child_nodes()
        .find(|child| !child.kind().is_erased() && child.kind() != ARGUMENT_LIST)
}

struct Argument<'a> {
    /// `None` for an empty argument, as in `x[, 1]`.
    value: Option<NodeRef<'a>>,
    named: bool,
}

/// The arguments of the call R's parser builds for `node`, in order. R's pipe looks for its
/// placeholder among these only, never inside one.
fn direct_arguments(node: NodeRef<'_>) -> Vec<Argument<'_>> {
    let unnamed = |value| Argument {
        value: Some(value),
        named: false,
    };
    let operands = node.child_nodes().filter(|child| !child.kind().is_erased());
    match node.kind() {
        CALL_EXPR | SUBSET_EXPR | SUBSET2_EXPR => {
            let mut arguments = Vec::new();
            for child in operands {
                if child.kind() != ARGUMENT_LIST {
                    // A call's function is not one of its arguments. The object of a subset
                    // is the first.
                    if node.kind() != CALL_EXPR {
                        arguments.push(unnamed(child));
                    }
                    continue;
                }
                for argument in child.child_nodes().filter(|item| item.kind() == ARGUMENT) {
                    let mut named = false;
                    let mut value = None;
                    for element in argument.children() {
                        match element {
                            NodeOrToken::Token(token) if token.kind() == EQ => {
                                named = true;
                                value = None;
                            }
                            NodeOrToken::Node(item)
                                if value.is_none() && !item.kind().is_erased() =>
                            {
                                value = Some(item);
                            }
                            _ => {}
                        }
                    }
                    arguments.push(Argument { value, named });
                }
            }
            arguments
        }
        // R's call holds the parameters too, but as a list its search never enters.
        FUNCTION_DEF => operands
            .filter(|child| child.kind() != PARAMETER_LIST)
            .map(unnamed)
            .collect(),
        BINARY_EXPR | UNARY_EXPR | PAREN_EXPR | BRACE_EXPR | DOLLAR_EXPR | AT_EXPR
        | NAMESPACE_EXPR | IF_EXPR | FOR_EXPR | WHILE_EXPR | REPEAT_EXPR => {
            operands.map(unnamed).collect()
        }
        _ => Vec::new(),
    }
}

fn repeated_parameters(function: NodeRef<'_>, errors: &mut Vec<SyntaxError>) {
    let Some(list) = function
        .child_nodes()
        .find(|child| child.kind() == PARAMETER_LIST)
    else {
        return;
    };
    let mut names: Vec<Cow<'_, [u8]>> = Vec::new();
    for parameter in list.child_nodes().filter(|child| child.kind() == PARAMETER) {
        let Some(token) = parameter
            .child_tokens()
            .find(|token| matches!(token.kind(), IDENT | DOTS | DOTDOTI))
        else {
            continue;
        };
        let name = decoded_name(token);
        if names.contains(&name) {
            let mut error = SyntaxError::new(
                format!(
                    "the parameter `{}` is declared twice; a function's parameter names must differ",
                    String::from_utf8_lossy(&name)
                ),
                token.range(),
            );
            error.unexpected = Unexpected::Classed(Classed::RepeatedFormal);
            errors.push(error);
        } else {
            names.push(name);
        }
    }
}

/// The name a name or string token spells, as the bytes R interns: without its quotes and
/// with its escapes resolved.
fn decoded_name(token: TokenRef<'_>) -> Cow<'_, [u8]> {
    let text = token.text();
    let quoted = match token.kind() {
        RAW_STRING => return Cow::Borrowed(raw_string_content(text).as_bytes()),
        STRING => true,
        _ => text.starts_with('`'),
    };
    if !quoted {
        return Cow::Borrowed(text.as_bytes());
    }
    let inner = text
        .get(1..text.len().saturating_sub(1))
        .unwrap_or_default();
    if !inner.contains('\\') {
        return Cow::Borrowed(inner.as_bytes());
    }
    let mut bytes = Vec::with_capacity(inner.len());
    let decoded = unescape_quoted(inner, |piece| match piece {
        EscapePiece::Text(character) | EscapePiece::Unicode(character) => {
            bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        }
        EscapePiece::Byte(byte) => bytes.push(byte),
    });
    // The lexer reports an escape it rejects, so the spelling stands in for the name here.
    match decoded {
        Ok(()) => Cow::Owned(bytes),
        Err(_) => Cow::Borrowed(inner.as_bytes()),
    }
}
