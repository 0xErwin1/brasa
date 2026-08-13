//! Module-level compilation state: the constant pool, the function
//! table under construction, and the id maps from HIR items to
//! bytecode indices (functions, struct/enum shapes, global slots).
//!
//! The `collect` pre-pass assigns every index before any code is
//! emitted, so forward references (a call to a function declared later,
//! a struct literal above its methods) compile to direct indices.

use std::collections::HashMap;

use brasa_bytecode::{
    ConstId, ConstPool, Constant, EnumId, EnumShape, FuncId, Function, GlobalIx, Module, StructId,
    StructShape, Variant,
};
use brasa_diagnostics::{Diagnostic, codes};
use brasa_hir::{Hir, Item, ItemId};
use brasa_resolver::Resolutions;
use brasa_source::Span;
use brasa_typeck::TypeTables;

use crate::CompileResult;
use crate::limits::{self, MAX_ARGS, MAX_BINDINGS, MAX_MEMBERS, MAX_PARAMS};

pub(crate) struct Cx<'a> {
    pub(crate) hir: &'a Hir,
    pub(crate) res: &'a Resolutions,
    pub(crate) types: &'a TypeTables,
    pub(crate) pool: ConstPool,
    /// Every bytecode limit this module breaks. A non-empty list makes
    /// [`Cx::finish`] discard the module: the values narrowed past their
    /// operands were clamped to keep lowering going, so the code emitted
    /// after the first report is not runnable.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Function slots, reserved by `collect` (and lambdas mid-compile)
    /// and filled by `define_function`.
    functions: Vec<Option<Function>>,
    pub(crate) func_of_item: HashMap<ItemId, FuncId>,
    pub(crate) func_of_method: HashMap<(ItemId, usize), FuncId>,
    pub(crate) struct_of_item: HashMap<ItemId, StructId>,
    pub(crate) enum_of_item: HashMap<ItemId, EnumId>,
    pub(crate) global_of_item: HashMap<ItemId, GlobalIx>,
    pub(crate) structs: Vec<StructShape>,
    pub(crate) enums: Vec<EnumShape>,
    pub(crate) globals: Vec<String>,
    /// The executed file's `main`, once bodies are compiled.
    pub(crate) entry: Option<FuncId>,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(hir: &'a Hir, res: &'a Resolutions, types: &'a TypeTables) -> Cx<'a> {
        Cx {
            hir,
            res,
            types,
            pool: ConstPool::new(),
            diagnostics: Vec::new(),
            functions: Vec::new(),
            func_of_item: HashMap::new(),
            func_of_method: HashMap::new(),
            struct_of_item: HashMap::new(),
            enum_of_item: HashMap::new(),
            global_of_item: HashMap::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            globals: Vec::new(),
            entry: None,
        }
    }

    pub(crate) fn report(&mut self, code: &str, message: String, label: &str, span: Span) {
        self.diagnostics
            .push(limits::error(code, message, label.to_string(), span));
    }

    /// Narrows an argument count to the `argc` operand, reporting the
    /// call that does not fit.
    pub(crate) fn argc(&mut self, count: usize, span: Span) -> u8 {
        u8::try_from(count).unwrap_or_else(|_| {
            self.report(
                codes::C_TOO_MANY_ARGUMENTS,
                format!("call takes {count} arguments, but the limit is {MAX_ARGS}"),
                "too many arguments",
                span,
            );
            u8::MAX
        })
    }

    /// Narrows a parameter count to a frame's `arity`, reporting the
    /// declaration that does not fit.
    pub(crate) fn arity(&mut self, what: &str, count: usize, span: Span) -> u8 {
        u8::try_from(count).unwrap_or_else(|_| {
            self.report(
                codes::C_TOO_MANY_PARAMETERS,
                format!("{what} takes {count} parameters, but the limit is {MAX_PARAMS}"),
                "too many parameters",
                span,
            );
            u8::MAX
        })
    }

    /// Narrows a captured-value count to `make_closure`'s operand.
    pub(crate) fn capture_count(&mut self, count: usize, span: Span) -> u16 {
        u16::try_from(count).unwrap_or_else(|_| {
            self.report(
                codes::C_TOO_MANY_BINDINGS,
                format!("closure captures {count} values, but the limit is {MAX_BINDINGS}"),
                "too many captured values",
                span,
            );
            u16::MAX
        })
    }

    /// Reserves a function-table slot and returns its id.
    pub(crate) fn reserve_function(&mut self) -> FuncId {
        let id = FuncId(u32::try_from(self.functions.len()).expect("function table overflow"));
        self.functions.push(None);
        id
    }

    pub(crate) fn define_function(&mut self, id: FuncId, function: Function) {
        let slot = &mut self.functions[id.0 as usize];
        debug_assert!(slot.is_none(), "function {} defined twice", id.0);
        *slot = Some(function);
    }

    pub(crate) fn const_str(&mut self, text: &str) -> ConstId {
        self.pool.insert(Constant::Str(text.to_string()))
    }

    /// Assigns every module-level index in source order: `FuncId(0)`
    /// for `<toplevel>`, then function/method ids, struct and enum
    /// shapes, and global slots.
    pub(crate) fn collect(&mut self, roots: &[ItemId]) {
        let toplevel = self.reserve_function();
        debug_assert_eq!(toplevel, FuncId(0));

        for &item_id in roots {
            match self.hir.item(item_id) {
                Item::FuncDef(_) => {
                    let id = self.reserve_function();
                    self.func_of_item.insert(item_id, id);
                }
                Item::StructDef(def) => {
                    let struct_id =
                        StructId(u32::try_from(self.structs.len()).expect("struct table overflow"));
                    self.struct_of_item.insert(item_id, struct_id);

                    if def.fields.len() > MAX_MEMBERS {
                        let (name, count) = (&def.name, def.fields.len());
                        self.diagnostics.push(limits::error(
                            codes::C_TOO_MANY_MEMBERS,
                            format!(
                                "struct `{name}` has {count} fields, but the limit is {MAX_MEMBERS}"
                            ),
                            "too many fields".to_string(),
                            self.hir.span_of_item(item_id),
                        ));
                    }

                    let mut methods = Vec::with_capacity(def.methods.len());
                    let mut to_string = None;
                    for (index, method) in def.methods.iter().enumerate() {
                        let id = self.reserve_function();
                        self.func_of_method.insert((item_id, index), id);
                        methods.push(id);
                        if method.name == "toString" {
                            to_string = Some(id);
                        }
                    }

                    self.structs.push(StructShape {
                        name: def.name.clone(),
                        fields: def.fields.iter().map(|f| f.name.clone()).collect(),
                        methods,
                        to_string,
                    });
                }
                Item::EnumDef(def) => {
                    let enum_id =
                        EnumId(u32::try_from(self.enums.len()).expect("enum table overflow"));
                    self.enum_of_item.insert(item_id, enum_id);

                    let span = self.hir.span_of_item(item_id);
                    if def.variants.len() > MAX_MEMBERS {
                        let (name, count) = (&def.name, def.variants.len());
                        self.diagnostics.push(limits::error(
                            codes::C_TOO_MANY_MEMBERS,
                            format!(
                                "enum `{name}` has {count} variants, but the limit is {MAX_MEMBERS}"
                            ),
                            "too many variants".to_string(),
                            span,
                        ));
                    }

                    let mut variants = Vec::with_capacity(def.variants.len());
                    for variant in &def.variants {
                        let arity = u8::try_from(variant.fields.len()).unwrap_or_else(|_| {
                            let (name, count) = (&variant.name, variant.fields.len());
                            self.diagnostics.push(limits::error(
                                codes::C_TOO_MANY_PARAMETERS,
                                format!(
                                    "variant `{name}` takes {count} parameters, but the limit is {MAX_PARAMS}"
                                ),
                                "too many parameters".to_string(),
                                span,
                            ));
                            u8::MAX
                        });
                        variants.push(Variant {
                            name: variant.name.clone(),
                            arity,
                        });
                    }

                    self.enums.push(EnumShape {
                        name: def.name.clone(),
                        variants,
                    });
                }
                Item::TopLet(top_let) => {
                    // Reported once, at the first global that does not
                    // fit: every later one breaks the same limit for the
                    // same reason.
                    if self.globals.len() == MAX_BINDINGS + 1 {
                        self.diagnostics.push(limits::error(
                            codes::C_TOO_MANY_BINDINGS,
                            format!("the module defines more than {MAX_BINDINGS} globals"),
                            "too many globals".to_string(),
                            self.hir.span_of_item(item_id),
                        ));
                    }
                    let ix = GlobalIx(u16::try_from(self.globals.len()).unwrap_or(u16::MAX));
                    self.global_of_item.insert(item_id, ix);
                    self.globals.push(top_let.let_stmt.name.clone());
                }
                // A test is not part of a normal run: `brasa test`
                // compiles them and `brasa script.bras` never sees one,
                // so they cost nothing at cold start.
                Item::TestDef(_) | Item::Import(_) | Item::InterfaceDef(_) | Item::Stmt(_) => {}
            }
        }
    }

    /// Builds the module, or an empty one when a limit was reported:
    /// past a clamped operand the emitted code no longer describes the
    /// program, so it must never reach a backend.
    pub(crate) fn finish(self) -> CompileResult {
        if !self.diagnostics.is_empty() {
            return CompileResult {
                module: Module {
                    constants: ConstPool::new(),
                    functions: Vec::new(),
                    structs: Vec::new(),
                    enums: Vec::new(),
                    globals: Vec::new(),
                    entry: None,
                },
                diagnostics: self.diagnostics,
            };
        }

        CompileResult {
            module: Module {
                constants: self.pool,
                functions: self
                    .functions
                    .into_iter()
                    .map(|f| f.expect("every reserved function is defined"))
                    .collect(),
                structs: self.structs,
                enums: self.enums,
                globals: self.globals,
                entry: self.entry,
            },
            diagnostics: Vec::new(),
        }
    }
}
