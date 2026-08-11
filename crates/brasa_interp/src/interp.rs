//! The tree-walking evaluator: expressions, statements, control flow,
//! calls, pattern matching, and the error/panic signal machinery.
//!
//! Execution model (`docs/spec/01-syntax.md`, entry point): top-level
//! statements and top-`let` initializers run in source order, then
//! `main()` if the executed file defines one. Panics and thrown errors
//! are separate signal classes (`docs/spec/04-errors.md`): a `_` catch
//! arm never catches a panic; only an arm naming the exact qualified
//! panic type does.
//!
//! M1 decisions recorded here:
//!
//! - Closures capture by value at creation: the visible locals (and
//!   `self`) are snapshotted into the closure, so rebinding a captured
//!   `let mut` after capture is not observable. Heap values remain
//!   shared through their `Rc`s, so interior mutation stays visible.
//!   Top-level `let`s are items, not locals (`brasa_resolver`), so
//!   closures read them live from module state rather than capturing.
//! - `for` iterates a snapshot of the collection taken at loop entry
//!   (ranges stay lazy), so mutating the collection inside the body
//!   never invalidates the iteration.
//! - Call depth is guarded by a configurable limit; exceeding it raises
//!   a `panics.StackOverflow` panic instead of overflowing the Rust
//!   stack.
//! - The `catch` binding for a panic arm is the panic's detail message
//!   as a `string` (panics carry no user payload in M1).
//! - Stdlib modules other than `std::math` (and every file import) are
//!   not loaded in M1: touching a member is a clean runtime error.

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use brasa_hir::{
    ArmBody, BinaryOp, Block, CatchArm, CatchType, Expr, ExprId, FuncDef, Hir, IfNode, ImportPath,
    Item, ItemId, LambdaBody, Literal, MatchArm, Pattern, PatternId, Stmt, StmtId, UnaryOp,
};
use brasa_resolver::{CtorRes, DefRef, LocalId, Res, Resolutions};
use brasa_typeck::{TypeTables, WrapDecision};

use crate::value::{
    BoundBuiltin, BoundMethod, ClosureValue, EnumValue, StructValue, Value, value_cmp, value_eq,
};

/// Maximum nested `toString` depth, an insurance policy against cyclic
/// heap values during rendering.
const MAX_DISPLAY_DEPTH: usize = 100;

/// The closed panic union of `docs/spec/04-errors.md`. Mirrors the
/// canonical [`brasa_resolver::PANIC_UNION`] list one-to-one (asserted
/// by `panic_kinds_match_the_resolver_union` below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanicKind {
    IndexOutOfBounds,
    DivisionByZero,
    IntegerOverflow,
    AssertionFailed,
    /// Raised when the call-depth guard trips (M1 decision: a
    /// panic-style runtime error, never a Rust stack overflow).
    StackOverflow,
}

impl PanicKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            PanicKind::IndexOutOfBounds => "panics.IndexOutOfBounds",
            PanicKind::DivisionByZero => "panics.DivisionByZero",
            PanicKind::IntegerOverflow => "panics.IntegerOverflow",
            PanicKind::AssertionFailed => "panics.AssertionFailed",
            PanicKind::StackOverflow => "panics.StackOverflow",
        }
    }
}

#[derive(Debug)]
pub(crate) struct PanicValue {
    pub(crate) kind: PanicKind,
    pub(crate) detail: String,
    /// Function names active when the panic was raised, innermost first.
    pub(crate) stack: Vec<String>,
}

/// Non-local control flow. `Error` and `Panic` are the two runtime
/// signal classes of `docs/spec/04-errors.md`; `Fatal` is an
/// interpreter-level failure no program construct can catch (module not
/// available, internal invariant breaks, I/O failure on the output
/// stream). `BrokenPipe` singles out a closed output stream so the CLI
/// can exit silently like standard Unix tools.
#[derive(Debug)]
pub(crate) enum Signal {
    Return(Value),
    Break,
    Continue,
    Error(Value),
    Panic(PanicValue),
    Fatal(String),
    BrokenPipe,
}

pub(crate) type EvalResult<T = Value> = Result<T, Signal>;

/// One call frame: the values of its locals plus `self` inside methods.
/// Every binding site has a unique [`LocalId`], so a flat map per frame
/// handles shadowing for free.
#[derive(Default)]
struct Frame {
    locals: HashMap<LocalId, Value>,
    self_value: Option<Value>,
}

pub(crate) struct Interp<'a> {
    pub(crate) hir: &'a Hir,
    res: &'a Resolutions,
    types: &'a TypeTables,
    /// Values of `TopLet` items, keyed by their `ItemId`.
    globals: HashMap<ItemId, Value>,
    pub(crate) out: &'a mut dyn Write,
    /// Active function names, outermost first.
    stack: Vec<String>,
    max_depth: usize,
}

impl<'a> Interp<'a> {
    pub(crate) fn new(
        hir: &'a Hir,
        res: &'a Resolutions,
        types: &'a TypeTables,
        out: &'a mut dyn Write,
        max_depth: usize,
    ) -> Self {
        Interp {
            hir,
            res,
            types,
            globals: HashMap::new(),
            out,
            stack: Vec::new(),
            max_depth,
        }
    }

    /// Runs the whole program: top-level statements and top-`let`
    /// initializers in source order, then the file's `main()` if defined
    /// (`docs/spec/01-syntax.md`, entry point).
    pub(crate) fn run_program(&mut self, roots: &[ItemId]) -> Result<(), Signal> {
        let mut frame = Frame::default();
        let mut main: Option<(ItemId, &FuncDef)> = None;

        for &item_id in roots {
            match self.hir.item(item_id) {
                Item::Stmt(block) => {
                    for &stmt in block {
                        self.exec_stmt(&mut frame, stmt)?;
                    }
                }
                Item::TopLet(top_let) => {
                    let value = self.eval_expr(&mut frame, top_let.let_stmt.value)?;
                    self.globals.insert(item_id, value);
                }
                Item::FuncDef(func) if func.name == "main" => {
                    main = Some((item_id, func));
                }
                _ => {}
            }
        }

        if let Some((item_id, func)) = main {
            if !func.params.is_empty() {
                return Err(Signal::Fatal(
                    "brasa: `main` must take no parameters".to_string(),
                ));
            }
            self.call_func("main", DefRef::Item(item_id), func, None, vec![])?;
        }

        Ok(())
    }

    pub(crate) fn panic(&self, kind: PanicKind, detail: impl Into<String>) -> Signal {
        let mut stack = self.stack.clone();
        stack.reverse();
        Signal::Panic(PanicValue {
            kind,
            detail: detail.into(),
            stack,
        })
    }

    fn fatal(&self, message: impl Into<String>) -> Signal {
        Signal::Fatal(message.into())
    }

    // --- statements ----------------------------------------------------

    fn exec_stmt(&mut self, frame: &mut Frame, id: StmtId) -> EvalResult<()> {
        match self.hir.stmt(id) {
            Stmt::Let(let_stmt) => {
                let value = self.eval_expr(frame, let_stmt.value)?;
                if let Some(&local) = self.res.stmt_locals.get(&id) {
                    frame.locals.insert(local, value);
                }
                Ok(())
            }
            Stmt::Assign { target, value } => self.exec_assign(frame, *target, *value),
            Stmt::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval_expr(frame, *expr)?,
                    None => Value::Unit,
                };
                Err(Signal::Return(value))
            }
            Stmt::Break => Err(Signal::Break),
            Stmt::Continue => Err(Signal::Continue),
            Stmt::Throw(value) => {
                let value = self.eval_expr(frame, *value)?;
                Err(Signal::Error(value))
            }
            Stmt::If(node) => {
                let node = node.clone();
                self.eval_if_node(frame, &node, false)?;
                Ok(())
            }
            Stmt::While { cond, body } => {
                let (cond, body) = (*cond, body.clone());
                loop {
                    if !self.eval_bool(frame, cond)? {
                        return Ok(());
                    }
                    match self.exec_block(frame, &body, false) {
                        Ok(_) => {}
                        Err(Signal::Break) => return Ok(()),
                        Err(Signal::Continue) => {}
                        Err(signal) => return Err(signal),
                    }
                }
            }
            Stmt::For {
                pattern,
                iterable,
                body,
            } => {
                let (pattern, iterable, body) = (*pattern, *iterable, body.clone());
                self.exec_for(frame, pattern, iterable, &body)
            }
            Stmt::Expr(expr) => {
                self.eval_expr(frame, *expr)?;
                Ok(())
            }
        }
    }

    fn exec_assign(&mut self, frame: &mut Frame, target: ExprId, value: ExprId) -> EvalResult<()> {
        match self.hir.expr(target) {
            Expr::Ident(name) => {
                let name = name.clone();
                let value = self.eval_expr(frame, value)?;
                match self.res.expr_res.get(&target).copied() {
                    Some(Res::Local(local)) => {
                        frame.locals.insert(local, value);
                        Ok(())
                    }
                    Some(Res::Item(item)) => {
                        self.globals.insert(item, value);
                        Ok(())
                    }
                    _ => Err(self.fatal(format!("brasa: cannot assign to `{name}`"))),
                }
            }
            Expr::Field { recv, name } => {
                let (recv, name) = (*recv, name.clone());
                let recv = self.eval_expr(frame, recv)?;
                let value = self.eval_expr(frame, value)?;
                self.set_field(&recv, &name, value)
            }
            Expr::Index { recv, index } => {
                let (recv, index) = (*recv, *index);
                let recv = self.eval_expr(frame, recv)?;
                let index = self.eval_expr(frame, index)?;
                let value = self.eval_expr(frame, value)?;
                self.set_index(&recv, index, value)
            }
            _ => Err(self.fatal("brasa: invalid assignment target")),
        }
    }

    fn set_field(&mut self, recv: &Value, name: &str, value: Value) -> EvalResult<()> {
        let Value::Struct(s) = recv else {
            return Err(self.fatal(format!("brasa: cannot assign to field `{name}`")));
        };
        let Some(index) = self.struct_field_index(s.item, name) else {
            return Err(self.fatal(format!("brasa: unknown field `{name}`")));
        };
        s.fields.borrow_mut()[index] = value;
        Ok(())
    }

    fn set_index(&mut self, recv: &Value, index: Value, value: Value) -> EvalResult<()> {
        match recv {
            Value::Vector(items) => {
                let Value::Int(i) = index else {
                    return Err(self.fatal("brasa: vector index must be an int"));
                };
                let mut items = items.borrow_mut();
                let len = items.len();
                if i < 0 || i as usize >= len {
                    return Err(self.panic(
                        PanicKind::IndexOutOfBounds,
                        format!("index {i} out of range (len {len})"),
                    ));
                }
                items[i as usize] = value;
                Ok(())
            }
            Value::Map(entries) => {
                let mut entries = entries.borrow_mut();
                match entries.iter_mut().find(|(k, _)| value_eq(k, &index)) {
                    Some(entry) => entry.1 = value,
                    None => entries.push((index, value)),
                }
                Ok(())
            }
            _ => Err(self.fatal("brasa: value does not support index assignment")),
        }
    }

    fn exec_for(
        &mut self,
        frame: &mut Frame,
        pattern: PatternId,
        iterable: ExprId,
        body: &Block,
    ) -> EvalResult<()> {
        let iterable = self.eval_expr(frame, iterable)?;

        match iterable {
            Value::Range { lo, hi, inclusive } => {
                let mut i = lo;
                loop {
                    let in_range = if inclusive { i <= hi } else { i < hi };
                    if !in_range {
                        return Ok(());
                    }
                    match self.run_iteration(frame, pattern, Value::Int(i), body)? {
                        LoopFlow::Continue => {}
                        LoopFlow::Break => return Ok(()),
                    }
                    match i.checked_add(1) {
                        Some(next) => i = next,
                        None => return Ok(()),
                    }
                }
            }
            other => {
                let items = self.snapshot_iterable(&other)?;
                for item in items {
                    match self.run_iteration(frame, pattern, item, body)? {
                        LoopFlow::Continue => {}
                        LoopFlow::Break => return Ok(()),
                    }
                }
                Ok(())
            }
        }
    }

    /// Snapshots a non-range iterable at loop entry (M1 decision, module
    /// docs): element clones share heap values, so this only pins the
    /// sequence, not the contents.
    fn snapshot_iterable(&mut self, value: &Value) -> EvalResult<Vec<Value>> {
        match value {
            Value::Vector(items) => Ok(items.borrow().clone()),
            Value::Map(entries) => Ok(entries
                .borrow()
                .iter()
                .map(|(k, v)| Value::Tuple(Rc::from(vec![k.clone(), v.clone()])))
                .collect()),
            Value::Set(items) => Ok(items.borrow().clone()),
            Value::Str(s) => Ok(s.chars().map(Value::Char).collect()),
            _ => {
                Err(self
                    .fatal("brasa: `for` iterates `Vector`, `Map`, `Set`, ranges, and `string`"))
            }
        }
    }

    fn run_iteration(
        &mut self,
        frame: &mut Frame,
        pattern: PatternId,
        item: Value,
        body: &Block,
    ) -> EvalResult<LoopFlow> {
        if !self.match_pattern(frame, pattern, &item)? {
            return Err(self.panic(
                PanicKind::AssertionFailed,
                "`for` pattern did not match the element",
            ));
        }
        match self.exec_block(frame, body, false) {
            Ok(_) => Ok(LoopFlow::Continue),
            Err(Signal::Break) => Ok(LoopFlow::Break),
            Err(Signal::Continue) => Ok(LoopFlow::Continue),
            Err(signal) => Err(signal),
        }
    }

    /// Executes a block, yielding its value: the trailing expression
    /// statement, or a trailing `if` when the block is consumed as a
    /// value — mirroring the checker's block typing exactly.
    fn exec_block(&mut self, frame: &mut Frame, block: &Block, used: bool) -> EvalResult {
        let Some((&last, init)) = block.split_last() else {
            return Ok(Value::Unit);
        };

        for &stmt in init {
            self.exec_stmt(frame, stmt)?;
        }

        match self.hir.stmt(last) {
            Stmt::Expr(value) => self.eval_expr(frame, *value),
            Stmt::If(node) if used => {
                let node = node.clone();
                self.eval_if_node(frame, &node, true)
            }
            _ => {
                self.exec_stmt(frame, last)?;
                Ok(Value::Unit)
            }
        }
    }

    // --- expressions ---------------------------------------------------

    fn eval_expr(&mut self, frame: &mut Frame, id: ExprId) -> EvalResult {
        match self.hir.expr(id) {
            Expr::Int(v) => Ok(Value::Int(*v)),
            Expr::Float(v) => Ok(Value::Float(*v)),
            Expr::Bool(v) => Ok(Value::Bool(*v)),
            Expr::Char(v) => Ok(Value::Char(*v)),
            Expr::Unit => Ok(Value::Unit),
            Expr::Str(s) => Ok(Value::str(s)),
            Expr::Ident(name) => {
                let name = name.clone();
                self.eval_ident(frame, id, &name)
            }
            Expr::SelfExpr => frame
                .self_value
                .clone()
                .ok_or_else(|| self.fatal("brasa: `self` outside a method")),
            Expr::Call { callee, args } => {
                let (callee, args) = (*callee, args.clone());
                self.eval_call(frame, callee, &args)
            }
            Expr::Field { recv, name } => {
                let (recv, name) = (*recv, name.clone());
                self.eval_field(frame, recv, &name)
            }
            Expr::Index { recv, index } => {
                let (recv, index) = (*recv, *index);
                let recv = self.eval_expr(frame, recv)?;
                let index = self.eval_expr(frame, index)?;
                self.eval_index(&recv, &index)
            }
            Expr::Unary { op, operand } => {
                let (op, operand) = (*op, *operand);
                let operand = self.eval_expr(frame, operand)?;
                self.eval_unary(op, operand)
            }
            Expr::Binary { op, lhs, rhs } => {
                let (op, lhs, rhs) = (*op, *lhs, *rhs);
                self.eval_binary(frame, op, lhs, rhs)
            }
            Expr::OptionWrap(inner) => {
                let inner = *inner;
                let value = self.eval_expr(frame, inner)?;
                // The checker's flatten decision for `?.`
                // (`docs/spec/03-types.md`): `Wrap` puts the member value
                // in `Some`, `NoOp` passes an already-Option through.
                // Nodes the checker deferred fall back to a dynamic
                // check.
                match self.types.wrap_decisions.get(&id) {
                    Some(WrapDecision::Wrap) => Ok(Value::some(value)),
                    Some(WrapDecision::NoOp) => Ok(value),
                    None => match value {
                        Value::Option(_) => Ok(value),
                        other => Ok(Value::some(other)),
                    },
                }
            }
            Expr::ToString(inner) => {
                let inner = *inner;
                let value = self.eval_expr(frame, inner)?;
                let text = self.display(&value)?;
                Ok(Value::str(text))
            }
            Expr::Lambda { .. } => {
                let captured = frame.locals.clone();
                Ok(Value::Closure(Rc::new(ClosureValue {
                    lambda: id,
                    captured,
                    self_value: frame.self_value.clone(),
                })))
            }
            Expr::If(node) => {
                let node = node.clone();
                self.eval_if_node(frame, &node, true)
            }
            Expr::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, arms.clone());
                let value = self.eval_expr(frame, scrutinee)?;
                self.eval_match(frame, &value, &arms)
            }
            Expr::VectorLit(elements) => {
                let elements = elements.clone();
                let mut items = Vec::with_capacity(elements.len());
                for element in elements {
                    items.push(self.eval_expr(frame, element)?);
                }
                Ok(Value::vector(items))
            }
            Expr::MapLit(pairs) => {
                let pairs = pairs.clone();
                let mut entries: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let key = self.eval_expr(frame, key)?;
                    let value = self.eval_expr(frame, value)?;
                    match entries.iter_mut().find(|(k, _)| value_eq(k, &key)) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(Value::Map(Rc::new(std::cell::RefCell::new(entries))))
            }
            Expr::StructLit { fields, .. } => {
                let fields = fields.clone();
                self.eval_struct_lit(frame, id, &fields)
            }
            Expr::Range { lo, hi, inclusive } => {
                let (lo, hi, inclusive) = (*lo, *hi, *inclusive);
                let lo = self.eval_int(frame, lo)?;
                let hi = self.eval_int(frame, hi)?;
                Ok(Value::Range { lo, hi, inclusive })
            }
            Expr::Catch {
                subject,
                binding: _,
                exhaustive: _,
                arms,
            } => {
                let (subject, arms) = (*subject, arms.clone());
                self.eval_catch(frame, id, subject, &arms)
            }
            Expr::EnumCtor { args, name } => {
                let (args, name) = (args.clone(), name.clone());
                self.eval_enum_ctor(frame, id, &name, &args)
            }
        }
    }

    fn eval_ident(&mut self, frame: &mut Frame, id: ExprId, name: &str) -> EvalResult {
        match self.res.expr_res.get(&id).copied() {
            Some(Res::Local(local)) => {
                frame.locals.get(&local).cloned().ok_or_else(|| {
                    self.fatal(format!("brasa: `{name}` used before initialization"))
                })
            }
            Some(Res::Item(item)) => match self.hir.item(item) {
                Item::FuncDef(_) => Ok(Value::Func(item)),
                Item::TopLet(_) => self.globals.get(&item).cloned().ok_or_else(|| {
                    self.fatal(format!("brasa: `{name}` used before initialization"))
                }),
                _ => Err(self.fatal(format!("brasa: `{name}` is not a value"))),
            },
            Some(Res::SelfParam) => frame
                .self_value
                .clone()
                .ok_or_else(|| self.fatal("brasa: `self` outside a method")),
            Some(Res::Module(_)) => Err(self.fatal(format!(
                "brasa: module `{name}` is not a value; access members as `{name}.member`"
            ))),
            Some(Res::Builtin(_)) => {
                Err(self.fatal(format!("brasa: `{name}` cannot be used as a value in M1")))
            }
            None => Err(self.fatal(format!("brasa: unresolved name `{name}`"))),
        }
    }

    fn eval_bool(&mut self, frame: &mut Frame, id: ExprId) -> EvalResult<bool> {
        match self.eval_expr(frame, id)? {
            Value::Bool(b) => Ok(b),
            _ => Err(self.fatal("brasa: condition is not a bool")),
        }
    }

    fn eval_int(&mut self, frame: &mut Frame, id: ExprId) -> EvalResult<i64> {
        match self.eval_expr(frame, id)? {
            Value::Int(i) => Ok(i),
            _ => Err(self.fatal("brasa: expected an int")),
        }
    }

    fn eval_if_node(&mut self, frame: &mut Frame, node: &IfNode, used: bool) -> EvalResult {
        for (cond, body) in &node.branches {
            if self.eval_bool(frame, *cond)? {
                return self.exec_block(frame, body, used);
            }
        }
        match &node.else_ {
            Some(body) => self.exec_block(frame, body, used),
            None => Ok(Value::Unit),
        }
    }

    fn eval_struct_lit(
        &mut self,
        frame: &mut Frame,
        id: ExprId,
        fields: &[(String, ExprId)],
    ) -> EvalResult {
        let Some(brasa_resolver::TypeRes::Item(item)) = self.res.struct_lit_res.get(&id).copied()
        else {
            return Err(self.fatal("brasa: unresolved struct literal"));
        };
        let Item::StructDef(def) = self.hir.item(item) else {
            return Err(self.fatal("brasa: struct literal of a non-struct type"));
        };
        let decl_order: Vec<String> = def.fields.iter().map(|f| f.name.clone()).collect();

        // Initializers evaluate in written order; the values then land
        // in declaration order so field indices stay stable.
        let mut by_name: Vec<(String, Value)> = Vec::with_capacity(fields.len());
        for (name, expr) in fields {
            let value = self.eval_expr(frame, *expr)?;
            by_name.push((name.clone(), value));
        }

        let mut ordered = Vec::with_capacity(decl_order.len());
        for name in &decl_order {
            match by_name.iter().find(|(n, _)| n == name) {
                Some((_, value)) => ordered.push(value.clone()),
                None => return Err(self.fatal(format!("brasa: missing field `{name}`"))),
            }
        }

        Ok(Value::Struct(Rc::new(StructValue {
            item,
            fields: std::cell::RefCell::new(ordered),
        })))
    }

    fn eval_enum_ctor(
        &mut self,
        frame: &mut Frame,
        id: ExprId,
        name: &str,
        args: &[ExprId],
    ) -> EvalResult {
        let mut values = Vec::with_capacity(args.len());
        for &arg in args {
            values.push(self.eval_expr(frame, arg)?);
        }

        match self.res.ctor_expr_res.get(&id).copied() {
            Some(CtorRes::OptionSome) => match values.pop() {
                Some(inner) if values.is_empty() => Ok(Value::some(inner)),
                _ => Err(self.fatal("brasa: `Some` takes exactly 1 argument")),
            },
            Some(CtorRes::OptionNone) => Ok(Value::NONE),
            // `Set(v)` is a set of the vector's contents: elements
            // deduplicated by structural equality, first occurrence
            // kept, insertion order preserved.
            Some(CtorRes::SetCtor) => match values.pop() {
                Some(Value::Vector(items)) if values.is_empty() => {
                    let items = items.borrow();
                    let mut set: Vec<Value> = Vec::new();
                    for item in items.iter() {
                        if !set.iter().any(|existing| value_eq(existing, item)) {
                            set.push(item.clone());
                        }
                    }
                    Ok(Value::Set(Rc::new(std::cell::RefCell::new(set))))
                }
                _ => Err(self.fatal("brasa: `Set` takes exactly 1 Vector argument")),
            },
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => Ok(Value::Enum(Rc::new(EnumValue {
                item: enum_item,
                variant: variant_index,
                fields: values,
            }))),
            None => Err(self.fatal(format!("brasa: unresolved constructor `{name}`"))),
        }
    }

    // --- operators -----------------------------------------------------

    fn eval_index(&mut self, recv: &Value, index: &Value) -> EvalResult {
        match (recv, index) {
            // Out of range on a Vector is a panic — the common case is a
            // bug (`docs/spec/03-types.md`, operator table).
            (Value::Vector(items), Value::Int(i)) => {
                let items = items.borrow();
                let len = items.len();
                if *i < 0 || *i as usize >= len {
                    return Err(self.panic(
                        PanicKind::IndexOutOfBounds,
                        format!("index {i} out of range (len {len})"),
                    ));
                }
                Ok(items[*i as usize].clone())
            }
            // A missing Map key is a normal case: `Option` (same table).
            (Value::Map(entries), key) => Ok(entries
                .borrow()
                .iter()
                .find(|(k, _)| value_eq(k, key))
                .map(|(_, v)| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            _ => Err(self.fatal("brasa: value does not support indexing")),
        }
    }

    fn eval_unary(&mut self, op: UnaryOp, operand: Value) -> EvalResult {
        match (op, operand) {
            (UnaryOp::Neg, Value::Int(v)) => v.checked_neg().map(Value::Int).ok_or_else(|| {
                self.panic(PanicKind::IntegerOverflow, "integer overflow in unary `-`")
            }),
            (UnaryOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
            (UnaryOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
            _ => Err(self.fatal("brasa: invalid operand for unary operator")),
        }
    }

    fn eval_binary(
        &mut self,
        frame: &mut Frame,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> EvalResult {
        // `&&`/`||` short-circuit (`docs/spec/03-types.md`).
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let lhs = self.eval_bool(frame, lhs)?;
            return match (op, lhs) {
                (BinaryOp::And, false) => Ok(Value::Bool(false)),
                (BinaryOp::Or, true) => Ok(Value::Bool(true)),
                _ => Ok(Value::Bool(self.eval_bool(frame, rhs)?)),
            };
        }

        let lhs = self.eval_expr(frame, lhs)?;
        let rhs = self.eval_expr(frame, rhs)?;

        match op {
            BinaryOp::Eq => Ok(Value::Bool(value_eq(&lhs, &rhs))),
            BinaryOp::NotEq => Ok(Value::Bool(!value_eq(&lhs, &rhs))),
            BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
                self.eval_ordering(op, &lhs, &rhs)
            }
            _ => self.eval_arithmetic(op, lhs, rhs),
        }
    }

    fn eval_ordering(&mut self, op: BinaryOp, lhs: &Value, rhs: &Value) -> EvalResult {
        let ordering = match value_cmp(lhs, rhs) {
            Some(ordering) => ordering,
            None => match (lhs, rhs) {
                // IEEE: comparisons involving NaN are all false
                // (`docs/spec/03-types.md`, float rules).
                (Value::Float(_), Value::Float(_)) => return Ok(Value::Bool(false)),
                // `T: Comparable` orders through the user's `cmp`
                // (`docs/spec/03-types.md`, structural interfaces).
                (Value::Struct(_), Value::Struct(_)) => {
                    let cmp = self.call_method_by_name(lhs.clone(), "cmp", vec![rhs.clone()])?;
                    match cmp {
                        Value::Int(v) => v.cmp(&0),
                        _ => return Err(self.fatal("brasa: `cmp` must return an int")),
                    }
                }
                _ => return Err(self.fatal("brasa: operands are not comparable")),
            },
        };

        let result = match op {
            BinaryOp::Lt => ordering.is_lt(),
            BinaryOp::LtEq => ordering.is_le(),
            BinaryOp::Gt => ordering.is_gt(),
            BinaryOp::GtEq => ordering.is_ge(),
            _ => unreachable!("only ordering operators reach here"),
        };
        Ok(Value::Bool(result))
    }

    /// Checked int arithmetic: overflow and division/remainder by zero
    /// are panics (`docs/spec/03-types.md`: bug = panic, no silent
    /// wrapping). Floats are IEEE: `1.0 / 0.0` is `inf`, never a panic.
    fn eval_arithmetic(&mut self, op: BinaryOp, lhs: Value, rhs: Value) -> EvalResult {
        match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => self.int_arithmetic(op, a, b),
            (Value::Float(a), Value::Float(b)) => {
                let result = match op {
                    BinaryOp::Add => a + b,
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => a / b,
                    BinaryOp::Rem => a % b,
                    BinaryOp::Pow => a.powf(b),
                    _ => return Err(self.fatal("brasa: invalid float operator")),
                };
                Ok(Value::Float(result))
            }
            (Value::Str(a), Value::Str(b)) if op == BinaryOp::Add => {
                Ok(Value::str(format!("{a}{b}")))
            }
            _ => Err(self.fatal("brasa: invalid operands for arithmetic operator")),
        }
    }

    fn int_arithmetic(&mut self, op: BinaryOp, a: i64, b: i64) -> EvalResult {
        let overflow = |this: &Self, op: &str| {
            this.panic(
                PanicKind::IntegerOverflow,
                format!("integer overflow in `{op}`"),
            )
        };

        let result = match op {
            BinaryOp::Add => a.checked_add(b).ok_or_else(|| overflow(self, "+"))?,
            BinaryOp::Sub => a.checked_sub(b).ok_or_else(|| overflow(self, "-"))?,
            BinaryOp::Mul => a.checked_mul(b).ok_or_else(|| overflow(self, "*"))?,
            BinaryOp::Div => {
                if b == 0 {
                    return Err(self.panic(PanicKind::DivisionByZero, "division by zero"));
                }
                a.checked_div(b).ok_or_else(|| overflow(self, "/"))?
            }
            BinaryOp::Rem => {
                if b == 0 {
                    return Err(self.panic(PanicKind::DivisionByZero, "remainder by zero"));
                }
                a.checked_rem(b).ok_or_else(|| overflow(self, "%"))?
            }
            BinaryOp::Pow => {
                // M1 decision: a negative int exponent has no int result,
                // so it fails the same assertion class as other misuse.
                if b < 0 {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        "negative exponent in integer `**`",
                    ));
                }
                let exp = u32::try_from(b).map_err(|_| overflow(self, "**"))?;
                a.checked_pow(exp).ok_or_else(|| overflow(self, "**"))?
            }
            _ => return Err(self.fatal("brasa: invalid int operator")),
        };
        Ok(Value::Int(result))
    }

    // --- match and catch -----------------------------------------------

    fn eval_match(&mut self, frame: &mut Frame, value: &Value, arms: &[MatchArm]) -> EvalResult {
        for arm in arms {
            if !self.match_pattern(frame, arm.pattern, value)? {
                continue;
            }
            if let Some(guard) = arm.guard
                && !self.eval_bool(frame, guard)?
            {
                continue;
            }
            return self.eval_arm_body(frame, &arm.body);
        }
        // The checker proves exhaustiveness; guards can still leave a
        // value unmatched at runtime.
        Err(self.panic(PanicKind::AssertionFailed, "no match arm matched the value"))
    }

    fn eval_arm_body(&mut self, frame: &mut Frame, body: &ArmBody) -> EvalResult {
        match body {
            ArmBody::Expr(expr) => self.eval_expr(frame, *expr),
            ArmBody::Block(block) => self.exec_block(frame, block, true),
        }
    }

    fn match_pattern(
        &mut self,
        frame: &mut Frame,
        pattern: PatternId,
        value: &Value,
    ) -> EvalResult<bool> {
        match self.hir.pattern(pattern) {
            Pattern::Wildcard => Ok(true),
            Pattern::Literal(literal) => {
                let literal = match literal {
                    Literal::Int(v) => Value::Int(*v),
                    Literal::Float(v) => Value::Float(*v),
                    Literal::Bool(v) => Value::Bool(*v),
                    Literal::Char(v) => Value::Char(*v),
                    Literal::Str(v) => Value::str(v),
                };
                Ok(value_eq(&literal, value))
            }
            Pattern::Binding(_) => {
                if let Some(&local) = self.res.pattern_locals.get(&pattern) {
                    frame.locals.insert(local, value.clone());
                }
                Ok(true)
            }
            Pattern::Ctor { args, .. } => {
                let args = args.clone();
                self.match_ctor_pattern(frame, pattern, &args, value)
            }
            Pattern::Tuple(elements) => {
                let elements = elements.clone();
                let Value::Tuple(values) = value else {
                    return Ok(false);
                };
                if values.len() != elements.len() {
                    return Ok(false);
                }
                let values = values.clone();
                for (element, item) in elements.iter().zip(values.iter()) {
                    if !self.match_pattern(frame, *element, item)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    fn match_ctor_pattern(
        &mut self,
        frame: &mut Frame,
        pattern: PatternId,
        args: &[PatternId],
        value: &Value,
    ) -> EvalResult<bool> {
        match self.res.ctor_pattern_res.get(&pattern).copied() {
            Some(CtorRes::OptionSome) => match value {
                Value::Option(Some(inner)) => {
                    let inner = (**inner).clone();
                    match args {
                        [arg] => self.match_pattern(frame, *arg, &inner),
                        [] => Ok(true),
                        _ => Ok(false),
                    }
                }
                _ => Ok(false),
            },
            Some(CtorRes::OptionNone) => Ok(matches!(value, Value::Option(None))),
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => {
                let Value::Enum(e) = value else {
                    return Ok(false);
                };
                if e.item != enum_item || e.variant != variant_index {
                    return Ok(false);
                }
                let fields = e.fields.clone();
                if !args.is_empty() && args.len() != fields.len() {
                    return Ok(false);
                }
                for (arg, field) in args.iter().zip(fields.iter()) {
                    if !self.match_pattern(frame, *arg, field)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // `Set` never resolves in pattern position, so a resolved
            // `SetCtor` here would be a resolver bug.
            None | Some(CtorRes::SetCtor) => {
                Err(self.fatal("brasa: unresolved constructor pattern"))
            }
        }
    }

    /// `catch` semantics (`docs/spec/04-errors.md`): arm matching is
    /// nominal by type name; `_` catches any error — including a native
    /// error, which IS an error — but never a panic; a panic or a
    /// native error is caught by an arm naming its exact qualified name
    /// (`panics.IndexOutOfBounds`, `string.ParseError`); an unmatched
    /// signal propagates automatically. Exact name equality is enough
    /// for every case: user type names never contain `.`, so a dotted
    /// arm can only match a dotted (panic or native) tag.
    ///
    /// The binding mirrors the checker's per-arm narrowing
    /// (`brasa_typeck`, `catch_arm_binding_type`): a named arm catching
    /// a native error binds the message `string` — like a panic arm
    /// binds the detail — while `_` binds the error value itself.
    fn eval_catch(
        &mut self,
        frame: &mut Frame,
        id: ExprId,
        subject: ExprId,
        arms: &[CatchArm],
    ) -> EvalResult {
        let signal = match self.eval_expr(frame, subject) {
            Ok(value) => return Ok(value),
            Err(signal @ (Signal::Error(_) | Signal::Panic(_))) => signal,
            Err(other) => return Err(other),
        };

        let (bound, tag): (Value, String) = match &signal {
            Signal::Error(value) => (value.clone(), self.nominal_tag(value)),
            Signal::Panic(panic) => (Value::str(&panic.detail), panic.kind.name().to_string()),
            _ => unreachable!("only error and panic signals reach here"),
        };
        let is_panic = matches!(signal, Signal::Panic(_));

        for arm in arms {
            let matched = arm.types.iter().find(|ty| match ty {
                CatchType::Wildcard { .. } => !is_panic,
                CatchType::Named { name, .. } => name == &tag,
            });
            let Some(matched) = matched else {
                continue;
            };

            let bound = match (&bound, matched) {
                (Value::NativeError { message, .. }, CatchType::Named { .. }) => {
                    Value::str(message.as_ref())
                }
                _ => bound.clone(),
            };
            if let Some(&local) = self.res.catch_bindings.get(&id) {
                frame.locals.insert(local, bound);
            }
            if let Some(guard) = arm.guard
                && !self.eval_bool(frame, guard)?
            {
                continue;
            }
            return self.eval_arm_body(frame, &arm.body);
        }

        Err(signal)
    }

    /// The nominal type tag `catch` matches against
    /// (`docs/spec/04-errors.md`): the declared name for user structs
    /// and enums, the type name for everything else.
    pub(crate) fn nominal_tag(&self, value: &Value) -> String {
        match value {
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Char(_) => "char".to_string(),
            Value::Unit => "unit".to_string(),
            Value::Str(_) => "string".to_string(),
            Value::Range { .. } => "Range".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
            Value::Vector(_) => "Vector".to_string(),
            Value::Map(_) => "Map".to_string(),
            Value::Set(_) => "Set".to_string(),
            Value::Option(_) => "Option".to_string(),
            Value::Struct(s) => self.item_name(s.item),
            Value::Enum(e) => self.item_name(e.item),
            Value::NativeError { name, .. } => name.to_string(),
            Value::Func(_) | Value::Closure(_) | Value::BoundMethod(_) | Value::BoundBuiltin(_) => {
                "function".to_string()
            }
        }
    }

    // --- calls ---------------------------------------------------------

    fn eval_call(&mut self, frame: &mut Frame, callee: ExprId, args: &[ExprId]) -> EvalResult {
        // `puts`/`print`: universal toString plus a newline for `puts`
        // (`docs/spec/05-stdlib.md`).
        if let Expr::Ident(_) = self.hir.expr(callee)
            && let Some(&Res::Builtin(builtin)) = self.res.expr_res.get(&callee)
        {
            let [arg] = args else {
                return Err(self.fatal("brasa: `puts`/`print` take exactly 1 argument"));
            };
            let value = self.eval_expr(frame, *arg)?;
            let text = self.display(&value)?;
            let result = match builtin {
                brasa_resolver::BuiltinValue::Puts => writeln!(self.out, "{text}"),
                brasa_resolver::BuiltinValue::Print => write!(self.out, "{text}"),
            };
            return match result {
                Ok(()) => Ok(Value::Unit),
                // A closed read end (`brasa ... | head`) is not a
                // program failure: standard Unix tools exit silently.
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Err(Signal::BrokenPipe),
                Err(err) => Err(self.fatal(format!("brasa: failed to write output: {err}"))),
            };
        }

        if let Expr::Field { recv, name } = self.hir.expr(callee) {
            let (recv, name) = (*recv, name.clone());

            if let Expr::Ident(_) = self.hir.expr(recv)
                && let Some(&Res::Module(item)) = self.res.expr_res.get(&recv)
            {
                let mut values = Vec::with_capacity(args.len());
                for &arg in args {
                    values.push(self.eval_expr(frame, arg)?);
                }
                return self.module_call(item, &name, values);
            }

            let recv = self.eval_expr(frame, recv)?;
            let mut values = Vec::with_capacity(args.len());
            for &arg in args {
                values.push(self.eval_expr(frame, arg)?);
            }
            return self.call_method_by_name(recv, &name, values);
        }

        if let Expr::Ident(_) = self.hir.expr(callee)
            && let Some(&Res::Item(item)) = self.res.expr_res.get(&callee)
            && let Item::FuncDef(func) = self.hir.item(item)
        {
            let name = func.name.clone();
            let mut values = Vec::with_capacity(args.len());
            for &arg in args {
                values.push(self.eval_expr(frame, arg)?);
            }
            return self.call_func(&name, DefRef::Item(item), func, None, values);
        }

        let callee = self.eval_expr(frame, callee)?;
        let mut values = Vec::with_capacity(args.len());
        for &arg in args {
            values.push(self.eval_expr(frame, arg)?);
        }
        self.call_value(callee, values)
    }

    pub(crate) fn call_value(&mut self, callee: Value, args: Vec<Value>) -> EvalResult {
        match callee {
            Value::Func(item) => match self.hir.item(item) {
                Item::FuncDef(func) => {
                    let name = func.name.clone();
                    self.call_func(&name, DefRef::Item(item), func, None, args)
                }
                _ => Err(self.fatal("brasa: value is not callable")),
            },
            Value::Closure(closure) => self.call_closure(&closure, args),
            Value::BoundMethod(bound) => {
                self.call_struct_method(bound.owner, bound.index, bound.recv.clone(), args)
            }
            Value::BoundBuiltin(bound) => self.call_builtin(bound.recv.clone(), &bound.name, args),
            _ => Err(self.fatal("brasa: value is not callable")),
        }
    }

    /// Dispatches `recv.name(args)` on an evaluated receiver: struct
    /// fields holding callables, declared struct methods, the builtin
    /// method table, and the universal derived `toString`
    /// (`docs/spec/03-types.md`).
    pub(crate) fn call_method_by_name(
        &mut self,
        recv: Value,
        name: &str,
        args: Vec<Value>,
    ) -> EvalResult {
        if let Value::Struct(s) = &recv {
            let item = s.item;
            if let Item::StructDef(def) = self.hir.item(item) {
                if let Some(index) = def.methods.iter().position(|m| m.name == name) {
                    return self.call_struct_method(item, index, recv.clone(), args);
                }
                if let Some(index) = self.struct_field_index(item, name) {
                    let field = s.fields.borrow()[index].clone();
                    return self.call_value(field, args);
                }
            }
            if name == "toString" {
                let text = self.display(&recv)?;
                return Ok(Value::str(text));
            }
            return Err(self.fatal(format!("brasa: unknown member `{name}`")));
        }

        self.call_builtin(recv, name, args)
    }

    fn call_struct_method(
        &mut self,
        owner: ItemId,
        index: usize,
        recv: Value,
        args: Vec<Value>,
    ) -> EvalResult {
        let Item::StructDef(def) = self.hir.item(owner) else {
            return Err(self.fatal("brasa: method owner is not a struct"));
        };
        let Some(func) = def.methods.get(index) else {
            return Err(self.fatal("brasa: unknown method"));
        };
        let name = func.name.clone();
        self.call_func(
            &name,
            DefRef::Method { owner, index },
            func,
            Some(recv),
            args,
        )
    }

    fn call_closure(&mut self, closure: &ClosureValue, args: Vec<Value>) -> EvalResult {
        let Expr::Lambda { body, .. } = self.hir.expr(closure.lambda) else {
            return Err(self.fatal("brasa: closure does not reference a lambda"));
        };
        let body = body.clone();

        let Some(params) = self.res.lambda_params.get(&closure.lambda) else {
            return Err(self.fatal("brasa: unresolved lambda parameters"));
        };
        if params.len() != args.len() {
            return Err(self.fatal(format!(
                "brasa: lambda takes {} argument(s), found {}",
                params.len(),
                args.len()
            )));
        }
        let params = params.clone();

        if self.stack.len() >= self.max_depth {
            return Err(self.recursion_limit());
        }

        let mut frame = Frame {
            locals: closure.captured.clone(),
            self_value: closure.self_value.clone(),
        };
        for (local, arg) in params.into_iter().zip(args) {
            frame.locals.insert(local, arg);
        }

        self.stack.push("<lambda>".to_string());
        let result = match &body {
            LambdaBody::Expr(expr) => self.eval_expr(&mut frame, *expr),
            LambdaBody::Block(block) => self.exec_block(&mut frame, block, true),
        };
        self.stack.pop();

        match result {
            Err(Signal::Return(value)) => Ok(value),
            other => other,
        }
    }

    fn call_func(
        &mut self,
        name: &str,
        def_ref: DefRef,
        func: &FuncDef,
        self_value: Option<Value>,
        args: Vec<Value>,
    ) -> EvalResult {
        if self.stack.len() >= self.max_depth {
            return Err(self.recursion_limit());
        }

        let Some(params) = self.res.func_params.get(&def_ref) else {
            return Err(self.fatal(format!("brasa: unresolved parameters of `{name}`")));
        };
        let named: Vec<LocalId> = params.iter().filter_map(|slot| *slot).collect();
        if named.len() != args.len() {
            return Err(self.fatal(format!(
                "brasa: `{name}` takes {} argument(s), found {}",
                named.len(),
                args.len()
            )));
        }

        let mut frame = Frame {
            locals: HashMap::new(),
            self_value,
        };
        for (local, arg) in named.into_iter().zip(args) {
            frame.locals.insert(local, arg);
        }

        let body = func.body.clone();
        let returns_value = func.ret.is_some();

        self.stack.push(name.to_string());
        let result = self.exec_block(&mut frame, &body, returns_value);
        self.stack.pop();

        let value = match result {
            Ok(value) => value,
            Err(Signal::Return(value)) => value,
            Err(signal) => return Err(signal),
        };
        // A function without a declared return type returns `unit`
        // (`docs/spec/03-types.md`, inference boundary).
        if returns_value {
            Ok(value)
        } else {
            Ok(Value::Unit)
        }
    }

    fn recursion_limit(&self) -> Signal {
        self.panic(
            PanicKind::StackOverflow,
            format!("recursion limit ({} frames) exceeded", self.max_depth),
        )
    }

    // --- modules -------------------------------------------------------

    /// Member calls on module handles. Only `std::math` executes in M1;
    /// every other module reports a clean runtime error (module loading
    /// lands after M1, stdlib module signatures close in M4).
    fn module_call(&mut self, item: ItemId, name: &str, args: Vec<Value>) -> EvalResult {
        let Item::Import(import) = self.hir.item(item) else {
            return Err(self.fatal("brasa: module handle is not an import"));
        };

        let module = match &import.path {
            ImportPath::Std(segments) => segments.last().cloned().unwrap_or_default(),
            ImportPath::File(path) => std::path::Path::new(path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone()),
        };

        if matches!(&import.path, ImportPath::Std(_)) && module == "math" {
            return self.math_call(name, args);
        }

        Err(self.fatal(format!(
            "brasa: module `{module}` is not available yet (module loading lands after M1)"
        )))
    }

    /// The `std::math` slice needed by M1 programs
    /// (`docs/spec/05-stdlib.md`): f64 semantics throughout; `abs`,
    /// `min`, and `max` also work on ints since the runtime value
    /// dispatches them trivially.
    fn math_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("sqrt", [Value::Float(v)]) => Ok(Value::Float(v.sqrt())),
            ("floor", [Value::Float(v)]) => Ok(Value::Float(v.floor())),
            ("ceil", [Value::Float(v)]) => Ok(Value::Float(v.ceil())),
            ("round", [Value::Float(v)]) => Ok(Value::Float(v.round())),
            ("pow", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.powf(*b))),
            ("abs", [Value::Float(v)]) => Ok(Value::Float(v.abs())),
            ("abs", [Value::Int(v)]) => v.checked_abs().map(Value::Int).ok_or_else(|| {
                self.panic(PanicKind::IntegerOverflow, "integer overflow in `math.abs`")
            }),
            ("min", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).min(*b))),
            ("max", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).max(*b))),
            ("min", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.min(*b))),
            ("max", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.max(*b))),
            ("sqrt" | "floor" | "ceil" | "round" | "pow" | "abs" | "min" | "max", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `math.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `math`"))),
        }
    }

    // --- member access -------------------------------------------------

    fn eval_field(&mut self, frame: &mut Frame, recv: ExprId, name: &str) -> EvalResult {
        if let Expr::Ident(_) = self.hir.expr(recv)
            && let Some(&Res::Module(item)) = self.res.expr_res.get(&recv)
        {
            // No module exposes plain values in M1 (math is
            // functions-only); reuse the module-call error path.
            return self.module_call(item, name, vec![]);
        }

        let recv = self.eval_expr(frame, recv)?;
        if let Value::Struct(s) = &recv {
            if let Some(index) = self.struct_field_index(s.item, name) {
                return Ok(s.fields.borrow()[index].clone());
            }
            if let Item::StructDef(def) = self.hir.item(s.item)
                && let Some(index) = def.methods.iter().position(|m| m.name == name)
            {
                return Ok(Value::BoundMethod(Rc::new(BoundMethod {
                    recv: recv.clone(),
                    owner: s.item,
                    index,
                })));
            }
            if name == "toString" {
                return Ok(Value::BoundBuiltin(Rc::new(BoundBuiltin {
                    recv: recv.clone(),
                    name: name.to_string(),
                })));
            }
            return Err(self.fatal(format!("brasa: unknown member `{name}`")));
        }

        // A builtin method accessed without calling it becomes a bound
        // value; argument validation happens at the call.
        Ok(Value::BoundBuiltin(Rc::new(BoundBuiltin {
            recv,
            name: name.to_string(),
        })))
    }

    pub(crate) fn struct_field_index(&self, item: ItemId, name: &str) -> Option<usize> {
        match self.hir.item(item) {
            Item::StructDef(def) => def.fields.iter().position(|f| f.name == name),
            _ => None,
        }
    }

    pub(crate) fn item_name(&self, item: ItemId) -> String {
        match self.hir.item(item) {
            Item::StructDef(def) => def.name.clone(),
            Item::EnumDef(def) => def.name.clone(),
            Item::FuncDef(def) => def.name.clone(),
            _ => "<item>".to_string(),
        }
    }

    // --- toString ------------------------------------------------------

    /// Renders a value the way `puts`, `print`, interpolation, and
    /// `.toString()` show it (`docs/spec/03-types.md`, implicit
    /// `toString`): structs as `Point { x: 1.0, y: 2.0 }`, enums as
    /// `Circle(1.0)` or bare `Dot`, floats always with a decimal point.
    /// A custom struct `toString` method replaces the derived rendering
    /// everywhere, including nested positions.
    ///
    /// M1 rendering decisions: `Vector` as `[1, 2]`; `Map` as
    /// `{ "a": 1 }` in insertion order; `Set` as `Set([1, 2])`; `Option`
    /// as `Some(1)`/`None`; tuples as `(1, 2)`; ranges as
    /// `0..10`/`0..=10`; `unit` as `unit`. A top-level string or char
    /// prints raw; inside containers and composites strings print
    /// double-quoted (escaped) and chars single-quoted.
    pub(crate) fn display(&mut self, value: &Value) -> EvalResult<String> {
        self.render(value, false, 0)
    }

    fn render(&mut self, value: &Value, quoted: bool, depth: usize) -> EvalResult<String> {
        if depth > MAX_DISPLAY_DEPTH {
            return Err(self.fatal("brasa: toString recursion too deep (cyclic value?)"));
        }

        match value {
            Value::Int(v) => Ok(v.to_string()),
            Value::Float(v) => Ok(render_float(*v)),
            Value::Bool(v) => Ok(v.to_string()),
            Value::Unit => Ok("unit".to_string()),
            Value::Char(v) => {
                if quoted {
                    Ok(format!("'{}'", escape_char(*v)))
                } else {
                    Ok(v.to_string())
                }
            }
            Value::Str(v) => {
                if quoted {
                    Ok(format!("\"{}\"", escape_str(v)))
                } else {
                    Ok(v.to_string())
                }
            }
            Value::Range { lo, hi, inclusive } => {
                let op = if *inclusive { "..=" } else { ".." };
                Ok(format!("{lo}{op}{hi}"))
            }
            Value::Tuple(items) => {
                let items = items.clone();
                let parts = self.render_all(items.iter(), depth)?;
                Ok(format!("({})", parts.join(", ")))
            }
            Value::Vector(items) => {
                let items = items.borrow().clone();
                let parts = self.render_all(items.iter(), depth)?;
                Ok(format!("[{}]", parts.join(", ")))
            }
            Value::Set(items) => {
                let items = items.borrow().clone();
                let parts = self.render_all(items.iter(), depth)?;
                Ok(format!("Set([{}])", parts.join(", ")))
            }
            Value::Map(entries) => {
                let entries = entries.borrow().clone();
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let mut parts = Vec::with_capacity(entries.len());
                for (key, value) in &entries {
                    let key = self.render(key, true, depth + 1)?;
                    let value = self.render(value, true, depth + 1)?;
                    parts.push(format!("{key}: {value}"));
                }
                Ok(format!("{{ {} }}", parts.join(", ")))
            }
            Value::Option(inner) => match inner {
                Some(inner) => {
                    let inner = (**inner).clone();
                    let inner = self.render(&inner, true, depth + 1)?;
                    Ok(format!("Some({inner})"))
                }
                None => Ok("None".to_string()),
            },
            Value::Struct(s) => {
                let item = s.item;
                if let Item::StructDef(def) = self.hir.item(item)
                    && def.methods.iter().any(|m| m.name == "toString")
                {
                    let text = self.call_method_by_name(value.clone(), "toString", vec![])?;
                    return match text {
                        Value::Str(text) => Ok(text.to_string()),
                        _ => Err(self.fatal("brasa: `toString` must return a string")),
                    };
                }

                let Item::StructDef(def) = self.hir.item(item) else {
                    return Err(self.fatal("brasa: struct value of a non-struct item"));
                };
                let names: Vec<String> = def.fields.iter().map(|f| f.name.clone()).collect();
                let struct_name = def.name.clone();

                let fields = s.fields.borrow().clone();
                if fields.is_empty() {
                    return Ok(format!("{struct_name} {{}}"));
                }
                let mut parts = Vec::with_capacity(fields.len());
                for (name, field) in names.iter().zip(fields.iter()) {
                    let field = self.render(field, true, depth + 1)?;
                    parts.push(format!("{name}: {field}"));
                }
                Ok(format!("{struct_name} {{ {} }}", parts.join(", ")))
            }
            Value::Enum(e) => {
                let variant_name = match self.hir.item(e.item) {
                    Item::EnumDef(def) => def
                        .variants
                        .get(e.variant)
                        .map(|v| v.name.clone())
                        .unwrap_or_else(|| "<variant>".to_string()),
                    _ => "<variant>".to_string(),
                };
                if e.fields.is_empty() {
                    return Ok(variant_name);
                }
                let fields = e.fields.clone();
                let parts = self.render_all(fields.iter(), depth)?;
                Ok(format!("{variant_name}({})", parts.join(", ")))
            }
            Value::Func(item) => Ok(format!("<function {}>", self.item_name(*item))),
            Value::Closure(_) => Ok("<lambda>".to_string()),
            Value::BoundMethod(_) | Value::BoundBuiltin(_) => Ok("<bound method>".to_string()),
            // Only the message: the uncaught-error path (`crate::finish`)
            // prepends the nominal tag itself, producing
            // `error: string.ParseError: <message>` without duplication.
            Value::NativeError { message, .. } => Ok(message.to_string()),
        }
    }

    fn render_all<'v>(
        &mut self,
        values: impl Iterator<Item = &'v Value>,
        depth: usize,
    ) -> EvalResult<Vec<String>> {
        let mut parts = Vec::new();
        for value in values {
            parts.push(self.render(value, true, depth + 1)?);
        }
        Ok(parts)
    }
}

enum LoopFlow {
    Continue,
    Break,
}

/// Floats always show the decimal point (`docs/spec/03-types.md`):
/// `1.0`, never `1`. `NaN`, `inf`, and exponent forms render as Rust
/// prints them.
fn render_float(v: f64) -> String {
    let text = format!("{v}");
    if v.is_finite() && !text.contains('.') && !text.contains('e') {
        format!("{text}.0")
    } else {
        text
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn escape_char(c: char) -> String {
    match c {
        '\'' => "\\'".to_string(),
        '\\' => "\\\\".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        '\r' => "\\r".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::PanicKind;

    /// [`PanicKind`] and the resolver's canonical closed union
    /// (`brasa_resolver::PANIC_UNION`, BRS-24) must stay identical in
    /// members, qualified names, and order: the resolver validates
    /// `catch` arm names against its list while `eval_catch` matches
    /// signals against these names.
    #[test]
    fn panic_kinds_match_the_resolver_union() {
        let kinds = [
            PanicKind::IndexOutOfBounds,
            PanicKind::DivisionByZero,
            PanicKind::IntegerOverflow,
            PanicKind::AssertionFailed,
            PanicKind::StackOverflow,
        ];

        let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
        assert_eq!(names.as_slice(), brasa_resolver::PANIC_UNION);
    }
}
