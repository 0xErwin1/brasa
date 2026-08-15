//! The frozen `BuiltinId` assignment.
//!
//! `BuiltinId` values are positions in `brasa_bytecode::BUILTINS`. This
//! test carries the complete id-to-name mapping written out by hand, so
//! a reordering, an insertion in the middle, or a removal fails here
//! with the exact position that moved.
//!
//! Adding a builtin means appending one row at the end of both tables.
//!
//! What the pin actually defends, since the distinction decides how
//! seriously to take a failure here:
//!
//! - **Not a wire format.** Bytecode is never serialized
//!   (`brasa_bytecode::Module`, spec non-goal) and `brasa bundle` embeds
//!   each module's SOURCE, recompiled at startup — so no id ever crosses
//!   a process boundary. Within one build the code generator and the VM
//!   read this same table, which makes any permutation internally
//!   consistent: a reordering is a churn problem, not a correctness one.
//! - **A review line per builtin.** Appending here is the diff that says
//!   the surface grew, next to the registry row that grew it.
//! - **Snapshot stability.** The disassembler prints the raw id
//!   (`call_builtin b0, 1  ; puts`), so `brasa_bytecode` and
//!   `brasa_codegen` goldens move with any renumbering.

/// Every registered builtin: its id, its surface name, and whether the
/// first pushed operand is the receiver.
const PINNED: &[(u16, &str, bool)] = &[
    (0, "puts", false),
    (1, "print", false),
    (2, "math.sqrt", false),
    (3, "math.floor", false),
    (4, "math.ceil", false),
    (5, "math.round", false),
    (6, "math.pow", false),
    (7, "math.abs", false),
    (8, "math.min", false),
    (9, "math.max", false),
    (10, "<fatal>", false),
    (11, "<assert-failed>", false),
    (12, "toString", true),
    (13, "len", true),
    (14, "count", true),
    (15, "trim", true),
    (16, "toUpper", true),
    (17, "toLower", true),
    (18, "contains?", true),
    (19, "startsWith?", true),
    (20, "endsWith?", true),
    (21, "split", true),
    (22, "lines", true),
    (23, "chars", true),
    (24, "slice", true),
    (25, "repeat", true),
    (26, "replace", true),
    (27, "find", true),
    (28, "toInt", true),
    (29, "toFloat", true),
    (30, "push", true),
    (31, "pop", true),
    (32, "first", true),
    (33, "last", true),
    (34, "reverse", true),
    (35, "join", true),
    (36, "map", true),
    (37, "filter", true),
    (38, "each", true),
    (39, "sortBy", true),
    (40, "keys", true),
    (41, "values", true),
    (42, "insert", true),
    (43, "remove", true),
    (44, "get", true),
    (45, "has?", true),
    (46, "add", true),
    (47, "bytes", true),
    (48, "trimStart", true),
    (49, "trimEnd", true),
    (50, "padStart", true),
    (51, "padEnd", true),
    (52, "match?", true),
    (53, "captures", true),
    (54, "replaceRe", true),
    (55, "scan", true),
    (56, "proc.run", false),
    (57, "proc.tryRun", false),
    (58, "proc.shell", false),
    (59, "proc.tryRunAll", false),
    (60, "http.get", false),
    (61, "http.post", false),
    (62, "status", true),
    (63, "body", true),
    (64, "header", true),
    (65, "cli.parse", false),
    (66, "cli.help", false),
    (67, "flag", true),
    (68, "option", true),
    (69, "rest", true),
    (70, "env.get", false),
    (71, "env.set", false),
    (72, "env.vars", false),
    (73, "env.args", false),
    (74, "stdout", true),
    (75, "stderr", true),
    (76, "code", true),
    (77, "fs.read", false),
    (78, "fs.write", false),
    (79, "fs.append", false),
    (80, "fs.exists?", false),
    (81, "fs.isFile?", false),
    (82, "fs.isDir?", false),
    (83, "fs.ls", false),
    (84, "fs.glob", false),
    (85, "fs.walk", false),
    (86, "fs.mkdir", false),
    (87, "fs.mkdirAll", false),
    (88, "fs.rm", false),
    (89, "fs.rmAll", false),
    (90, "fs.cp", false),
    (91, "fs.mv", false),
    (92, "fs.join", false),
    (93, "fs.base", false),
    (94, "fs.dir", false),
    (95, "fs.ext", false),
    (96, "fs.abs", false),
    (97, "env.cwd", false),
    (98, "env.cd", false),
    (99, "json.parse", false),
    (100, "json.stringify", false),
    (101, "io.puts", false),
    (102, "io.print", false),
    (103, "io.eprint", false),
    (104, "io.readLine", false),
    (105, "io.readAll", false),
    (106, "asString", true),
    (107, "asInt", true),
    (108, "asFloat", true),
    (109, "asBool", true),
    (110, "asArray", true),
    (111, "asObject", true),
    (112, "null?", true),
    (113, "reduce", true),
    (114, "any?", true),
    (115, "all?", true),
    (116, "sort", true),
    (117, "zip", true),
    (118, "flatten", true),
    (119, "uniq", true),
    (120, "entries", true),
    (121, "merge", true),
    (122, "union", true),
    (123, "intersect", true),
    (124, "diff", true),
    (125, "math.pi", false),
    (126, "math.e", false),
    (127, "time.now", false),
    (128, "time.nowMillis", false),
    (129, "time.sleep", false),
    (130, "time.iso", false),
    (131, "rand.seed", false),
    (132, "rand.int", false),
    (133, "rand.float", false),
    (134, "rand.choice", false),
    (135, "rand.shuffle", false),
    (136, "toFixed", true),
    (137, "env.exit", false),
    (138, "fs.resolve", false),
    (139, "fs.isSymlink?", false),
    (140, "paths", true),
    (141, "unreadable", true),
    (142, "fs.tryWalk", false),
    (143, "removePrefix", true),
    (144, "concurrent", false),
    (145, "spawn", true),
    (146, "value", true),
    (147, "http.getWith", false),
    (148, "http.postWith", false),
    (149, "message", true),
    (150, "json.of", false),
];

#[test]
fn builtin_ids_match_the_frozen_assignment() {
    assert_eq!(
        brasa_bytecode::BUILTINS.len(),
        PINNED.len(),
        "the registry gained or lost entries: only appending is compatible"
    );

    for &(id, name, has_receiver) in PINNED {
        let def = brasa_bytecode::builtin_def(brasa_bytecode::BuiltinId(id))
            .unwrap_or_else(|| panic!("id {id} (`{name}`) left the registry"));

        assert_eq!(def.name, name, "id {id} no longer names `{name}`");
        assert_eq!(
            def.has_receiver, has_receiver,
            "`{name}` changed receiver kind"
        );

        assert_eq!(
            brasa_bytecode::builtin_id(name),
            Some(brasa_bytecode::BuiltinId(id)),
            "`{name}` no longer resolves to id {id}"
        );
    }
}

/// The names shared across receiver kinds (BRS-53): one id serves every
/// receiver, and the VM dispatches on the runtime kind. Splitting one of
/// these into a second entry would renumber everything after it.
#[test]
fn receiver_polymorphic_names_hold_exactly_one_id() {
    for name in [
        "len",
        "slice",
        "join",
        "find",
        "each",
        "remove",
        "has?",
        "reverse",
        "contains?",
    ] {
        let ids = PINNED.iter().filter(|(_, n, _)| *n == name).count();
        assert_eq!(ids, 1, "`{name}` must keep exactly one shared id");
    }
}
