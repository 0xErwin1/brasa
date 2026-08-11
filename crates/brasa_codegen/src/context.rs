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
use brasa_hir::{Hir, Item, ItemId};
use brasa_resolver::Resolutions;
use brasa_typeck::TypeTables;

pub(crate) struct Cx<'a> {
    pub(crate) hir: &'a Hir,
    pub(crate) res: &'a Resolutions,
    pub(crate) types: &'a TypeTables,
    pub(crate) pool: ConstPool,
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
}

impl<'a> Cx<'a> {
    pub(crate) fn new(hir: &'a Hir, res: &'a Resolutions, types: &'a TypeTables) -> Cx<'a> {
        Cx {
            hir,
            res,
            types,
            pool: ConstPool::new(),
            functions: Vec::new(),
            func_of_item: HashMap::new(),
            func_of_method: HashMap::new(),
            struct_of_item: HashMap::new(),
            enum_of_item: HashMap::new(),
            global_of_item: HashMap::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            globals: Vec::new(),
        }
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

                    self.enums.push(EnumShape {
                        name: def.name.clone(),
                        variants: def
                            .variants
                            .iter()
                            .map(|v| Variant {
                                name: v.name.clone(),
                                arity: u8::try_from(v.fields.len()).expect("payload overflow"),
                            })
                            .collect(),
                    });
                }
                Item::TopLet(top_let) => {
                    let ix = GlobalIx(u16::try_from(self.globals.len()).expect("global overflow"));
                    self.global_of_item.insert(item_id, ix);
                    self.globals.push(top_let.let_stmt.name.clone());
                }
                Item::Import(_) | Item::InterfaceDef(_) | Item::Stmt(_) => {}
            }
        }
    }

    pub(crate) fn finish(self) -> Module {
        Module {
            constants: self.pool,
            functions: self
                .functions
                .into_iter()
                .map(|f| f.expect("every reserved function is defined"))
                .collect(),
            structs: self.structs,
            enums: self.enums,
            globals: self.globals,
        }
    }
}
