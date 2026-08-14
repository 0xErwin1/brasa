//! The `Vector<T>` method surface (spec: 05 — Stdlib de scripting).
//!
//! Types are written in the [`crate::ty!`] language: bare words for
//! primitives, `elem` for the receiver's element type, and brackets
//! around every composite type.
//!
//! Names shared with another receiver kind (`len`, `slice`, `join`,
//! `find`, `each`, `contains?`, `reverse`) are declared here for the
//! `Vector` receiver only. They share ONE `brasa_bytecode` id with the
//! other receivers that carry them, and the VM dispatches on the
//! receiver's runtime kind, so declaring the `Vector` arm here does not
//! duplicate the string or `Map` arm.

crate::method_table! {
    /// Every `Vector<T>` method, in declaration order.
    VectorMember => VECTOR_METHODS, receiver "Vector" Elem {
        Len      "len"       ()                     -> int;
        Push     "push"      (elem)                 -> unit;
        Pop      "pop"       ()                     -> [Option<elem>];
        First    "first"     ()                     -> [Option<elem>];
        Last     "last"      ()                     -> [Option<elem>];
        Reverse  "reverse"   ()                     -> [Vector<elem>];
        Contains "contains?" (elem)                 -> bool;

        /// Shares `string.slice`'s contract, including its clamping:
        /// two members named `slice` that disagreed on the out-of-range
        /// cases would be worse than one of them missing.
        Slice    "slice"     (int, int)             -> [Vector<elem>];

        /// Accepts any element type: every value has the derived
        /// `toString`, so demanding `Vector<string>` only forced the
        /// caller to write the `map` the builtin can do itself (BRS-53).
        Join     "join"      (string)               -> string;

        Map      "map"       ([fn(elem) -> unknown]) -> fnRetVector;
        Filter   "filter"    ([fn(elem) -> bool])   -> [Vector<elem>];
        Each     "each"      ([fn(elem) -> unit])   -> unit;
        SortBy   "sortBy"    ([fn(elem) -> unknown]) -> [Vector<elem>];

        /// `reduce(init, f)` folds left: `(U, (U, T) -> U) -> U`. A
        /// table cannot see the `init` argument, so this row serves the
        /// bound-value form; the call form is special-cased in the
        /// checker, which infers `U` from `init` (BRS-35).
        Reduce   "reduce"    (unknown, [fn(unknown, elem) -> unknown]) -> unknown;

        Find     "find"      ([fn(elem) -> bool])   -> [Option<elem>];
        Any      "any?"      ([fn(elem) -> bool])   -> bool;
        All      "all?"      ([fn(elem) -> bool])   -> bool;

        /// Natural ascending order, so it only exists on vectors of
        /// orderable elements — the same key rule `sortBy` enforces at
        /// runtime (BRS-35). That existence rule is not data, hence
        /// `custom`.
        Sort     "sort"      ()                     -> custom;

        /// `zip(other)`'s pair type depends on the argument; like
        /// `reduce`, this row serves the bound-value form and the call
        /// form is special-cased in the checker (BRS-35).
        Zip      "zip"       ([Vector<unknown>])    -> [Vector<[Tuple<elem, unknown>]>];

        /// Removes exactly one nesting level, so it only exists on
        /// `Vector<Vector<T>>` and its result type is the receiver's
        /// inner element — neither is expressible as data, hence
        /// `custom`.
        Flatten  "flatten"   ()                     -> custom;

        Uniq     "uniq"      ()                     -> [Vector<elem>];
    }
}

#[cfg(test)]
mod tests {
    use super::{VECTOR_METHODS, VectorMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order. A table whose rows and
    /// variants drifted apart would hand every layer the wrong
    /// signature silently.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in VECTOR_METHODS {
            let member = VectorMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn names_are_unique() {
        for decl in VECTOR_METHODS {
            let declared = VECTOR_METHODS
                .iter()
                .filter(|d| d.name == decl.name)
                .count();
            assert_eq!(declared, 1, "`{}` is declared twice", decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(VectorMember::from_name("definitelyNotAMember"), None);
    }
}
