//! GNU R's parse data: the token and grammar table that `getParseData` reads from a srcfile's
//! `parseData`. Tools such as lintr, styler and covr walk it, and they depend on GNU R's exact
//! ids and parent links, which come from the order in which its bison parser lexes tokens and
//! reduces grammar rules.
//!
//! ry's parser builds a different tree, so the table is produced by replaying GNU R's parse over
//! ry's tree. The replay keeps a lexer cursor over the source's tokens, comments and newlines,
//! with one token of lookahead, and walks the tree in the parser's order: every lexed item and
//! every non-empty grammar reduction takes the next id, and a reduction that needs the lookahead
//! to decide lexes it first. The rules that GNU R hides (such as `expr_or_assign_or_help`
//! wrapping a statement) still take ids, but they are left out of the table, and their children
//! hang off the nearest visible ancestor.

use {
    crate::{
        lower::{SourceMap, is_skipped, position},
        unwind::R,
        values::{self, symbols},
    },
    heap::object::Sexp,
    syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken},
};

/// GNU R's parse data for the source that `root` parsed: the integer matrix with its `tokens`,
/// `text` and `class` attributes, or `None` when the tree holds a construct GNU R cannot parse,
/// such as a ry record literal.
pub unsafe fn parse_data(root: &SyntaxNode, source: &SourceMap) -> R<Option<Sexp>> {
    let mut replay = Replay {
        lexemes: Vec::new(),
        cursor: 0,
        lookahead: None,
        last_id: 0,
        items: Vec::new(),
        comments: Vec::new(),
        contexts: Vec::new(),
        eat_lines: false,
        argument_start: false,
    };
    collect_lexemes(root, &mut replay.lexemes, source.text);
    let Some(roots) = replay.program(root) else {
        return Ok(None);
    };
    for &item in &roots {
        assign_parents(&mut replay.items, item, 0);
    }
    let mut top_level: Vec<i32> = replay
        .items
        .iter()
        .filter(|item| item.visible && item.parent == 0 && item.name != "COMMENT")
        .map(|item| item.id)
        .collect();
    top_level.sort_unstable();
    for index in 0..replay.comments.len() {
        let comment = replay.comments[index];
        let parent = comment_parent(&replay.items, &roots, &top_level, comment);
        replay.items[comment].parent = parent;
    }
    let mut visible: Vec<&Item> = replay.items.iter().filter(|item| item.visible).collect();
    visible.sort_by_key(|item| item.id);
    let mut matrix = Vec::with_capacity(visible.len() * 8);
    let mut tokens = Vec::with_capacity(visible.len());
    let mut texts = Vec::with_capacity(visible.len());
    for item in &visible {
        let (start, end) = item.range.unwrap_or((0, 0));
        let last = source.text[..end]
            .chars()
            .next_back()
            .map_or(start, |character| end - character.len_utf8())
            .max(start);
        let (first_line, _, first_column) = position(source, start);
        let (last_line, _, last_column) = position(source, last);
        matrix.extend_from_slice(&[
            first_line,
            first_column,
            last_line,
            last_column,
            i32::from(item.terminal),
            token_number(item.name),
            item.id,
            item.parent,
        ]);
        tokens.push(item.name);
        texts.push(if item.terminal {
            &source.text[start..end]
        } else {
            ""
        });
    }
    let data = values::integer_vector(&matrix)?;
    values::set_attr(
        data,
        symbols().dim,
        values::integer_vector(&[8, visible.len() as i32])?,
    )?;
    values::set_attr(
        data,
        values::install("tokens")?,
        values::string_vector(&tokens)?,
    )?;
    values::set_attr(
        data,
        values::install("text")?,
        values::string_vector(&texts)?,
    )?;
    values::set_attr(data, symbols().class, values::scalar_string("parseData")?)?;
    Ok(Some(data))
}

/// One item of R's token stream: a comment, one newline, or a token the grammar reads.
#[derive(Clone)]
enum Lexeme {
    Comment(usize, usize),
    /// A newline that ends a statement where it stands, at the top level or directly inside
    /// braces, is significant; any other newline is one GNU R's lexer skips.
    Newline {
        offset: usize,
        significant: bool,
    },
    Token(SyntaxToken),
}

/// The lexed token the parser has not consumed yet, with the id it took.
#[derive(Clone)]
enum Next {
    Token(SyntaxToken),
    Newline(usize),
    End,
}

struct Item {
    id: i32,
    name: &'static str,
    terminal: bool,
    visible: bool,
    /// The byte range it spans, or `None` for a rule that matched nothing.
    range: Option<(usize, usize)>,
    children: Vec<usize>,
    parent: i32,
}

/// The node an expression reduced to, as an index into the replay's items.
type ItemIndex = usize;

struct Replay {
    lexemes: Vec<Lexeme>,
    cursor: usize,
    lookahead: Option<(Next, i32)>,
    last_id: i32,
    items: Vec<Item>,
    comments: Vec<ItemIndex>,
    /// GNU R's lexer context stack: `(`, `[` and `{` for open brackets, and `i` for an `if`
    /// whose body may still be followed by `else` on a later line.
    contexts: Vec<u8>,
    /// Set after `{`, where GNU R's lexer skips the newlines before the first statement.
    eat_lines: bool,
    /// Set while the next token shifted is the first of an argument.
    argument_start: bool,
}

impl Replay {
    /// The top-level statements.
    fn program(&mut self, root: &SyntaxNode) -> Option<Vec<ItemIndex>> {
        let mut statements = root.children().filter(|child| !is_skipped(child));
        let mut roots = Vec::new();
        loop {
            match self.peek() {
                Next::End => return Some(roots),
                Next::Newline(_) => {
                    let newline = self.shift_separator()?;
                    self.reduce("prog", false, vec![newline], false);
                }
                Next::Token(token) if token.kind() == SyntaxKind::SEMICOLON => {
                    let separator = self.shift_separator()?;
                    self.reduce("prog", false, vec![separator], false);
                }
                Next::Token(_) => {
                    let statement = self.statement(&statements.next()?)?;
                    if matches!(&self.peek(), Next::Token(token) if token.kind() != SyntaxKind::SEMICOLON)
                    {
                        return None;
                    }
                    roots.push(statement);
                    if !matches!(self.peek(), Next::End) {
                        let separator = self.shift_separator()?;
                        self.reduce("prog", false, vec![statement, separator], false);
                    }
                }
            }
        }
    }

    /// A statement, which GNU R's grammar calls `expr_or_assign_or_help`: an expression, or an
    /// `=` assignment or binary `?` whose sides are statements too.
    fn statement(&mut self, node: &SyntaxNode) -> Option<ItemIndex> {
        if node.kind() == SyntaxKind::BINARY_EXPR {
            let operator = operator_token(node)?;
            if matches!(operator.kind(), SyntaxKind::EQ | SyntaxKind::QUESTION) {
                let [left, right] = operands(node)?;
                let left = self.statement(&left)?;
                let operator = self.shift(&operator, None)?;
                let right = self.statement(&right)?;
                return Some(self.reduce(
                    "expr_or_assign_or_help",
                    true,
                    vec![left, operator, right],
                    true,
                ));
            }
        }
        let expression = self.expression(node)?;
        Some(self.reduce("expr_or_assign_or_help", false, vec![expression], true))
    }

    /// An expression in a position GNU R's grammar reads as `expr_or_help`: a condition, an
    /// argument or a parameter default.
    fn help(&mut self, node: &SyntaxNode) -> Option<ItemIndex> {
        if node.kind() == SyntaxKind::BINARY_EXPR
            && operator_token(node)?.kind() == SyntaxKind::QUESTION
        {
            let [left, right] = operands(node)?;
            let left = self.help(&left)?;
            let operator = self.shift(&operator_token(node)?, None)?;
            let right = self.help(&right)?;
            return Some(self.reduce("expr_or_help", true, vec![left, operator, right], true));
        }
        let expression = self.expression(node)?;
        Some(self.reduce("expr_or_help", false, vec![expression], true))
    }

    fn expression(&mut self, node: &SyntaxNode) -> Option<ItemIndex> {
        let children: Vec<SyntaxNode> =
            node.children().filter(|child| !is_skipped(child)).collect();
        match node.kind() {
            SyntaxKind::NAME => {
                let token = first_token(node)?;
                let placeholder = token.kind() == SyntaxKind::UNDERSCORE;
                let symbol = self.shift(&token, None)?;
                Some(self.reduce("expr", true, vec![symbol], !placeholder))
            }
            SyntaxKind::LITERAL => {
                let token = first_token(node)?;
                let lookahead = match token.kind() {
                    SyntaxKind::STRING | SyntaxKind::RAW_STRING => true,
                    SyntaxKind::NULL_KW => self.argument_start,
                    _ => false,
                };
                let constant = self.shift(&token, None)?;
                Some(self.reduce("expr", true, vec![constant], lookahead))
            }
            SyntaxKind::BREAK_EXPR | SyntaxKind::NEXT_EXPR => {
                let keyword = self.shift(&first_token(node)?, None)?;
                Some(self.reduce("expr", true, vec![keyword], false))
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = operator_token(node)?;
                let operator = self.shift(&operator, None)?;
                let operand = children.first()?;
                let operand = if self.items[operator].name == "'?'" {
                    self.statement(operand)?
                } else {
                    self.expression(operand)?
                };
                Some(self.reduce("expr", true, vec![operator, operand], true))
            }
            SyntaxKind::BINARY_EXPR => {
                let operator = operator_token(node)?;
                if matches!(operator.kind(), SyntaxKind::EQ | SyntaxKind::QUESTION) {
                    return self.statement(node);
                }
                let [left, right] = operands(node)?;
                let left = self.expression(&left)?;
                let operator = self.shift(&operator, None)?;
                let right = self.expression(&right)?;
                Some(self.reduce("expr", true, vec![left, operator, right], true))
            }
            SyntaxKind::PAREN_EXPR => {
                let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
                let inner = self.statement(children.first()?)?;
                let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
                Some(self.reduce("expr", true, vec![open, inner, close], false))
            }
            SyntaxKind::BRACE_EXPR => self.brace(node, children),
            SyntaxKind::CALL_EXPR | SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let [callee, arguments] = <[SyntaxNode; 2]>::try_from(children).ok()?;
                let callee = self.expression(&callee)?;
                let mut parts = vec![callee];
                parts.extend(self.arguments(&arguments)?);
                Some(self.reduce("expr", true, parts, false))
            }
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => {
                let [object, member] = <[SyntaxNode; 2]>::try_from(children).ok()?;
                let object = self.expression(&object)?;
                let operator = self.shift(&operator_token(node)?, None)?;
                let name = (node.kind() == SyntaxKind::AT_EXPR
                    && first_token(&member)?.kind() == SyntaxKind::IDENT)
                    .then_some("SLOT");
                let member = self.shift(&first_token(&member)?, name)?;
                Some(self.reduce("expr", true, vec![object, operator, member], false))
            }
            SyntaxKind::NAMESPACE_EXPR => {
                let [package, name] = <[SyntaxNode; 2]>::try_from(children).ok()?;
                let package_token = first_token(&package)?;
                let package_name =
                    (package_token.kind() == SyntaxKind::IDENT).then_some("SYMBOL_PACKAGE");
                let package = self.shift(&package_token, package_name)?;
                let operator = self.shift(&operator_token(node)?, None)?;
                let name = self.shift(&first_token(&name)?, None)?;
                Some(self.reduce("expr", true, vec![package, operator, name], false))
            }
            SyntaxKind::FUNCTION_DEF => {
                let keyword = self.shift(&first_token(node)?, None)?;
                let parameters = children
                    .iter()
                    .find(|child| child.kind() == SyntaxKind::PARAMETER_LIST)?;
                let mut parts = vec![keyword];
                parts.extend(self.parameters(parameters)?);
                let body = children
                    .iter()
                    .find(|child| child.kind() != SyntaxKind::PARAMETER_LIST)?;
                parts.push(self.statement(body)?);
                Some(self.reduce("expr", true, parts, true))
            }
            SyntaxKind::IF_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::IF_KW)?;
                let condition = self.condition(node, children.first()?, "ifcond")?;
                let then = self.statement(children.get(1)?)?;
                let mut parts = vec![keyword, condition, then];
                if let Some(otherwise) = children.get(2) {
                    parts.push(self.shift_kind(node, SyntaxKind::ELSE_KW)?);
                    parts.push(self.statement(otherwise)?);
                }
                Some(self.reduce("expr", true, parts, true))
            }
            SyntaxKind::WHILE_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::WHILE_KW)?;
                let condition = self.condition(node, children.first()?, "cond")?;
                let body = self.statement(children.get(1)?)?;
                Some(self.reduce("expr", true, vec![keyword, condition, body], true))
            }
            SyntaxKind::FOR_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::FOR_KW)?;
                let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
                let variable = self.shift(&first_token(children.first()?)?, None)?;
                let keyword_in = self.shift_kind(node, SyntaxKind::IN_KW)?;
                let sequence = self.help(children.get(1)?)?;
                let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
                let header = self.reduce(
                    "forcond",
                    true,
                    vec![open, variable, keyword_in, sequence, close],
                    false,
                );
                let body = self.statement(children.get(2)?)?;
                Some(self.reduce("expr", true, vec![keyword, header, body], true))
            }
            SyntaxKind::REPEAT_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::REPEAT_KW)?;
                let body = self.statement(children.first()?)?;
                Some(self.reduce("expr", true, vec![keyword, body], true))
            }
            _ => None,
        }
    }

    /// `( condition )` after `if` or `while`, which GNU R's grammar reduces to a hidden rule as
    /// soon as the `)` is read.
    fn condition(
        &mut self,
        node: &SyntaxNode,
        condition: &SyntaxNode,
        name: &'static str,
    ) -> Option<ItemIndex> {
        let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
        let condition = self.help(condition)?;
        let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
        Some(self.reduce(name, false, vec![open, condition, close], false))
    }

    /// `{ ... }`: GNU R's `exprlist`, whose statements are separated by `;` or significant
    /// newlines and may be empty. Only the rule that ends a list with `;` is visible.
    fn brace(&mut self, node: &SyntaxNode, children: Vec<SyntaxNode>) -> Option<ItemIndex> {
        let open = self.shift_kind(node, SyntaxKind::L_BRACE)?;
        let mut statements = children.into_iter();
        let mut list: Option<ItemIndex> = None;
        let mut separator: Option<ItemIndex> = None;
        loop {
            let next = self.peek();
            let is_separator = match &next {
                Next::Newline(_) => true,
                Next::Token(token) => token.kind() == SyntaxKind::SEMICOLON,
                Next::End => return None,
            };
            let is_close = is_close_brace(&next);
            if (is_separator || is_close)
                && let Some(pending) = separator.take()
            {
                let visible = self.items[pending].name == "';'";
                let empty = list.is_none();
                let parts = list.into_iter().chain([pending]).collect();
                let reduced = self.reduce("exprlist", visible, parts, true);
                // An empty list sits where the token before it ends, so a list that starts
                // empty starts on the last character of `{`.
                if empty && let Some((start, end)) = self.items[reduced].range {
                    let brace = self.items[open].range.map_or(start, |(_, end)| end - 1);
                    self.items[reduced].range = Some((brace.min(start), end));
                }
                list = Some(reduced);
            }
            if is_close {
                break;
            }
            if is_separator {
                separator = Some(self.shift_separator()?);
                continue;
            }
            let statement = self.statement(&statements.next()?)?;
            let mut parts: Vec<ItemIndex> = list.into_iter().collect();
            parts.extend(separator.take());
            parts.push(statement);
            list = Some(self.reduce("exprlist", false, parts, true));
        }
        let close = self.shift_kind(node, SyntaxKind::R_BRACE)?;
        let mut parts = vec![open];
        parts.extend(list);
        parts.push(close);
        Some(self.reduce("expr", true, parts, false))
    }

    /// The bracketed arguments of a call or subset, GNU R's `sublist`: the opening token, the
    /// list, and the closing token or tokens (two `]` close `[[`).
    fn arguments(&mut self, list: &SyntaxNode) -> Option<Vec<ItemIndex>> {
        let mut elements = significant_elements(list).into_iter();
        let open = self.shift(&elements.next()?.into_token()?, None)?;
        let mut previous: Option<(ItemIndex, ItemIndex)> = None;
        let mut slot: Option<SyntaxNode> = None;
        for element in elements.by_ref() {
            match element {
                SyntaxElement::Node(argument) => slot = Some(argument),
                SyntaxElement::Token(token) => {
                    let argument = self.argument(slot.take())?;
                    let mut parts = Vec::new();
                    if let Some((list, comma)) = previous {
                        parts.extend([list, comma]);
                    }
                    parts.extend(argument);
                    let sublist = self.reduce("sublist", false, parts, true);
                    if token.kind() == SyntaxKind::COMMA {
                        previous = Some((sublist, self.shift(&token, None)?));
                        continue;
                    }
                    let mut result = vec![open, sublist, self.shift(&token, None)?];
                    for element in elements.by_ref() {
                        result.push(self.shift(&element.into_token()?, None)?);
                    }
                    return Some(result);
                }
            }
        }
        None
    }

    /// One argument, GNU R's `sub`: empty, a value, or a name and `=` with an optional value.
    fn argument(&mut self, argument: Option<SyntaxNode>) -> Option<Option<ItemIndex>> {
        let Some(argument) = argument else {
            self.peek();
            return Some(None);
        };
        let elements = significant_elements(&argument);
        let Some(equals) = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::EQ)
        else {
            let Some(value) = elements
                .first()
                .and_then(|element| element.as_node().cloned())
            else {
                self.peek();
                return Some(None);
            };
            // A sub that starts with NULL may be `NULL = value`, so the parser lexes the token
            // after NULL before it reduces it.
            self.argument_start = true;
            let value = self.help(&value)?;
            return Some(Some(self.reduce("sub", false, vec![value], true)));
        };
        let name_token = first_token(elements.first()?.as_node()?)?;
        let name = (name_token.kind() == SyntaxKind::IDENT).then_some("SYMBOL_SUB");
        let name = self.shift(&name_token, name)?;
        let equals_sign = self.shift(&elements.get(equals)?.as_token()?.clone(), Some("EQ_SUB"))?;
        let mut parts = vec![name, equals_sign];
        if let Some(value) = elements
            .get(equals + 1)
            .and_then(|element| element.as_node().cloned())
        {
            parts.push(self.help(&value)?);
        }
        Some(Some(self.reduce("sub", false, parts, true)))
    }

    /// `( formals )` of a function, GNU R's `formlist`, which reduces once per parameter.
    fn parameters(&mut self, list: &SyntaxNode) -> Option<Vec<ItemIndex>> {
        let open = self.shift_kind(list, SyntaxKind::L_PAREN)?;
        let mut formals: Option<ItemIndex> = None;
        let mut comma: Option<ItemIndex> = None;
        for element in significant_elements(list).into_iter().skip(1) {
            match element {
                SyntaxElement::Node(parameter) => {
                    let elements = significant_elements(&parameter);
                    let mut parts: Vec<ItemIndex> = formals.into_iter().collect();
                    parts.extend(comma.take());
                    let name = elements.first()?.as_token()?.clone();
                    parts.push(self.shift(&name, Some("SYMBOL_FORMALS"))?);
                    if let Some(equals) = elements
                        .iter()
                        .find(|element| element.kind() == SyntaxKind::EQ)
                    {
                        let equals = equals.as_token()?.clone();
                        parts.push(self.shift(&equals, Some("EQ_FORMALS"))?);
                        let default = elements.iter().find_map(|element| element.as_node())?;
                        parts.push(self.help(&default.clone())?);
                    }
                    formals = Some(self.reduce("formlist", false, parts, true));
                }
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => {
                    comma = Some(self.shift(&token, None)?);
                }
                SyntaxElement::Token(token) => {
                    let close = self.shift(&token, None)?;
                    let mut result = vec![open];
                    result.extend(formals);
                    result.push(close);
                    return Some(result);
                }
            }
        }
        None
    }

    /// The lookahead, lexing it first if the parser has not read it yet.
    fn peek(&mut self) -> Next {
        if self.lookahead.is_none() {
            self.lex();
        }
        self.lookahead
            .as_ref()
            .map_or(Next::End, |(next, _)| next.clone())
    }

    /// Lexes the next token the grammar reads, giving an id to every comment and skipped newline
    /// on the way, as GNU R's lexer does.
    fn lex(&mut self) {
        loop {
            let Some(lexeme) = self.lexemes.get(self.cursor).cloned() else {
                let id = self.allocate();
                self.lookahead = Some((Next::End, id));
                return;
            };
            self.cursor += 1;
            match lexeme {
                Lexeme::Comment(start, end) => self.comment(start, end),
                Lexeme::Newline {
                    significant: false, ..
                } => {
                    self.allocate();
                }
                Lexeme::Newline {
                    offset,
                    significant: true,
                } => {
                    let id = self.allocate();
                    let top = self.contexts.last().copied();
                    if self.eat_lines || matches!(top, Some(b'(' | b'[')) {
                        continue;
                    }
                    if top == Some(b'i') && !self.ends_if() {
                        continue;
                    }
                    self.lookahead = Some((Next::Newline(offset), id));
                    return;
                }
                Lexeme::Token(token) => {
                    let id = self.allocate();
                    self.track_context(token.kind());
                    self.lookahead = Some((Next::Token(token), id));
                    return;
                }
            }
        }
    }

    /// GNU R's lexer, at a newline inside braces after an `if` body, reads ahead past further
    /// newlines and comments to see whether `else` follows. If `else` or a closing bracket comes
    /// next, the newlines are dropped and the lexer goes on. Otherwise the newline ends the
    /// statement; the token it read ahead took an id, and it takes a fresh one when it is lexed
    /// again. Returns whether the newline ends the statement.
    fn ends_if(&mut self) -> bool {
        while let Some(lexeme) = self.lexemes.get(self.cursor).cloned() {
            match lexeme {
                Lexeme::Comment(start, end) => self.comment(start, end),
                Lexeme::Newline { .. } => {
                    self.allocate();
                }
                Lexeme::Token(_) => break,
            }
            self.cursor += 1;
        }
        let ahead = match self.lexemes.get(self.cursor) {
            Some(Lexeme::Token(token)) => Some(token.kind()),
            _ => None,
        };
        match ahead {
            Some(
                SyntaxKind::ELSE_KW
                | SyntaxKind::R_BRACE
                | SyntaxKind::R_PAREN
                | SyntaxKind::R_BRACKET
                | SyntaxKind::COMMA,
            ) => false,
            _ => {
                self.allocate();
                self.pop_if();
                true
            }
        }
    }

    fn comment(&mut self, start: usize, end: usize) {
        let id = self.allocate();
        self.items.push(Item {
            id,
            name: "COMMENT",
            terminal: true,
            visible: true,
            range: Some((start, end)),
            children: Vec::new(),
            parent: 0,
        });
        self.comments.push(self.items.len() - 1);
    }

    fn track_context(&mut self, kind: SyntaxKind) {
        match kind {
            SyntaxKind::L_PAREN => self.contexts.push(b'('),
            SyntaxKind::L_BRACKET => self.contexts.push(b'['),
            SyntaxKind::L_BRACKET2 => self.contexts.extend([b'[', b'[']),
            SyntaxKind::L_BRACE => self.contexts.push(b'{'),
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET | SyntaxKind::R_BRACE => {
                while self.contexts.last() == Some(&b'i') {
                    self.contexts.pop();
                }
                self.contexts.pop();
            }
            SyntaxKind::IF_KW => {
                if matches!(self.contexts.last(), Some(b'{' | b'[' | b'(' | b'i')) {
                    self.contexts.push(b'i');
                }
            }
            SyntaxKind::ELSE_KW | SyntaxKind::SEMICOLON | SyntaxKind::COMMA => self.pop_if(),
            _ => {}
        }
        self.eat_lines = kind == SyntaxKind::L_BRACE;
    }

    fn pop_if(&mut self) {
        if self.contexts.last() == Some(&b'i') {
            self.contexts.pop();
        }
    }

    fn allocate(&mut self) -> i32 {
        self.last_id += 1;
        self.last_id
    }

    /// Consumes the lookahead, which must be `token`, as a terminal named `name` or its own
    /// token name.
    fn shift(&mut self, token: &SyntaxToken, name: Option<&'static str>) -> Option<ItemIndex> {
        self.peek();
        let (Next::Token(next), id) = self.lookahead.take()? else {
            return None;
        };
        if next.text_range() != token.text_range() {
            return None;
        }
        self.argument_start = false;
        let name = match name {
            Some(name) => name,
            None if token.kind() == SyntaxKind::IDENT && self.calls_next() => {
                "SYMBOL_FUNCTION_CALL"
            }
            None => token_name(token.kind())?,
        };
        let start = usize::from(token.text_range().start());
        self.items.push(Item {
            id,
            name,
            terminal: true,
            visible: true,
            range: Some((start, usize::from(token.text_range().end()))),
            children: Vec::new(),
            parent: 0,
        });
        Some(self.items.len() - 1)
    }

    /// Whether the next lexeme is `(`, which makes the symbol just lexed a function name.
    fn calls_next(&self) -> bool {
        matches!(self.lexemes.get(self.cursor), Some(Lexeme::Token(token)) if token.kind() == SyntaxKind::L_PAREN)
    }

    /// Consumes the lookahead, which must be a token of `kind` in `node`.
    fn shift_kind(&mut self, node: &SyntaxNode, kind: SyntaxKind) -> Option<ItemIndex> {
        let Next::Token(token) = self.peek() else {
            return None;
        };
        if token.kind() != kind || token.parent().as_ref() != Some(node) {
            return None;
        }
        self.shift(&token, None)
    }

    /// Consumes a `;` as a visible terminal or a significant newline as a hidden one.
    fn shift_separator(&mut self) -> Option<ItemIndex> {
        match self.peek() {
            Next::Newline(offset) => {
                let (_, id) = self.lookahead.take()?;
                self.items.push(Item {
                    id,
                    name: "'\\n'",
                    terminal: true,
                    visible: false,
                    range: Some((offset, offset + 1)),
                    children: Vec::new(),
                    parent: 0,
                });
                Some(self.items.len() - 1)
            }
            Next::Token(token) => self.shift(&token, None),
            Next::End => None,
        }
    }

    /// Reduces `children` by a grammar rule, which takes the next id, after lexing the lookahead
    /// when the parser needs it to choose the rule.
    fn reduce(
        &mut self,
        name: &'static str,
        visible: bool,
        children: Vec<ItemIndex>,
        lookahead: bool,
    ) -> ItemIndex {
        if lookahead {
            self.peek();
        }
        let id = self.allocate();
        let range = children
            .iter()
            .filter_map(|&child| self.items[child].range)
            .reduce(|(first, last), (start, end)| (first.min(start), last.max(end)));
        self.items.push(Item {
            id,
            name,
            terminal: false,
            visible,
            range,
            children,
            parent: 0,
        });
        self.items.len() - 1
    }
}

fn is_close_brace(next: &Next) -> bool {
    matches!(next, Next::Token(token) if token.kind() == SyntaxKind::R_BRACE)
}

/// Gives every visible item below `item` the id of its nearest visible ancestor as its parent.
fn assign_parents(items: &mut [Item], item: ItemIndex, parent: i32) {
    let Some(entry) = items.get_mut(item) else {
        return;
    };
    let below = if entry.visible {
        entry.parent = parent;
        entry.id
    } else {
        parent
    };
    let children = entry.children.clone();
    for child in children {
        assign_parents(items, child, below);
    }
}

/// A comment's parent: the smallest visible expression whose range holds it, or, at the top
/// level, the negated id of the first top-level item, such as an expression or a `;`, that took
/// an id after it (0 if none did). `top_level` holds those ids in order.
fn comment_parent(
    items: &[Item],
    roots: &[ItemIndex],
    top_level: &[i32],
    comment: ItemIndex,
) -> i32 {
    let Some((start, end)) = items[comment].range else {
        return 0;
    };
    let contains = |item: ItemIndex| {
        !items[item].terminal
            && items[item]
                .range
                .is_some_and(|(first, last)| first <= start && end <= last)
    };
    let mut found = None;
    let mut candidates: Vec<ItemIndex> = roots.to_vec();
    while let Some(item) = candidates.iter().copied().find(|&item| contains(item)) {
        if items[item].visible {
            found = Some(items[item].id);
        }
        candidates = items[item].children.clone();
    }
    if let Some(parent) = found {
        return parent;
    }
    let comment_id = items[comment].id;
    top_level
        .iter()
        .copied()
        .find(|&id| id > comment_id)
        .map_or(0, |id| -id)
}

fn collect_lexemes(node: &SyntaxNode, lexemes: &mut Vec<Lexeme>, text: &str) {
    let significant = matches!(
        node.kind(),
        SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
    );
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Node(child) if child.kind() == SyntaxKind::ANNOTATION => {
                // `#:` lines are comments to R, one per line.
                let start = usize::from(child.text_range().start());
                let end = usize::from(child.text_range().end()).min(text.len());
                let mut line_start = start;
                for (index, character) in text[start..end].char_indices() {
                    if character == '\n' {
                        push_comment(lexemes, text, line_start, start + index);
                        lexemes.push(Lexeme::Newline {
                            offset: start + index,
                            significant,
                        });
                        line_start = start + index + 1;
                    }
                }
                push_comment(lexemes, text, line_start, end);
            }
            SyntaxElement::Node(child) if is_skipped(&child) => {}
            SyntaxElement::Node(child) => collect_lexemes(&child, lexemes, text),
            SyntaxElement::Token(token) => match token.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMENT => lexemes.push(Lexeme::Comment(
                    usize::from(token.text_range().start()),
                    usize::from(token.text_range().end()),
                )),
                SyntaxKind::NEWLINE => {
                    let start = usize::from(token.text_range().start());
                    for (index, _) in token.text().match_indices('\n') {
                        lexemes.push(Lexeme::Newline {
                            offset: start + index,
                            significant,
                        });
                    }
                }
                _ => lexemes.push(Lexeme::Token(token)),
            },
        }
    }
}

fn push_comment(lexemes: &mut Vec<Lexeme>, text: &str, start: usize, end: usize) {
    if let Some(hash) = text[start..end].find('#') {
        let end = start + text[start..end].trim_end_matches('\r').len();
        lexemes.push(Lexeme::Comment(start + hash, end));
    }
}

fn significant_elements(node: &SyntaxNode) -> Vec<SyntaxElement> {
    node.children_with_tokens()
        .filter(|element| match element {
            SyntaxElement::Node(child) => !is_skipped(child),
            SyntaxElement::Token(token) => !token.kind().is_trivia(),
        })
        .collect()
}

fn first_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    std::iter::successors(node.first_token(), SyntaxToken::next_token)
        .find(|token| !token.kind().is_trivia())
}

fn operator_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| !token.kind().is_trivia())
}

fn operands(node: &SyntaxNode) -> Option<[SyntaxNode; 2]> {
    let children: Vec<SyntaxNode> = node.children().filter(|child| !is_skipped(child)).collect();
    <[SyntaxNode; 2]>::try_from(children).ok()
}

/// GNU R's name for a token, as `getParseData` shows it.
fn token_name(kind: SyntaxKind) -> Option<&'static str> {
    use SyntaxKind::*;
    Some(match kind {
        INTEGER | DOUBLE | COMPLEX | TRUE_KW | FALSE_KW | INF_KW | NAN_KW | NA_KW
        | NA_INTEGER_KW | NA_REAL_KW | NA_COMPLEX_KW | NA_CHARACTER_KW => "NUM_CONST",
        STRING | RAW_STRING => "STR_CONST",
        NULL_KW => "NULL_CONST",
        IDENT | DOTS | DOTDOTI => "SYMBOL",
        UNDERSCORE => "PLACEHOLDER",
        FUNCTION_KW => "FUNCTION",
        BACKSLASH => "'\\\\'",
        IF_KW => "IF",
        ELSE_KW => "ELSE",
        REPEAT_KW => "REPEAT",
        WHILE_KW => "WHILE",
        FOR_KW => "FOR",
        IN_KW => "IN",
        NEXT_KW => "NEXT",
        BREAK_KW => "BREAK",
        PLUS => "'+'",
        MINUS => "'-'",
        STAR => "'*'",
        SLASH => "'/'",
        CARET => "'^'",
        SPECIAL => "SPECIAL",
        PIPE_GREATER => "PIPE",
        LESS => "LT",
        GREATER => "GT",
        LESS_EQ => "LE",
        GREATER_EQ => "GE",
        EQ2 => "EQ",
        BANG_EQ => "NE",
        BANG => "'!'",
        AMP => "AND",
        AMP2 => "AND2",
        PIPE => "OR",
        PIPE2 => "OR2",
        TILDE => "'~'",
        QUESTION => "'?'",
        LESS_MINUS | LESS2_MINUS | COLON_EQ => "LEFT_ASSIGN",
        MINUS_GREATER | MINUS_GREATER2 => "RIGHT_ASSIGN",
        EQ => "EQ_ASSIGN",
        FAT_ARROW => "PIPEBIND",
        COLON => "':'",
        COLON2 => "NS_GET",
        COLON3 => "NS_GET_INT",
        DOLLAR => "'$'",
        AT => "'@'",
        L_PAREN => "'('",
        R_PAREN => "')'",
        L_BRACE => "'{'",
        R_BRACE => "'}'",
        L_BRACKET => "'['",
        L_BRACKET2 => "LBB",
        R_BRACKET => "']'",
        COMMA => "','",
        SEMICOLON => "';'",
        _ => return None,
    })
}

/// GNU R's bison token number for a token or grammar rule name.
fn token_number(name: &str) -> i32 {
    match name {
        "'!'" => 33,
        "'$'" => 36,
        "'('" => 40,
        "')'" => 41,
        "'*'" => 42,
        "'+'" => 43,
        "','" => 44,
        "'-'" => 45,
        "'/'" => 47,
        "':'" => 58,
        "';'" => 59,
        "'?'" => 63,
        "'@'" => 64,
        "'['" => 91,
        "'\\\\'" => 92,
        "']'" => 93,
        "'^'" => 94,
        "'{'" => 123,
        "'}'" => 125,
        "'~'" => 126,
        "expr_or_assign_or_help" => 77,
        "expr_or_help" => 78,
        "expr" => 79,
        "forcond" => 82,
        "exprlist" => 83,
        "STR_CONST" => 260,
        "NUM_CONST" => 261,
        "NULL_CONST" => 262,
        "SYMBOL" => 263,
        "FUNCTION" => 264,
        "LEFT_ASSIGN" => 266,
        "EQ_ASSIGN" => 267,
        "RIGHT_ASSIGN" => 268,
        "LBB" => 269,
        "FOR" => 270,
        "IN" => 271,
        "IF" => 272,
        "ELSE" => 273,
        "WHILE" => 274,
        "NEXT" => 275,
        "BREAK" => 276,
        "REPEAT" => 277,
        "GT" => 278,
        "GE" => 279,
        "LT" => 280,
        "LE" => 281,
        "EQ" => 282,
        "NE" => 283,
        "AND" => 284,
        "OR" => 285,
        "AND2" => 286,
        "OR2" => 287,
        "NS_GET" => 288,
        "NS_GET_INT" => 289,
        "COMMENT" => 290,
        "SYMBOL_FORMALS" => 292,
        "EQ_FORMALS" => 293,
        "EQ_SUB" => 294,
        "SYMBOL_SUB" => 295,
        "SYMBOL_FUNCTION_CALL" => 296,
        "SYMBOL_PACKAGE" => 297,
        "SLOT" => 298,
        "PIPE" => 299,
        "PLACEHOLDER" => 300,
        "PIPEBIND" => 301,
        "SPECIAL" => 306,
        _ => 0,
    }
}
