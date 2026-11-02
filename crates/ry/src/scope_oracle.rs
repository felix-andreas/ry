//! An independent lexical resolver for a package's R files. `ry debug analysis-stats` checks
//! find-references against it, so it shares no code with the analysis it checks: it reads only
//! the syntax tree and R's scoping rules.

use {
    rustc_hash::{FxHashMap, FxHashSet},
    syntax::{
        SyntaxKind,
        SyntaxKind::{
            ANNOTATION, ARGUMENT, ARGUMENT_LIST, AT_EXPR, BINARY_EXPR, CALL_EXPR, COLON_EQ,
            DOLLAR_EXPR, DOTS, EQ, FOR_EXPR, FUNCTION_DEF, IDENT, LESS_MINUS, LESS2_MINUS,
            MATCH_ARM, MINUS_GREATER, MINUS_GREATER2, NAME, NAMESPACE_EXPR, PARAMETER,
            PARAMETER_LIST, RECORD_EXPR, TAG_EXPR, TILDE, UNARY_EXPR,
        },
        SyntaxNode, SyntaxToken, TextRange, TextSize,
    },
};

/// What a name occurrence binds to: a variable of one function, or a package-level name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Binding {
    Local {
        file: usize,
        function: TextSize,
        name: String,
        /// The `for` loop that binds the variable, or `None` for the variable its ordinary
        /// assignments share.
        generation: Option<TextSize>,
    },
    Global(String),
}

#[derive(Clone, Debug)]
pub struct Site {
    pub file: usize,
    pub range: TextRange,
    pub name: String,
    /// `None` where lexical scoping does not decide the answer: inside a formula or a quoted
    /// expression, a data.table `:=` target, ry's own syntax, or an argument name of a function
    /// the resolver cannot see. Such a site is left out of every comparison.
    pub binding: Option<Binding>,
}

pub struct ScopeOracle {
    pub sites: Vec<Site>,
    /// Package-level names some file assigns, as opposed to names only the library defines.
    globals: FxHashSet<String>,
}

impl ScopeOracle {
    /// `roots` are the syntax trees of the package's R files, which share one namespace.
    pub fn build(roots: &[SyntaxNode]) -> ScopeOracle {
        let mut scopes = Scopes::default();
        for (file, root) in roots.iter().enumerate() {
            scopes.collect_functions(file, root);
        }
        for (file, root) in roots.iter().enumerate() {
            scopes.collect_super_assignments(file, root);
        }
        let mut sites = Vec::new();
        for (file, root) in roots.iter().enumerate() {
            for token in root
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.kind() == IDENT)
            {
                if let Some(site) = scopes.classify(file, &token) {
                    sites.push(site);
                }
            }
        }
        ScopeOracle {
            sites,
            globals: scopes.globals,
        }
    }

    /// Whether the binding is the package's own, so references and rename have project code to
    /// find.
    pub fn is_project(&self, binding: &Binding) -> bool {
        match binding {
            Binding::Local { .. } => true,
            Binding::Global(name) => self.globals.contains(name),
        }
    }
}

type FunctionKey = (usize, TextSize);

#[derive(Default)]
struct Scopes {
    functions: FxHashMap<FunctionKey, Function>,
    /// The function literals assigned to a binding, so a named argument can find its parameter.
    function_values: FxHashMap<Binding, Vec<FunctionKey>>,
    globals: FxHashSet<String>,
    /// Names a function `<<-`-assigns with no enclosing variable and no top-level assignment.
    /// The variable lives wherever the function's environment is pointed at run time (R6 moves
    /// its generator methods into the generator's environment), so lexical scoping does not
    /// decide what such a name refers to.
    dynamic: FxHashSet<String>,
}

struct Function {
    parent: Option<TextSize>,
    parameters: Vec<String>,
    locals: FxHashMap<String, Local>,
}

/// R has one variable per name and function, but each `for` loop gives its variable a fresh
/// value that has nothing to do with the value before the loop. ry therefore links each loop
/// variable to its own loop, and a later read to the loop or assignment that last bound it, so
/// the resolver splits the variable the same way.
struct Local {
    /// The offset from which a read in the function's own body sees the variable. R creates it
    /// when the assignment runs, so a read in an earlier statement, or on the right of the
    /// assignment itself, still reads the enclosing scope.
    visible_from: TextSize,
    /// Each definition's start with its generation, in source order.
    definitions: Vec<(TextSize, Option<TextSize>)>,
}

enum Assignment {
    Local,
    Super,
}

impl Scopes {
    fn collect_functions(&mut self, file: usize, root: &SyntaxNode) {
        for node in root.descendants() {
            match node.kind() {
                FUNCTION_DEF => {
                    let parameters = node
                        .children()
                        .find(|child| child.kind() == PARAMETER_LIST)
                        .into_iter()
                        .flat_map(|list| list.children())
                        .filter(|parameter| parameter.kind() == PARAMETER)
                        .filter_map(|parameter| {
                            parameter
                                .children_with_tokens()
                                .filter_map(|element| element.into_token())
                                .find(|token| matches!(token.kind(), IDENT | DOTS))
                                .map(|token| spelled(&token))
                        })
                        .collect();
                    self.functions.insert(
                        (file, node.text_range().start()),
                        Function {
                            parent: enclosing_function(&node),
                            parameters,
                            locals: FxHashMap::default(),
                        },
                    );
                }
                NAME => {
                    let Some(token) = name_token(&node) else {
                        continue;
                    };
                    if let Some((Assignment::Local, assignment)) = assignment_kind(&node) {
                        let value = if assignment.first_child().as_ref() == Some(&node) {
                            assignment.last_child()
                        } else {
                            assignment.first_child()
                        };
                        let binding =
                            self.bind(file, &node, &token, assignment.text_range().end(), None);
                        if let Some(value) = value.filter(|value| value.kind() == FUNCTION_DEF) {
                            self.function_values
                                .entry(binding)
                                .or_default()
                                .push((file, value.text_range().start()));
                        }
                        continue;
                    }
                    if let Some(loop_start) = loop_start(&node) {
                        self.bind(
                            file,
                            &node,
                            &token,
                            node.text_range().end(),
                            Some(loop_start),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn bind(
        &mut self,
        file: usize,
        node: &SyntaxNode,
        token: &SyntaxToken,
        visible_from: TextSize,
        generation: Option<TextSize>,
    ) -> Binding {
        let name = spelled(token);
        match enclosing_function(node) {
            Some(function) => {
                if let Some(scope) = self.functions.get_mut(&(file, function)) {
                    let local = scope.locals.entry(name.clone()).or_insert(Local {
                        visible_from,
                        definitions: Vec::new(),
                    });
                    local.visible_from = local.visible_from.min(visible_from);
                    local
                        .definitions
                        .push((node.text_range().start(), generation));
                }
                Binding::Local {
                    file,
                    function,
                    name,
                    generation,
                }
            }
            None => {
                self.globals.insert(name.clone());
                Binding::Global(name)
            }
        }
    }

    /// `x <<- value` assigns the nearest enclosing function's `x`, or else the package-level one
    /// when some file defines it, which needs every function's own locals first.
    fn collect_super_assignments(&mut self, file: usize, root: &SyntaxNode) {
        for node in root.descendants().filter(|node| node.kind() == NAME) {
            let (Some(token), Some((Assignment::Super, _))) =
                (name_token(&node), assignment_kind(&node))
            else {
                continue;
            };
            let name = spelled(&token);
            let parent = enclosing_function(&node)
                .and_then(|function| self.functions.get(&(file, function)))
                .and_then(|function| function.parent);
            if let Binding::Global(name) = self.lookup(file, &name, parent, None)
                && !self.globals.contains(&name)
            {
                self.dynamic.insert(name);
            }
        }
    }

    fn classify(&self, file: usize, token: &SyntaxToken) -> Option<Site> {
        let parent = token.parent()?;
        let name = spelled(token);
        let site = |binding| {
            Some(Site {
                file,
                range: token.text_range(),
                name: name.clone(),
                binding,
            })
        };
        if parent.kind() == PARAMETER {
            let function = parent.parent()?.parent()?;
            return site(Some(Binding::Local {
                file,
                function: function.text_range().start(),
                name: name.clone(),
                generation: None,
            }));
        }
        if parent.kind() != NAME {
            return None;
        }
        let holder = parent.parent()?;
        let is_first = holder.first_child().as_ref() == Some(&parent);
        match holder.kind() {
            DOLLAR_EXPR | AT_EXPR if !is_first => return None,
            NAMESPACE_EXPR => return None,
            _ => {}
        }
        if is_opaque(&parent) {
            return site(None);
        }
        if holder.kind() == ARGUMENT && is_first && followed_by_eq(&parent) {
            return site(self.argument_parameter(file, &holder, &name));
        }
        if holder.kind() == BINARY_EXPR && is_first && operator(&holder) == Some(COLON_EQ) {
            return site(None);
        }
        let function = enclosing_function(&parent);
        let binding = match assignment_kind(&parent) {
            Some((Assignment::Super, _)) => {
                let outer = function
                    .and_then(|function| self.functions.get(&(file, function)))
                    .and_then(|function| function.parent);
                self.lookup(file, &name, outer, None)
            }
            Some((Assignment::Local, _)) => {
                self.local_or_global(file, function, name.clone(), None)
            }
            None => match loop_start(&parent) {
                Some(start) => self.local_or_global(file, function, name.clone(), Some(start)),
                None => self.lookup(file, &name, function, Some(token.text_range().start())),
            },
        };
        if let Binding::Global(name) = &binding
            && self.dynamic.contains(name)
        {
            return site(None);
        }
        site(Some(binding))
    }

    fn local_or_global(
        &self,
        file: usize,
        function: Option<TextSize>,
        name: String,
        generation: Option<TextSize>,
    ) -> Binding {
        match function {
            Some(function) => Binding::Local {
                file,
                function,
                name,
                generation,
            },
            None => Binding::Global(name),
        }
    }

    /// Resolves a read: the innermost function sees its parameters and the locals already
    /// created at `read_at`, and an enclosing function, whose body has run by the time an inner
    /// function is called, sees all of its own.
    fn lookup(
        &self,
        file: usize,
        name: &str,
        mut function: Option<TextSize>,
        read_at: Option<TextSize>,
    ) -> Binding {
        let mut innermost = true;
        while let Some(start) = function {
            let Some(scope) = self.functions.get(&(file, start)) else {
                break;
            };
            let local = |generation| Binding::Local {
                file,
                function: start,
                name: name.to_owned(),
                generation,
            };
            if scope.parameters.iter().any(|parameter| parameter == name) {
                return local(None);
            }
            if let Some(variable) = scope.locals.get(name) {
                let created =
                    !innermost || read_at.is_none_or(|read_at| variable.visible_from <= read_at);
                if created {
                    let position = read_at.unwrap_or(TextSize::from(u32::MAX));
                    let generation = variable
                        .definitions
                        .iter()
                        .rev()
                        .find(|(start, _)| *start <= position)
                        .or(variable.definitions.first())
                        .and_then(|(_, generation)| *generation);
                    return local(generation);
                }
            }
            innermost = false;
            function = scope.parent;
        }
        Binding::Global(name.to_owned())
    }

    /// `f(name = value)` fills the parameter `name` of the one function literal bound to `f`.
    fn argument_parameter(
        &self,
        file: usize,
        argument: &SyntaxNode,
        name: &str,
    ) -> Option<Binding> {
        let call = argument
            .parent()
            .filter(|list| list.kind() == ARGUMENT_LIST)?
            .parent()?;
        let callee = call.first_child().filter(|_| call.kind() == CALL_EXPR)?;
        let callee_token = name_token(&callee).filter(|_| callee.kind() == NAME)?;
        let callee_binding = self.lookup(
            file,
            &spelled(&callee_token),
            enclosing_function(&call),
            Some(callee.text_range().start()),
        );
        let [function] = self.function_values.get(&callee_binding)?.as_slice() else {
            return None;
        };
        self.functions
            .get(function)?
            .parameters
            .iter()
            .any(|parameter| parameter == name)
            .then(|| Binding::Local {
                file: function.0,
                function: function.1,
                name: name.to_owned(),
                generation: None,
            })
    }
}

fn spelled(token: &SyntaxToken) -> String {
    token.text().trim_matches('`').to_owned()
}

fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.first_token().filter(|token| token.kind() == IDENT)
}

fn enclosing_function(node: &SyntaxNode) -> Option<TextSize> {
    node.ancestors()
        .skip(1)
        .find(|ancestor| ancestor.kind() == FUNCTION_DEF)
        .map(|function| function.text_range().start())
}

fn operator(node: &SyntaxNode) -> Option<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !token.kind().is_trivia())
        .map(|token| token.kind())
}

fn assignment_kind(name: &SyntaxNode) -> Option<(Assignment, SyntaxNode)> {
    let parent = name
        .parent()
        .filter(|parent| parent.kind() == BINARY_EXPR)?;
    let is_first = parent.first_child().as_ref() == Some(name);
    let is_last = parent.last_child().as_ref() == Some(name);
    let kind = match operator(&parent)? {
        LESS_MINUS | EQ if is_first => Assignment::Local,
        MINUS_GREATER if is_last => Assignment::Local,
        LESS2_MINUS if is_first => Assignment::Super,
        MINUS_GREATER2 if is_last => Assignment::Super,
        _ => return None,
    };
    Some((kind, parent))
}

/// The start of the `for` loop whose variable `name` is.
fn loop_start(name: &SyntaxNode) -> Option<TextSize> {
    let parent = name.parent().filter(|parent| parent.kind() == FOR_EXPR)?;
    (parent.first_child().as_ref() == Some(name)).then(|| parent.text_range().start())
}

fn followed_by_eq(name: &SyntaxNode) -> bool {
    std::iter::successors(name.next_sibling_or_token(), |element| {
        element.next_sibling_or_token()
    })
    .find(|element| !element.kind().is_trivia())
    .is_some_and(|element| element.kind() == EQ)
}

/// A formula, a quoted expression and ry's own syntax are not evaluated where they are written,
/// so their names are not lexical reads.
fn is_opaque(name: &SyntaxNode) -> bool {
    name.ancestors()
        .take_while(|node| node.kind() != FUNCTION_DEF)
        .any(|node| match node.kind() {
            BINARY_EXPR | UNARY_EXPR => operator(&node) == Some(TILDE),
            MATCH_ARM | RECORD_EXPR | TAG_EXPR | ANNOTATION => true,
            CALL_EXPR => node
                .first_child()
                .and_then(|callee| name_token(&callee))
                .is_some_and(|callee| {
                    matches!(
                        callee.text(),
                        "quote" | "bquote" | "substitute" | "expression" | "alist"
                    )
                }),
            _ => false,
        })
}
