//! Match exhaustiveness checking (BRS-18).
//!
//! `match` must cover every case or use `_`
//! (`docs/spec/01-syntax.md`); the checker understands enums, bools,
//! tuples, and nested patterns, and requires `_` for open types like
//! `int` and `string` (`docs/spec/03-types.md`). Exhaustiveness applies
//! whether the `match` is a value or a statement — only `catch` is
//! non-exhaustive by default, and that is M2 (`docs/spec/04-errors.md`).
//!
//! The algorithm is Maranget-style usefulness: the arm patterns form a
//! matrix, and the check asks whether a row of wildcards is still
//! "useful" against it — i.e. whether some value falls through every
//! arm. Specialization peels one constructor at a time: when the first
//! column's type has a finite constructor set and the arms cover all of
//! it, each constructor's rows recurse with the payload spread into new
//! columns; otherwise only the wildcard-headed rows survive (the default
//! matrix) and any missing constructor becomes a witness head.
//!
//! Decisions this unit fixes (`docs/spec/03-types.md`):
//!
//! - A guarded arm never counts toward exhaustiveness (the guard could
//!   be false), so its row is excluded from the matrix.
//! - A binding pattern counts as a wildcard.
//! - Finite constructor sets: user enums (their variants), `Option`
//!   (`Some`/`None`), `bool` (`true`/`false`), and tuples (one
//!   constructor whose fields recurse). Everything else — `int`,
//!   `float`, `string`, `char`, `Range`, collections, functions,
//!   structs, generics, and `unit` — is open and requires a wildcard or
//!   binding row. `unit` is treated as open because there is no unit
//!   literal pattern, so `_` (or a binding) is the only way to cover it.
//! - Literals of open types never complete their type; only
//!   `true`+`false` complete `bool`.
//! - A constructor pattern the resolver could not resolve (or whose
//!   shape mismatches the scrutinee — both already reported) lowers to a
//!   wildcard row so one error never cascades into a bogus missing-case
//!   report.
//!
//! Witnesses are rendered with `_` for payload holes (`Some(_)`,
//! `Rect(_, _)`, `(true, None)`). For an open column the witness is `_`
//! itself: "some value no literal arm matches". Payload field types come
//! from a diagnostic-free approximation of the checker's annotation
//! conversion — anything unresolvable becomes `Unknown`, which is open
//! and therefore at worst demands a `_` the program with prior errors
//! already needs.

use std::collections::HashMap;

use brasa_hir::{Hir, Item, ItemId, Literal, MatchArm, Pattern, PatternId, TypeExpr, TypeExprId};
use brasa_resolver::{BuiltinType, CtorRes, DefRef, Resolutions, TypeRes};

use crate::types::{Type, substitute};

/// How many witnesses a diagnostic spells out; the rest are counted.
pub(crate) const MAX_WITNESSES: usize = 3;

/// The uncovered cases of one `match`: up to [`MAX_WITNESSES`] rendered
/// witnesses plus the exact (saturating) total.
pub(crate) struct MissingCases {
    pub witnesses: Vec<String>,
    pub total: u64,
}

/// Checks one `match` for exhaustiveness. Returns `None` when the match
/// covers everything, and skips the check entirely for a flexible
/// scrutinee (`Unknown`/`Never`): deferred constructs must not error.
pub(crate) fn missing_cases(
    hir: &Hir,
    res: &Resolutions,
    scrutinee_ty: &Type,
    arms: &[MatchArm],
) -> Option<MissingCases> {
    if scrutinee_ty.is_flexible() {
        return None;
    }

    let cx = Cx { hir, res };

    let matrix: Vec<Vec<Pat>> = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .map(|arm| vec![cx.lower(arm.pattern, scrutinee_ty)])
        .collect();

    let (witnesses, total) = cx.witnesses(&matrix, std::slice::from_ref(scrutinee_ty));
    if total == 0 {
        return None;
    }

    let witnesses = witnesses.iter().map(|row| cx.render(&row[0])).collect();
    Some(MissingCases { witnesses, total })
}

/// A pattern as the matrix sees it: an irrefutable row entry or a head
/// constructor with sub-patterns.
#[derive(Clone)]
enum Pat {
    Wild,
    Ctor(Ctor, Vec<Pat>),
}

/// A head constructor. `OpaqueLit` stands for any literal of an open
/// type (`int`/`float`/`string`/`char`): such literals never complete
/// their type and are never specialized, so their value is irrelevant —
/// only `bool` literals participate in completeness.
#[derive(Clone, PartialEq)]
enum Ctor {
    Bool(bool),
    OptionSome,
    OptionNone,
    Variant { enum_item: ItemId, index: usize },
    Tuple,
    OpaqueLit,
}

struct Cx<'a> {
    hir: &'a Hir,
    res: &'a Resolutions,
}

impl<'a> Cx<'a> {
    /// Lowers one HIR pattern against its column type. Anything that
    /// does not fit the type — the type checker already reported it —
    /// becomes a wildcard so the error does not cascade.
    fn lower(&self, id: PatternId, ty: &Type) -> Pat {
        match self.hir.pattern(id) {
            Pattern::Wildcard | Pattern::Binding(_) => Pat::Wild,
            Pattern::Literal(literal) => match (literal, ty) {
                (Literal::Bool(b), Type::Bool) => Pat::Ctor(Ctor::Bool(*b), vec![]),
                (Literal::Int(_), Type::Int)
                | (Literal::Float(_), Type::Float)
                | (Literal::Char(_), Type::Char)
                | (Literal::Str(_), Type::String) => Pat::Ctor(Ctor::OpaqueLit, vec![]),
                _ => Pat::Wild,
            },
            Pattern::Ctor { args, .. } => self.lower_ctor(id, args, ty),
            Pattern::Tuple(elements) => match ty {
                Type::Tuple(tys) if tys.len() == elements.len() => {
                    let args = elements
                        .iter()
                        .zip(tys)
                        .map(|(&element, ty)| self.lower(element, ty))
                        .collect();
                    Pat::Ctor(Ctor::Tuple, args)
                }
                _ => Pat::Wild,
            },
        }
    }

    fn lower_ctor(&self, id: PatternId, args: &[PatternId], ty: &Type) -> Pat {
        match self.res.ctor_pattern_res.get(&id).copied() {
            // Unresolved constructor (the resolver already errored) and
            // `Set`, which never resolves in pattern position.
            None | Some(CtorRes::SetCtor) => Pat::Wild,
            Some(CtorRes::OptionSome) => match ty {
                Type::Option(inner) if args.len() == 1 => {
                    Pat::Ctor(Ctor::OptionSome, vec![self.lower(args[0], inner)])
                }
                _ => Pat::Wild,
            },
            Some(CtorRes::OptionNone) => match ty {
                Type::Option(_) if args.is_empty() => Pat::Ctor(Ctor::OptionNone, vec![]),
                _ => Pat::Wild,
            },
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => match ty {
                Type::Enum(item, ty_args) if *item == enum_item => {
                    let fields = self.variant_field_types(enum_item, variant_index, ty_args);
                    if args.len() != fields.len() {
                        return Pat::Wild;
                    }

                    let sub = args
                        .iter()
                        .zip(&fields)
                        .map(|(&arg, field_ty)| self.lower(arg, field_ty))
                        .collect();
                    Pat::Ctor(
                        Ctor::Variant {
                            enum_item,
                            index: variant_index,
                        },
                        sub,
                    )
                }
                _ => Pat::Wild,
            },
        }
    }

    /// Whether some value of `tys` falls through every row of `matrix`:
    /// up to [`MAX_WITNESSES`] materialized witness rows plus the exact
    /// (saturating) count of witnesses this generation scheme produces.
    fn witnesses(&self, matrix: &[Vec<Pat>], tys: &[Type]) -> (Vec<Vec<Pat>>, u64) {
        let Some((first_ty, rest_tys)) = tys.split_first() else {
            // No columns left: an empty matrix rejects the empty value.
            return if matrix.is_empty() {
                (vec![vec![]], 1)
            } else {
                (vec![], 0)
            };
        };

        let universe = self.constructor_universe(first_ty);
        let heads: Vec<&Ctor> = matrix
            .iter()
            .filter_map(|row| match &row[0] {
                Pat::Ctor(ctor, _) => Some(ctor),
                Pat::Wild => None,
            })
            .collect();

        let complete = universe
            .as_ref()
            .is_some_and(|ctors| ctors.iter().all(|(ctor, _)| heads.contains(&ctor)));

        if complete {
            self.witnesses_complete(matrix, rest_tys, &universe.expect("checked above"))
        } else {
            self.witnesses_incomplete(matrix, rest_tys, universe.as_deref(), &heads)
        }
    }

    /// Every constructor of the column type is matched somewhere:
    /// specialize each one, spreading its payload into new columns, and
    /// take the union of the sub-witnesses (disjoint by head).
    fn witnesses_complete(
        &self,
        matrix: &[Vec<Pat>],
        rest_tys: &[Type],
        universe: &[(Ctor, Vec<Type>)],
    ) -> (Vec<Vec<Pat>>, u64) {
        let mut witnesses = Vec::new();
        let mut total: u64 = 0;

        for (ctor, field_tys) in universe {
            let arity = field_tys.len();
            let specialized = specialize(matrix, ctor, arity);

            let mut sub_tys = field_tys.clone();
            sub_tys.extend_from_slice(rest_tys);
            let (sub_witnesses, sub_total) = self.witnesses(&specialized, &sub_tys);

            total = total.saturating_add(sub_total);
            for row in sub_witnesses {
                if witnesses.len() >= MAX_WITNESSES {
                    break;
                }

                let (fields, rest) = row.split_at(arity);
                let mut witness = vec![Pat::Ctor(ctor.clone(), fields.to_vec())];
                witness.extend_from_slice(rest);
                witnesses.push(witness);
            }
        }

        (witnesses, total)
    }

    /// Some constructor is unmatched (or the type is open): only the
    /// wildcard-headed rows can still cover, so recurse on the default
    /// matrix and prefix each sub-witness with every missing
    /// constructor — or a plain `_` when the type is open.
    fn witnesses_incomplete(
        &self,
        matrix: &[Vec<Pat>],
        rest_tys: &[Type],
        universe: Option<&[(Ctor, Vec<Type>)]>,
        heads: &[&Ctor],
    ) -> (Vec<Vec<Pat>>, u64) {
        let default: Vec<Vec<Pat>> = matrix
            .iter()
            .filter(|row| matches!(row[0], Pat::Wild))
            .map(|row| row[1..].to_vec())
            .collect();
        let (sub_witnesses, sub_total) = self.witnesses(&default, rest_tys);

        let head_witnesses: Vec<Pat> = match universe {
            Some(ctors) => ctors
                .iter()
                .filter(|(ctor, _)| !heads.contains(&ctor))
                .map(|(ctor, field_tys)| Pat::Ctor(ctor.clone(), vec![Pat::Wild; field_tys.len()]))
                .collect(),
            None => vec![Pat::Wild],
        };

        let total = sub_total.saturating_mul(head_witnesses.len() as u64);

        let mut witnesses = Vec::new();
        'outer: for head in &head_witnesses {
            for row in &sub_witnesses {
                if witnesses.len() >= MAX_WITNESSES {
                    break 'outer;
                }

                let mut witness = vec![head.clone()];
                witness.extend_from_slice(row);
                witnesses.push(witness);
            }
        }

        (witnesses, total)
    }

    /// The finite constructor set of a column type with each
    /// constructor's field types, or `None` for an open type (see the
    /// module docs for the decision table).
    fn constructor_universe(&self, ty: &Type) -> Option<Vec<(Ctor, Vec<Type>)>> {
        match ty {
            Type::Bool => Some(vec![
                (Ctor::Bool(true), vec![]),
                (Ctor::Bool(false), vec![]),
            ]),
            Type::Option(inner) => Some(vec![
                (Ctor::OptionSome, vec![(**inner).clone()]),
                (Ctor::OptionNone, vec![]),
            ]),
            Type::Enum(item, args) => {
                let Item::EnumDef(def) = self.hir.item(*item) else {
                    return None;
                };

                Some(
                    (0..def.variants.len())
                        .map(|index| {
                            (
                                Ctor::Variant {
                                    enum_item: *item,
                                    index,
                                },
                                self.variant_field_types(*item, index, args),
                            )
                        })
                        .collect(),
                )
            }
            Type::Tuple(tys) => Some(vec![(Ctor::Tuple, tys.clone())]),
            _ => None,
        }
    }

    /// The field types of one enum variant with the scrutinee's generic
    /// arguments substituted in. Types here only steer nested
    /// finiteness, so unresolvable annotations degrade to `Unknown`
    /// (open) rather than reporting anything.
    fn variant_field_types(&self, enum_item: ItemId, index: usize, args: &[Type]) -> Vec<Type> {
        let Item::EnumDef(def) = self.hir.item(enum_item) else {
            return vec![];
        };
        let Some(variant) = def.variants.get(index) else {
            return vec![];
        };

        let owner = DefRef::Item(enum_item);
        let map: HashMap<(DefRef, usize), Type> = args
            .iter()
            .enumerate()
            .map(|(arg_index, arg)| ((owner, arg_index), arg.clone()))
            .collect();

        variant
            .fields
            .iter()
            .map(|field| substitute(&self.conv_quiet(field.ty), &map))
            .collect()
    }

    /// A diagnostic-free approximation of the checker's annotation
    /// conversion: anything unresolved, mis-applied, or out of scope
    /// here (interfaces, `Self`) becomes `Unknown`, which is open.
    fn conv_quiet(&self, id: TypeExprId) -> Type {
        match self.hir.type_expr(id) {
            TypeExpr::Named { args, .. } => match self.res.type_res.get(&id).copied() {
                Some(TypeRes::Builtin(builtin)) => self.conv_quiet_builtin(builtin, args),
                Some(TypeRes::Item(item)) => {
                    let conv_args: Vec<Type> = args.iter().map(|&a| self.conv_quiet(a)).collect();
                    match self.hir.item(item) {
                        Item::StructDef(def) if def.generics.len() == conv_args.len() => {
                            Type::Struct(item, conv_args)
                        }
                        Item::EnumDef(def) if def.generics.len() == conv_args.len() => {
                            Type::Enum(item, conv_args)
                        }
                        _ => Type::Unknown,
                    }
                }
                Some(TypeRes::GenericParam { owner, index }) => Type::Generic { owner, index },
                Some(TypeRes::SelfType) | None => Type::Unknown,
            },
            TypeExpr::Tuple(elements) => {
                Type::Tuple(elements.iter().map(|&e| self.conv_quiet(e)).collect())
            }
            TypeExpr::Fn { params, ret } => Type::Fn {
                params: params.iter().map(|&p| self.conv_quiet(p)).collect(),
                ret: Box::new(self.conv_quiet(*ret)),
            },
        }
    }

    fn conv_quiet_builtin(&self, builtin: BuiltinType, args: &[TypeExprId]) -> Type {
        let arg = |i: usize| {
            args.get(i)
                .map(|&a| self.conv_quiet(a))
                .unwrap_or(Type::Unknown)
        };

        match builtin {
            BuiltinType::Int => Type::Int,
            BuiltinType::Float => Type::Float,
            BuiltinType::Bool => Type::Bool,
            BuiltinType::String => Type::String,
            BuiltinType::Char => Type::Char,
            BuiltinType::Unit => Type::Unit,
            BuiltinType::Range => Type::Range,
            BuiltinType::Option => Type::option(arg(0)),
            BuiltinType::Vector => Type::vector(arg(0)),
            BuiltinType::Set => Type::Set(Box::new(arg(0))),
            BuiltinType::Map => Type::Map(Box::new(arg(0)), Box::new(arg(1))),
            BuiltinType::Comparable | BuiltinType::Printable | BuiltinType::Hashable => {
                Type::Unknown
            }
        }
    }

    /// Renders a witness the way diagnostics spell patterns: `_` for
    /// holes, `Some(_)`, `Rect(_, _)`, `(true, None)`.
    fn render(&self, pat: &Pat) -> String {
        let (ctor, args) = match pat {
            Pat::Wild => return "_".to_string(),
            Pat::Ctor(ctor, args) => (ctor, args),
        };

        let rendered_args = || {
            let parts: Vec<String> = args.iter().map(|arg| self.render(arg)).collect();
            parts.join(", ")
        };

        match ctor {
            Ctor::Bool(b) => b.to_string(),
            Ctor::OptionSome => format!("Some({})", rendered_args()),
            Ctor::OptionNone => "None".to_string(),
            Ctor::Tuple => format!("({})", rendered_args()),
            Ctor::Variant { enum_item, index } => {
                let name = match self.hir.item(*enum_item) {
                    Item::EnumDef(def) => def
                        .variants
                        .get(*index)
                        .map(|v| v.name.as_str())
                        .unwrap_or("<variant>"),
                    _ => "<variant>",
                };

                if args.is_empty() {
                    name.to_string()
                } else {
                    format!("{}({})", name, rendered_args())
                }
            }
            // Literals never appear in witnesses: they are only matrix
            // rows, and rows are never turned into witnesses.
            Ctor::OpaqueLit => "_".to_string(),
        }
    }
}

/// Maranget's S(ctor, matrix): rows whose head is `ctor` spread their
/// sub-patterns into the payload columns; wildcard-headed rows spread
/// wildcards; other heads drop.
fn specialize(matrix: &[Vec<Pat>], ctor: &Ctor, arity: usize) -> Vec<Vec<Pat>> {
    let mut specialized = Vec::new();

    for row in matrix {
        let mut new_row: Vec<Pat> = match &row[0] {
            Pat::Ctor(head, args) if head == ctor => args.clone(),
            Pat::Ctor(..) => continue,
            Pat::Wild => vec![Pat::Wild; arity],
        };

        new_row.extend_from_slice(&row[1..]);
        specialized.push(new_row);
    }

    specialized
}
