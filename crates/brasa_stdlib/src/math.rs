//! The `std::math` member surface (spec: 05 — Stdlib de scripting, BRS-35).
//!
//! The module that uses all three row forms, which is why it was left
//! for last: the float members are ordinary calls, `pi` and `e` are
//! read without a call, and `abs`/`min`/`max` are the delegated ones.
//!
//! Nothing here throws. `math.sqrt(-1.0)` is `NaN` and `1.0 / 0.0` is
//! `inf` — IEEE 754 answers rather than failures — so the whole surface
//! is infallible and its `throws` column is empty on every row.

crate::module_table! {
    /// Every `std::math` member, in surface order.
    MathMember => MATH_MEMBERS, module "math" {
        /// The float members: each takes and answers a float, with no
        /// int arm. An int caller writes `.toFloat()`, which is the
        /// language's rule against implicit coercion, not this
        /// module's choice.
        Sqrt  "sqrt"  (float)        -> float;
        Floor "floor" (float)        -> float;
        Ceil  "ceil"  (float)        -> float;
        Round "round" (float)        -> float;
        Pow   "pow"   (float, float) -> float;

        /// The three that must answer in the kind they were given:
        /// `math.abs(-1)` is an int and `math.abs(-1.0)` is a float,
        /// and collapsing them onto float would make integer code
        /// round-trip through a type it never asked for.
        Abs   "abs"   custom "polymorphic over int and float, and answers in the kind it was given";
        Min   "min"   custom "polymorphic over int and float, and answers in the kind it was given";
        Max   "max"   custom "polymorphic over int and float, and answers in the kind it was given";

        /// Read without a call. `math.pi()` is a call on a plain value,
        /// which the checker reports as such rather than as an unknown
        /// member.
        Pi    "pi"    constant float;
        E     "e"     constant float;
    }
}

#[cfg(test)]
mod tests {
    use super::{MATH_MEMBERS, MathMember};
    use crate::{ModuleKind, TyDesc};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in MATH_MEMBERS {
            let member = MathMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(MathMember::from_name("log"), None);
    }

    /// IEEE 754 answers instead of failures, all the way across.
    #[test]
    fn no_member_throws() {
        for decl in MATH_MEMBERS {
            assert!(
                decl.throws.is_empty(),
                "`math.{}` declares an error, but the surface is infallible",
                decl.name
            );
        }
    }

    /// The escape hatch is exactly the three numeric-polymorphic
    /// members. A fourth appearing here without a reason is how
    /// `custom` would quietly become the ordinary way to add a member.
    #[test]
    fn only_the_numeric_polymorphic_members_are_delegated() {
        for decl in MATH_MEMBERS {
            let delegated = matches!(decl.kind, ModuleKind::Custom(_));

            assert_eq!(
                delegated,
                matches!(decl.name, "abs" | "min" | "max"),
                "`math.{}` disagrees with the delegation rule",
                decl.name
            );
        }
    }

    /// The two constants are the only members read without a call, and
    /// both are floats. An int constant here would be a silent trap:
    /// `math.pi` is not a value anyone should be able to use as one.
    #[test]
    fn the_constants_are_pi_and_e() {
        for decl in MATH_MEMBERS {
            match decl.kind {
                ModuleKind::Constant(ret) => {
                    assert!(matches!(decl.name, "pi" | "e"));
                    assert_eq!(ret, TyDesc::Float);
                }
                _ => assert!(!matches!(decl.name, "pi" | "e")),
            }
        }
    }

    /// Every called member takes and answers a float. This is the rule
    /// that keeps `math` from growing an int arm by accident, which the
    /// language forbids elsewhere as an implicit coercion.
    #[test]
    fn every_called_member_is_float_to_float() {
        for decl in MATH_MEMBERS {
            let ModuleKind::Call {
                required,
                optional,
                ret,
            } = decl.kind
            else {
                continue;
            };

            assert!(
                optional.is_empty(),
                "`math.{}` has an optional parameter; none of them should",
                decl.name
            );
            assert_eq!(ret, TyDesc::Float);

            for param in required {
                assert_eq!(
                    *param,
                    crate::ParamDesc::Ty(TyDesc::Float),
                    "`math.{}` takes something other than a float",
                    decl.name
                );
            }
        }
    }
}
