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
//!
//! The replay walks the flat tree through borrowed cursors, so a step costs no reference count,
//! and every item carries its token name as a [`Name`], so a token number, a name's CHARSXP and
//! a name comparison are each an array read or an integer compare.

use {
    crate::{
        lower::{SourceMap, position_near},
        unwind::R,
        values::{self, symbols},
    },
    heap::object::{Sexp, SexpType},
    smallvec::{SmallVec, smallvec},
    syntax::{ElementRef, NodeOrToken, NodeRef, SyntaxKind, TokenRef},
};

/// GNU R's parse data for the source that `root` parsed: the integer matrix with its `tokens`,
/// `text` and `class` attributes, or `None` when the tree holds a construct GNU R cannot parse,
/// such as a ry record literal.
pub unsafe fn parse_data(root: NodeRef<'_>, source: &SourceMap) -> R<Option<Sexp>> {
    let mut lexemes = Vec::with_capacity(source.text.len() / 4);
    collect_lexemes(root, &mut lexemes, source.text);
    let mut replay = Replay {
        source,
        // Each lexeme becomes an item or is skipped, and the grammar's reductions add fewer than
        // two items per token.
        items: Vec::with_capacity(lexemes.len() * 3),
        lexemes,
        cursor: 0,
        lookahead: None,
        last_id: 0,
        open_comments: Vec::new(),
        contexts: Vec::new(),
        eat_lines: false,
        argument_start: false,
        line: 0,
    };
    if replay.program(root).is_none() {
        return Ok(None);
    }
    let items = &replay.items;
    // A reduction is pushed after its children, so walking the items backwards reaches every
    // item's reduction before the item itself, and each item hands its visible ancestor's id
    // (its own id if it is visible) down to its children. A comment inside a visible rule has
    // that rule as its `reduced_by`, so it takes the rule's id the same way.
    let mut parents = vec![0; items.len()];
    let mut handed_down = vec![0; items.len()];
    for index in (0..items.len()).rev() {
        let item = &items[index];
        let parent = match item.reduced_by {
            NO_ITEM => 0,
            reduction => handed_down[reduction as usize],
        };
        parents[index] = parent;
        handed_down[index] = if item.visible { item.id } else { parent };
    }
    // Ids are dense, so placing each visible item at its id orders the rows without a sort.
    let mut row_of_id = vec![NO_ITEM; replay.last_id as usize + 1];
    for (index, item) in items.iter().enumerate() {
        if item.visible {
            row_of_id[item.id as usize] = index as u32;
        }
    }
    // A comment outside every rule hangs off the first top-level item, such as an expression or
    // a `;`, that took an id after it, negated (0 if none did).
    let mut next_top_level = 0;
    for (id, &index) in row_of_id.iter().enumerate().rev() {
        if index == NO_ITEM {
            continue;
        }
        let item = &items[index as usize];
        if item.name == Name::Comment {
            if item.reduced_by == NO_ITEM {
                parents[index as usize] = -next_top_level;
            }
        } else if parents[index as usize] == 0 {
            next_top_level = id as i32;
        }
    }
    let rows = row_of_id.iter().filter(|&&index| index != NO_ITEM).count();
    let data = values::alloc_uninit(SexpType::Integer, rows * 8)?;
    let tokens = values::alloc(SexpType::String, rows)?;
    let texts = values::alloc(SexpType::String, rows)?;
    let matrix = data.slice_mut::<i32>();
    let heap = crate::heap();
    // A table repeats a few dozen token names thousands of times, and most terminals of one name
    // spell the same text, so each name keeps its CHARSXP and the last text it was spelled with.
    let mut name_strings: [Option<Sexp>; NAME_COUNT] = [None; NAME_COUNT];
    let mut last_texts: [Option<(&str, Sexp)>; NAME_COUNT] = [None; NAME_COUNT];
    let blank = values::blank_string();
    for (row, &index) in row_of_id
        .iter()
        .filter(|&&index| index != NO_ITEM)
        .enumerate()
    {
        let item = &items[index as usize];
        let (first, last) = if item.start == NO_RANGE {
            let mut line = 0;
            let (line_number, _, column) = position_near(source, 0, &mut line);
            let position = Position {
                line: line_number,
                column,
            };
            (position, position)
        } else {
            (item.first, item.last)
        };
        matrix[row * 8..row * 8 + 8].copy_from_slice(&[
            first.line,
            first.column,
            last.line,
            last.column,
            i32::from(item.terminal),
            NAMES[item.name as usize].1,
            item.id,
            parents[index as usize],
        ]);
        let slot = item.name as usize;
        let name = match name_strings[slot] {
            Some(name) => name,
            None => {
                values::set_mkchar(tokens, row, NAMES[slot].0)?;
                let name = values::string_elt(tokens, row);
                name_strings[slot] = Some(name);
                name
            }
        };
        heap.set_element(tokens, row, name);
        if item.terminal {
            let text = &source.text[item.start as usize..item.end as usize];
            match last_texts[slot] {
                Some((last, charsxp)) if last == text => heap.set_element(texts, row, charsxp),
                _ => {
                    values::set_mkchar(texts, row, text)?;
                    last_texts[slot] = Some((text, values::string_elt(texts, row)));
                }
            }
        } else {
            heap.set_element(texts, row, blank);
        }
    }
    values::set_attr(
        data,
        symbols().dim,
        values::integer_vector(&[8, rows as i32])?,
    )?;
    values::set_attr(data, values::install("tokens")?, tokens)?;
    values::set_attr(data, values::install("text")?, texts)?;
    values::set_attr(data, symbols().class, values::scalar_string("parseData")?)?;
    Ok(Some(data))
}

/// One item of R's token stream: a comment, one newline, or a token the grammar reads.
#[derive(Clone, Copy)]
enum Lexeme<'a> {
    Comment(u32, u32),
    /// A newline that ends a statement where it stands, at the top level or directly inside
    /// braces, is significant; any other newline is one GNU R's lexer skips.
    Newline {
        offset: u32,
        significant: bool,
    },
    Token(TokenRef<'a>),
}

/// The lexed token the parser has not consumed yet, with the id it took.
#[derive(Clone, Copy)]
enum Next<'a> {
    Token(TokenRef<'a>),
    Newline(u32),
    End,
}

struct Item {
    id: i32,
    name: Name,
    terminal: bool,
    visible: bool,
    /// The byte range it spans, with `start` [`NO_RANGE`] for a rule that matched nothing.
    start: u32,
    end: u32,
    /// The line and column of the first and of the last character of the range.
    first: Position,
    last: Position,
    /// The index of the reduction that took this item as a child, or [`NO_ITEM`]. For a comment,
    /// the smallest visible rule that holds it.
    reduced_by: u32,
}

#[derive(Clone, Copy, Default)]
struct Position {
    line: i32,
    column: i32,
}

const NO_ITEM: u32 = u32::MAX;
const NO_RANGE: u32 = u32::MAX;

/// The node an expression reduced to, as an index into the replay's items.
type ItemIndex = usize;

/// The children of one grammar reduction, which are few.
type Parts = SmallVec<[ItemIndex; 8]>;

struct Replay<'a> {
    source: &'a SourceMap<'a>,
    lexemes: Vec<Lexeme<'a>>,
    cursor: usize,
    lookahead: Option<(Next<'a>, i32)>,
    last_id: i32,
    items: Vec<Item>,
    /// The comments lexed so far that no visible rule holds yet, in source order.
    open_comments: Vec<ItemIndex>,
    /// GNU R's lexer context stack: `(`, `[` and `{` for open brackets, and `i` for an `if`
    /// whose body may still be followed by `else` on a later line.
    contexts: Vec<u8>,
    /// Set after `{`, where GNU R's lexer skips the newlines before the first statement.
    eat_lines: bool,
    /// Set while the next token shifted is the first of an argument.
    argument_start: bool,
    /// The line of the last terminal placed, where the next one is searched for first: the
    /// lexer reads the source forwards.
    line: usize,
}

impl<'a> Replay<'a> {
    /// The top-level statements.
    fn program(&mut self, root: NodeRef<'a>) -> Option<()> {
        let mut statements = expression_children(root);

        loop {
            match self.peek() {
                Next::End => return Some(()),
                Next::Newline(_) => {
                    let newline = self.shift_separator()?;
                    self.reduce(Name::Prog, false, &[newline], false);
                }
                Next::Token(token) if token.kind() == SyntaxKind::SEMICOLON => {
                    let separator = self.shift_separator()?;
                    self.reduce(Name::Prog, false, &[separator], false);
                }
                Next::Token(_) => {
                    let statement = self.statement(statements.next()?)?;
                    if matches!(self.peek(), Next::Token(token) if token.kind() != SyntaxKind::SEMICOLON)
                    {
                        return None;
                    }
                    if !matches!(self.peek(), Next::End) {
                        let separator = self.shift_separator()?;
                        self.reduce(Name::Prog, false, &[statement, separator], false);
                    }
                }
            }
        }
    }

    /// A statement, which GNU R's grammar calls `expr_or_assign_or_help`: an expression, or an
    /// `=` assignment or binary `?` whose sides are statements too.
    fn statement(&mut self, node: NodeRef<'a>) -> Option<ItemIndex> {
        if node.kind() == SyntaxKind::BINARY_EXPR {
            let operator = operator_token(node)?;
            if matches!(operator.kind(), SyntaxKind::EQ | SyntaxKind::QUESTION) {
                let [left, right] = operands(node)?;
                let left = self.statement(left)?;
                let operator = self.shift(operator, None)?;
                let right = self.statement(right)?;
                return Some(self.reduce(
                    Name::ExprOrAssignOrHelp,
                    true,
                    &[left, operator, right],
                    true,
                ));
            }
        }
        let expression = self.expression(node)?;
        Some(self.reduce(Name::ExprOrAssignOrHelp, false, &[expression], true))
    }

    /// An expression in a position GNU R's grammar reads as `expr_or_help`: a condition, an
    /// argument or a parameter default.
    fn help(&mut self, node: NodeRef<'a>) -> Option<ItemIndex> {
        if node.kind() == SyntaxKind::BINARY_EXPR {
            let operator = operator_token(node)?;
            if operator.kind() == SyntaxKind::QUESTION {
                let [left, right] = operands(node)?;
                let left = self.help(left)?;
                let operator = self.shift(operator, None)?;
                let right = self.help(right)?;
                return Some(self.reduce(Name::ExprOrHelp, true, &[left, operator, right], true));
            }
        }
        let expression = self.expression(node)?;
        Some(self.reduce(Name::ExprOrHelp, false, &[expression], true))
    }

    fn expression(&mut self, node: NodeRef<'a>) -> Option<ItemIndex> {
        let mut children = expression_children(node);
        match node.kind() {
            SyntaxKind::NAME => {
                let token = first_token(node)?;
                let placeholder = token.kind() == SyntaxKind::UNDERSCORE;
                let symbol = self.shift(token, None)?;
                Some(self.reduce(Name::Expr, true, &[symbol], !placeholder))
            }
            SyntaxKind::LITERAL => {
                let token = first_token(node)?;
                let lookahead = match token.kind() {
                    SyntaxKind::STRING | SyntaxKind::RAW_STRING => true,
                    SyntaxKind::NULL_KW => self.argument_start,
                    _ => false,
                };
                let constant = self.shift(token, None)?;
                Some(self.reduce(Name::Expr, true, &[constant], lookahead))
            }
            SyntaxKind::BREAK_EXPR | SyntaxKind::NEXT_EXPR => {
                let keyword = self.shift(first_token(node)?, None)?;
                Some(self.reduce(Name::Expr, true, &[keyword], false))
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = operator_token(node)?;
                let operator = self.shift(operator, None)?;
                let operand = children.next()?;
                let operand = if self.items[operator].name == Name::Question {
                    self.statement(operand)?
                } else {
                    self.expression(operand)?
                };
                Some(self.reduce(Name::Expr, true, &[operator, operand], true))
            }
            SyntaxKind::BINARY_EXPR => {
                let operator = operator_token(node)?;
                if matches!(operator.kind(), SyntaxKind::EQ | SyntaxKind::QUESTION) {
                    return self.statement(node);
                }
                let [left, right] = operands(node)?;
                let left = self.expression(left)?;
                let operator = self.shift(operator, None)?;
                let right = self.expression(right)?;
                Some(self.reduce(Name::Expr, true, &[left, operator, right], true))
            }
            SyntaxKind::PAREN_EXPR => {
                let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
                let inner = self.statement(children.next()?)?;
                let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
                Some(self.reduce(Name::Expr, true, &[open, inner, close], false))
            }
            SyntaxKind::BRACE_EXPR => self.brace(node, children),
            SyntaxKind::CALL_EXPR | SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let [callee, arguments] = exactly_two(children)?;
                let callee = self.expression(callee)?;
                let mut parts: Parts = smallvec![callee];
                parts.extend(self.arguments(arguments)?);
                Some(self.reduce(Name::Expr, true, &parts, false))
            }
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => {
                let [object, member] = exactly_two(children)?;
                let object = self.expression(object)?;
                let operator = self.shift(operator_token(node)?, None)?;
                let member_token = first_token(member)?;
                let name = (node.kind() == SyntaxKind::AT_EXPR
                    && member_token.kind() == SyntaxKind::IDENT)
                    .then_some(Name::Slot);
                let member = self.shift(member_token, name)?;
                Some(self.reduce(Name::Expr, true, &[object, operator, member], false))
            }
            SyntaxKind::NAMESPACE_EXPR => {
                let [package, name] = exactly_two(children)?;
                let package_token = first_token(package)?;
                let package_name =
                    (package_token.kind() == SyntaxKind::IDENT).then_some(Name::SymbolPackage);
                let package = self.shift(package_token, package_name)?;
                let operator = self.shift(operator_token(node)?, None)?;
                let name = self.shift(first_token(name)?, None)?;
                Some(self.reduce(Name::Expr, true, &[package, operator, name], false))
            }
            SyntaxKind::FUNCTION_DEF => {
                let keyword = self.shift(first_token(node)?, None)?;
                let parameters =
                    children.find(|child| child.kind() == SyntaxKind::PARAMETER_LIST)?;
                let mut parts: Parts = smallvec![keyword];
                parts.extend(self.parameters(parameters)?);
                let body = expression_children(node)
                    .find(|child| child.kind() != SyntaxKind::PARAMETER_LIST)?;
                parts.push(self.statement(body)?);
                Some(self.reduce(Name::Expr, true, &parts, true))
            }
            SyntaxKind::IF_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::IF_KW)?;
                let condition = self.condition(node, children.next()?, Name::Ifcond)?;
                let then = self.statement(children.next()?)?;
                let mut parts: Parts = smallvec![keyword, condition, then];
                if let Some(otherwise) = children.next() {
                    parts.push(self.shift_kind(node, SyntaxKind::ELSE_KW)?);
                    parts.push(self.statement(otherwise)?);
                }
                Some(self.reduce(Name::Expr, true, &parts, true))
            }
            SyntaxKind::WHILE_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::WHILE_KW)?;
                let condition = self.condition(node, children.next()?, Name::Cond)?;
                let body = self.statement(children.next()?)?;
                Some(self.reduce(Name::Expr, true, &[keyword, condition, body], true))
            }
            SyntaxKind::FOR_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::FOR_KW)?;
                let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
                let variable = self.shift(first_token(children.next()?)?, None)?;
                let keyword_in = self.shift_kind(node, SyntaxKind::IN_KW)?;
                let sequence = self.help(children.next()?)?;
                let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
                let header = self.reduce(
                    Name::Forcond,
                    true,
                    &[open, variable, keyword_in, sequence, close],
                    false,
                );
                let body = self.statement(children.next()?)?;
                Some(self.reduce(Name::Expr, true, &[keyword, header, body], true))
            }
            SyntaxKind::REPEAT_EXPR => {
                let keyword = self.shift_kind(node, SyntaxKind::REPEAT_KW)?;
                let body = self.statement(children.next()?)?;
                Some(self.reduce(Name::Expr, true, &[keyword, body], true))
            }
            _ => None,
        }
    }

    /// `( condition )` after `if` or `while`, which GNU R's grammar reduces to a hidden rule as
    /// soon as the `)` is read.
    fn condition(
        &mut self,
        node: NodeRef<'a>,
        condition: NodeRef<'a>,
        name: Name,
    ) -> Option<ItemIndex> {
        let open = self.shift_kind(node, SyntaxKind::L_PAREN)?;
        let condition = self.help(condition)?;
        let close = self.shift_kind(node, SyntaxKind::R_PAREN)?;
        Some(self.reduce(name, false, &[open, condition, close], false))
    }

    /// `{ ... }`: GNU R's `exprlist`, whose statements are separated by `;` or significant
    /// newlines and may be empty. Only the rule that ends a list with `;` is visible.
    fn brace(
        &mut self,
        node: NodeRef<'a>,
        mut statements: impl Iterator<Item = NodeRef<'a>>,
    ) -> Option<ItemIndex> {
        let open = self.shift_kind(node, SyntaxKind::L_BRACE)?;
        let mut list: Option<ItemIndex> = None;
        let mut separator: Option<ItemIndex> = None;
        loop {
            let next = self.peek();
            let is_separator = match next {
                Next::Newline(_) => true,
                Next::Token(token) => token.kind() == SyntaxKind::SEMICOLON,
                Next::End => return None,
            };
            let is_close =
                matches!(next, Next::Token(token) if token.kind() == SyntaxKind::R_BRACE);
            if (is_separator || is_close)
                && let Some(pending) = separator.take()
            {
                let visible = self.items[pending].name == Name::Semicolon;
                // An empty list sits where the token before it ends, so a list that starts
                // empty starts on the last character of `{`, which is `{` itself.
                let floor = list.is_none().then_some(open);
                let parts: Parts = list.into_iter().chain([pending]).collect();
                list = Some(self.reduce_from(Name::Exprlist, visible, &parts, true, floor));
            }
            if is_close {
                break;
            }
            if is_separator {
                separator = Some(self.shift_separator()?);
                continue;
            }
            let statement = self.statement(statements.next()?)?;
            let mut parts: Parts = list.into_iter().collect();
            parts.extend(separator.take());
            parts.push(statement);
            list = Some(self.reduce(Name::Exprlist, false, &parts, true));
        }
        let close = self.shift_kind(node, SyntaxKind::R_BRACE)?;
        let mut parts: Parts = smallvec![open];
        parts.extend(list);
        parts.push(close);
        Some(self.reduce(Name::Expr, true, &parts, false))
    }

    /// The bracketed arguments of a call or subset, GNU R's `sublist`: the opening token, the
    /// list, and the closing token or tokens (two `]` close `[[`).
    fn arguments(&mut self, list: NodeRef<'a>) -> Option<Parts> {
        let mut elements = significant_elements(list);
        let open = self.shift(elements.next()?.into_token()?, None)?;
        let mut previous: Option<(ItemIndex, ItemIndex)> = None;
        let mut slot: Option<NodeRef<'a>> = None;
        while let Some(element) = elements.next() {
            match element {
                NodeOrToken::Node(argument) => slot = Some(argument),
                NodeOrToken::Token(token) => {
                    let argument = self.argument(slot.take())?;
                    let mut parts = Parts::new();
                    if let Some((list, comma)) = previous {
                        parts.extend([list, comma]);
                    }
                    parts.extend(argument);
                    let sublist = self.reduce(Name::Sublist, false, &parts, true);
                    if token.kind() == SyntaxKind::COMMA {
                        previous = Some((sublist, self.shift(token, None)?));
                        continue;
                    }
                    let mut result: Parts = smallvec![open, sublist, self.shift(token, None)?];
                    for element in elements.by_ref() {
                        result.push(self.shift(element.into_token()?, None)?);
                    }
                    return Some(result);
                }
            }
        }
        None
    }

    /// One argument, GNU R's `sub`: empty, a value, or a name and `=` with an optional value.
    fn argument(&mut self, argument: Option<NodeRef<'a>>) -> Option<Option<ItemIndex>> {
        let Some(argument) = argument else {
            self.peek();
            return Some(None);
        };
        let elements: SmallVec<[ElementRef<'a>; 4]> = significant_elements(argument).collect();
        let Some(equals) = elements
            .iter()
            .position(|element| element.kind() == SyntaxKind::EQ)
        else {
            let Some(value) = elements.first().and_then(|element| element.into_node()) else {
                self.peek();
                return Some(None);
            };
            // A sub that starts with NULL may be `NULL = value`, so the parser lexes the token
            // after NULL before it reduces it.
            self.argument_start = true;
            let value = self.help(value)?;
            return Some(Some(self.reduce(Name::Sub, false, &[value], true)));
        };
        let name_token = first_token(elements.first()?.into_node()?)?;
        let name = matches!(
            name_token.kind(),
            SyntaxKind::IDENT | SyntaxKind::DOTS | SyntaxKind::DOTDOTI
        )
        .then_some(Name::SymbolSub);
        let name = self.shift(name_token, name)?;
        let equals_sign = self.shift(elements.get(equals)?.into_token()?, Some(Name::EqSub))?;
        let mut parts: Parts = smallvec![name, equals_sign];
        if let Some(value) = elements
            .get(equals + 1)
            .and_then(|element| element.into_node())
        {
            parts.push(self.help(value)?);
        }
        Some(Some(self.reduce(Name::Sub, false, &parts, true)))
    }

    /// `( formals )` of a function, GNU R's `formlist`, which reduces once per parameter.
    fn parameters(&mut self, list: NodeRef<'a>) -> Option<Parts> {
        let open = self.shift_kind(list, SyntaxKind::L_PAREN)?;
        let mut formals: Option<ItemIndex> = None;
        let mut comma: Option<ItemIndex> = None;
        for element in significant_elements(list).skip(1) {
            match element {
                NodeOrToken::Node(parameter) => {
                    let mut parts: Parts = formals.into_iter().collect();
                    parts.extend(comma.take());
                    let mut elements = significant_elements(parameter);
                    let name = elements.next()?.into_token()?;
                    parts.push(self.shift(name, Some(Name::SymbolFormals))?);
                    if let Some(equals) = elements.find(|element| element.kind() == SyntaxKind::EQ)
                    {
                        parts.push(self.shift(equals.into_token()?, Some(Name::EqFormals))?);
                        let default = significant_elements(parameter)
                            .find_map(|element| element.into_node())?;
                        parts.push(self.help(default)?);
                    }
                    formals = Some(self.reduce(Name::Formlist, false, &parts, true));
                }
                NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMA => {
                    comma = Some(self.shift(token, None)?);
                }
                NodeOrToken::Token(token) => {
                    let close = self.shift(token, None)?;
                    let mut result: Parts = smallvec![open];
                    result.extend(formals);
                    result.push(close);
                    return Some(result);
                }
            }
        }
        None
    }

    /// The lookahead, lexing it first if the parser has not read it yet.
    fn peek(&mut self) -> Next<'a> {
        if self.lookahead.is_none() {
            self.lex();
        }
        self.lookahead.map_or(Next::End, |(next, _)| next)
    }

    /// Lexes the next token the grammar reads, giving an id to every comment and skipped newline
    /// on the way, as GNU R's lexer does.
    fn lex(&mut self) {
        loop {
            let Some(&lexeme) = self.lexemes.get(self.cursor) else {
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
        while let Some(&lexeme) = self.lexemes.get(self.cursor) {
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

    fn comment(&mut self, start: u32, end: u32) {
        let id = self.allocate();
        self.push_terminal(id, Name::Comment, true, start, end);
        self.open_comments.push(self.items.len() - 1);
    }

    fn track_context(&mut self, kind: SyntaxKind) {
        match kind {
            SyntaxKind::L_PAREN => self.contexts.push(b'('),
            SyntaxKind::L_BRACKET => self.contexts.push(b'['),
            SyntaxKind::L_BRACKET2 => self.contexts.extend(*b"[["),
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
    fn shift(&mut self, token: TokenRef<'a>, name: Option<Name>) -> Option<ItemIndex> {
        self.peek();
        let (Next::Token(next), id) = self.lookahead.take()? else {
            return None;
        };
        if next.position() != token.position() {
            return None;
        }
        self.argument_start = false;
        let name = match name {
            Some(name) => name,
            None if token.kind() == SyntaxKind::IDENT && self.calls_next() => {
                Name::SymbolFunctionCall
            }
            None => token_name(token.kind())?,
        };
        let range = token.range();
        self.push_terminal(id, name, true, range.start().into(), range.end().into());
        Some(self.items.len() - 1)
    }

    /// Pushes a terminal item, placing its first and last character on their lines. Terminals
    /// arrive in source order, so the line search starts where the last one ended.
    fn push_terminal(&mut self, id: i32, name: Name, visible: bool, start: u32, end: u32) {
        let text = self.source.text;
        let last = match text.as_bytes().get((end as usize).wrapping_sub(1)) {
            Some(&byte) if byte.is_ascii() => end - 1,
            Some(_) => text[..end as usize]
                .chars()
                .next_back()
                .map_or(start, |character| end - character.len_utf8() as u32),
            None => start,
        }
        .max(start);
        let (first_line, _, first_column) =
            position_near(self.source, start as usize, &mut self.line);
        let (last_line, _, last_column) = if last == start {
            (first_line, 0, first_column)
        } else {
            position_near(self.source, last as usize, &mut self.line)
        };
        self.items.push(Item {
            id,
            name,
            terminal: true,
            visible,
            start,
            end,
            first: Position {
                line: first_line,
                column: first_column,
            },
            last: Position {
                line: last_line,
                column: last_column,
            },
            reduced_by: NO_ITEM,
        });
    }

    /// Whether the next lexeme is `(`, which makes the symbol just lexed a function name.
    fn calls_next(&self) -> bool {
        matches!(self.lexemes.get(self.cursor), Some(Lexeme::Token(token)) if token.kind() == SyntaxKind::L_PAREN)
    }

    /// Consumes the lookahead, which must be a token of `kind` in `node`.
    fn shift_kind(&mut self, node: NodeRef<'a>, kind: SyntaxKind) -> Option<ItemIndex> {
        let Next::Token(token) = self.peek() else {
            return None;
        };
        if token.kind() != kind || token.parent() != Some(node) {
            return None;
        }
        self.shift(token, None)
    }

    /// Consumes a `;` as a visible terminal or a significant newline as a hidden one.
    fn shift_separator(&mut self) -> Option<ItemIndex> {
        match self.peek() {
            Next::Newline(offset) => {
                let (_, id) = self.lookahead.take()?;
                self.push_terminal(id, Name::Newline, false, offset, offset + 1);
                Some(self.items.len() - 1)
            }
            Next::Token(token) => self.shift(token, None),
            Next::End => None,
        }
    }

    /// Reduces `children` by a grammar rule, which takes the next id, after lexing the lookahead
    /// when the parser needs it to choose the rule.
    fn reduce(
        &mut self,
        name: Name,
        visible: bool,
        children: &[ItemIndex],
        lookahead: bool,
    ) -> ItemIndex {
        self.reduce_from(name, visible, children, lookahead, None)
    }

    /// [`Replay::reduce`] for a rule that starts no later than the start of the item `floor`.
    fn reduce_from(
        &mut self,
        name: Name,
        visible: bool,
        children: &[ItemIndex],
        lookahead: bool,
        floor: Option<ItemIndex>,
    ) -> ItemIndex {
        if lookahead {
            self.peek();
        }
        let id = self.allocate();
        let index = self.items.len();
        let mut rule = Item {
            id,
            name,
            terminal: false,
            visible,
            start: NO_RANGE,
            end: 0,
            first: Position::default(),
            last: Position::default(),
            reduced_by: NO_ITEM,
        };
        for &child in children {
            let child = &mut self.items[child];
            debug_assert_eq!(child.reduced_by, NO_ITEM, "an item reduced twice");
            child.reduced_by = index as u32;
            if child.start == NO_RANGE {
                continue;
            }
            if rule.start == NO_RANGE || child.start < rule.start {
                rule.start = child.start;
                rule.first = child.first;
            }
            if rule.end < child.end {
                rule.end = child.end;
                rule.last = child.last;
            }
        }
        if rule.start != NO_RANGE
            && let Some(floor) = floor
        {
            let floor = &self.items[floor];
            if floor.start < rule.start {
                rule.start = floor.start;
                rule.first = floor.first;
            }
        }
        if visible && rule.start != NO_RANGE {
            self.adopt_comments(index, rule.start, rule.end);
        }
        self.items.push(rule);
        index
    }

    /// Gives the comments inside a visible rule's range that no smaller rule holds to the rule
    /// at `index`. Rules reduce inner first and ranges nest, so the first visible rule to hold
    /// a comment is the smallest, and every comment it holds was lexed before it reduced.
    fn adopt_comments(&mut self, index: ItemIndex, start: u32, end: u32) {
        let open = &mut self.open_comments;
        let mut first = open.len();
        while first > 0 && self.items[open[first - 1]].start >= start {
            first -= 1;
        }
        let mut kept = first;
        for read in first..open.len() {
            let comment = open[read];
            let item = &mut self.items[comment];
            if item.end <= end {
                item.reduced_by = index as u32;
            } else {
                open[kept] = comment;
                kept += 1;
            }
        }
        open.truncate(kept);
    }
}

fn collect_lexemes<'a>(node: NodeRef<'a>, lexemes: &mut Vec<Lexeme<'a>>, text: &str) {
    let significant = matches!(
        node.kind(),
        SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
    );
    for element in node.children() {
        match element {
            NodeOrToken::Node(child) if child.kind() == SyntaxKind::ANNOTATION => {
                // `#:` lines are comments to R, one per line. A trailing annotation that runs
                // on over `#:` lines sits inside the expression it follows, but its newlines
                // end that expression for R wherever a newline after it would.
                let significant = annotation_newline_significant(child);
                let range = child.range();
                let start = usize::from(range.start());
                let end = usize::from(range.end()).min(text.len());
                let mut line_start = start;
                for (index, character) in text[start..end].char_indices() {
                    if character == '\n' {
                        push_comment(lexemes, text, line_start, start + index);
                        lexemes.push(Lexeme::Newline {
                            offset: (start + index) as u32,
                            significant,
                        });
                        line_start = start + index + 1;
                    }
                }
                push_comment(lexemes, text, line_start, end);
            }
            NodeOrToken::Node(child) if child.kind().is_erased() => {}
            NodeOrToken::Node(child) => collect_lexemes(child, lexemes, text),
            NodeOrToken::Token(token) => match token.kind() {
                SyntaxKind::WHITESPACE => {}
                SyntaxKind::COMMENT => {
                    let range = token.range();
                    lexemes.push(Lexeme::Comment(range.start().into(), range.end().into()));
                }
                SyntaxKind::NEWLINE => {
                    let start = u32::from(token.range().start());
                    for (index, _) in token.text().match_indices('\n') {
                        lexemes.push(Lexeme::Newline {
                            offset: start + index as u32,
                            significant,
                        });
                    }
                }
                _ => lexemes.push(Lexeme::Token(token)),
            },
        }
    }
}

/// Whether R's lexer returns the newlines inside `annotation` as statement separators: walking
/// out from the annotation, the first node that has a token after it decides, and a statement
/// list (the file or a brace) decides it directly.
fn annotation_newline_significant(annotation: NodeRef<'_>) -> bool {
    let mut child = annotation;
    while let Some(parent) = child.parent() {
        if matches!(
            parent.kind(),
            SyntaxKind::SOURCE_FILE | SyntaxKind::BRACE_EXPR
        ) {
            return true;
        }
        let follows = parent
            .children()
            .skip_while(|element| *element != NodeOrToken::Node(child))
            .skip(1)
            .any(|element| match element {
                NodeOrToken::Token(token) => !token.kind().is_trivia(),
                NodeOrToken::Node(node) => {
                    node.kind() != SyntaxKind::ANNOTATION && !node.kind().is_erased()
                }
            });
        if follows {
            return false;
        }
        child = parent;
    }
    true
}

fn push_comment(lexemes: &mut Vec<Lexeme<'_>>, text: &str, start: usize, end: usize) {
    if let Some(hash) = text[start..end].find('#') {
        let end = start + text[start..end].trim_end_matches('\r').len();
        lexemes.push(Lexeme::Comment((start + hash) as u32, end as u32));
    }
}

/// The child nodes of `node` that are R code, without annotations.
fn expression_children<'a>(node: NodeRef<'a>) -> impl Iterator<Item = NodeRef<'a>> + use<'a> {
    node.child_nodes().filter(|child| !child.kind().is_erased())
}

/// The children of `node` that are R code: tokens other than trivia, and nodes other than
/// annotations.
fn significant_elements<'a>(node: NodeRef<'a>) -> impl Iterator<Item = ElementRef<'a>> + use<'a> {
    node.children().filter(|element| match element {
        NodeOrToken::Node(child) => !child.kind().is_erased(),
        NodeOrToken::Token(token) => !token.kind().is_trivia(),
    })
}

fn first_token(node: NodeRef<'_>) -> Option<TokenRef<'_>> {
    node.tokens().find(|token| !token.kind().is_trivia())
}

fn operator_token(node: NodeRef<'_>) -> Option<TokenRef<'_>> {
    node.child_tokens().find(|token| !token.kind().is_trivia())
}

fn operands(node: NodeRef<'_>) -> Option<[NodeRef<'_>; 2]> {
    exactly_two(expression_children(node))
}

fn exactly_two<'a>(mut nodes: impl Iterator<Item = NodeRef<'a>>) -> Option<[NodeRef<'a>; 2]> {
    let pair = [nodes.next()?, nodes.next()?];
    nodes.next().is_none().then_some(pair)
}

/// GNU R's name for a token, as `getParseData` shows it.
fn token_name(kind: SyntaxKind) -> Option<Name> {
    use SyntaxKind::*;
    Some(match kind {
        INTEGER | DOUBLE | COMPLEX | SUFFIXED_NUMBER | TRUE_KW | FALSE_KW | INF_KW | NAN_KW
        | NA_KW | NA_INTEGER_KW | NA_REAL_KW | NA_COMPLEX_KW | NA_CHARACTER_KW => Name::NumConst,
        STRING | RAW_STRING => Name::StrConst,
        NULL_KW => Name::NullConst,
        IDENT | DOTS | DOTDOTI => Name::Symbol,
        UNDERSCORE => Name::Placeholder,
        FUNCTION_KW => Name::Function,
        BACKSLASH => Name::Backslash,
        IF_KW => Name::If,
        ELSE_KW => Name::Else,
        REPEAT_KW => Name::Repeat,
        WHILE_KW => Name::While,
        FOR_KW => Name::For,
        IN_KW => Name::In,
        NEXT_KW => Name::Next,
        BREAK_KW => Name::Break,
        PLUS => Name::Plus,
        MINUS => Name::Minus,
        STAR => Name::Star,
        SLASH => Name::Slash,
        CARET => Name::Caret,
        SPECIAL => Name::Special,
        PIPE_GREATER => Name::Pipe,
        LESS => Name::Lt,
        GREATER => Name::Gt,
        LESS_EQ => Name::Le,
        GREATER_EQ => Name::Ge,
        EQ2 => Name::Eq,
        BANG_EQ => Name::Ne,
        BANG => Name::Bang,
        AMP => Name::And,
        AMP2 => Name::And2,
        PIPE => Name::Or,
        PIPE2 => Name::Or2,
        TILDE => Name::Tilde,
        QUESTION => Name::Question,
        LESS_MINUS | LESS2_MINUS | COLON_EQ => Name::LeftAssign,
        MINUS_GREATER | MINUS_GREATER2 => Name::RightAssign,
        EQ => Name::EqAssign,
        FAT_ARROW => Name::Pipebind,
        COLON => Name::Colon,
        COLON2 => Name::NsGet,
        COLON3 => Name::NsGetInt,
        DOLLAR => Name::Dollar,
        AT => Name::At,
        L_PAREN => Name::LParen,
        R_PAREN => Name::RParen,
        L_BRACE => Name::LBrace,
        R_BRACE => Name::RBrace,
        L_BRACKET => Name::LBracket,
        L_BRACKET2 => Name::Lbb,
        R_BRACKET => Name::RBracket,
        COMMA => Name::Comma,
        SEMICOLON => Name::Semicolon,
        _ => return None,
    })
}

/// A token or grammar rule name of GNU R's parse data, an index into [`NAMES`].
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Name {
    NumConst,
    StrConst,
    NullConst,
    Symbol,
    Placeholder,
    Function,
    Backslash,
    If,
    Else,
    Repeat,
    While,
    For,
    In,
    Next,
    Break,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Special,
    Pipe,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    Bang,
    And,
    And2,
    Or,
    Or2,
    Tilde,
    Question,
    LeftAssign,
    RightAssign,
    EqAssign,
    Pipebind,
    Colon,
    NsGet,
    NsGetInt,
    Dollar,
    At,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    Lbb,
    RBracket,
    Comma,
    Semicolon,
    Comment,
    SymbolFormals,
    EqFormals,
    EqSub,
    SymbolSub,
    SymbolFunctionCall,
    SymbolPackage,
    Slot,
    Newline,
    ExprOrAssignOrHelp,
    ExprOrHelp,
    Expr,
    Forcond,
    Exprlist,
    Prog,
    Ifcond,
    Cond,
    Sub,
    Sublist,
    Formlist,
}

const NAME_COUNT: usize = Name::Formlist as usize + 1;

/// Each [`Name`]'s text and GNU R's bison token number for it, 0 for the rules GNU R hides.
const NAMES: [(&str, i32); NAME_COUNT] = [
    ("NUM_CONST", 261),
    ("STR_CONST", 260),
    ("NULL_CONST", 262),
    ("SYMBOL", 263),
    ("PLACEHOLDER", 300),
    ("FUNCTION", 264),
    ("'\\\\'", 92),
    ("IF", 272),
    ("ELSE", 273),
    ("REPEAT", 277),
    ("WHILE", 274),
    ("FOR", 270),
    ("IN", 271),
    ("NEXT", 275),
    ("BREAK", 276),
    ("'+'", 43),
    ("'-'", 45),
    ("'*'", 42),
    ("'/'", 47),
    ("'^'", 94),
    ("SPECIAL", 306),
    ("PIPE", 299),
    ("LT", 280),
    ("GT", 278),
    ("LE", 281),
    ("GE", 279),
    ("EQ", 282),
    ("NE", 283),
    ("'!'", 33),
    ("AND", 284),
    ("AND2", 286),
    ("OR", 285),
    ("OR2", 287),
    ("'~'", 126),
    ("'?'", 63),
    ("LEFT_ASSIGN", 266),
    ("RIGHT_ASSIGN", 268),
    ("EQ_ASSIGN", 267),
    ("PIPEBIND", 301),
    ("':'", 58),
    ("NS_GET", 288),
    ("NS_GET_INT", 289),
    ("'$'", 36),
    ("'@'", 64),
    ("'('", 40),
    ("')'", 41),
    ("'{'", 123),
    ("'}'", 125),
    ("'['", 91),
    ("LBB", 269),
    ("']'", 93),
    ("','", 44),
    ("';'", 59),
    ("COMMENT", 290),
    ("SYMBOL_FORMALS", 292),
    ("EQ_FORMALS", 293),
    ("EQ_SUB", 294),
    ("SYMBOL_SUB", 295),
    ("SYMBOL_FUNCTION_CALL", 296),
    ("SYMBOL_PACKAGE", 297),
    ("SLOT", 298),
    ("'\\n'", 0),
    ("expr_or_assign_or_help", 77),
    ("expr_or_help", 78),
    ("expr", 79),
    ("forcond", 82),
    ("exprlist", 83),
    ("prog", 0),
    ("ifcond", 0),
    ("cond", 0),
    ("sub", 0),
    ("sublist", 0),
    ("formlist", 0),
];
