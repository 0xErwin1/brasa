//! The `int` and `float` method surfaces (`docs/spec/05-stdlib.md`).
//!
//! Two [`crate::RecvShape::Plain`] receivers in one file because they
//! are each three rows and they mirror each other: every conversion
//! here is explicit, since the language has no implicit numeric
//! coercion.
//!
//! Their `toString` rows are the one place the universal derived
//! `toString` is declared rather than layered on. That is not a
//! duplicate: the VM special-cases the numeric receivers so a number
//! renders through the numeric formatter rather than the generic
//! display, and declaring it keeps the checker and that arm agreeing.

crate::method_table! {
    /// Every `int` method, in declaration order.
    IntMember => INT_METHODS, receiver "int" Plain {
        /// Widening is explicit: an int does not become a float by
        /// being used as one.
        ToFloat  "toFloat"  ()    -> float;

        /// Renders with a fixed number of decimal places.
        ToFixed  "toFixed"  (int) -> string;
        ToString "toString" ()    -> string;
    }
}

crate::method_table! {
    /// Every `float` method, in declaration order.
    FloatMember => FLOAT_METHODS, receiver "float" Plain {
        /// Truncates toward zero. Rounding is `math.round`, which is a
        /// different question and lives in a different place.
        ToInt    "toInt"    ()    -> int;

        ToFixed  "toFixed"  (int) -> string;
        ToString "toString" ()    -> string;
    }
}

#[cfg(test)]
mod tests {
    use super::{FLOAT_METHODS, FloatMember, INT_METHODS, IntMember};

    /// `decl` indexes each table by the variant's position, so a row
    /// and its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in INT_METHODS {
            let member = IntMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));
            assert_eq!(member.decl().name, decl.name);
        }

        for decl in FLOAT_METHODS {
            let member = FloatMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));
            assert_eq!(member.decl().name, decl.name);
        }
    }

    /// Each converts to the OTHER kind, and neither offers a no-op
    /// conversion to its own. `1.toInt()` would be a way to write
    /// nothing, and the checker reports it as an unknown member.
    #[test]
    fn each_converts_only_to_the_other_kind() {
        assert!(IntMember::from_name("toFloat").is_some());
        assert!(IntMember::from_name("toInt").is_none());

        assert!(FloatMember::from_name("toInt").is_some());
        assert!(FloatMember::from_name("toFloat").is_none());
    }

    /// Arithmetic answers, never failures: truncating and widening are
    /// total, and formatting cannot fail.
    #[test]
    fn no_numeric_method_throws() {
        for decl in INT_METHODS.iter().chain(FLOAT_METHODS) {
            assert!(
                decl.throws.is_empty(),
                "`{}` declares an error, but the numeric surface is infallible",
                decl.name
            );
        }
    }
}
