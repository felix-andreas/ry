//! Coverage of `match` arms by Maranget's usefulness algorithm ("Warnings for pattern
//! matching", 2007), over constructors read off the scrutinee's settled type. A row is
//! useful against a matrix when some value matches it and no row of the matrix; an arm is
//! unreachable when its pattern is not useful against the unguarded arms above it, and a
//! `match` is exhaustive when a catch-all would not be useful against all of them. The
//! witnesses of a catch-all's usefulness are the missing cases.

use crate::Db;
use crate::annotations::NamedDefinition;
use crate::check::ExpressionTypes;
use crate::hir::{ExpressionKind, LiteralKind, Module, PatId, PatternKind};
use crate::infer::apply_definition;
use crate::types::{Atomic, Name, Ty, TyKind, union_of, unknown};
use rustc_hash::FxHashMap;
use syntax::TextRange;

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct MatchFinding {
    pub range: TextRange,
    pub kind: MatchFindingKind,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum MatchFindingKind {
    /// Cases no arm covers, written as patterns, at most [`WITNESS_LIMIT`] of them;
    /// `more` says whether others exist.
    NonExhaustive {
        missing: Vec<String>,
        more: bool,
    },
    Unreachable,
}

pub const WITNESS_LIMIT: usize = 3;

/// The project's `type` and `alias` definitions, through which a named scrutinee shows
/// its cases.
pub type Definitions<'db> = Option<&'db FxHashMap<Name<'db>, NamedDefinition<'db>>>;

/// Every `match` of an item, in expression order. Ranges are item-relative.
pub fn match_findings<'db>(
    db: &'db dyn Db,
    definitions: Definitions<'db>,
    module: &Module,
    types: &ExpressionTypes<'db>,
) -> Vec<MatchFinding> {
    let mut findings = Vec::new();
    let checker = Coverage { db, definitions };
    for expression in &module.expressions {
        let ExpressionKind::Match { scrutinee, arms } = &expression.kind else {
            continue;
        };
        let subject = types.get(scrutinee).copied().unwrap_or_else(|| unknown(db));
        let subject = structural(db, definitions, subject);
        let mut rows: Vec<Vec<Pat>> = Vec::with_capacity(arms.len());
        for arm in arms.iter() {
            let row = vec![lower(db, definitions, module, arm.pattern, subject)];
            if checker.useful(&rows, &row, &[subject], 1).is_empty() {
                findings.push(MatchFinding {
                    range: module.pattern(arm.pattern).range,
                    kind: MatchFindingKind::Unreachable,
                });
            }
            // A guard can fail, so its arm covers nothing.
            if arm.guard.is_none() {
                rows.push(row);
            }
        }
        if opaque(db, subject) {
            continue;
        }
        let witnesses = checker.useful(&rows, &[Pat::Wild], &[subject], WITNESS_LIMIT + 1);
        if witnesses.is_empty() {
            continue;
        }
        let more = witnesses.len() > WITNESS_LIMIT;
        let missing = witnesses
            .iter()
            .take(WITNESS_LIMIT)
            .filter_map(|witness| witness.first().map(render))
            .collect();
        let scrutinee_range = module.expression(*scrutinee).range;
        findings.push(MatchFinding {
            range: TextRange::new(expression.range.start(), scrutinee_range.end()),
            kind: MatchFindingKind::NonExhaustive { missing, more },
        });
    }
    findings
}

/// A named type's cases are its definition's, so a nominal tag union is checked like the
/// union it wraps. Only the head and a union's members unfold, which keeps a recursive
/// definition such as `Tree<T>` from unfolding forever; deeper positions unfold when the
/// search reaches them. An opaque nominal, which has no definition body, stays named.
fn structural<'db>(db: &'db dyn Db, definitions: Definitions<'db>, ty: Ty<'db>) -> Ty<'db> {
    let unfold = |ty: Ty<'db>| {
        let mut current = ty;
        // Bounded, because a definition may name another definition as its whole body.
        for _ in 0..16 {
            let TyKind::Named(name, arguments) = current.kind(db) else {
                break;
            };
            let Some(definition) = definitions.and_then(|table| table.get(name)) else {
                break;
            };
            if matches!(definition.body.kind(db), TyKind::Unknown)
                || definition.parameters.len() != arguments.len()
            {
                break;
            }
            current = apply_definition(db, definition, arguments);
        }
        current
    };
    let ty = unfold(ty);
    match ty.kind(db) {
        TyKind::Union(members)
            if members
                .iter()
                .any(|member| matches!(member.kind(db), TyKind::Named(..))) =>
        {
            union_of(db, members.iter().map(|&member| unfold(member)))
        }
        _ => ty,
    }
}

/// A scrutinee whose type makes no claim (an unknown, an unsolved variable, an opaque
/// nominal whose values ry cannot enumerate) has no cases to count.
fn opaque(db: &dyn Db, subject: Ty<'_>) -> bool {
    matches!(
        subject.kind(db),
        TyKind::Unknown | TyKind::Any | TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Named(..)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pat {
    Wild,
    Or(Vec<Pat>),
    Constructed(Constructor, Vec<Pat>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Constructor {
    /// A tag with a payload has one field, the payload's tuple or record.
    Tag {
        name: String,
        payload: bool,
    },
    Tuple(usize),
    /// Field names sorted as the record type stores them.
    Record(Vec<String>),
    True,
    False,
    Na,
    Null,
    /// A number or string, from a domain too large to enumerate.
    Literal(String),
}

impl Constructor {
    fn arity(&self) -> usize {
        match self {
            Constructor::Tag { payload, .. } => usize::from(*payload),
            Constructor::Tuple(items) => *items,
            Constructor::Record(fields) => fields.len(),
            _ => 0,
        }
    }
}

struct Coverage<'db> {
    db: &'db dyn Db,
    definitions: Definitions<'db>,
}

impl<'db> Coverage<'db> {
    /// Witnesses that `query` is useful against `rows`: value vectors, as patterns, that
    /// match `query` and no row. Empty means not useful. Stops after `limit` witnesses.
    fn useful(
        &self,
        rows: &[Vec<Pat>],
        query: &[Pat],
        types: &[Ty<'db>],
        limit: usize,
    ) -> Vec<Vec<Pat>> {
        let Some((head, rest)) = query.split_first() else {
            return if rows.is_empty() {
                vec![Vec::new()]
            } else {
                Vec::new()
            };
        };
        let head_type = types.first().copied().unwrap_or_else(|| unknown(self.db));
        let head_type = structural(self.db, self.definitions, head_type);
        let rest_types = types.get(1..).unwrap_or_default();
        let rows = expand_or_rows(rows);
        match head {
            Pat::Or(alternatives) => {
                let mut witnesses = Vec::new();
                for alternative in alternatives {
                    let mut alternative_query = vec![alternative.clone()];
                    alternative_query.extend_from_slice(rest);
                    witnesses.extend(self.useful(&rows, &alternative_query, types, limit));
                    if witnesses.len() >= limit {
                        break;
                    }
                }
                witnesses.truncate(limit);
                witnesses
            }
            Pat::Constructed(constructor, fields) => self.useful_constructor(
                &rows,
                constructor,
                fields,
                rest,
                head_type,
                rest_types,
                limit,
            ),
            Pat::Wild => {
                let (constructors, complete) = type_constructors(self.db, head_type);
                let heads: Vec<&Constructor> = rows
                    .iter()
                    .filter_map(|row| match row.first() {
                        Some(Pat::Constructed(constructor, _)) => Some(constructor),
                        _ => None,
                    })
                    .collect();
                let missing: Vec<&Constructor> = constructors
                    .iter()
                    .filter(|constructor| !heads.contains(constructor))
                    .collect();
                // A finite set splits on every constructor, in the type's order, so the
                // witnesses list every missing case rather than only those under a
                // constructor no row names. With no row naming one there is nothing to
                // split for, and splitting anyway would unfold a recursive type forever.
                if complete && !constructors.is_empty() && !heads.is_empty() {
                    let mut witnesses = Vec::new();
                    for constructor in &constructors {
                        let fields = vec![Pat::Wild; constructor.arity()];
                        witnesses.extend(self.useful_constructor(
                            &rows,
                            constructor,
                            &fields,
                            rest,
                            head_type,
                            rest_types,
                            limit - witnesses.len(),
                        ));
                        if witnesses.len() >= limit {
                            break;
                        }
                    }
                    return witnesses;
                }
                // Some case no row names: only the rows with a catch-all head cover it.
                let default: Vec<Vec<Pat>> = rows
                    .iter()
                    .filter(|row| matches!(row.first(), Some(Pat::Wild)))
                    .map(|row| row.get(1..).unwrap_or_default().to_vec())
                    .collect();
                let tails = self.useful(&default, rest, rest_types, limit);
                if tails.is_empty() {
                    return Vec::new();
                }
                let mut firsts: Vec<Pat> = missing
                    .iter()
                    .map(|constructor| {
                        let fields = constructor_field_types(self.db, head_type, constructor)
                            .into_iter()
                            .map(|field| self.sample(field))
                            .collect();
                        Pat::Constructed((*constructor).clone(), fields)
                    })
                    .collect();
                if !complete || constructors.is_empty() {
                    firsts.push(Pat::Wild);
                }
                let mut witnesses = Vec::new();
                'outer: for first in firsts {
                    for tail in &tails {
                        let mut witness = vec![first.clone()];
                        witness.extend(tail.iter().cloned());
                        witnesses.push(witness);
                        if witnesses.len() >= limit {
                            break 'outer;
                        }
                    }
                }
                witnesses
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn useful_constructor(
        &self,
        rows: &[Vec<Pat>],
        constructor: &Constructor,
        fields: &[Pat],
        rest: &[Pat],
        head_type: Ty<'db>,
        rest_types: &[Ty<'db>],
        limit: usize,
    ) -> Vec<Vec<Pat>> {
        let arity = constructor.arity();
        let specialized = specialize(rows, constructor, arity);
        let mut query = fields.to_vec();
        query.extend_from_slice(rest);
        let mut types = constructor_field_types(self.db, head_type, constructor);
        types.extend_from_slice(rest_types);
        self.useful(&specialized, &query, &types, limit)
            .into_iter()
            .map(|mut witness| {
                let tail = witness.split_off(arity.min(witness.len()));
                let mut rebuilt = vec![Pat::Constructed(constructor.clone(), witness)];
                rebuilt.extend(tail);
                rebuilt
            })
            .collect()
    }

    /// The pattern a witness shows for a value no row looked inside: a tuple or record
    /// keeps its shape, so a missing tag prints with its payload's arity.
    fn sample(&self, ty: Ty<'db>) -> Pat {
        match ty.kind(self.db) {
            TyKind::NativeTuple(items) => Pat::Constructed(
                Constructor::Tuple(items.len()),
                items.iter().map(|&item| self.sample(item)).collect(),
            ),
            TyKind::NativeRecord(fields) => Pat::Constructed(
                Constructor::Record(
                    fields
                        .iter()
                        .map(|field| field.name.text(self.db).to_owned())
                        .collect(),
                ),
                vec![Pat::Wild; fields.len()],
            ),
            _ => Pat::Wild,
        }
    }
}

fn expand_or_rows(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut expanded = Vec::with_capacity(rows.len());
    for row in rows {
        push_alternatives(row, &mut expanded);
    }
    expanded
}

fn push_alternatives(row: &[Pat], expanded: &mut Vec<Vec<Pat>>) {
    match row.split_first() {
        Some((Pat::Or(alternatives), rest)) => {
            for alternative in alternatives {
                let mut alternative_row = vec![alternative.clone()];
                alternative_row.extend_from_slice(rest);
                push_alternatives(&alternative_row, expanded);
            }
        }
        _ => expanded.push(row.to_vec()),
    }
}

fn specialize(rows: &[Vec<Pat>], constructor: &Constructor, arity: usize) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter_map(|row| {
            let (head, rest) = row.split_first()?;
            let mut specialized = match head {
                Pat::Constructed(other, fields) if other == constructor => fields.clone(),
                Pat::Wild => vec![Pat::Wild; arity],
                _ => return None,
            };
            specialized.extend_from_slice(rest);
            Some(specialized)
        })
        .collect()
}

/// The constructors of a type's values, and whether they are all of them. A union adds
/// each member's; `..`, numbers, strings and unknowns leave the set incomplete.
fn type_constructors(db: &dyn Db, ty: Ty<'_>) -> (Vec<Constructor>, bool) {
    match ty.kind(db) {
        TyKind::Union(members) => {
            let mut constructors = Vec::new();
            let mut complete = true;
            for &member in members {
                let (member_constructors, member_complete) = type_constructors(db, member);
                for constructor in member_constructors {
                    if !constructors.contains(&constructor) {
                        constructors.push(constructor);
                    }
                }
                complete &= member_complete;
            }
            (constructors, complete)
        }
        TyKind::Tag(name, payload) => (
            vec![Constructor::Tag {
                name: name.text(db).to_owned(),
                payload: payload.is_some(),
            }],
            true,
        ),
        TyKind::NativeTuple(items) => (vec![Constructor::Tuple(items.len())], true),
        TyKind::NativeRecord(fields) => (
            vec![Constructor::Record(
                fields
                    .iter()
                    .map(|field| field.name.text(db).to_owned())
                    .collect(),
            )],
            true,
        ),
        TyKind::Scalar(Atomic::Logical) => (
            vec![Constructor::True, Constructor::False, Constructor::Na],
            true,
        ),
        TyKind::Null => (vec![Constructor::Null], true),
        _ => (Vec::new(), false),
    }
}

/// The types of a constructor's fields within `ty`, `Unknown` where `ty` does not say.
fn constructor_field_types<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    constructor: &Constructor,
) -> Vec<Ty<'db>> {
    let members: &[Ty<'db>] = match ty.kind(db) {
        TyKind::Union(members) => members,
        _ => std::slice::from_ref(&ty),
    };
    let found = members
        .iter()
        .find_map(|member| match (constructor, member.kind(db)) {
            (
                Constructor::Tag {
                    name,
                    payload: true,
                },
                TyKind::Tag(member_name, Some(payload)),
            ) if member_name.text(db) == name => Some(vec![*payload]),
            (Constructor::Tuple(length), TyKind::NativeTuple(items)) if items.len() == *length => {
                Some(items.clone())
            }
            (Constructor::Record(names), TyKind::NativeRecord(fields))
                if fields.len() == names.len()
                    && fields
                        .iter()
                        .zip(names)
                        .all(|(field, name)| field.name.text(db) == name) =>
            {
                Some(fields.iter().map(|field| field.ty).collect())
            }
            _ => None,
        });
    found.unwrap_or_else(|| vec![unknown(db); constructor.arity()])
}

fn lower<'db>(
    db: &'db dyn Db,
    definitions: Definitions<'db>,
    module: &Module,
    pattern: PatId,
    ty: Ty<'db>,
) -> Pat {
    let ty = structural(db, definitions, ty);
    match &module.pattern(pattern).kind {
        PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => Pat::Wild,
        PatternKind::Or(alternatives) => Pat::Or(
            alternatives
                .iter()
                .map(|&alternative| lower(db, definitions, module, alternative, ty))
                .collect(),
        ),
        PatternKind::Literal { literal, negative } => {
            let constructor = match literal {
                LiteralKind::Logical(true) => Constructor::True,
                LiteralKind::Logical(false) => Constructor::False,
                LiteralKind::Na(_) => Constructor::Na,
                LiteralKind::Null => Constructor::Null,
                LiteralKind::Integer(text)
                | LiteralKind::Double(text)
                | LiteralKind::Complex(text) => {
                    let value = text.trim_end_matches('L');
                    Constructor::Literal(if *negative {
                        format!("-{value}")
                    } else {
                        value.to_owned()
                    })
                }
                LiteralKind::String(text) => Constructor::Literal(format!("{text:?}")),
                LiteralKind::Inf => {
                    Constructor::Literal(if *negative { "-Inf" } else { "Inf" }.to_owned())
                }
                LiteralKind::NaN => Constructor::Literal("NaN".to_owned()),
            };
            Pat::Constructed(constructor, Vec::new())
        }
        PatternKind::Tag {
            name,
            payload: Some(payload),
            ..
        } if module.is_any_payload(*payload) => {
            let tag = |payload| Constructor::Tag {
                name: name.as_str().to_owned(),
                payload,
            };
            Pat::Or(vec![
                Pat::Constructed(tag(false), Vec::new()),
                Pat::Constructed(tag(true), vec![Pat::Wild]),
            ])
        }
        PatternKind::Tag { name, payload, .. } => {
            let constructor = Constructor::Tag {
                name: name.as_str().to_owned(),
                payload: payload.is_some(),
            };
            let fields = match payload {
                Some(payload) => {
                    let payload_type = constructor_field_types(db, ty, &constructor)
                        .first()
                        .copied()
                        .unwrap_or_else(|| unknown(db));
                    vec![lower(db, definitions, module, *payload, payload_type)]
                }
                None => Vec::new(),
            };
            Pat::Constructed(constructor, fields)
        }
        PatternKind::Tuple(items) => {
            let constructor = Constructor::Tuple(items.len());
            let item_types = constructor_field_types(db, ty, &constructor);
            let fields = items
                .iter()
                .zip(item_types)
                .map(|(&item, item_type)| lower(db, definitions, module, item, item_type))
                .collect();
            Pat::Constructed(constructor, fields)
        }
        PatternKind::Record { fields, rest } => {
            // A rest pattern takes its field list from the record type it matches.
            let members: &[Ty<'db>] = match ty.kind(db) {
                TyKind::Union(members) => members,
                _ => std::slice::from_ref(&ty),
            };
            let shape = members.iter().find_map(|member| match member.kind(db) {
                TyKind::NativeRecord(native)
                    if (*rest || native.len() == fields.len())
                        && fields.iter().all(|field| {
                            native
                                .iter()
                                .any(|native| native.name.text(db) == field.name.as_str())
                        }) =>
                {
                    Some(
                        native
                            .iter()
                            .map(|native| (native.name.text(db).to_owned(), native.ty))
                            .collect::<Vec<_>>(),
                    )
                }
                _ => None,
            });
            let shape = shape.unwrap_or_else(|| {
                let mut names: Vec<(String, Ty<'db>)> = fields
                    .iter()
                    .map(|field| (field.name.as_str().to_owned(), unknown(db)))
                    .collect();
                names.sort_by(|left, right| left.0.cmp(&right.0));
                names
            });
            let lowered = shape
                .iter()
                .map(|(name, field_type)| {
                    match fields.iter().find(|field| field.name.as_str() == name) {
                        Some(field) => lower(db, definitions, module, field.pattern, *field_type),
                        None => Pat::Wild,
                    }
                })
                .collect();
            Pat::Constructed(
                Constructor::Record(shape.into_iter().map(|(name, _)| name).collect()),
                lowered,
            )
        }
    }
}

fn render(pattern: &Pat) -> String {
    match pattern {
        Pat::Wild => "_".to_owned(),
        Pat::Or(alternatives) => alternatives
            .iter()
            .map(render)
            .collect::<Vec<_>>()
            .join(" | "),
        Pat::Constructed(constructor, fields) => match constructor {
            // A payload needs no trailing comma: `:Some(_)`, `:Point(x = _)`.
            Constructor::Tag { name, .. } => match fields.first() {
                None => format!(":{name}"),
                Some(Pat::Constructed(payload, payload_fields)) => {
                    format!(
                        ":{name}({})",
                        render_fields(payload, payload_fields).join(", ")
                    )
                }
                Some(payload) => format!(":{name}({})", render(payload)),
            },
            Constructor::Tuple(_) | Constructor::Record(_) => {
                // One item needs a trailing comma, the literal syntax's rule.
                match render_fields(constructor, fields).as_slice() {
                    [only] => format!("({only},)"),
                    items => format!("({})", items.join(", ")),
                }
            }
            Constructor::True => "TRUE".to_owned(),
            Constructor::False => "FALSE".to_owned(),
            Constructor::Na => "NA".to_owned(),
            Constructor::Null => "NULL".to_owned(),
            Constructor::Literal(text) => text.clone(),
        },
    }
}

fn render_fields(constructor: &Constructor, fields: &[Pat]) -> Vec<String> {
    match constructor {
        Constructor::Record(names) => names
            .iter()
            .zip(fields)
            .map(|(name, field)| format!("{name} = {}", render(field)))
            .collect(),
        _ => fields.iter().map(render).collect(),
    }
}
