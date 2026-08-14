//! The `Map<K, V>` method surface (spec: 05 — Stdlib de scripting, BRS-35).
//!
//! The receiver that forced [`crate::RecvShape`] to exist. Every other
//! converted receiver has one type argument or none, and `elem` served
//! both; a map has two and neither of them is "the element", so its
//! rows say `key` and `value` instead.
//!
//! Names shared with another receiver kind (`len`, `remove`, `has?`,
//! `get`, `each`) are declared here for the `Map` receiver only, with
//! the signature that receiver gives them — `remove` answers the
//! removed value here and a bool on a `Set`. One id, one runtime
//! dispatch, two declarations that do not have to agree because they
//! describe different receivers.

crate::method_table! {
    /// Every `Map<K, V>` method, in declaration order.
    MapMember => MAP_METHODS, receiver "Map" KeyValue {
        Len     "len"     ()                          -> int;

        Keys    "keys"    ()                          -> [Vector<key>];
        Values  "values"  ()                          -> [Vector<value>];

        /// Insertion is a statement: the previous value, if any, is not
        /// returned. A caller that wants it asks first.
        Insert  "insert"  (key, value)                -> unit;

        /// Both lookups answer `Option`, since absence is the ordinary
        /// case rather than an error.
        Remove  "remove"  (key)                       -> [Option<value>];
        Get     "get"     (key)                       -> [Option<value>];
        Has     "has?"    (key)                       -> bool;

        /// Pairs in iteration order, which is what makes a map
        /// destructurable in a `for` binding.
        Entries "entries" ()                          -> [Vector<[Tuple<key, value>]>];

        /// Answers a NEW map; the receiver is untouched.
        Merge   "merge"   ([Map<key, value>])         -> [Map<key, value>];

        /// The callback takes the pair split, not as a tuple — the
        /// same shape the `for` binding gives it.
        Each    "each"    ([fn(key, value) -> unit])  -> unit;
    }
}

#[cfg(test)]
mod tests {
    use super::{MAP_METHODS, MapMember};
    use crate::TyDesc;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in MAP_METHODS {
            let member = MapMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(MapMember::from_name("clear"), None);
    }

    /// Reading a map cannot fail — absence is an `Option`, not an
    /// error — so nothing here contributes to a caller's error-set.
    #[test]
    fn no_member_throws() {
        for decl in MAP_METHODS {
            assert!(
                decl.throws.is_empty(),
                "`Map.{}` declares an error, but absence is an Option here",
                decl.name
            );
        }
    }

    /// Every keyed lookup takes the key type, not the value type.
    /// Swapping the two is the mistake this receiver's shape exists to
    /// make catchable, and it would typecheck for any map whose key and
    /// value happen to coincide.
    #[test]
    fn the_lookups_take_a_key() {
        for name in ["get", "remove", "has?"] {
            let member = MapMember::from_name(name).expect("the lookup exists");

            assert_eq!(
                member.decl().params,
                &[TyDesc::Key],
                "`Map.{name}` does not take exactly a key"
            );
        }

        assert_eq!(
            MapMember::Insert.decl().params,
            &[TyDesc::Key, TyDesc::Value],
            "`Map.insert` takes the pair in key-then-value order"
        );
    }
}
