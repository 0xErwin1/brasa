//! The `Set<T>` method surface (`docs/spec/05-stdlib.md`, BRS-35).
//!
//! A [`crate::RecvShape::Elem`] receiver, like `Vector<T>`.
//!
//! `remove` is the clearest case for why a shared name is declared per
//! receiver rather than once: on a `Set` it answers whether the element
//! was there, and on a `Map` it answers the value that was removed.
//! Same name, same id, same runtime dispatch, two different contracts.

crate::method_table! {
    /// Every `Set<T>` method, in declaration order.
    SetMember => SET_METHODS, receiver "Set" Elem {
        Len       "len"       ()          -> int;

        /// Adding is idempotent and says nothing about whether the
        /// element was new; `has?` is the question to ask first.
        Add       "add"       (elem)      -> unit;

        /// Answers whether the element was present, which is the only
        /// information a removal from a set can carry.
        Remove    "remove"    (elem)      -> bool;
        Has       "has?"      (elem)      -> bool;

        /// The algebra, each answering a NEW set.
        Union     "union"     ([Set<elem>]) -> [Set<elem>];
        Intersect "intersect" ([Set<elem>]) -> [Set<elem>];
        Diff      "diff"      ([Set<elem>]) -> [Set<elem>];
    }
}

#[cfg(test)]
mod tests {
    use super::{SET_METHODS, SetMember};
    use crate::{RetDesc, TyDesc};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in SET_METHODS {
            let member = SetMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(SetMember::from_name("pop"), None);
    }

    #[test]
    fn no_member_throws() {
        for decl in SET_METHODS {
            assert!(
                decl.throws.is_empty(),
                "`Set.{}` declares an error, but the surface is infallible",
                decl.name
            );
        }
    }

    /// The three algebraic operations are closed over the receiver:
    /// each takes a set of the same element and answers one. A row that
    /// answered a `Vector` would break chaining without anyone noticing
    /// until a caller chained.
    #[test]
    fn the_algebra_is_closed_over_the_receiver() {
        for name in ["union", "intersect", "diff"] {
            let decl = SetMember::from_name(name)
                .expect("the operation exists")
                .decl();

            assert_eq!(decl.params, &[TyDesc::Set(&TyDesc::Elem)]);
            assert_eq!(decl.ret, RetDesc::Ty(TyDesc::Set(&TyDesc::Elem)));
        }
    }

    /// `remove` answers a bool here. On a `Map` the same name answers
    /// the removed value; the two share one id and differ only by
    /// receiver, which is exactly why each declares its own row.
    #[test]
    fn remove_reports_presence_rather_than_a_value() {
        assert_eq!(
            SetMember::Remove.decl().ret,
            RetDesc::Ty(TyDesc::Bool),
            "a set removal has no value to hand back"
        );
    }
}
