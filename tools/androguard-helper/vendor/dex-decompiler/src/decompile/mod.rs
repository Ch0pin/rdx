//! RDX modification (2026-11-14): preserve fully qualified Java names in
//! shorten_java_names so per-method output does not depend on merged imports.
//! DEX to Java decompilation: decode bytecode with dex-bytecode, map to Java source.
//! Supports structured control flow: if/else, while loops (via CFG).
//! Simplification pass: collapse invoke + move-result + return into single return.

pub mod accessors;
pub mod annotations;
pub mod cfg;
pub mod const_fields;
pub mod deobf;
pub mod graph;
mod ir;
pub mod json_export;
pub mod kotlin;
pub mod mapping;
pub mod pass;
mod read_write;
pub mod region;
pub mod rename;
pub mod resource_consts;
mod simplify;
pub mod ssa;
pub use simplify::restore_string_switch as simplify_restore_string_switch_for_tests;
pub use simplify::simplify_method_body as simplify_method_body_for_tests;
mod try_catch;
use try_catch::{
    catch_all_byte_range, first_handler_start_byte, looks_like_finally,
    nested_runtime_exception_handler_pair, peel_trailing_finally_from_try,
    split_pre_try_for_finally, try_and_handler_byte_ranges_with_end, try_handler_pairs,
};
mod type_infer;
pub mod value_flow;

use crate::error::{DexDecompilerError, Result};
use crate::java;
use cfg::{BlockEnd, BlockId, MethodCfg};
use dex_bytecode::{decode_all, Instruction};
use dex_parser::{ClassDef, CodeItem, DexFile, EncodedMethod, NO_INDEX};
use ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
use pass::{
    run_dead_assign_with_used_regs, used_regs, ConstructorMergePass, CopyPropPass, DeadAssignPass,
    ExprSimplifyPass, InlineFilledArrayPass, InvokeChainPass, Pass, PassRunner, SsaRenamePass,
};
use region::{
    as_single_if, build_regions, build_regions_filtered, for_loop_pattern,
    loop_body_do_while_pattern, region_contains_loop, region_is_empty, region_is_empty_with_cfg,
    Region,
};
use ssa::{apply_canonical_names, construct_ssa, phi_canonical_map, phi_registers, strip_phis};
use simplify::is_temp_like_name;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use type_infer::{
    build_var_names_with_regs, enrich_types_with_field_index, infer_types_cached,
    is_primitive_java_type, FieldTypeIndex, MethodReturnIndex, preferred_debug_type_for_reg,
    types_compatible_for_naming,
};
use value_flow::{
    build_api_return_sources, build_insn_labels, build_instruction_rw_map, build_invoke_method_map,
    ValueFlowAnalysisOwned,
};

/// Decompilation strategy (jadx-inspired).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecompilationMode {
    /// Structured control flow (if/else, loops, switch, try/catch). Default.
    #[default]
    Restructure,
    /// Simplified: emit CFG blocks in order without full region nesting.
    Simple,
    /// Linear IR listing (minimal structure) — use when restructuring fails or for debugging.
    Fallback,
}

/// Options for filtering which classes are decompiled (e.g. `--only-package`, `--exclude`).
#[derive(Clone)]
pub struct DecompilerOptions {
    /// If set, only classes in this package (or the exact class) are decompiled. E.g. `com.example`.
    pub only_package: Option<String>,
    /// Exclude classes whose name equals or starts with any of these (after trimming trailing `.` or `.*`). E.g. `android.`
    pub exclude: Vec<String>,
    /// If true, emit raw DEX instructions as comments before each method body.
    pub show_bytecode: bool,
    /// Optional renames for package, class, method, field, and local variables in decompiled output.
    pub rename_map: Option<rename::RenameMap>,
    /// Decompilation mode (restructure / simple / fallback).
    pub mode: DecompilationMode,
    /// Prefer DEX debug_info local/parameter names when present (default: true).
    pub use_debug_names: bool,
    /// Optional resource id → `R.type.name` (from `resources.arsc` / R classes).
    pub resource_map: Option<std::collections::HashMap<u32, String>>,
}

impl Default for DecompilerOptions {
    fn default() -> Self {
        Self {
            only_package: None,
            exclude: Vec::new(),
            show_bytecode: false,
            rename_map: None,
            mode: DecompilationMode::Restructure,
            use_debug_names: true,
            resource_map: None,
        }
    }
}

impl DecompilerOptions {
    /// Options with debug names enabled (same as [`Default`]).
    pub fn with_defaults() -> Self {
        Self::default()
    }
}

/// One instruction row for bytecode display (e.g. in web UI).
#[derive(Debug, Clone)]
pub struct MethodBytecodeRow {
    pub offset: u32,
    pub mnemonic: String,
    pub operands: String,
}

/// One CFG block node for graph visualization.
#[derive(Debug, Clone)]
pub struct CfgNodeInfo {
    pub id: usize,
    pub start_offset: u32,
    pub end_offset: u32,
    pub label: String,
}

/// One CFG edge for graph visualization.
#[derive(Debug, Clone)]
pub struct CfgEdgeInfo {
    pub from_id: usize,
    pub to_id: usize,
}

/// Returns true if `class_name` should be included given only_package and exclude.
fn class_matches_filter(class_name: &str, only_package: Option<&str>, exclude: &[String]) -> bool {
    if let Some(prefix) = only_package {
        let prefix = prefix.trim_end_matches('.');
        if !(class_name == prefix || class_name.starts_with(&format!("{}.", prefix))) {
            return false;
        }
    }
    for exc in exclude {
        let exc = exc.trim_end_matches('.').trim_end_matches('*').trim();
        if class_name == exc || class_name.starts_with(&format!("{}.", exc)) {
            return false;
        }
    }
    true
}

/// True if the DEX class name is nested (`Outer$…`).
fn is_nested_dex_class(class_name: &str) -> bool {
    class_name.contains('$')
}

/// Direct named member of `outer` (`Outer$Foo`, not `Outer$1` / `Outer$Foo$Bar`).
fn is_direct_named_inner(outer: &str, candidate: &str) -> bool {
    let prefix = format!("{}$", outer);
    let Some(suffix) = candidate.strip_prefix(&prefix) else {
        return false;
    };
    !suffix.is_empty() && !suffix.contains('$') && !suffix.chars().all(|c| c.is_ascii_digit())
}

/// Last emitted statement is return/throw/break/continue (no trailing break needed).
fn body_ends_with_exit(out: &str) -> bool {
    let Some(last) = out.trim_end().lines().last() else {
        return false;
    };
    let t = last.trim();
    t.starts_with("return") || t.starts_with("throw") || t == "break;" || t == "continue;"
}

fn is_synthetic_local_name(s: &str) -> bool {
    if s.starts_with("local") && s.bytes().skip(5).all(|b| b.is_ascii_digit()) {
        return true;
    }
    if s.starts_with('v') && s.len() > 1 && s.bytes().skip(1).all(|b| b.is_ascii_digit()) {
        return true;
    }
    // Typed temps: i0, s0, v0, o0, t0, ...
    let b = s.as_bytes();
    if b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(|c| c.is_ascii_digit())
        && matches!(
            b[0],
            b'i' | b's'
                | b'z'
                | b'j'
                | b'f'
                | b'd'
                | b'l'
                | b'b'
                | b'c'
                | b'v'
                | b'o'
                | b't'
                | b'a'
                | b'x'
        )
    {
        return true;
    }
    false
}

/// Signature-style param names (`p0`, `s1`) — not debug/user names.
fn is_signature_style_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(|c| c.is_ascii_digit())
        && matches!(
            b[0],
            b'p' | b's' | b'i' | b'z' | b'l' | b'f' | b'd' | b'c' | b'x'
        )
}

fn is_java_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Merge `debug_info.parameter_names` with `DBG_START_LOCAL` names on param registers.
/// Locals fill gaps so the signature matches the body (`grantResults` vs `arr2`).
fn debug_param_names_from_tables(
    parameter_names: &[Option<String>],
    register_names: &HashMap<u32, String>,
    param_base: u32,
    is_static: bool,
    param_types: &[String],
) -> Vec<Option<String>> {
    let mut slot = if is_static { 0u32 } else { 1u32 };
    let mut out = Vec::with_capacity(param_types.len());
    for (i, ty) in param_types.iter().enumerate() {
        let from_table = parameter_names
            .get(i)
            .and_then(|p| p.as_ref())
            .filter(|n| is_java_ident(n))
            .cloned();
        let from_local = register_names
            .get(&(param_base + slot))
            .filter(|n| is_java_ident(n) && n.as_str() != "this")
            .cloned();
        out.push(from_table.or(from_local));
        slot += if matches!(ty.as_str(), "long" | "double" | "J" | "D") {
            2
        } else {
            1
        };
    }
    out
}

/// Names already used in an `if (` / `else if (` condition are in scope.
/// Prevents `if (b == 10) { int b = b + 1; }` when `b` is a prior SSA version.
fn mark_condition_idents_declared(condition: &str, declared: &mut HashSet<String>) {
    let mut cur = String::new();
    let flush = |cur: &mut String, declared: &mut HashSet<String>| {
        if cur.is_empty() {
            return;
        }
        let name = std::mem::take(cur);
        if !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return;
        }
        if matches!(
            name.as_str(),
            "true"
                | "false"
                | "null"
                | "new"
                | "this"
                | "super"
                | "instanceof"
                | "int"
                | "long"
                | "boolean"
                | "byte"
                | "char"
                | "short"
                | "float"
                | "double"
        ) {
            return;
        }
        declared.insert(name);
    };
    for c in condition.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            flush(&mut cur, declared);
        }
    }
    flush(&mut cur, declared);
}

/// Decompiler: takes a parsed DexFile and emits Java source.
pub struct Decompiler<'a> {
    pub dex: &'a DexFile,
    only_package: Option<String>,
    exclude: Vec<String>,
    show_bytecode: bool,
    /// Optional renames for package, class, method, field, and variables.
    rename_map: Option<rename::RenameMap>,
    mode: DecompilationMode,
    use_debug_names: bool,
    /// Registers that have φ-nodes in the current method (shared Java names across versions).
    phi_regs: RefCell<HashSet<u32>>,
    /// Lazy index: class_name -> (dex_slot, ClassDef). Slot 0 = primary `dex`, 1+ = `extra_dexes`.
    class_def_index: RefCell<Option<HashMap<String, (usize, ClassDef)>>>,
    /// Lazy index: enclosing_class -> (inner_class_name, constructor_param_count) for inner classes extending Thread.
    inner_thread_index: RefCell<Option<HashMap<String, Vec<(String, usize)>>>>,
    /// Lazy index: enclosing -> (inner, ctor_params, superclass_or_iface) for general anonymous inlining.
    inner_anon_index: RefCell<Option<HashMap<String, Vec<(String, usize, String)>>>>,
    /// Cache: inner_class_name -> decompiled run() body (before capture replacement). Avoids re-decompiling same run().
    inner_run_body_cache: RefCell<HashMap<String, String>>,
    /// Nesting depth for lambda body inlining (avoid recursive explode).
    lambda_inline_depth: RefCell<u32>,
    /// Literal → `Class.FIELD` replacements for the class currently being decompiled.
    const_field_reps: RefCell<Vec<(String, String)>>,
    /// Literal → `R.type.name` replacements (ARSC + DEX R$*).
    resource_reps: Vec<(String, String)>,
    /// Current class `access$*` replacements (set while decompiling a class).
    accessor_reps: RefCell<Vec<(String, String)>>,
    /// Extra DEXes for cross-DEX inner/anonymous class lookup (APK multi-dex).
    extra_dexes: Vec<&'a DexFile>,
    /// Register → Java type for the method currently being decompiled (cross-block enrichment).
    method_reg_types: RefCell<Option<HashMap<u32, String>>>,
    /// Register → preferred Java name for the method (stable across CFG blocks).
    method_reg_names: RefCell<Option<HashMap<u32, String>>>,
    /// Register → Java type from DEX debug locals (`float`, `double`, …).
    method_debug_types: RefCell<Option<HashMap<u32, String>>>,
    /// Return type of the method currently being decompiled (`long`, `double`, …).
    method_return_type: RefCell<Option<String>>,
    /// Lazy DEX-wide field owner→name→type index (amortized across methods / CFG blocks).
    field_type_index: RefCell<Option<FieldTypeIndex>>,
    /// Lazy DEX-wide method return-type index (amortized across methods / CFG blocks).
    method_return_index: RefCell<Option<MethodReturnIndex>>,
}

impl<'a> Decompiler<'a> {
    pub fn new(dex: &'a DexFile) -> Self {
        let resource_map = resource_consts::build_resource_name_map(dex, None);
        let resource_reps = resource_consts::resource_replacements(&resource_map);
        Self {
            dex,
            only_package: None,
            exclude: vec![],
            show_bytecode: false,
            rename_map: None,
            mode: DecompilationMode::Restructure,
            use_debug_names: true,
            phi_regs: RefCell::new(HashSet::new()),
            class_def_index: RefCell::new(None),
            inner_thread_index: RefCell::new(None),
            inner_anon_index: RefCell::new(None),
            inner_run_body_cache: RefCell::new(HashMap::new()),
            lambda_inline_depth: RefCell::new(0),
            const_field_reps: RefCell::new(Vec::new()),
            resource_reps,
            accessor_reps: RefCell::new(Vec::new()),
            extra_dexes: Vec::new(),
            method_reg_types: RefCell::new(None),
            method_reg_names: RefCell::new(None),
            method_debug_types: RefCell::new(None),
            method_return_type: RefCell::new(None),
            field_type_index: RefCell::new(None),
            method_return_index: RefCell::new(None),
        }
    }

    /// Create a decompiler with package/class filters and optional rename map.
    pub fn with_options(dex: &'a DexFile, options: DecompilerOptions) -> Self {
        let resource_map =
            resource_consts::build_resource_name_map(dex, options.resource_map.as_ref());
        let resource_reps = resource_consts::resource_replacements(&resource_map);
        Self {
            dex,
            only_package: options.only_package,
            exclude: options.exclude,
            show_bytecode: options.show_bytecode,
            rename_map: options.rename_map,
            mode: options.mode,
            use_debug_names: options.use_debug_names,
            phi_regs: RefCell::new(HashSet::new()),
            class_def_index: RefCell::new(None),
            inner_thread_index: RefCell::new(None),
            inner_anon_index: RefCell::new(None),
            inner_run_body_cache: RefCell::new(HashMap::new()),
            lambda_inline_depth: RefCell::new(0),
            const_field_reps: RefCell::new(Vec::new()),
            resource_reps,
            accessor_reps: RefCell::new(Vec::new()),
            extra_dexes: Vec::new(),
            method_reg_types: RefCell::new(None),
            method_reg_names: RefCell::new(None),
            method_debug_types: RefCell::new(None),
            method_return_type: RefCell::new(None),
            field_type_index: RefCell::new(None),
            method_return_index: RefCell::new(None),
        }
    }

    /// Attach additional DEX files so inner/anonymous classes can be resolved across multi-DEX APKs.
    pub fn with_extra_dexes(mut self, extra: Vec<&'a DexFile>) -> Self {
        self.extra_dexes = extra;
        // Force rebuild of class indexes to include extras.
        *self.class_def_index.borrow_mut() = None;
        *self.inner_thread_index.borrow_mut() = None;
        *self.inner_anon_index.borrow_mut() = None;
        self
    }

    /// Ensure DEX-wide type indexes exist (built once per [`Decompiler`] instance).
    fn ensure_type_indexes(&self) {
        if self.field_type_index.borrow().is_none() {
            *self.field_type_index.borrow_mut() = Some(FieldTypeIndex::build(self.dex));
        }
        if self.method_return_index.borrow().is_none() {
            *self.method_return_index.borrow_mut() = Some(MethodReturnIndex::build(self.dex));
        }
    }

    /// Type-infer for a method body, reusing cached field / return indexes.
    fn infer_types_for(
        &self,
        encoded: &EncodedMethod,
        code: &CodeItem,
        stmts: &[IrStmt],
    ) -> HashMap<VarId, String> {
        self.ensure_type_indexes();
        let fields = self.field_type_index.borrow();
        let returns = self.method_return_index.borrow();
        infer_types_cached(
            self.dex,
            encoded,
            code,
            stmts,
            fields.as_ref().expect("field index"),
            returns.as_ref().expect("return index"),
        )
    }

    /// Enrich per-block types from method-wide / debug maps, reusing the field index.
    fn enrich_types_for(
        &self,
        type_map: &mut HashMap<VarId, String>,
        reg_types: &HashMap<u32, String>,
        debug_types: Option<&HashMap<u32, String>>,
        stmts: &[IrStmt],
    ) {
        self.ensure_type_indexes();
        let fields = self.field_type_index.borrow();
        enrich_types_with_field_index(
            self.dex,
            type_map,
            reg_types,
            debug_types,
            stmts,
            fields.as_ref().expect("field index"),
        );
    }

    /// Field type with generics when `dalvik.annotation.Signature` is present.
    fn field_type_java(&self, class_def: &ClassDef, field_idx: u32, descriptor: &str) -> String {
        if let Some(sig) = annotations::field_generic_signature(
            self.dex.data.as_ref(),
            class_def,
            field_idx,
            &|idx| self.dex.get_string(idx).ok(),
            &|idx| self.dex.get_type(idx).ok(),
        ) {
            if let Some(ty) = annotations::signature_type_to_java(&sig) {
                return ty;
            }
        }
        java::descriptor_to_java(descriptor)
    }

    /// Interfaces implemented by a class (`interfaces_off` type_list).
    fn class_interfaces(&self, class_def: &ClassDef) -> Vec<String> {
        if class_def.interfaces_off == 0 {
            return vec![];
        }
        let data = self.dex.data.as_ref();
        let off = class_def.interfaces_off as usize;
        if off + 4 > data.len() {
            return vec![];
        }
        let size = u32::from_le_bytes(data[off..off + 4].try_into().unwrap_or([0; 4])) as usize;
        let mut out = Vec::new();
        for i in 0..size {
            let base = off + 4 + i * 2;
            if base + 2 > data.len() {
                break;
            }
            let type_idx =
                u16::from_le_bytes(data[base..base + 2].try_into().unwrap_or([0; 2])) as u32;
            if let Ok(desc) = self.dex.get_type(type_idx) {
                out.push(java::descriptor_to_java(&desc));
            }
        }
        out
    }

    /// ClassDefs that are direct named inners of `outer` (`Outer$Member`).
    fn direct_named_inner_defs(&self, outer: &str) -> Result<Vec<ClassDef>> {
        let mut out = Vec::new();
        for class_def_result in self.dex.class_defs() {
            let class_def =
                class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_type = self
                .dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let name = java::descriptor_to_java(&class_type);
            if is_direct_named_inner(outer, &name) {
                out.push(class_def);
            }
        }
        out.sort_by_key(|cd| cd.class_idx);
        Ok(out)
    }

    /// Build class_name -> (dex_slot, ClassDef) index (primary DEX + extra_dexes).
    fn ensure_class_def_index(&self) -> Result<()> {
        if self.class_def_index.borrow().is_some() {
            return Ok(());
        }
        let mut map = HashMap::new();
        let mut ingest = |slot: usize, dex: &DexFile| -> Result<()> {
            for class_def_result in dex.class_defs() {
                let class_def =
                    class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let class_type = dex
                    .get_type(class_def.class_idx)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let name = java::descriptor_to_java(&class_type);
                map.entry(name).or_insert((slot, class_def.clone()));
            }
            Ok(())
        };
        ingest(0, self.dex)?;
        for (i, d) in self.extra_dexes.iter().enumerate() {
            ingest(i + 1, d)?;
        }
        *self.class_def_index.borrow_mut() = Some(map);
        Ok(())
    }

    fn dex_at(&self, slot: usize) -> &'a DexFile {
        if slot == 0 {
            self.dex
        } else {
            self.extra_dexes[slot - 1]
        }
    }

    /// Build enclosing_class -> [(inner_name, param_count)] for inner classes extending Thread.
    fn ensure_inner_thread_index(&self) -> Result<()> {
        if self.inner_thread_index.borrow().is_some() {
            return Ok(());
        }
        let thread_java = java::descriptor_to_java("Ljava/lang/Thread;");
        let mut map: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        let mut ingest = |dex: &DexFile| -> Result<()> {
            for class_def_result in dex.class_defs() {
                let class_def =
                    class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let class_type = dex
                    .get_type(class_def.class_idx)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let name = java::descriptor_to_java(&class_type);
                let Some((enclosing, suffix)) = name.rsplit_once('$') else {
                    continue;
                };
                if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if class_def.superclass_idx == NO_INDEX {
                    continue;
                }
                let super_type = dex
                    .get_type(class_def.superclass_idx)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                if java::descriptor_to_java(&super_type) != thread_java {
                    continue;
                }
                let class_data_opt = dex
                    .get_class_data(&class_def)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let Some(ref class_data) = class_data_opt.as_ref() else {
                    continue;
                };
                for enc in &class_data.direct_methods {
                    let info = dex
                        .get_method_info(enc.method_idx)
                        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                    if info.name == "<init>" {
                        map.entry(enclosing.to_string())
                            .or_default()
                            .push((name.clone(), info.params.len()));
                        break;
                    }
                }
            }
            Ok(())
        };
        ingest(self.dex)?;
        for d in &self.extra_dexes {
            ingest(d)?;
        }
        *self.inner_thread_index.borrow_mut() = Some(map);
        Ok(())
    }

    /// Decompile entire DEX to Java source (one or more class declarations).
    /// Respects only_package and exclude when set.
    pub fn decompile(&self) -> Result<String> {
        let mut out = String::new();
        let mut first = true;
        for class_def_result in self.dex.class_defs() {
            let class_def =
                class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_type = self
                .dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_name = java::descriptor_to_java(&class_type);
            if !class_matches_filter(&class_name, self.only_package.as_deref(), &self.exclude) {
                continue;
            }
            // Named/anonymous inners are nested or inlined — not top-level classes.
            if is_nested_dex_class(&class_name) {
                continue;
            }
            if !first {
                writeln!(&mut out)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            first = false;
            let class_java = self.decompile_class(&class_def)?;
            write!(&mut out, "{}", class_java)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        Ok(out)
    }

    /// Export class/method/field inventory as JSON (optionally with decompiled method bodies).
    pub fn export_json(&self, include_bodies: bool) -> Result<String> {
        use json_export::{ClassJson, DexJsonExport, FieldJson, MethodJson};
        let mut classes = Vec::new();
        for class_def_result in self.dex.class_defs() {
            let class_def =
                class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_type = self
                .dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_name = java::descriptor_to_java(&class_type);
            if !class_matches_filter(&class_name, self.only_package.as_deref(), &self.exclude) {
                continue;
            }
            let class_data = self
                .dex
                .get_class_data(&class_def)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let mut fields = Vec::new();
            let mut methods = Vec::new();
            if let Some(ref cd) = class_data {
                for f in &cd.static_fields {
                    if let Ok(fi) = self.dex.get_field_info(f.field_idx) {
                        fields.push(FieldJson {
                            name: fi.name.to_string(),
                            typ: java::descriptor_to_java(&fi.typ),
                            static_field: true,
                        });
                    }
                }
                for f in &cd.instance_fields {
                    if let Ok(fi) = self.dex.get_field_info(f.field_idx) {
                        fields.push(FieldJson {
                            name: fi.name.to_string(),
                            typ: java::descriptor_to_java(&fi.typ),
                            static_field: false,
                        });
                    }
                }
                let simple = class_name.rsplit('.').next().unwrap_or(&class_name);
                for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                    let info = self
                        .dex
                        .get_method_info(m.method_idx)
                        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                    let params: Vec<String> = info
                        .params
                        .iter()
                        .map(|p| java::descriptor_to_java(p))
                        .collect();
                    let descriptor = format!(
                        "({}){}",
                        params.join(","),
                        java::descriptor_to_java(&info.return_type)
                    );
                    let java_src = if include_bodies {
                        self.decompile_method(m, Some(simple), Some(&class_name))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    methods.push(MethodJson {
                        name: info.name.to_string(),
                        descriptor,
                        access_flags: m.access_flags,
                        java: java_src,
                    });
                }
            }
            classes.push(ClassJson {
                name: class_name,
                fields,
                methods,
            });
        }
        serde_json::to_string_pretty(&DexJsonExport { classes })
            .map_err(|e| DexDecompilerError::Decompilation(format!("json export: {e}")))
    }

    /// Decompile entire DEX into a directory with package structure (e.g. `out/com/example/MyClass.java`).
    pub fn decompile_to_dir(&self, base_path: &std::path::Path) -> Result<()> {
        self.decompile_to_dir_with_progress(base_path, None)
    }

    /// Collect (ClassDef, class_name) for all classes that pass only_package and exclude filters.
    /// Used for parallel decompilation (e.g. by the CLI).
    pub fn collect_included_classes(&self) -> Result<Vec<(ClassDef, String)>> {
        let mut out = Vec::new();
        for class_def_result in self.dex.class_defs() {
            let class_def =
                class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_type = self
                .dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            let class_name = java::descriptor_to_java(&class_type);
            if is_nested_dex_class(&class_name) {
                continue;
            }
            if class_matches_filter(&class_name, self.only_package.as_deref(), &self.exclude) {
                out.push((class_def, class_name));
            }
        }
        Ok(out)
    }

    /// Like `decompile_to_dir`, but calls `progress(current, total, class_name)` after each class (1-based current).
    /// Respects only_package and exclude; progress total is the number of included classes.
    pub fn decompile_to_dir_with_progress(
        &self,
        base_path: &std::path::Path,
        mut progress: Option<&mut dyn FnMut(usize, usize, &str)>,
    ) -> Result<()> {
        let included = self.collect_included_classes()?;
        let total = included.len();
        for (i, (class_def, class_name)) in included.into_iter().enumerate() {
            if let Some(p) = &mut progress {
                p(i + 1, total, &class_name);
            }
            let class_java = self.decompile_class(&class_def)?;
            let output_class_name = self
                .rename_map
                .as_ref()
                .and_then(|r| r.class.get(&class_name).cloned())
                .unwrap_or_else(|| class_name.clone());
            write_decompiled_class(base_path, &output_class_name, &class_java)?;
        }
        Ok(())
    }

    /// Parallel decompile of all included classes into `base_path`.
    ///
    /// Each rayon worker builds its own [`Decompiler`] (internal caches are `!Sync`).
    /// `DexFile` must be shareable across threads (`Arc` bytes in dex-parser).
    /// Optional `progress` receives `(current, total, class_name)` as classes complete.
    pub fn decompile_to_dir_parallel(
        dex: &DexFile,
        options: DecompilerOptions,
        base_path: &std::path::Path,
        progress: Option<&(dyn Fn(usize, usize, &str) + Sync)>,
    ) -> Result<()> {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let probe = Decompiler::with_options(dex, options.clone());
        let included = probe.collect_included_classes()?;
        let total = included.len();
        if total == 0 {
            return Ok(());
        }

        let done = AtomicUsize::new(0);

        let results: Vec<Result<()>> = included
            .par_iter()
            .map(|(class_def, class_name)| {
                let d = Decompiler::with_options(dex, options.clone());
                let class_java = d.decompile_class(class_def)?;
                let output_class_name = d
                    .rename_map
                    .as_ref()
                    .and_then(|r| r.class.get(class_name).cloned())
                    .unwrap_or_else(|| class_name.clone());
                write_decompiled_class(base_path, &output_class_name, &class_java)?;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(p) = progress {
                    p(n, total, class_name);
                }
                Ok(())
            })
            .collect();

        for r in results {
            r?;
        }
        Ok(())
    }

    /// Parallel decompile of **multiple** DEX files into one output tree.
    ///
    /// Classes are collected from every DEX; on duplicate FQN the first wins.
    pub fn decompile_dexes_to_dir_parallel(
        dexes: &[&DexFile],
        options: DecompilerOptions,
        base_path: &std::path::Path,
        progress: Option<&(dyn Fn(usize, usize, &str) + Sync)>,
    ) -> Result<()> {
        use rayon::prelude::*;
        use std::collections::HashSet;
        use std::sync::atomic::{AtomicUsize, Ordering};

        if dexes.is_empty() {
            return Ok(());
        }
        if dexes.len() == 1 {
            return Self::decompile_to_dir_parallel(dexes[0], options, base_path, progress);
        }

        // (dex_index, ClassDef, class_name)
        let mut jobs: Vec<(usize, ClassDef, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (di, dex) in dexes.iter().enumerate() {
            let probe = Decompiler::with_options(dex, options.clone());
            for (class_def, class_name) in probe.collect_included_classes()? {
                if seen.insert(class_name.clone()) {
                    jobs.push((di, class_def, class_name));
                }
            }
        }
        let total = jobs.len();
        if total == 0 {
            return Ok(());
        }
        let done = AtomicUsize::new(0);
        let results: Vec<Result<()>> = jobs
            .par_iter()
            .map(|(di, class_def, class_name)| {
                let dex = dexes[*di];
                let d = Decompiler::with_options(dex, options.clone());
                let class_java = d.decompile_class(class_def)?;
                let output_class_name = d
                    .rename_map
                    .as_ref()
                    .and_then(|r| r.class.get(class_name).cloned())
                    .unwrap_or_else(|| class_name.clone());
                write_decompiled_class(base_path, &output_class_name, &class_java)?;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(p) = progress {
                    p(n, total, class_name);
                }
                Ok(())
            })
            .collect();
        for r in results {
            r?;
        }
        Ok(())
    }

    /// Decompile one class to Java source.
    pub fn decompile_class(&self, class_def: &ClassDef) -> Result<String> {
        self.decompile_class_ex(class_def, false)
    }

    /// Decompile one class. When `as_member`, skip package/imports (nested class body).
    fn decompile_class_ex(&self, class_def: &ClassDef, as_member: bool) -> Result<String> {
        let class_type = self
            .dex
            .get_type(class_def.class_idx)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let class_name = java::descriptor_to_java(&class_type);
        let (package, simple_raw) = split_package_and_class(&class_name);
        let simple_class_name = simple_raw
            .rsplit_once('$')
            .map(|(_, s)| s.to_string())
            .unwrap_or(simple_raw);
        let class_data = self
            .dex
            .get_class_data(class_def)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let super_type = if class_def.superclass_idx != NO_INDEX {
            let s = self
                .dex
                .get_type(class_def.superclass_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            shorten_java_names(&java::descriptor_to_java(&s))
        } else {
            "Object".to_string()
        };
        let flags = java::access_flags_to_java(class_def.access_flags, true);
        let mut out = String::new();
        if !as_member {
            if !package.is_empty() {
                writeln!(&mut out, "// package {}", package)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            let imports = collect_class_imports(
                self.dex,
                class_def,
                class_data.as_ref(),
                &class_name,
                &package,
            )?;
            for fqn in &imports {
                writeln!(&mut out, "import {};", fqn)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            if !imports.is_empty() {
                writeln!(&mut out)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        let class_anns =
            annotations::class_annotations(self.dex.data.as_ref(), class_def, &|idx| {
                self.dex.get_string(idx).ok()
            })
            .unwrap_or_default();
        for ann in &class_anns {
            if let Some(line) = annotations::format_annotation_java(
                ann,
                &|idx| self.dex.get_string(idx).ok(),
                &|idx| self.dex.get_type(idx).ok(),
            ) {
                writeln!(&mut out, "{}", line)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        let method_names: Vec<String> = class_data
            .as_ref()
            .map(|cd| {
                cd.direct_methods
                    .iter()
                    .chain(cd.virtual_methods.iter())
                    .filter_map(|m| self.dex.get_method_info(m.method_idx).ok())
                    .map(|info| info.name.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let kt = kotlin::analyze_kotlin_class(
            self.dex.data.as_ref(),
            class_def,
            &class_name,
            &method_names,
            &|idx| self.dex.get_string(idx).ok(),
            &|idx| self.dex.get_type(idx).ok(),
        );
        if let Some(comment) = kt.comment_prefix() {
            writeln!(&mut out, "{}", comment)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        let class_sig = annotations::class_generic_signature(
            self.dex.data.as_ref(),
            class_def,
            &|idx| self.dex.get_string(idx).ok(),
            &|idx| self.dex.get_type(idx).ok(),
        )
        .and_then(|s| annotations::parse_class_signature(&s));
        let type_params = class_sig
            .as_ref()
            .and_then(|c| c.type_params.clone())
            .unwrap_or_default();
        let extends_ty = class_sig
            .as_ref()
            .and_then(|c| c.superclass.clone())
            .filter(|s| s != "java.lang.Object" && s != "Object")
            .unwrap_or_else(|| super_type.clone());
        let mut ifaces: Vec<String> = class_sig
            .as_ref()
            .map(|c| c.interfaces.clone())
            .unwrap_or_default();
        if ifaces.is_empty() {
            ifaces = self.class_interfaces(class_def);
        }
        let enum_constants = detect_enum_constants(
            self.dex,
            class_def,
            class_data.as_ref(),
            &class_name,
            &super_type,
        );
        let is_enum = !enum_constants.is_empty();

        for f in &flags {
            write!(&mut out, "{} ", f)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        let class_kw = if kt.is_data {
            "/* data */ class"
        } else if kt.is_companion {
            "/* companion */ class"
        } else if kt.is_coroutine {
            "/* coroutine */ class"
        } else {
            "class"
        };
        if is_enum {
            write!(&mut out, "enum {}{} ", simple_class_name, type_params)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        } else {
            write!(
                &mut out,
                "{} {}{}",
                class_kw, simple_class_name, type_params
            )
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            if !extends_ty.is_empty() && extends_ty != "java.lang.Object" && extends_ty != "Object"
            {
                write!(&mut out, " extends {}", shorten_java_names(&extends_ty))
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            if !ifaces.is_empty() {
                let iface_str = ifaces
                    .iter()
                    .map(|i| shorten_java_names(i))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(&mut out, " implements {}", iface_str)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        writeln!(&mut out, " {{").map_err(|_| DexDecompilerError::Decompilation("write".into()))?;

        if let Some(ref cd) = class_data {
            let static_inits = const_fields::parse_static_inits(self.dex, class_def, cd);
            let access: Vec<(u32, u32)> = cd
                .static_fields
                .iter()
                .map(|f| (f.field_idx, f.access_flags))
                .collect();
            *self.const_field_reps.borrow_mut() =
                const_fields::const_field_replacements(&class_name, &static_inits, &access);
            let methods: Vec<&EncodedMethod> = cd
                .direct_methods
                .iter()
                .chain(cd.virtual_methods.iter())
                .collect();
            *self.accessor_reps.borrow_mut() =
                accessors::build_accessor_replacements(self.dex, &class_name, &methods);
            let init_by_idx: HashMap<u32, &const_fields::StaticInit> =
                static_inits.iter().map(|s| (s.field_idx, s)).collect();

            if is_enum {
                writeln!(&mut out, "    {}", enum_constants.join(", "))
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                writeln!(&mut out, "    ;")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            for f in &cd.static_fields {
                if let Ok(fi) = self.dex.get_field_info(f.field_idx) {
                    if is_enum && enum_constants.contains(&fi.name.to_string()) {
                        continue;
                    }
                    let typ = self.field_type_java(class_def, f.field_idx, &fi.typ);
                    let name = fi.name;
                    let fflags = java::access_flags_to_java(f.access_flags, false);
                    write!(&mut out, "    ")
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    if !fflags.is_empty() {
                        write!(&mut out, "{} ", fflags.join(" "))
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                    if let Some(init) = init_by_idx.get(&f.field_idx) {
                        writeln!(&mut out, "{} {} = {};", typ, name, init.value_java)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    } else {
                        writeln!(&mut out, "{} {};", typ, name)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                }
            }
            let mut instance_fields: Vec<(String, String, String)> = Vec::new();
            for f in &cd.instance_fields {
                if let Ok(fi) = self.dex.get_field_info(f.field_idx) {
                    // Synthetic outer/captures — reconstructed via Outer.this / inlining.
                    if fi.name == "this$0" || fi.name.starts_with("val$") {
                        continue;
                    }
                    let typ = self.field_type_java(class_def, f.field_idx, &fi.typ);
                    let fflags = java::access_flags_to_java(f.access_flags, false);
                    instance_fields.push((fflags.join(" "), typ, fi.name.to_string()));
                }
            }
            let mut methods_java: Vec<String> = Vec::new();
            for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                if let Ok(info) = self.dex.get_method_info(m.method_idx) {
                    if accessors::should_skip_method_emit(&info.name, m.access_flags) {
                        continue;
                    }
                    if kotlin::should_hide_kotlin_method(&info.name, &kt) {
                        continue;
                    }
                }
                methods_java.push(self.decompile_method(
                    m,
                    Some(&simple_class_name),
                    Some(&class_name),
                )?);
            }
            // Jadx kotlin-metadata: Km property names (or data-class toString) → field aliases.
            let mut prop_names = kt.property_names.clone();
            if prop_names.is_empty() && kt.is_data {
                for mj in &methods_java {
                    if mj.contains(" toString(") || mj.contains("String toString(") {
                        let parsed = kotlin::parse_data_class_tostring_props(mj);
                        if !parsed.is_empty() {
                            prop_names = parsed;
                            break;
                        }
                    }
                }
            }
            if !prop_names.is_empty() {
                let field_name_list: Vec<String> =
                    instance_fields.iter().map(|(_, _, n)| n.clone()).collect();
                let field_renames =
                    kotlin::suggest_property_field_renames(&field_name_list, &prop_names);
                if !field_renames.is_empty() {
                    for (_, _, name) in &mut instance_fields {
                        if let Some((_, new)) = field_renames.iter().find(|(o, _)| o == name) {
                            *name = new.clone();
                        }
                    }
                    for mj in &mut methods_java {
                        *mj = kotlin::apply_field_alias_renames(mj, &field_renames);
                    }
                }
            }
            let field_names: HashSet<String> = instance_fields
                .iter()
                .map(|(_, _, name)| name.clone())
                .collect();
            let field_inits = hoist_common_ctor_field_inits(
                &mut methods_java,
                &simple_class_name,
                &field_names,
            );
            for (fflags, typ, name) in &instance_fields {
                write!(&mut out, "    ")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                if !fflags.is_empty() {
                    write!(&mut out, "{} ", fflags)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                }
                if let Some(expr) = field_inits.get(name) {
                    writeln!(&mut out, "{} {} = {};", typ, name, expr)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                } else {
                    writeln!(&mut out, "{} {};", typ, name)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                }
            }
            for method_java in &methods_java {
                // Blank line between members so methods are not glued together.
                writeln!(&mut out)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                write!(&mut out, "{}", method_java)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
            self.const_field_reps.borrow_mut().clear();
            self.accessor_reps.borrow_mut().clear();
        }

        // Nest direct named inner classes (Outer$Foo, not Outer$1).
        for nested_def in self.direct_named_inner_defs(&class_name)? {
            let nested_java = self.decompile_class_ex(&nested_def, true)?;
            for line in nested_java.lines() {
                writeln!(&mut out, "    {}", line)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }

        writeln!(&mut out, "}}").map_err(|_| DexDecompilerError::Decompilation("write".into()))?;

        // Apply user renames (package, class, method, field) via identifier-safe replacement.
        if let Some(ref r) = self.rename_map {
            let method_names: Vec<String> = class_data
                .as_ref()
                .map(|cd| {
                    cd.direct_methods
                        .iter()
                        .chain(cd.virtual_methods.iter())
                        .filter_map(|m| self.dex.get_method_info(m.method_idx).ok())
                        .map(|info| {
                            if info.name == "<init>" {
                                simple_class_name.clone()
                            } else {
                                info.name.to_string()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let field_names: Vec<String> = class_data
                .as_ref()
                .map(|cd| {
                    cd.static_fields
                        .iter()
                        .chain(cd.instance_fields.iter())
                        .filter_map(|f| self.dex.get_field_info(f.field_idx).ok())
                        .map(|fi| fi.name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let replacements = r.replacements_for_class(&class_name, &method_names, &field_names);
            out = r.apply_to_java(&out, &replacements);
        }
        if !as_member {
            out = insert_used_short_imports(self.dex, &out, &class_name, &package);
        }
        Ok(out)
    }

    /// Decompile one method: signature + body (disassembly-based Java-like body).
    /// When called from decompile_class, pass class_simple_name so constructors emit as "ClassName()" not "void <init>()".
    /// Pass class_name (full) when decompiling a class method so anonymous Thread inlining can resolve inner classes.
    pub fn decompile_method(
        &self,
        encoded: &EncodedMethod,
        class_simple_name: Option<&str>,
        class_name: Option<&str>,
    ) -> Result<String> {
        let info = self
            .dex
            .get_method_info(encoded.method_idx)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let mut return_type = java::descriptor_to_java(&info.return_type);
        let mut params: Vec<String> = info
            .params
            .iter()
            .map(|p| java::descriptor_to_java(p))
            .collect();
        let mut type_params = String::new();
        if let Some(cname) = class_name {
            if let Some(class_def) = self.find_class_def(cname) {
                if let Some(sig) = annotations::method_generic_signature(
                    self.dex.data.as_ref(),
                    &class_def,
                    encoded.method_idx,
                    &|idx| self.dex.get_string(idx).ok(),
                    &|idx| self.dex.get_type(idx).ok(),
                ) {
                    if let Some(msig) = annotations::signature_method_to_java(&sig) {
                        if let Some(tp) = msig.type_params {
                            type_params = tp;
                        }
                        if msig.params.len() == params.len() {
                            params = msig.params;
                        }
                        return_type = msig.return_type;
                    }
                }
            }
        }
        let flags = java::access_flags_to_java_kind(encoded.access_flags, java::MemberKind::Method);
        let mut out = String::new();
        write!(&mut out, "    ").map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        if !flags.is_empty() {
            write!(&mut out, "{} ", flags.join(" "))
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        let is_constructor = info.name == "<init>";
        let name = if is_constructor && class_simple_name.is_some() {
            class_simple_name.unwrap()
        } else {
            &info.name
        };
        let debug_pnames = self.debug_parameter_display_names(encoded, &params);
        let params_str = params
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pname = debug_pnames
                    .get(i)
                    .and_then(|p| p.clone())
                    .unwrap_or_else(|| type_infer::param_display_name(i, t));
                format!("{} {}", t, pname)
            })
            .collect::<Vec<_>>()
            .join(", ");
        if is_constructor && class_simple_name.is_some() {
            write!(&mut out, "{}({}) ", name, params_str)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        } else {
            // Java: `<T> ReturnType name(params)` — type params before return type.
            let tp = if type_params.is_empty() {
                String::new()
            } else {
                format!("{} ", type_params)
            };
            write!(&mut out, "{}{} {}({}) ", tp, return_type, name, params_str)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        if flags.contains(&"abstract") || flags.contains(&"native") {
            writeln!(&mut out, ";")
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            return Ok(out);
        }
        if encoded.code_off == 0 {
            writeln!(&mut out, "{{ }}")
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            return Ok(out);
        }
        let code = self
            .dex
            .get_code_item(encoded.code_off)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        // Raw DEX instructions as comments before the method body (only when requested).
        if self.show_bytecode {
            let raw_listing = self.raw_dex_instructions_listing(&code)?;
            if !raw_listing.is_empty() {
                writeln!(&mut out)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                write!(&mut out, "{}", raw_listing)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        let body = self.decompile_method_body(&code, encoded, class_name)?;
        writeln!(&mut out, "{{").map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        write!(&mut out, "{}", body)
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        if !body.ends_with('\n') {
            writeln!(&mut out).map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        writeln!(&mut out, "    }}")
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        Ok(promote_this_synchronized(&out))
    }

    /// Bytecode rows and CFG graph for a method (for web UI / visualization).
    pub fn get_method_bytecode_and_cfg(
        &self,
        encoded: &EncodedMethod,
    ) -> Result<(Vec<MethodBytecodeRow>, Vec<CfgNodeInfo>, Vec<CfgEdgeInfo>)> {
        if encoded.code_off == 0 {
            return Ok((vec![], vec![], vec![]));
        }
        let code = self
            .dex
            .get_code_item(encoded.code_off)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let insns_bytes = code.insns_slice(&*self.dex.data);
        let base_offset = 0usize;
        let instructions = decode_all(insns_bytes, base_offset)
            .map_err(|e| DexDecompilerError::Disassembly(e.to_string()))?;
        let base_off = code.insns_off;
        let rows: Vec<MethodBytecodeRow> = instructions
            .iter()
            .map(|ins| {
                let offset = (ins.offset as usize + base_off) as u32;
                let operands = self.resolve_operands(ins.operands());
                MethodBytecodeRow {
                    offset,
                    mnemonic: ins.mnemonic().to_string(),
                    operands,
                }
            })
            .collect();
        let condition_for = |ins: &Instruction| {
            let ops = ins.operands();
            let resolved = self.resolve_operands(ops);
            format_condition(ins.mnemonic(), &resolved)
        };
        let cfg = MethodCfg::build(&instructions, insns_bytes, base_offset, &condition_for);
        let base_off_u32 = base_off as u32;
        let nodes: Vec<CfgNodeInfo> = cfg
            .blocks
            .iter()
            .enumerate()
            .map(|(id, block)| {
                let label = block
                    .instruction_offsets
                    .first()
                    .and_then(|&off| {
                        instructions
                            .iter()
                            .find(|i| (i.offset as usize) + base_offset == off as usize)
                    })
                    .map(|ins| {
                        let ops = self.resolve_operands(ins.operands());
                        let abs_off = block.start_offset + base_off_u32;
                        format!("0x{:04x}: {} {}", abs_off, ins.mnemonic(), ops)
                    })
                    .unwrap_or_else(|| {
                        let start_abs = block.start_offset + base_off_u32;
                        let end_abs = block.end_offset.wrapping_add(base_off_u32);
                        format!("B{} (0x{:04x}-0x{:04x})", id, start_abs, end_abs)
                    });
                let start_offset = block.start_offset + base_off_u32;
                let end_offset = if block.end_offset == u32::MAX {
                    u32::MAX
                } else {
                    block.end_offset + base_off_u32
                };
                CfgNodeInfo {
                    id,
                    start_offset,
                    end_offset,
                    label,
                }
            })
            .collect();
        let edges: Vec<CfgEdgeInfo> = cfg
            .successor_edges()
            .into_iter()
            .map(|(from_id, to_id)| CfgEdgeInfo { from_id, to_id })
            .collect();
        Ok((rows, nodes, edges))
    }

    /// Find the first ClassDef (and owning DEX slot) for the given class name.
    fn find_class_def_slot(&self, class_name: &str) -> Option<(usize, ClassDef)> {
        let _ = self.ensure_class_def_index();
        self.class_def_index
            .borrow()
            .as_ref()?
            .get(class_name)
            .cloned()
    }

    /// Find the first ClassDef in the primary/extra DEX set for the given class name.
    fn find_class_def(&self, class_name: &str) -> Option<ClassDef> {
        self.find_class_def_slot(class_name).map(|(_, cd)| cd)
    }

    /// Find EncodedMethod by method_ids index (any class) on the primary DEX.
    pub fn find_method_by_idx(&self, method_idx: u32) -> Option<EncodedMethod> {
        let info = self.dex.get_method_info(method_idx).ok()?;
        let class_name = java::descriptor_to_java(&info.class);
        let (slot, class_def) = self.find_class_def_slot(&class_name)?;
        let dex = self.dex_at(slot);
        let class_data_opt = dex.get_class_data(&class_def).ok()?;
        let class_data = class_data_opt.as_ref()?;
        for encoded in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            if encoded.method_idx == method_idx {
                return Some(encoded.clone());
            }
        }
        None
    }

    /// Build `(params) -> { body }` / expression lambda from invoke-custom operands.
    fn format_lambda_expression(&self, raw_ops: &str) -> String {
        let captures = extract_brace_regs(raw_ops);
        let Some(idx) = extract_callsite_idx(raw_ops) else {
            return format_invoke_custom_stmt(self.dex, raw_ops);
        };
        let Ok(info) = self.dex.get_call_site(idx) else {
            return format_invoke_custom_stmt(self.dex, raw_ops);
        };
        let Some(mid) = dex_parser::DexCallSites::impl_method_id(&info) else {
            return format_invoke_custom_stmt(self.dex, raw_ops);
        };
        {
            let mut depth = self.lambda_inline_depth.borrow_mut();
            if *depth >= 4 {
                return format_invoke_custom_stmt(self.dex, raw_ops);
            }
            *depth += 1;
        }
        let out = self
            .try_format_lambda_with_body(mid as u32, &info.method_name, &captures)
            .unwrap_or_else(|| format_invoke_custom_stmt(self.dex, raw_ops));
        *self.lambda_inline_depth.borrow_mut() -= 1;
        out
    }

    fn try_format_lambda_with_body(
        &self,
        method_idx: u32,
        sam_name: &str,
        captures: &[String],
    ) -> Option<String> {
        let encoded = self.find_method_by_idx(method_idx)?;
        let info = self.dex.get_method_info(method_idx).ok()?;
        let class = java::descriptor_to_java(&info.class);
        let is_static = (encoded.access_flags & 0x8) != 0;
        let param_start = if is_static { 0 } else { 1 };

        // Method reference: named method (not lambda$/access$), prefer `Type::name` / `recv::name`.
        let is_lambda_synth = info.name.contains("lambda$")
            || info.name.starts_with("access$")
            || info.name == "<init>";
        if !is_lambda_synth {
            if is_static && captures.is_empty() {
                return Some(format!("{}::{}", shorten_java_names(&class), info.name));
            }
            if !is_static && captures.len() == 1 {
                return Some(format!("{}::{}", captures[0], info.name));
            }
            if is_static && captures.len() == 1 {
                // Bound static / capturing — fall through to body inline.
            } else if captures.is_empty() {
                return Some(format!("{}::{}", shorten_java_names(&class), info.name));
            }
        }

        if encoded.code_off == 0 {
            return None;
        }
        let cache_key = format!("lambda@{}", method_idx);
        let mut body = {
            let mut cache = self.inner_run_body_cache.borrow_mut();
            if let Some(cached) = cache.get(&cache_key) {
                cached.clone()
            } else {
                let code = self.dex.get_code_item(encoded.code_off).ok()?;
                let b = self.decompile_method_body(&code, &encoded, None).ok()?;
                cache.insert(cache_key, b.clone());
                b
            }
        };
        // Substitute capture params with outer register names.
        for (i, cap) in captures.iter().enumerate() {
            let from = format!("p{}", param_start + i);
            // Also try type-based names after rename.
            body = replace_ident_in_body(&body, &from, cap);
            let t = info
                .params
                .get(param_start + i)
                .map(|p| java::descriptor_to_java(p))
                .unwrap_or_default();
            let typed = type_infer::param_display_name(i, &t);
            if typed != from {
                body = replace_ident_in_body(&body, &typed, cap);
            }
        }
        let lambda_types: Vec<String> = info
            .params
            .iter()
            .skip(param_start + captures.len())
            .map(|p| java::descriptor_to_java(p))
            .collect();
        let lambda_params: Vec<String> = lambda_types
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let pname = type_infer::param_display_name(i, t);
                let short_t = shorten_java_names(t);
                // Rewrite body pN → pname if needed.
                let old = format!("p{}", param_start + captures.len() + i);
                if old != pname {
                    body = replace_ident_in_body(&body, &old, &pname);
                }
                if short_t == "int"
                    || short_t == "long"
                    || short_t == "boolean"
                    || short_t == "double"
                    || short_t == "float"
                    || short_t == "byte"
                    || short_t == "short"
                    || short_t == "char"
                    || short_t == "String"
                    || short_t.contains('.')
                {
                    format!("{} {}", short_t, pname)
                } else {
                    pname
                }
            })
            .collect();

        let header = match lambda_params.len() {
            0 => "()".to_string(),
            1 => lambda_params[0].clone(),
            _ => format!("({})", lambda_params.join(", ")),
        };

        if let Some(expr) = try_lambda_expression_body(&body) {
            return Some(format!("/* {} */ {} -> {}", sam_name, header, expr));
        }

        let mut out = format!("/* {} */ {} -> {{\n", sam_name, header);
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            out.push_str("    ");
            out.push_str(line.trim_start());
            out.push('\n');
        }
        out.push('}');
        Some(out)
    }

    /// Find the first EncodedMethod in the DEX for the given class and method name.
    pub fn find_method(&self, class_name: &str, method_name: &str) -> Option<EncodedMethod> {
        let (slot, class_def) = self.find_class_def_slot(class_name)?;
        let dex = self.dex_at(slot);
        let class_data_opt = dex.get_class_data(&class_def).ok()?;
        let class_data = class_data_opt.as_ref()?;
        for encoded in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            let info = dex.get_method_info(encoded.method_idx).ok()?;
            if info.name == method_name {
                return Some(encoded.clone());
            }
        }
        None
    }

    /// Value-flow / tainting: build CFG and per-instruction read/write map for a method.
    /// Use `.analysis().value_flow_from_seed(offset, reg)` to get all reads/writes of a value.
    pub fn value_flow_analysis(&self, encoded: &EncodedMethod) -> Result<ValueFlowAnalysisOwned> {
        if encoded.code_off == 0 {
            return Err(DexDecompilerError::Decompilation(
                "value_flow_analysis: method has no code".into(),
            ));
        }
        let code = self
            .dex
            .get_code_item(encoded.code_off)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let insns_bytes = code.insns_slice(&*self.dex.data);
        let base_offset = 0usize;
        let instructions = decode_all(insns_bytes, base_offset)
            .map_err(|e| DexDecompilerError::Disassembly(e.to_string()))?;
        let condition_for = |ins: &Instruction| {
            let resolved = self.resolve_operands(ins.operands());
            format_condition(ins.mnemonic(), &resolved)
        };
        // CFG and rw_map use the same offset space (relative to method code start: 0, 2, 4, ...).
        let mut cfg = MethodCfg::build(&instructions, insns_bytes, base_offset, &condition_for);
        let mut exceptional_edges = Vec::new();
        if code.tries_size > 0 {
            if let Some(pairs) = try_handler_pairs(&self.dex.data, encoded.code_off, &code) {
                let mut exception_ranges = Vec::new();
                let mut handler_leaders = Vec::new();
                for (try_item, handler) in pairs {
                    let try_start = try_item.start_addr * 2;
                    let try_end = (try_item.start_addr + try_item.insn_count as u32) * 2;
                    let mut handler_starts: Vec<u32> = handler
                        .handlers
                        .iter()
                        .map(|entry| entry.addr * 2)
                        .collect();
                    if let Some(catch_all) = handler.catch_all_addr {
                        handler_starts.push(catch_all * 2);
                    }
                    handler_leaders.extend(handler_starts.iter().copied());
                    exception_ranges.push((try_start, try_end, handler_starts));
                }
                cfg.split_at_offsets(&handler_leaders);
                for (try_start, try_end, handler_starts) in exception_ranges {
                    exceptional_edges.extend(cfg.exceptional_edges_for_range(
                        try_start,
                        try_end,
                        &handler_starts,
                    ));
                }
                exceptional_edges.sort_unstable();
                exceptional_edges.dedup();
            }
        }
        let rw_map = build_instruction_rw_map(&instructions, base_offset as u32, |ops| {
            self.resolve_operands(ops)
        });
        let api_return_sources =
            build_api_return_sources(&instructions, base_offset as u32, |ops| {
                self.resolve_operands(ops)
            });
        let mut invoke_method_map =
            build_invoke_method_map(&instructions, base_offset as u32, |ops| {
                self.resolve_operands(ops)
            });
        // invoke-custom references call_site_ids rather than method_ids. Resolve the
        // lambda implementation handle so taint/call-graph analysis can follow
        // captured values into the callback body.
        for ins in &instructions {
            if !matches!(ins.opcode, 0xfc | 0xfd) {
                continue;
            }
            let Some(callsite_idx) = extract_callsite_idx(ins.operands()) else {
                continue;
            };
            let Ok(callsite) = self.dex.get_call_site(callsite_idx) else {
                continue;
            };
            let Some(method_idx) = dex_parser::DexCallSites::impl_method_id(&callsite) else {
                continue;
            };
            let Ok(method) = self.dex.get_method_info(method_idx as u32) else {
                continue;
            };
            invoke_method_map.insert(
                (ins.offset as u32).wrapping_add(base_offset as u32),
                format!(
                    "{}.{}",
                    java::descriptor_to_java(&method.class),
                    method.name
                ),
            );
        }
        let insn_at = build_insn_labels(&instructions, base_offset as u32, |ops| {
            self.resolve_operands(ops)
        });
        Ok(ValueFlowAnalysisOwned {
            cfg,
            rw_map,
            exceptional_edges,
            api_return_sources,
            invoke_method_map,
            insn_at,
            registers_size: code.registers_size as u32,
            ins_size: code.ins_size as u32,
        })
    }

    /// Raw bytes for instruction at index `idx` in `instructions` (from its offset to next instruction or end of code).
    /// When `instructions` is a block subset and has no next element, pass `full_instructions` so the next
    /// instruction's offset in the method is used (avoids including the rest of the method as "one instruction").
    fn instruction_raw_hex(
        instructions: &[Instruction],
        idx: usize,
        code_insns: &[u8],
        full_instructions: Option<&[Instruction]>,
    ) -> String {
        let ins = match instructions.get(idx) {
            Some(i) => i,
            None => return String::new(),
        };
        let start = ins.offset as usize;
        let end = instructions
            .get(idx + 1)
            .map(|n| n.offset as usize)
            .or_else(|| {
                full_instructions.and_then(|full| {
                    full.iter()
                        .find(|n| n.offset as usize > start)
                        .map(|n| n.offset as usize)
                })
            })
            .unwrap_or_else(|| code_insns.len());
        let slice = code_insns.get(start..end).unwrap_or(&[]);
        slice
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Raw DEX instructions line by line as comments (before the method body).
    /// Format: // [id] offset (raw_hex): mnemonic operands
    fn raw_dex_instructions_listing(&self, code: &CodeItem) -> Result<String> {
        let insns_bytes = code.insns_slice(&*self.dex.data);
        let instructions = decode_all(insns_bytes, 0)
            .map_err(|e| DexDecompilerError::Disassembly(e.to_string()))?;
        let base_off = code.insns_off;
        let mut out = String::new();
        for (idx, ins) in instructions.iter().enumerate() {
            let offset = ins.offset as usize + base_off;
            let m = ins.mnemonic();
            let mut ops = self.resolve_operands(ins.operands());
            let raw_hex = Self::instruction_raw_hex(&instructions, idx, insns_bytes, None);
            if m == "goto" {
                ops = format_goto_operands_signed(&raw_hex, &ops);
            }
            writeln!(
                out,
                "    // [{}] {:04x} ({}): {} {}",
                idx, offset, raw_hex, m, ops
            )
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        if !instructions.is_empty() {
            writeln!(out).map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        Ok(out)
    }

    /// Decompile method body: build CFG, emit structured Java (if/else, while).
    fn decompile_method_body(
        &self,
        code: &CodeItem,
        encoded: &EncodedMethod,
        class_name: Option<&str>,
    ) -> Result<String> {
        let insns_bytes = code.insns_slice(&*self.dex.data);
        let base_offset = 0usize;
        let instructions = decode_all(insns_bytes, base_offset)
            .map_err(|e| DexDecompilerError::Disassembly(e.to_string()))?;
        if instructions.is_empty() {
            return Ok("        // (no instructions)\n".to_string());
        }

        *self.method_return_type.borrow_mut() = self
            .dex
            .get_method_info(encoded.method_idx)
            .ok()
            .map(|info| java::descriptor_to_java(&info.return_type))
            .filter(|t| t != "void");

        self.load_method_debug_types(code);

        if self.mode == DecompilationMode::Fallback {
            return self.decompile_method_body_linear(
                &instructions,
                code.insns_off,
                encoded,
                code,
                insns_bytes,
                class_name,
            );
        }

        let condition_for = |ins: &Instruction| {
            let ops = ins.operands();
            let resolved = self.resolve_operands(ops);
            format_condition(ins.mnemonic(), &resolved)
        };
        let mut cfg = MethodCfg::build(&instructions, insns_bytes, base_offset, &condition_for);
        Self::fold_constants_into_conditions(&mut cfg, &instructions, Some(self.dex));
        self.fold_field_reads_into_conditions(&mut cfg, &instructions);
        self.fold_aget_into_switch_conditions(&mut cfg, &instructions);
        self.prepare_method_reg_types(
            &instructions,
            code.insns_off,
            encoded,
            code,
            insns_bytes,
            class_name,
        );
        self.rename_condition_registers(&mut cfg, &instructions, encoded, code);
        self.rename_switch_conditions(&mut cfg);
        if cfg.block_count() == 0 {
            return self.decompile_method_body_linear(
                &instructions,
                code.insns_off,
                encoded,
                code,
                insns_bytes,
                class_name,
            );
        }

        if self.mode == DecompilationMode::Simple {
            return self.decompile_method_body_simple(
                &cfg,
                &instructions,
                code,
                encoded,
                class_name,
            );
        }

        // Method-level φ analysis for shared naming across branches (restructure path).
        self.refresh_phi_regs(&cfg, &instructions, code, encoded);

        let global_used_regs =
            self.method_used_regs(&cfg, &instructions, code.insns_off, insns_bytes);
        let is_constructor = self
            .dex
            .get_method_info(encoded.method_idx)
            .map(|info| info.name == "<init>")
            .unwrap_or(false);

        let mut out = if code.tries_size > 0 {
            match self.emit_all_try_catch(
                &cfg,
                &instructions,
                code,
                encoded,
                &global_used_regs,
                class_name,
            ) {
                Ok(s) if !s.trim().is_empty() => Some(s),
                _ => None,
            }
        } else {
            None
        };

        if out.is_none() {
            let mut fallback = String::new();
            let mut declared = HashSet::new();
            self.seed_declared_from_params(&mut declared, code, encoded);
            if let Some(root) = build_regions(&cfg, cfg.entry) {
                self.emit_region(
                    &root,
                    &cfg,
                    &instructions,
                    code.insns_off,
                    encoded,
                    code,
                    &mut fallback,
                    2,
                    None,
                    None,
                    &mut declared,
                    Some(&global_used_regs),
                    None,
                    class_name,
                )?;
            }
            out = Some(fallback);
        }

        let mut out = out.unwrap_or_else(|| "        // (no instructions)\n".to_string());
        if !out.trim().is_empty() && out != "        // (no instructions)\n" {
            if std::env::var_os("DUMP_PRE_SIMPLIFY").is_some() && out.contains("bfsShortestPath")
            {
                eprintln!("=== PRE-SIMPLIFY bfs ===\n{out}\n=== END PRE-SIMPLIFY ===");
            }
            out = simplify::simplify_method_body(&out, is_constructor);
            out = self.wrap_caught_postinc_div(&out, &instructions, encoded, code);
            out = simplify::restore_string_switch(&out);
            if let Some(enclosing) = class_name {
                out = self.inline_anonymous_classes(&out, enclosing)?;
            }
            let reps = self.const_field_reps.borrow();
            if !reps.is_empty() {
                out = const_fields::apply_const_field_replacements(&out, &reps);
            }
            if !self.resource_reps.is_empty() {
                out = resource_consts::apply_resource_replacements(&out, &self.resource_reps);
            }
            {
                let acc = self.accessor_reps.borrow();
                if !acc.is_empty() {
                    out = accessors::apply_accessor_replacements(&out, &acc);
                }
            }
            out = accessors::polish_field_accessor_calls(&out);
            let method_name = self
                .dex
                .get_method_info(encoded.method_idx)
                .map(|i| i.name.to_string())
                .unwrap_or_default();
            if method_name == "invokeSuspend" {
                out = kotlin::restore_coroutine_invoke_suspend(&out);
            }
            out = kotlin::restore_kotlin_idioms(&out);
        }
        const CATCH_BLOCK_MARKER: &str = "} catch (";
        const FINALLY_MARKER: &str = "} finally {";
        const TRY_WITH_RESOURCES_MARKER: &str = "try (";
        if code.tries_size > 0
            && !out.contains(CATCH_BLOCK_MARKER)
            && !out.contains(FINALLY_MARKER)
            && !out.contains(TRY_WITH_RESOURCES_MARKER)
        {
            out = self.wrap_body_with_try_catch(&out, encoded.code_off, code)?;
        }
        out = simplify::simplify_synchronized_blocks(&out);
        // Structured emission sometimes drops trailing return blocks (string-switch
        // packed-switch tails). Synthesize a return from a live `result` local.
        {
            let rt = self.method_return_type.borrow();
            if rt.as_deref().is_some_and(|t| t != "void")
                && cfg.has_return_block()
                && !out.lines().any(|l| l.trim().starts_with("return"))
            {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                if out.contains("int result") || out.contains("result =") {
                    out.push_str("        return result;\n");
                } else if out.contains("case \"") {
                    out.push_str("        return -1;\n");
                } else if self.show_bytecode {
                    out.push_str(
                        "        // decompiler-note: CFG contains return block but emission has no return\n",
                    );
                }
            }
        }
        Ok(out)
    }

    /// Inline anonymous inner classes (Thread, Runnable, and other `Outer$N` synthetics).
    fn inline_anonymous_classes(&self, body: &str, enclosing_class: &str) -> Result<String> {
        let lines: Vec<&str> = body.lines().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let stmt = line.trim().trim_end_matches(|c| c == ' ' || c == '\t');
            let stmt_clean = if let Some(comment) = stmt.find("  // ") {
                stmt[..comment].trim_end()
            } else {
                stmt
            };
            if let Some(open) = stmt_clean.find(".<init>(") {
                let receiver = stmt_clean[..open].trim();
                let close = stmt_clean.find(");").unwrap_or(stmt_clean.len());
                let args_str = stmt_clean[open + ".<init>(".len()..close].trim();
                let args: Vec<String> = if args_str.is_empty() {
                    vec![]
                } else {
                    args_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                let next_line = lines
                    .get(i + 1)
                    .map(|l| {
                        let t = l.trim();
                        if let Some(c) = t.find("  // ") {
                            t[..c].trim_end()
                        } else {
                            t
                        }
                    })
                    .unwrap_or("");
                if !receiver.is_empty()
                    && receiver
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    // Thread: <init>; start();
                    if next_line == format!("{}.start();", receiver) {
                        if let Some((replacement, skip)) =
                            self.build_anonymous_thread_inline(enclosing_class, &args, line)?
                        {
                            out.push_str(&replacement);
                            if !replacement.ends_with('\n') {
                                out.push('\n');
                            }
                            i += skip;
                            continue;
                        }
                    }
                    // General SAM / anonymous: <init>; then any use — try Runnable-style inline.
                    if let Some((replacement, skip)) =
                        self.build_anonymous_sam_inline(enclosing_class, &args, line, receiver)?
                    {
                        // Only replace the <init> line; leave subsequent uses (they still
                        // reference the receiver variable — best-effort readability).
                        out.push_str(&replacement);
                        if !replacement.ends_with('\n') {
                            out.push('\n');
                        }
                        i += skip;
                        continue;
                    }
                }
            }
            // invoke-custom / lambda stub already appears as comments from resolve_one.
            out.push_str(line);
            if i < lines.len().saturating_sub(1) {
                out.push('\n');
            }
            i += 1;
        }
        Ok(out)
    }

    /// Build `new SuperType() { public void sam() { ... } };` for non-Thread anonymous classes.
    fn build_anonymous_sam_inline(
        &self,
        enclosing_class: &str,
        args: &[String],
        first_line: &str,
        _receiver: &str,
    ) -> Result<Option<(String, usize)>> {
        self.ensure_inner_anon_index()?;
        let index = self.inner_anon_index.borrow();
        let Some(list) = index.as_ref().and_then(|m| m.get(enclosing_class)) else {
            return Ok(None);
        };
        let Some((inner_class_name, _nparams, super_name)) =
            list.iter().find(|(_, n, _)| *n == args.len())
        else {
            return Ok(None);
        };
        // Prefer known SAM method names.
        const SAM_NAMES: &[&str] = &[
            "run",
            "onClick",
            "accept",
            "apply",
            "test",
            "call",
            "compare",
            "onReceive",
        ];
        let mut sam_method = None;
        for &name in SAM_NAMES {
            if self.find_method(inner_class_name, name).is_some() {
                sam_method = Some(name.to_string());
                break;
            }
        }
        if sam_method.is_none() {
            // Fall back to the first non-init virtual/direct method with code.
            if let Some((slot, class_def)) = self.find_class_def_slot(inner_class_name) {
                let dex = self.dex_at(slot);
                if let Ok(Some(cd)) = dex.get_class_data(&class_def) {
                    for enc in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                        if enc.code_off == 0 {
                            continue;
                        }
                        if let Ok(info) = dex.get_method_info(enc.method_idx) {
                            if info.name != "<init>" && info.name != "<clinit>" {
                                sam_method = Some(info.name.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
        let Some(sam_name) = sam_method else {
            return Ok(None);
        };
        let Some(run_encoded) = self.find_method(inner_class_name, &sam_name) else {
            return Ok(None);
        };
        if run_encoded.code_off == 0 {
            return Ok(None);
        }
        let indent = first_line.len() - first_line.trim_start().len();
        let indent_str: String = first_line.chars().take(indent).collect();
        let mut body = {
            let mut cache = self.inner_run_body_cache.borrow_mut();
            let cache_key = format!("{inner_class_name}#{sam_name}");
            if let Some(cached) = cache.get(&cache_key) {
                cached.clone()
            } else {
                let code = self
                    .dex
                    .get_code_item(run_encoded.code_off)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let b = self.decompile_method_body(&code, &run_encoded, None)?;
                cache.insert(cache_key, b.clone());
                b
            }
        };
        let val_replacements = self.inner_class_capture_map(inner_class_name, args)?;
        for (field_name, arg) in &val_replacements {
            body = replace_capture_in_body(&body, field_name, arg);
        }
        body = replace_capture_assignees_in_body(&body, &val_replacements);
        let sam_info = self
            .dex
            .get_method_info(run_encoded.method_idx)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let ret = java::descriptor_to_java(&sam_info.return_type);
        let ret_short = shorten_java_names(&ret);
        let params: Vec<String> = sam_info
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let t = shorten_java_names(&java::descriptor_to_java(p));
                let n = type_infer::param_display_name(i, &java::descriptor_to_java(p));
                format!("{t} {n}")
            })
            .collect();
        let param_list = params.join(", ");
        let super_short = shorten_java_names(super_name);
        let mut replacement = format!("{indent_str}new {}() {{\n", super_short);
        replacement.push_str(&format!(
            "{indent_str}    public {ret_short} {sam_name}({param_list}) {{\n"
        ));
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            replacement.push_str(&indent_str);
            replacement.push_str("        ");
            replacement.push_str(line.trim_start());
            replacement.push('\n');
        }
        replacement.push_str(&format!("{indent_str}    }}\n"));
        replacement.push_str(&format!("{indent_str}}};"));
        Ok(Some((replacement, 1)))
    }

    fn ensure_inner_anon_index(&self) -> Result<()> {
        if self.inner_anon_index.borrow().is_some() {
            return Ok(());
        }
        let mut map: HashMap<String, Vec<(String, usize, String)>> = HashMap::new();
        let mut ingest = |dex: &DexFile| -> Result<()> {
            for class_def_result in dex.class_defs() {
                let class_def =
                    class_def_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let class_type = dex
                    .get_type(class_def.class_idx)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let name = java::descriptor_to_java(&class_type);
                let Some((enclosing, suffix)) = name.rsplit_once('$') else {
                    continue;
                };
                if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let super_name = if class_def.superclass_idx != NO_INDEX {
                    let s = dex
                        .get_type(class_def.superclass_idx)
                        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                    java::descriptor_to_java(&s)
                } else {
                    "Object".into()
                };
                if super_name == "java.lang.Thread" || super_name == "Thread" {
                    continue;
                }
                let class_data_opt = dex
                    .get_class_data(&class_def)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let Some(ref class_data) = class_data_opt.as_ref() else {
                    continue;
                };
                let mut ctor_params = 0usize;
                for enc in &class_data.direct_methods {
                    let info = dex
                        .get_method_info(enc.method_idx)
                        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                    if info.name == "<init>" {
                        ctor_params = info.params.len();
                        break;
                    }
                }
                map.entry(enclosing.to_string())
                    .or_default()
                    .push((name, ctor_params, super_name));
            }
            Ok(())
        };
        ingest(self.dex)?;
        for d in &self.extra_dexes {
            ingest(d)?;
        }
        *self.inner_anon_index.borrow_mut() = Some(map);
        Ok(())
    }

    /// Inline anonymous Thread: replace "X.<init>(args);" + "X.start();" with
    /// "new Thread() { public void run() { <inner run body> } }.start();"
    #[allow(dead_code)]
    fn inline_anonymous_threads(&self, body: &str, enclosing_class: &str) -> Result<String> {
        // Kept for compatibility; prefer [`inline_anonymous_classes`].
        self.inline_anonymous_classes(body, enclosing_class)
    }

    /// Build replacement for anonymous Thread and number of lines to skip (2 or 3 if assignment line included).
    fn build_anonymous_thread_inline(
        &self,
        enclosing_class: &str,
        args: &[String],
        first_line: &str,
    ) -> Result<Option<(String, usize)>> {
        let indent = first_line.len() - first_line.trim_start().len();
        let indent_str: String = first_line.chars().take(indent).collect();
        let Some(inner_class_name) = self.find_inner_thread_class(enclosing_class, args.len())?
        else {
            return Ok(None);
        };
        let run_encoded = match self.find_method(&inner_class_name, "run") {
            Some(enc) => enc,
            None => return Ok(None),
        };
        if run_encoded.code_off == 0 {
            return Ok(None);
        }
        let mut run_java = {
            let mut cache = self.inner_run_body_cache.borrow_mut();
            if let Some(cached) = cache.get(&inner_class_name) {
                cached.clone()
            } else {
                let code = self
                    .dex
                    .get_code_item(run_encoded.code_off)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
                let body = self.decompile_method_body(&code, &run_encoded, None)?;
                cache.insert(inner_class_name.clone(), body.clone());
                body
            }
        };
        let val_replacements = self.inner_class_capture_map(&inner_class_name, args)?;
        for (field_name, arg) in &val_replacements {
            run_java = replace_capture_in_body(&run_java, field_name, arg);
        }
        run_java = replace_capture_assignees_in_body(&run_java, &val_replacements);
        if let Some(first_arg) = args.first() {
            run_java = replace_synchronized_lock_with_arg(&run_java, first_arg);
        }
        if args.len() == 2 {
            if let Some(second_arg) = args.get(1) {
                run_java = replace_whole_word(&run_java, "v1", second_arg);
            }
        }
        run_java = strip_unreachable_exception_junk_after_return(&run_java);
        let run_indent = format!("{}    ", indent_str);
        let run_lines: String = run_java
            .lines()
            .map(|l| {
                let trimmed = l.trim_start();
                if trimmed.is_empty() {
                    run_indent.clone()
                } else {
                    format!("{}{}", run_indent, l.trim())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let block = format!(
            "{}new Thread() {{\n{}    public void run() {{\n{}\n{}    }}\n{}}}.start();",
            indent_str, indent_str, run_lines, indent_str, indent_str,
        );
        Ok(Some((block, 2)))
    }

    /// Find inner class (enclosing_class$N) that extends Thread and has constructor with given arity.
    fn find_inner_thread_class(
        &self,
        enclosing_class: &str,
        num_args: usize,
    ) -> Result<Option<String>> {
        self.ensure_inner_thread_index()?;
        let inner = self
            .inner_thread_index
            .borrow()
            .as_ref()
            .and_then(|m| m.get(enclosing_class))
            .and_then(|v| v.iter().find(|(_, n)| *n == num_args))
            .map(|(name, _)| name.clone());
        Ok(inner)
    }

    /// Map this$0 / val$* field names to outer arg names for inlining.
    fn inner_class_capture_map(
        &self,
        inner_class_name: &str,
        args: &[String],
    ) -> Result<Vec<(String, String)>> {
        let Some((slot, class_def)) = self.find_class_def_slot(inner_class_name) else {
            return Ok(vec![]);
        };
        let dex = self.dex_at(slot);
        let class_data_opt = dex
            .get_class_data(&class_def)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let class_data = class_data_opt.as_ref();
        let Some(cd) = class_data else {
            return Ok(vec![]);
        };
        let fields: Vec<(String, bool)> = cd
            .instance_fields
            .iter()
            .filter_map(|f| dex.get_field_info(f.field_idx).ok())
            .filter(|fi| fi.name == "this$0" || fi.name.starts_with("val$"))
            .map(|fi| (fi.name.clone(), fi.name == "this$0"))
            .collect();
        let mut out = Vec::with_capacity(fields.len());
        let mut arg_i = 0usize;
        for (name, is_outer) in fields {
            let Some(arg) = args.get(arg_i) else {
                break;
            };
            arg_i += 1;
            if is_outer {
                let replacement = if let Some((outer, _)) = inner_class_name.rsplit_once('$') {
                    let short = shorten_java_names(outer);
                    let simple = short.rsplit('.').next().unwrap_or(short.as_str());
                    if arg == "this" || arg == simple {
                        format!("{}.this", simple)
                    } else {
                        arg.clone()
                    }
                } else {
                    arg.clone()
                };
                out.push((name, replacement));
            } else {
                out.push((name, arg.clone()));
            }
        }
        Ok(out)
    }

    /// Emit all try/catch/finally regions in a method (sorted by try start).
    fn wrap_caught_postinc_div(
        &self,
        body: &str,
        instructions: &[Instruction],
        encoded: &EncodedMethod,
        code: &CodeItem,
    ) -> String {
        if body.contains("} catch (") || code.tries_size == 0 {
            return body.to_string();
        }
        let Some(pairs) = try_handler_pairs(self.dex.data.as_ref(), encoded.code_off, code) else {
            return body.to_string();
        };
        // Prefer continue-style handlers (foo2/foobis); also accept const-only
        // handlers (foo4: `i = 10`) when the body has a postinc-division.
        let Some((_, handler)) = pairs
            .iter()
            .find(|(_, handler)| {
                handler
                    .handlers
                    .iter()
                    .any(|h| handler_goto_continues(instructions, h.addr * 2))
            })
            .or_else(|| {
                if body.contains("++ / ") {
                    pairs.iter().find(|(_, handler)| {
                        handler.handlers.iter().any(|h| {
                            handler_const_literal(instructions, h.addr * 2).is_some()
                        })
                    })
                } else {
                    None
                }
            })
        else {
            return body.to_string();
        };
        let Some(typed) = handler.handlers.first() else {
            return body.to_string();
        };
        let Ok(desc) = self.dex.get_type(typed.type_idx) else {
            return body.to_string();
        };
        let exc = shorten_java_names(&java::descriptor_to_java(&desc));
        let rhs = handler_const_literal(instructions, typed.addr * 2).unwrap_or_else(|| "10".into());
        let continues = handler_goto_targets_loop_if(instructions, typed.addr * 2);
        simplify::wrap_postinc_div_try(body, &exc, &rhs, continues)
    }

    fn emit_all_try_catch(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        code: &CodeItem,
        encoded: &EncodedMethod,
        global_used_regs: &HashSet<u32>,
        class_name: Option<&str>,
    ) -> Result<String> {
        let data = self.dex.data.as_ref();
        let Some(mut pairs) = try_handler_pairs(data, encoded.code_off, code) else {
            return Ok(String::new());
        };
        if pairs.is_empty() {
            return Ok(String::new());
        }
        // A typed handler that `goto`s backward is a catch inside a loop (`foo2`, `pouet2`).
        // Splitting the method at try boundaries drops that loop. Fall back to region emission.
        if pairs.iter().any(|(_, handler)| {
            handler
                .handlers
                .iter()
                .any(|h| handler_goto_continues(instructions, h.addr * 2))
        }) {
            return Ok(String::new());
        }
        pairs.sort_by_key(|(t, _)| t.start_addr);
        let code_end_byte = (code.insns_size as u32) * 2;

        let blocks_overlapping = |lo: u32, hi: u32| -> HashSet<BlockId> {
            cfg.blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    let be = if b.end_offset == u32::MAX {
                        code_end_byte
                    } else {
                        b.end_offset
                    };
                    b.start_offset < hi && be > lo
                })
                .map(|(i, _)| i)
                .collect()
        };

        let mut full = String::new();
        let mut declared = HashSet::new();
        self.seed_declared_from_params(&mut declared, code, encoded);
        let mut cursor: u32 = 0;

        for (idx, (try_item, handler)) in pairs.iter().enumerate() {
            let next_try_start = pairs.get(idx + 1).map(|(t, _)| t.start_addr * 2);
            let try_end_preview = (try_item.start_addr + try_item.insn_count as u32) * 2;
            let last_handler_start = handler
                .handlers
                .iter()
                .map(|h| h.addr * 2)
                .chain(handler.catch_all_addr.map(|a| a * 2))
                .max();
            let mut post_end = next_try_start.or(Some(code_end_byte));
            // Javac emits `goto` right after the try to skip the handler and join the
            // shared tail (`if` / `switch` after the catch). Without this cap the last
            // handler stretches to the next try or the method end and swallows that tail.
            if let Some(last_h) = last_handler_start {
                if let Some(merge) = try_exit_merge_byte(instructions, try_end_preview, last_h) {
                    if handler_falls_through(instructions, last_h, merge) {
                        if let Some(post) = post_end {
                            if merge < post {
                                post_end = Some(merge);
                            }
                        }
                    }
                }
            }
            let (try_start_byte, try_end_byte, handler_ranges) =
                try_and_handler_byte_ranges_with_end(try_item, handler, code.insns_size, post_end);

            // Code before this try (from cursor). When a catch-all handler exists, defer
            // emission so catch-all/finally cleanup can pull pre-try statements into the try body.
            let mut pre_try_buf = String::new();
            let defer_pre_try = handler.catch_all_addr.is_some();
            if try_start_byte > cursor {
                let pre_out = if defer_pre_try {
                    &mut pre_try_buf
                } else {
                    &mut full
                };
                if let Some(entry) = cfg.block_id_at_offset(cursor.max(0)) {
                    let pre_blocks = blocks_overlapping(cursor, try_start_byte);
                    if let Some(pre_region) = build_regions_filtered(cfg, entry, &pre_blocks) {
                        self.emit_region(
                            &pre_region,
                            cfg,
                            instructions,
                            code.insns_off,
                            encoded,
                            code,
                            pre_out,
                            2,
                            None,
                            None,
                            &mut declared,
                            Some(global_used_regs),
                            Some((cursor, try_start_byte)),
                            class_name,
                        )?;
                    } else {
                        self.emit_block_instructions(
                            cfg,
                            instructions,
                            code.insns_off,
                            entry,
                            None,
                            None,
                            encoded,
                            code,
                            pre_out,
                            2,
                            &mut declared,
                            Some(global_used_regs),
                            false,
                            Some((cursor, try_start_byte)),
                            class_name,
                        )?;
                    }
                }
            }

            let try_blocks = blocks_overlapping(try_start_byte, try_end_byte);
            let Some(try_entry) = cfg
                .block_id_at_offset(try_start_byte)
                .filter(|bid| try_blocks.contains(bid))
            else {
                cursor = try_end_byte;
                continue;
            };

            let mut try_body = String::new();
            if let Some(try_region) = build_regions_filtered(cfg, try_entry, &try_blocks) {
                self.emit_region(
                    &try_region,
                    cfg,
                    instructions,
                    code.insns_off,
                    encoded,
                    code,
                    &mut try_body,
                    2,
                    None,
                    None,
                    &mut declared,
                    Some(global_used_regs),
                    Some((try_start_byte, try_end_byte)),
                    class_name,
                )?;
            }
            if try_body.is_empty() {
                let _ = self.emit_block_instructions(
                    cfg,
                    instructions,
                    code.insns_off,
                    try_entry,
                    None,
                    None,
                    encoded,
                    code,
                    &mut try_body,
                    2,
                    &mut declared,
                    Some(global_used_regs),
                    false,
                    Some((try_start_byte, try_end_byte)),
                    class_name,
                )?;
            }
            if try_body.is_empty() {
                cursor = try_end_byte;
                continue;
            }

            if let Some(hstart) = first_handler_start_byte(handler, &handler_ranges) {
                if hstart > try_end_byte {
                    // Code between try-end and the first handler (Kotlin `use` /
                    // closeFinally holes, success-path Log.*, …). Must emit the full
                    // CFG region — a single-block walk drops later blocks (e.g. Log.d
                    // after CloseableKt.closeFinally in MASTG-DEMO-0001).
                    if let Some(gap_entry) = cfg.block_id_at_offset(try_end_byte) {
                        let gap_blocks = blocks_overlapping(try_end_byte, hstart);
                        let mut gap_emitted = false;
                        if let Some(gap_region) =
                            build_regions_filtered(cfg, gap_entry, &gap_blocks)
                        {
                            let before = try_body.len();
                            self.emit_region(
                                &gap_region,
                                cfg,
                                instructions,
                                code.insns_off,
                                encoded,
                                code,
                                &mut try_body,
                                2,
                                None,
                                None,
                                &mut declared,
                                Some(global_used_regs),
                                Some((try_end_byte, hstart)),
                                class_name,
                            )?;
                            gap_emitted = try_body.len() > before;
                        }
                        if !gap_emitted {
                            let _ = self.emit_block_instructions(
                                cfg,
                                instructions,
                                code.insns_off,
                                gap_entry,
                                None,
                                None,
                                encoded,
                                code,
                                &mut try_body,
                                2,
                                &mut declared,
                                Some(global_used_regs),
                                false,
                                Some((try_end_byte, hstart)),
                                class_name,
                            )?;
                        }
                    }
                }
            }

            struct TypedHandler {
                type_name: String,
                body: String,
                start_byte: u32,
            }
            let mut typed_handlers: Vec<TypedHandler> = Vec::new();
            for (type_idx, start_byte, end_byte) in &handler_ranges {
                let type_name = self
                    .dex
                    .get_type(*type_idx)
                    .map_err(|e| DexDecompilerError::Parse(e.to_string()))
                    .map(|d| shorten_java_names(&java::descriptor_to_java(&d)))?;
                let handler_blocks = blocks_overlapping(*start_byte, *end_byte);
                if let Some(handler_entry) = cfg.block_id_at_offset(*start_byte) {
                    if handler_blocks.contains(&handler_entry) {
                        let mut handler_body = String::new();
                        if let Some(handler_region) =
                            build_regions_filtered(cfg, handler_entry, &handler_blocks)
                        {
                            self.emit_region(
                                &handler_region,
                                cfg,
                                instructions,
                                code.insns_off,
                                encoded,
                                code,
                                &mut handler_body,
                                2,
                                None,
                                None,
                                &mut declared,
                                Some(global_used_regs),
                                Some((*start_byte, *end_byte)),
                                class_name,
                            )?;
                        }
                        if handler_body.is_empty() {
                            let _ = self.emit_block_instructions(
                                cfg,
                                instructions,
                                code.insns_off,
                                handler_entry,
                                None,
                                None,
                                encoded,
                                code,
                                &mut handler_body,
                                2,
                                &mut declared,
                                Some(global_used_regs),
                                false,
                                Some((*start_byte, *end_byte)),
                                class_name,
                            )?;
                        }
                        typed_handlers.push(TypedHandler {
                            type_name,
                            body: handler_body,
                            start_byte: *start_byte,
                        });
                    }
                }
            }
            typed_handlers.sort_by_key(|h| h.start_byte);

            let mut catch_all_cleanup = String::new();
            let mut catch_all_is_finally = false;
            if let Some((ca_start, ca_end)) =
                catch_all_byte_range(handler, &handler_ranges, code.insns_size, post_end)
            {
                let handler_blocks = blocks_overlapping(ca_start, ca_end);
                if let Some(handler_entry) = cfg.block_id_at_offset(ca_start) {
                    if handler_blocks.contains(&handler_entry) {
                        let mut handler_body = String::new();
                        if let Some(handler_region) =
                            build_regions_filtered(cfg, handler_entry, &handler_blocks)
                        {
                            self.emit_region(
                                &handler_region,
                                cfg,
                                instructions,
                                code.insns_off,
                                encoded,
                                code,
                                &mut handler_body,
                                2,
                                None,
                                None,
                                &mut declared,
                                Some(global_used_regs),
                                Some((ca_start, ca_end)),
                                class_name,
                            )?;
                        }
                        catch_all_cleanup = strip_finally_exception_noise(&handler_body);
                        catch_all_is_finally = looks_like_finally(&catch_all_cleanup)
                            || catch_all_cleanup.trim().is_empty()
                            || handler.handlers.is_empty();
                        if catch_all_is_finally {
                            try_body =
                                peel_trailing_finally_from_try(&try_body, &catch_all_cleanup);
                        }
                    }
                }
            }

            if catch_all_is_finally && !pre_try_buf.is_empty() {
                let (pre_outside, pre_inside) = split_pre_try_for_finally(&pre_try_buf);
                if !pre_outside.is_empty() {
                    full.push_str(&pre_outside);
                }
                if !pre_inside.is_empty() {
                    try_body = format!("{pre_inside}{try_body}");
                }
            } else if !pre_try_buf.is_empty() {
                full.push_str(&pre_try_buf);
            }

            let nest_pair = nested_runtime_exception_handler_pair(
                &typed_handlers
                    .iter()
                    .map(|h| h.type_name.clone())
                    .collect::<Vec<_>>(),
            );

            if let Some((re_idx, ex_idx)) = nest_pair {
                writeln!(full, "        try {{")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                writeln!(full, "            try {{")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                full.push_str(&try_body);
                if !try_body.ends_with('\n') {
                    full.push('\n');
                }
                writeln!(
                    full,
                    "            }} catch ({} e) {{",
                    typed_handlers[re_idx].type_name
                )
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                full.push_str(&typed_handlers[re_idx].body);
                if !typed_handlers[re_idx].body.ends_with('\n') {
                    full.push('\n');
                }
                writeln!(full, "            }}")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                writeln!(
                    full,
                    "        }} catch ({} e) {{",
                    typed_handlers[ex_idx].type_name
                )
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                full.push_str(&typed_handlers[ex_idx].body);
                if !typed_handlers[ex_idx].body.ends_with('\n') {
                    full.push('\n');
                }
            } else {
                writeln!(full, "        try {{")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                full.push_str(&try_body);
                if !try_body.ends_with('\n') {
                    full.push('\n');
                }

                for h in &typed_handlers {
                    writeln!(full, "        }} catch ({} e) {{", h.type_name)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    full.push_str(&h.body);
                    if !h.body.ends_with('\n') {
                        full.push('\n');
                    }
                }
            }

            if let Some((ca_start, ca_end)) =
                catch_all_byte_range(handler, &handler_ranges, code.insns_size, post_end)
            {
                if catch_all_is_finally && !catch_all_cleanup.trim().is_empty() {
                    writeln!(full, "        }} finally {{")
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    full.push_str(&catch_all_cleanup);
                    if !catch_all_cleanup.ends_with('\n') {
                        full.push('\n');
                    }
                } else if let Some(handler_entry) = cfg.block_id_at_offset(ca_start) {
                    let handler_blocks = blocks_overlapping(ca_start, ca_end);
                    if handler_blocks.contains(&handler_entry) {
                        let mut handler_body = String::new();
                        if let Some(handler_region) =
                            build_regions_filtered(cfg, handler_entry, &handler_blocks)
                        {
                            self.emit_region(
                                &handler_region,
                                cfg,
                                instructions,
                                code.insns_off,
                                encoded,
                                code,
                                &mut handler_body,
                                2,
                                None,
                                None,
                                &mut declared,
                                Some(global_used_regs),
                                Some((ca_start, ca_end)),
                                class_name,
                            )?;
                        }
                        let cleaned = strip_finally_exception_noise(&handler_body);
                        writeln!(full, "        }} catch (Throwable e) {{")
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        full.push_str(&cleaned);
                        if !cleaned.ends_with('\n') {
                            full.push('\n');
                        }
                    }
                }
            }
            writeln!(full, "        }}")
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;

            // Advance cursor past try + all handlers.
            let mut covered_end = try_end_byte;
            for (_, _, e) in &handler_ranges {
                covered_end = covered_end.max(*e);
            }
            if let Some((_, e)) =
                catch_all_byte_range(handler, &handler_ranges, code.insns_size, post_end)
            {
                covered_end = covered_end.max(e);
            }
            cursor = covered_end;
        }

        // Trailing code after last try/handlers.
        if cursor < code_end_byte {
            if let Some(entry) = cfg.block_id_at_offset(cursor) {
                let post_blocks = blocks_overlapping(cursor, code_end_byte);
                if let Some(post_region) = build_regions_filtered(cfg, entry, &post_blocks) {
                    self.emit_region(
                        &post_region,
                        cfg,
                        instructions,
                        code.insns_off,
                        encoded,
                        code,
                        &mut full,
                        2,
                        None,
                        None,
                        &mut declared,
                        Some(global_used_regs),
                        Some((cursor, code_end_byte)),
                        class_name,
                    )?;
                }
            }
        }
        Ok(full)
    }

    /// Wrap method body in try { body } catch (Type e) { ... } when code has try items.
    fn wrap_body_with_try_catch(
        &self,
        body: &str,
        code_off: u32,
        code: &CodeItem,
    ) -> Result<String> {
        let data = self.dex.data.as_ref();
        let Some(pairs) = try_catch::try_handler_pairs(data, code_off, code) else {
            return Ok(format!(
                "        // try/catch ({} tries) - failed to parse handlers\n{}",
                code.tries_size, body
            ));
        };
        if pairs.is_empty() {
            return Ok(format!(
                "        // try/catch ({} tries)\n{}",
                code.tries_size, body
            ));
        }
        let mut out = String::new();
        writeln!(out, "        try {{")
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        let (_, first_handler) = &pairs[0];
        for type_addr in &first_handler.handlers {
            let type_name = self
                .dex
                .get_type(type_addr.type_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))
                .map(|d| shorten_java_names(&java::descriptor_to_java(&d)))?;
            writeln!(out, "        }} catch ({} e) {{", type_name)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            writeln!(
                out,
                "            // handler at code unit {}",
                type_addr.addr
            )
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        }
        let as_finally =
            !first_handler.handlers.is_empty() && first_handler.catch_all_addr.is_some();
        if let Some(addr) = first_handler.catch_all_addr {
            if as_finally {
                writeln!(out, "        }} finally {{")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                writeln!(
                    out,
                    "            // finally (catch-all) at code unit {}",
                    addr
                )
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            } else {
                writeln!(out, "        }} catch (Throwable e) {{")
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                writeln!(
                    out,
                    "            // catch-all handler at code unit {}",
                    addr
                )
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        writeln!(out, "        }}")
            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
        Ok(out)
    }

    /// Simple mode: emit CFG blocks in order with labels; uses full CFG SSA with φ-nodes.
    fn decompile_method_body_simple(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        code: &CodeItem,
        encoded: &EncodedMethod,
        class_name: Option<&str>,
    ) -> Result<String> {
        let code_insns = code.insns_slice(&*self.dex.data);
        let mut block_ir: HashMap<BlockId, Vec<IrStmt>> = HashMap::new();
        for bid in 0..cfg.block_count() {
            let seq = self.block_instruction_seq(cfg, instructions, bid, None, false, None);
            let stmts =
                self.instructions_to_ir(&seq, code.insns_off, code_insns, Some(instructions))?;
            let mut runner = PassRunner::new();
            runner.add(InvokeChainPass);
            runner.add(ConstructorMergePass);
            runner.add(ExprSimplifyPass);
            block_ir.insert(bid, runner.run(stmts));
        }
        construct_ssa(cfg, &mut block_ir);
        let canonical = phi_canonical_map(&block_ir);
        strip_phis(&mut block_ir);

        let mut out = String::new();
        let mut declared = HashSet::new();
        self.seed_declared_from_params(&mut declared, code, encoded);
        let global_used_regs = self.method_used_regs(cfg, instructions, code.insns_off, code_insns);
        for bid in 0..cfg.block_count() {
            let block = &cfg.blocks[bid];
            writeln!(out, "        // block_{} @ 0x{:x}", bid, block.start_offset)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            let stmts = block_ir.remove(&bid).unwrap_or_default();
            let stmts = CopyPropPass.run(stmts);
            let stmts = run_dead_assign_with_used_regs(stmts, &global_used_regs);
            let type_map = self.infer_types_for(encoded, code, &stmts);
            let registers_size = code.registers_size as u32;
            let ins_size = code.ins_size as u32;
            let is_static = (encoded.access_flags & 0x8) != 0;
            let mut name_map =
                build_var_names_with_regs(&stmts, &type_map, registers_size, ins_size, is_static);
            apply_canonical_names(&mut name_map, &canonical);
            self.apply_debug_names_to_name_map(&mut name_map, &type_map, code, encoded);
            self.apply_signature_param_names(&mut name_map, encoded, code);
            self.apply_variable_renames_to_name_map(&mut name_map, class_name, encoded);
            for line in
                self.codegen_ir_lines(&stmts, Some(&type_map), Some(&name_map), &mut declared)
            {
                if !line.is_empty() {
                    writeln!(out, "        {}", line)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                }
            }
        }
        Ok(out)
    }

    /// Analyze the method CFG to find registers that need φ-nodes (for shared naming).
    fn refresh_phi_regs(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        code: &CodeItem,
        _encoded: &EncodedMethod,
    ) {
        let code_insns = code.insns_slice(&*self.dex.data);
        let mut block_ir: HashMap<BlockId, Vec<IrStmt>> = HashMap::new();
        for bid in 0..cfg.block_count() {
            let seq = self.block_instruction_seq(cfg, instructions, bid, None, false, None);
            let Ok(stmts) =
                self.instructions_to_ir(&seq, code.insns_off, code_insns, Some(instructions))
            else {
                continue;
            };
            let mut runner = PassRunner::new();
            runner.add(InvokeChainPass);
            block_ir.insert(bid, runner.run(stmts));
        }
        construct_ssa(cfg, &mut block_ir);
        *self.phi_regs.borrow_mut() = phi_registers(&block_ir);
    }

    /// Share one Java name across SSA versions of φ-web registers **only when types match**.
    fn apply_phi_register_names(
        &self,
        name_map: &mut HashMap<VarId, String>,
        type_map: &HashMap<VarId, String>,
    ) {
        let phi_regs = self.phi_regs.borrow();
        if phi_regs.is_empty() {
            return;
        }
        let vids: Vec<VarId> = name_map
            .keys()
            .copied()
            .filter(|v| phi_regs.contains(&v.reg))
            .collect();
        for i in 0..vids.len() {
            for j in (i + 1)..vids.len() {
                let a = vids[i];
                let b = vids[j];
                if a.reg != b.reg {
                    continue;
                }
                let ta = type_map.get(&a).map(|s| s.as_str());
                let tb = type_map.get(&b).map(|s| s.as_str());
                if !types_compatible_for_naming(ta, tb) {
                    continue;
                }
                let na = name_map.get(&a).cloned().unwrap_or_default();
                let nb = name_map.get(&b).cloned().unwrap_or_default();
                let prefer = if na == "this" {
                    na
                } else if nb == "this" {
                    nb
                } else if is_synthetic_local_name(&na) {
                    nb
                } else {
                    na
                };
                if let Some(n) = name_map.get_mut(&a) {
                    *n = prefer.clone();
                }
                if let Some(n) = name_map.get_mut(&b) {
                    *n = prefer;
                }
            }
        }
    }

    /// Fallback: linear instruction list (no CFG structure).
    fn decompile_method_body_linear(
        &self,
        instructions: &[Instruction],
        base_off: usize,
        encoded: &EncodedMethod,
        code: &CodeItem,
        code_insns: &[u8],
        class_name: Option<&str>,
    ) -> Result<String> {
        let mut out = String::new();
        let stmts = self.instructions_to_ir(instructions, base_off, code_insns, None)?;
        let stmts = self.default_pass_runner().run(stmts);
        let type_map = self.infer_types_for(encoded, code, &stmts);
        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;
        let mut name_map =
            build_var_names_with_regs(&stmts, &type_map, registers_size, ins_size, is_static);
        self.apply_debug_names_to_name_map(&mut name_map, &type_map, code, encoded);
        self.apply_signature_param_names(&mut name_map, encoded, code);
        self.apply_variable_renames_to_name_map(&mut name_map, class_name, encoded);
        let mut declared = HashSet::new();
        for line in self.codegen_ir_lines(&stmts, Some(&type_map), Some(&name_map), &mut declared) {
            if !line.is_empty() {
                writeln!(&mut out, "        {}", line)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        if out.is_empty() {
            out = "        // (no instructions)\n".to_string();
        } else {
            let is_constructor = self
                .dex
                .get_method_info(encoded.method_idx)
                .map(|info| info.name == "<init>")
                .unwrap_or(false);
            out = simplify::simplify_method_body(&out, is_constructor);
            out = simplify::restore_string_switch(&out);
            if let Some(enclosing) = class_name {
                out = self.inline_anonymous_classes(&out, enclosing)?;
            }
            let reps = self.const_field_reps.borrow();
            if !reps.is_empty() {
                out = const_fields::apply_const_field_replacements(&out, &reps);
            }
            if !self.resource_reps.is_empty() {
                out = resource_consts::apply_resource_replacements(&out, &self.resource_reps);
            }
            {
                let acc = self.accessor_reps.borrow();
                if !acc.is_empty() {
                    out = accessors::apply_accessor_replacements(&out, &acc);
                }
            }
            out = accessors::polish_field_accessor_calls(&out);
            let method_name = self
                .dex
                .get_method_info(encoded.method_idx)
                .map(|i| i.name.to_string())
                .unwrap_or_default();
            if method_name == "invokeSuspend" {
                out = kotlin::restore_coroutine_invoke_suspend(&out);
            }
            out = kotlin::restore_kotlin_idioms(&out);
        }
        if code.tries_size > 0
            && !out.contains("} catch (")
            && !out.contains("} finally {")
            && !out.contains("try (")
        {
            out = self.wrap_body_with_try_catch(&out, encoded.code_off, code)?;
        }
        Ok(out)
    }

    /// Emit Java from a region tree (structured control flow).
    /// Returns true if we emitted break (caller should stop emitting siblings).
    /// When emit_range is Some((min, max)), only emit instructions with offset in [min, max).
    fn emit_region(
        &self,
        region: &Region,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        base_off: usize,
        encoded: &EncodedMethod,
        code: &CodeItem,
        out: &mut String,
        indent: usize,
        skip_goto_to: Option<BlockId>,
        break_target: Option<BlockId>,
        declared: &mut HashSet<String>,
        global_used_regs: Option<&HashSet<u32>>,
        emit_range: Option<(u32, u32)>,
        class_name: Option<&str>,
    ) -> Result<bool> {
        let ind = "    ".repeat(indent);
        match region {
            Region::Block(block_id) => self.emit_block_instructions(
                cfg,
                instructions,
                base_off,
                *block_id,
                skip_goto_to,
                break_target,
                encoded,
                code,
                out,
                indent,
                declared,
                global_used_regs,
                false,
                emit_range,
                class_name,
            ),
            Region::Seq(children) => {
                if let Some((
                    init_block,
                    header,
                    condition,
                    body_without_update,
                    then_branch,
                    update_block,
                )) = for_loop_pattern(children)
                {
                    if self.block_is_single_const_init(cfg, instructions, init_block)
                        && self.block_is_single_update_and_back_edge(
                            cfg,
                            instructions,
                            update_block,
                            header,
                        )
                    {
                        let mut init_buf = String::new();
                        self.emit_block_instructions(
                            cfg,
                            instructions,
                            base_off,
                            init_block,
                            skip_goto_to,
                            break_target,
                            encoded,
                            code,
                            &mut init_buf,
                            indent,
                            declared,
                            global_used_regs,
                            false,
                            emit_range,
                            class_name,
                        )?;
                        let init_str = init_buf
                            .trim()
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .trim_end_matches(';')
                            .trim();
                        let mut update_buf = String::new();
                        self.emit_block_instructions(
                            cfg,
                            instructions,
                            base_off,
                            update_block,
                            Some(header),
                            break_target,
                            encoded,
                            code,
                            &mut update_buf,
                            indent,
                            declared,
                            global_used_regs,
                            false,
                            emit_range,
                            class_name,
                        )?;
                        let update_str = update_buf
                            .trim()
                            .lines()
                            .find(|l| !l.trim().is_empty() && l.trim() != "continue;")
                            .unwrap_or(&"")
                            .trim()
                            .trim_end_matches(';')
                            .trim();
                        if !init_str.is_empty() && !update_str.is_empty() {
                            writeln!(
                                out,
                                "{}for ({}; {}; {}) {{",
                                ind,
                                shorten_java_names(init_str),
                                shorten_java_names(condition),
                                shorten_java_names(update_str),
                            )
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            let _ = self.emit_block_instructions(
                                cfg,
                                instructions,
                                base_off,
                                header,
                                Some(header),
                                None,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                declared,
                                global_used_regs,
                                true,
                                emit_range,
                                class_name,
                            )?;
                            let _ = self.emit_region(
                                &body_without_update,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                Some(header),
                                region::first_block(then_branch),
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                            writeln!(out, "{}}}", ind)
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            if !region_is_empty(then_branch) {
                                let _ = self.emit_region(
                                    then_branch,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent,
                                    skip_goto_to,
                                    break_target,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                            }
                            return Ok(false);
                        }
                    }
                }
                let mut skip_next = false;
                for (i, r) in children.iter().enumerate() {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    // `if (i == 10) goto return` inside a loop is a break, not a
                    // guard around the rest of the body (`foobis`).
                    if let (
                        Region::Block(bid),
                        Some(Region::If {
                            condition,
                            then_branch,
                            else_branch,
                        }),
                    ) = (r, children.get(i + 1))
                    {
                        if let BlockEnd::Conditional { branch_target, .. } = &cfg.blocks[*bid].end
                        {
                            let then_empty = region_is_empty_with_cfg(then_branch, cfg);
                            let else_empty = region_is_empty_with_cfg(else_branch, cfg);
                            if then_empty
                                && !else_empty
                                && skip_goto_to.is_some()
                                && break_target == Some(*branch_target)
                            {
                                let _ = self.emit_block_instructions(
                                    cfg,
                                    instructions,
                                    base_off,
                                    *bid,
                                    skip_goto_to,
                                    break_target,
                                    encoded,
                                    code,
                                    out,
                                    indent,
                                    declared,
                                    global_used_regs,
                                    false,
                                    emit_range,
                                    class_name,
                                )?;
                                mark_condition_idents_declared(condition, declared);
                                let cond = shorten_java_names(condition);
                                writeln!(out, "{}if ({}) {{", ind, cond).map_err(|_| {
                                    DexDecompilerError::Decompilation("write".into())
                                })?;
                                writeln!(out, "{}    break;", ind).map_err(|_| {
                                    DexDecompilerError::Decompilation("write".into())
                                })?;
                                writeln!(out, "{}}}", ind).map_err(|_| {
                                    DexDecompilerError::Decompilation("write".into())
                                })?;
                                let _ = self.emit_region(
                                    else_branch,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent,
                                    skip_goto_to,
                                    break_target,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                                skip_next = true;
                                continue;
                            }
                        }
                    }
                    let skip_block_last = match (r, children.get(i + 1)) {
                        (Region::Block(bid), Some(Region::Switch { .. })) => {
                            matches!(&cfg.blocks[*bid].end, BlockEnd::Switch { .. })
                        }
                        _ => false,
                    };
                    let emitted_break = if skip_block_last {
                        if let Region::Block(block_id) = r {
                            self.emit_block_instructions(
                                cfg,
                                instructions,
                                base_off,
                                *block_id,
                                skip_goto_to,
                                break_target,
                                encoded,
                                code,
                                out,
                                indent,
                                declared,
                                global_used_regs,
                                true,
                                emit_range,
                                class_name,
                            )?
                        } else {
                            self.emit_region(
                                r,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent,
                                skip_goto_to,
                                break_target,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?
                        }
                    } else {
                        self.emit_region(
                            r,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent,
                            skip_goto_to,
                            break_target,
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?
                    };
                    if emitted_break {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Region::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_empty = region_is_empty_with_cfg(then_branch, cfg);
                let else_empty = region_is_empty_with_cfg(else_branch, cfg);
                if then_empty && else_empty {
                    // Skip emitting empty if (cond) { } — no effect.
                    return Ok(false);
                }
                // At top level only: prefer "if (cond) { short/return } else { loop }" by swapping when then has loop and else doesn't.
                let at_top_level = skip_goto_to.is_none() && break_target.is_none();
                let then_has_loop = region_contains_loop(then_branch);
                let else_has_loop = region_contains_loop(else_branch);
                let (condition, then_branch, else_branch) =
                    if at_top_level && then_has_loop && !else_has_loop {
                        (negate_condition(condition), else_branch, then_branch)
                    } else {
                        (condition.clone(), then_branch, else_branch)
                    };
                let then_empty_after = region_is_empty_with_cfg(then_branch, cfg)
                    && !region::region_is_loop_break(then_branch, cfg, break_target);
                let else_empty_after = region_is_empty_with_cfg(else_branch, cfg)
                    && !region::region_is_loop_break(else_branch, cfg, break_target);
                if then_empty_after && !else_empty_after {
                    let neg = negate_condition(&condition);
                    mark_condition_idents_declared(&neg, declared);
                    writeln!(out, "{}if ({}) {{", ind, shorten_java_names(&neg))
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    let _ = self.emit_region(
                        else_branch,
                        cfg,
                        instructions,
                        base_off,
                        encoded,
                        code,
                        out,
                        indent + 1,
                        skip_goto_to,
                        break_target,
                        declared,
                        global_used_regs,
                        emit_range,
                        class_name,
                    )?;
                    writeln!(out, "{}}}", ind)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                } else {
                    mark_condition_idents_declared(&condition, declared);
                    writeln!(out, "{}if ({}) {{", ind, shorten_java_names(&condition))
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    let _ = self.emit_region(
                        then_branch,
                        cfg,
                        instructions,
                        base_off,
                        encoded,
                        code,
                        out,
                        indent + 1,
                        skip_goto_to,
                        break_target,
                        declared,
                        global_used_regs,
                        emit_range,
                        class_name,
                    )?;
                    if !else_empty_after {
                        // Emit-time else-if: `} else { if (c)` → `} else if (c)`
                        self.emit_else_chain(
                            else_branch,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent,
                            skip_goto_to,
                            break_target,
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?;
                    } else {
                        writeln!(out, "{}}}", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                }
                Ok(false)
            }
            Region::Loop { header, body } => {
                let mut loop_emitted = false;
                if let Some((continue_cond, do_body, exit_tail)) =
                    region::loop_body_do_while_exit_in_else(body, *header)
                {
                    let mis_peeled = !region::loop_has_substantial_body(&do_body, *header, cfg)
                        && !region_is_empty_with_cfg(exit_tail, cfg)
                        && region::region_has_non_return_work(exit_tail, cfg)
                        && !region::region_contains_loop(exit_tail)
                        && !region::region_contains_if(exit_tail);
                    if mis_peeled {
                        if let Some((condition, else_branch, then_branch)) =
                            region::loop_body_break_pattern_trailing(body, *header)
                        {
                            let exit_block = region::first_block(then_branch);
                            let while_cond = negate_condition(&condition);
                            writeln!(out, "{}while ({}) {{", ind, shorten_java_names(&while_cond))
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            let _ = self.emit_block_instructions(
                                cfg,
                                instructions,
                                base_off,
                                *header,
                                Some(*header),
                                None,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                declared,
                                global_used_regs,
                                true,
                                emit_range,
                                class_name,
                            )?;
                            if let Some(middle) = region::loop_body_break_middle(body, *header) {
                                if !region_is_empty_with_cfg(&middle, cfg) {
                                    let _ = self.emit_region(
                                        &middle,
                                        cfg,
                                        instructions,
                                        base_off,
                                        encoded,
                                        code,
                                        out,
                                        indent + 1,
                                        Some(*header),
                                        exit_block,
                                        declared,
                                        global_used_regs,
                                        emit_range,
                                        class_name,
                                    )?;
                                }
                            }
                            let _ = self.emit_region(
                                else_branch,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                Some(*header),
                                exit_block,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                            writeln!(out, "{}}}", ind)
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            if !region_is_empty_with_cfg(then_branch, cfg) {
                                let _ = self.emit_region(
                                    then_branch,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent,
                                    skip_goto_to,
                                    break_target,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                            }
                            loop_emitted = true;
                        }
                    }
                    if !loop_emitted {
                        writeln!(out, "{}do {{", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        let _ = self.emit_region(
                            &do_body,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            Some(*header),
                            region::first_block(exit_tail),
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?;
                        writeln!(
                            out,
                            "{}}} while ({});",
                            ind,
                            shorten_java_names(continue_cond)
                        )
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        if !region_is_empty_with_cfg(exit_tail, cfg) {
                            let _ = self.emit_region(
                                exit_tail,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent,
                                skip_goto_to,
                                break_target,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                        }
                        loop_emitted = true;
                    }
                }
                if !loop_emitted {
                    if let Some((exit_cond, do_body, then_branch)) =
                        region::loop_body_do_while_loose(body, *header)
                            .or_else(|| loop_body_do_while_pattern(body, *header))
                    {
                        let while_cond = negate_condition(exit_cond);
                        writeln!(out, "{}do {{", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        let _ = self.emit_region(
                            &do_body,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            Some(*header),
                            region::first_block(then_branch),
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?;
                        writeln!(
                            out,
                            "{}}} while ({});",
                            ind,
                            shorten_java_names(&while_cond)
                        )
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        if !region_is_empty_with_cfg(then_branch, cfg) {
                            let _ = self.emit_region(
                                then_branch,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent,
                                skip_goto_to,
                                break_target,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                        }
                        loop_emitted = true;
                    } else if let Some((condition, else_branch, then_branch)) =
                        region::loop_body_break_pattern(body, *header)
                    {
                        let exit_block = region::first_block(then_branch);
                        let while_cond = negate_condition(&condition);
                        writeln!(out, "{}while ({}) {{", ind, shorten_java_names(&while_cond))
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        let _ = self.emit_block_instructions(
                            cfg,
                            instructions,
                            base_off,
                            *header,
                            Some(*header),
                            None,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            declared,
                            global_used_regs,
                            true,
                            emit_range,
                            class_name,
                        )?;
                        if let Some(middle) = region::loop_body_break_middle(body, *header) {
                            if !region_is_empty_with_cfg(&middle, cfg) {
                                let _ = self.emit_region(
                                    &middle,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent + 1,
                                    Some(*header),
                                    exit_block,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                            }
                        }
                        let _ = self.emit_region(
                            else_branch,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            Some(*header),
                            exit_block,
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?;
                        writeln!(out, "{}}}", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        if !region_is_empty_with_cfg(then_branch, cfg) {
                            let _ = self.emit_region(
                                then_branch,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent,
                                skip_goto_to,
                                break_target,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                        }
                        loop_emitted = true;
                    } else if let Some((condition, else_branch, then_branch)) =
                        region::loop_body_break_pattern_trailing(body, *header)
                    {
                        // Nested trailing exit-if (merge tail loops): require a non-empty exit `then`.
                        if !region_is_empty_with_cfg(then_branch, cfg) {
                            let exit_block = region::first_block(then_branch);
                            let while_cond = negate_condition(&condition);
                            writeln!(out, "{}while ({}) {{", ind, shorten_java_names(&while_cond))
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            let _ = self.emit_block_instructions(
                                cfg,
                                instructions,
                                base_off,
                                *header,
                                Some(*header),
                                None,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                declared,
                                global_used_regs,
                                true,
                                emit_range,
                                class_name,
                            )?;
                            if let Some(middle) = region::loop_body_break_middle(body, *header) {
                                if !region_is_empty_with_cfg(&middle, cfg) {
                                    let _ = self.emit_region(
                                        &middle,
                                        cfg,
                                        instructions,
                                        base_off,
                                        encoded,
                                        code,
                                        out,
                                        indent + 1,
                                        Some(*header),
                                        exit_block,
                                        declared,
                                        global_used_regs,
                                        emit_range,
                                        class_name,
                                    )?;
                                }
                            }
                            let _ = self.emit_region(
                                else_branch,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent + 1,
                                Some(*header),
                                exit_block,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                            writeln!(out, "{}}}", ind)
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                            if !region_is_empty_with_cfg(then_branch, cfg) {
                                let _ = self.emit_region(
                                    then_branch,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent,
                                    skip_goto_to,
                                    break_target,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                            }
                            loop_emitted = true;
                        }
                    }
                }
                if !loop_emitted {
                    let loop_break = region::loop_exit_break_target(body, *header, cfg);
                    let multi_exits = region::loop_prefix_multi_exit_ifs(body, *header);
                    if multi_exits.len() >= 2 {
                        writeln!(out, "{}while (true) {{", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        let _ = self.emit_block_instructions(
                            cfg,
                            instructions,
                            base_off,
                            *header,
                            Some(*header),
                            loop_break,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            declared,
                            global_used_regs,
                            true,
                            emit_range,
                            class_name,
                        )?;
                        if let Some(middle) = region::loop_body_break_middle(body, *header) {
                            if !region_is_empty_with_cfg(&middle, cfg) {
                                let _ = self.emit_region(
                                    &middle,
                                    cfg,
                                    instructions,
                                    base_off,
                                    encoded,
                                    code,
                                    out,
                                    indent + 1,
                                    Some(*header),
                                    loop_break,
                                    declared,
                                    global_used_regs,
                                    emit_range,
                                    class_name,
                                )?;
                            }
                        }
                        for (i, (cond, exit_r)) in multi_exits.iter().enumerate() {
                            if i == 0 {
                                writeln!(out, "{}if ({}) {{", ind, shorten_java_names(cond))
                                    .map_err(|_| {
                                        DexDecompilerError::Decompilation("write".into())
                                    })?;
                            } else {
                                writeln!(out, "{} else if ({}) {{", ind, shorten_java_names(cond))
                                    .map_err(|_| {
                                        DexDecompilerError::Decompilation("write".into())
                                    })?;
                            }
                            let _ = self.emit_region(
                                exit_r,
                                cfg,
                                instructions,
                                base_off,
                                encoded,
                                code,
                                out,
                                indent + 2,
                                Some(*header),
                                loop_break,
                                declared,
                                global_used_regs,
                                emit_range,
                                class_name,
                            )?;
                        }
                        if !multi_exits.is_empty() {
                            writeln!(out, "{}}}", ind)
                                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        }
                        writeln!(out, "{}}}", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    } else {
                        writeln!(out, "{}while (true) {{", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                        let _ = self.emit_region(
                            body,
                            cfg,
                            instructions,
                            base_off,
                            encoded,
                            code,
                            out,
                            indent + 1,
                            Some(*header),
                            loop_break,
                            declared,
                            global_used_regs,
                            emit_range,
                            class_name,
                        )?;
                        writeln!(out, "{}}}", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                }
                Ok(false)
            }
            Region::Switch {
                condition,
                cases,
                default,
                default_case,
            } => {
                // Switch exit ≈ default block when cases break to it; use as break target.
                let switch_break = region::first_block(default);
                writeln!(out, "{}switch ({}) {{", ind, condition)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                for (value, body) in cases {
                    writeln!(out, "{}case {}:", ind, value)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    if region_is_empty_with_cfg(body, cfg) {
                        // Shared target with a later case: bare label, fall through.
                        continue;
                    }
                    if *default_case == Some(*value) {
                        writeln!(out, "{}default:", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                    let emitted_break = self.emit_region(
                        body,
                        cfg,
                        instructions,
                        base_off,
                        encoded,
                        code,
                        out,
                        indent + 1,
                        skip_goto_to,
                        switch_break.or(break_target),
                        declared,
                        global_used_regs,
                        emit_range,
                        class_name,
                    )?;
                    let falls_through = region::last_block(body).is_some_and(|last| {
                        matches!(cfg.blocks[last].end, BlockEnd::Goto(t) if cases
                            .iter()
                            .any(|(_, other)| !std::ptr::eq(other.as_ref(), body.as_ref())
                                && region::region_contains_block(other, t))
                            || (*default_case != Some(*value)
                                && region::region_contains_block(default, t)))
                    });
                    if !emitted_break && !body_ends_with_exit(out) && !falls_through {
                        writeln!(out, "{}    break;", ind)
                            .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    }
                }
                if default_case.is_none() && !region_is_empty_with_cfg(default, cfg) {
                    writeln!(out, "{}default:", ind)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    let _ = self.emit_region(
                        default,
                        cfg,
                        instructions,
                        base_off,
                        encoded,
                        code,
                        out,
                        indent + 1,
                        skip_goto_to,
                        break_target,
                        declared,
                        global_used_regs,
                        emit_range,
                        class_name,
                    )?;
                }
                writeln!(out, "{}}}", ind)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                Ok(false)
            }
        }
    }

    /// Fold numeric constants and param copies from instructions immediately before
    /// conditionals into the condition string.
    /// E.g. `const/16 v0, 1002; if-ne v4, v0` → `v4 != 1002`,
    /// `move v4, v5; if-eq v4, v0` → `v5 == v0`.
    ///
    /// On loop headers only, skip registers mutated later (`add-int/lit8 v0, v0, 1`)
    /// so `while (i < n)` does not become `while (0 < n)`.
    ///
    /// Also folds from immediate FallThrough predecessors: dex-bytecode marks every
    /// branch as a block start, so `const/16 v2, 100; if-ge v1, v2` often lands in
    /// two blocks (`[const]` → `[if-ge]`). Without this, loop bounds become undefined
    /// temps (`while (i < i2)`).
    fn fold_constants_into_conditions(
        cfg: &mut MethodCfg,
        instructions: &[Instruction],
        dex: Option<&DexFile>,
    ) {
        let mut mutated = HashSet::new();
        for ins in instructions {
            let m = ins.mnemonic();
            if matches!(
                m,
                "const/4"
                    | "const/16"
                    | "const"
                    | "const/high16"
                    | "const-wide"
                    | "const-wide/16"
                    | "const-wide/32"
                    | "const-wide/high16"
                    | "const-string"
                    | "const-string/jumbo"
                    | "const-class"
            ) {
                continue;
            }
            let (_reads, writes) = read_write::instruction_reads_writes(m, ins.operands());
            mutated.extend(writes);
        }
        let loop_headers = cfg.loop_headers.clone();
        // Precompute FallThrough preds before mutably borrowing blocks.
        let ft_preds: Vec<Vec<BlockId>> = (0..cfg.blocks.len())
            .map(|bid| cfg.blocks_that_fall_through_to(bid))
            .collect();
        let pred_offs: Vec<Vec<u32>> = ft_preds
            .iter()
            .map(|preds| {
                let mut offs = Vec::new();
                for &p in preds {
                    // Only fold across trivial const/move predecessors (loop bound materialization).
                    let block = &cfg.blocks[p];
                    if !matches!(block.end, BlockEnd::FallThrough | BlockEnd::Goto(_)) {
                        continue;
                    }
                    let only_foldable = block.instruction_offsets.iter().all(|&off| {
                        instructions
                            .iter()
                            .find(|i| i.offset as u32 == off)
                            .map(|ins| {
                                let m = ins.mnemonic();
                                m.starts_with("const")
                                    || m.starts_with("move")
                                    || m == "instance-of"
                                    || m == "nop"
                            })
                            .unwrap_or(false)
                    });
                    if only_foldable {
                        offs.extend_from_slice(&block.instruction_offsets);
                    }
                }
                offs
            })
            .collect();
        let mut folded: HashSet<u32> = HashSet::new();
        for (bid, block) in cfg.blocks.iter_mut().enumerate() {
            if let BlockEnd::Conditional {
                ref mut condition, ..
            } = block.end
            {
                let offs = block.instruction_offsets.clone();
                // Same-block materialization before the if (skip the if itself).
                let same_block_foldable: Vec<u32> = if offs.len() >= 2 {
                    offs[..offs.len() - 1].to_vec()
                } else {
                    Vec::new()
                };
                let mut candidates: Vec<u32> = pred_offs[bid].clone();
                candidates.extend(same_block_foldable);
                if candidates.is_empty() {
                    continue;
                }
                // Prefer nearest defs (same-block first via reverse walk of combined list).
                candidates.reverse();
                let find_ins = |off: u32| -> Option<&Instruction> {
                    instructions.iter().find(|i| i.offset as u32 == off)
                };
                let is_loop = loop_headers.contains(&bid)
                    || ft_preds[bid].iter().any(|p| loop_headers.contains(p));
                for &off in candidates.iter().take(6) {
                    let Some(ins) = find_ins(off) else { continue };
                    let m = ins.mnemonic();
                    if m == "const/4" || m == "const/16" || m == "const" || m == "const/high16" {
                        let parts: Vec<&str> =
                            ins.operands().split(',').map(|s| s.trim()).collect();
                        if parts.len() >= 2 {
                            if let Some(reg) = parse_one_reg(parts[0]) {
                                if is_loop && mutated.contains(&reg) {
                                    continue;
                                }
                                let mut map = HashMap::new();
                                map.insert(reg, parts[1].to_string());
                                let next = replace_register_names(condition, &map);
                                if next != *condition {
                                    *condition = next;
                                    folded.insert(off);
                                }
                            }
                        }
                    } else if matches!(
                        m,
                        "move"
                            | "move/from16"
                            | "move/16"
                            | "move-object"
                            | "move-object/from16"
                            | "move-object/16"
                            | "move-wide"
                            | "move-wide/from16"
                            | "move-wide/16"
                    ) {
                        if let Some((dst, src)) = parse_two_regs(ins.operands()) {
                            if is_loop && mutated.contains(&dst) {
                                continue;
                            }
                            let mut map = HashMap::new();
                            map.insert(dst, format!("v{}", src));
                            let next = replace_register_names(condition, &map);
                            if next != *condition {
                                *condition = next;
                                folded.insert(off);
                            }
                        }
                    } else if m == "instance-of" {
                        let parts: Vec<&str> =
                            ins.operands().split(',').map(|s| s.trim()).collect();
                        if parts.len() >= 3 {
                            if let Some(dst) = parse_one_reg(parts[0]) {
                                if is_loop && mutated.contains(&dst) {
                                    continue;
                                }
                                let src = parse_one_reg(parts[1])
                                    .map(|s| format!("v{}", s))
                                    .unwrap_or_else(|| parts[1].to_string());
                                let ty = dex
                                    .map(|d| resolve_one(d, parts[2]))
                                    .unwrap_or_else(|| parts[2].to_string());
                                let expr = format!("{} instanceof {}", src, ty);
                                let mut map = HashMap::new();
                                map.insert(dst, expr);
                                let next = replace_register_names(condition, &map);
                                if next != *condition {
                                    *condition = next;
                                    folded.insert(off);
                                }
                            }
                        }
                    }
                }
            }
        }
        cfg.folded_const_offsets = folded;
    }

    /// `iget v0, this, field; if-lez v0` → condition `this.field`, and the same for
    /// a packed-switch on that register. The iget itself is not emitted.
    fn fold_field_reads_into_conditions(&self, cfg: &mut MethodCfg, instructions: &[Instruction]) {
        let mut folded_igets = Vec::new();
        for block in &mut cfg.blocks {
            let probe = match &block.end {
                BlockEnd::Conditional { .. } | BlockEnd::Switch { .. } => {
                    let Some(&off) = block.instruction_offsets.last() else {
                        continue;
                    };
                    off
                }
                _ => continue,
            };
            let Some(ins) = instructions.iter().find(|i| i.offset == probe) else {
                continue;
            };
            let Some(reg) = parse_one_reg(ins.operands().split(',').next().unwrap_or("")) else {
                continue;
            };
            let Some((iget_off, expr)) = self.field_expr_reaching(instructions, probe, reg) else {
                continue;
            };
            let mut map = HashMap::new();
            map.insert(reg, expr);
            match &mut block.end {
                BlockEnd::Conditional { condition, .. } | BlockEnd::Switch { condition, .. } => {
                    let next = replace_register_names(condition, &map);
                    if next != *condition {
                        *condition = next;
                        folded_igets.push(iget_off);
                    }
                }
                _ => {}
            }
        }
        for off in folded_igets {
            cfg.folded_const_offsets.insert(off);
        }
    }

    /// Last `iget` of `reg` before `before`, if nothing else writes `reg` in between.
    fn field_expr_reaching(
        &self,
        instructions: &[Instruction],
        before: u32,
        reg: u32,
    ) -> Option<(u32, String)> {
        let pos = instructions.iter().rposition(|i| i.offset < before)?;
        let mut left = 8u32;
        for ins in instructions[..=pos].iter().rev() {
            if left == 0 {
                break;
            }
            left -= 1;
            let m = ins.mnemonic();
            if m == "iget" || m == "iget-boolean" || m == "iget-byte" || m == "iget-char"
                || m == "iget-short" || m == "iget-wide"
            {
                let ops = self.resolve_operands(ins.operands());
                if let Some((dest, obj, field)) = parse_instance_field_operands(&ops) {
                    if dest == reg {
                        return Some((ins.offset, format!("v{}.{}", obj, field)));
                    }
                }
            }
            let (_, writes) = read_write::instruction_reads_writes(m, ins.operands());
            if writes.contains(&reg) {
                return None;
            }
        }
        None
    }

    /// `sget SwitchMap; ordinal; aget vD, map, idx; packed-switch vD` → switch on
    /// `map[idx]` (and fold the aget). Needed because D8 reuses the map register for
    /// the aget dest, so a plain rename would turn the selector into the bare map field.
    fn fold_aget_into_switch_conditions(&self, cfg: &mut MethodCfg, instructions: &[Instruction]) {
        let mut folded_agets = Vec::new();
        for block in &mut cfg.blocks {
            let BlockEnd::Switch {
                ref mut condition, ..
            } = block.end
            else {
                continue;
            };
            let Some(&probe) = block.instruction_offsets.last() else {
                continue;
            };
            let Some(ins) = instructions.iter().find(|i| i.offset == probe) else {
                continue;
            };
            if ins.mnemonic() != "packed-switch" && ins.mnemonic() != "sparse-switch" {
                continue;
            }
            let Some(reg) = parse_one_reg(ins.operands().split(',').next().unwrap_or("")) else {
                continue;
            };
            let Some(pos) = instructions.iter().rposition(|i| i.offset < probe) else {
                continue;
            };
            let mut left = 8u32;
            let mut aget_off = None;
            let mut array_reg = None;
            let mut index_reg = None;
            for ins in instructions[..=pos].iter().rev() {
                if left == 0 {
                    break;
                }
                left -= 1;
                let m = ins.mnemonic();
                if matches!(
                    m,
                    "aget"
                        | "aget-boolean"
                        | "aget-byte"
                        | "aget-char"
                        | "aget-short"
                        | "aget-object"
                ) {
                    if let Some((dest, arr, idx)) = parse_three_regs(ins.operands()) {
                        if dest == reg {
                            aget_off = Some(ins.offset);
                            array_reg = Some(arr);
                            index_reg = Some(idx);
                            break;
                        }
                    }
                }
                let (_, writes) = read_write::instruction_reads_writes(m, ins.operands());
                if writes.contains(&reg) {
                    break;
                }
            }
            let (Some(aoff), Some(arr), Some(idx)) = (aget_off, array_reg, index_reg) else {
                continue;
            };
            // Keep index as a register so `idx = e.ordinal()` stays in the body
            // (fixture tests look for `ordinal()`; full enum restore can still run later).
            let expr = format!("v{arr}[v{idx}]");
            let mut map = HashMap::new();
            map.insert(reg, expr.clone());
            let next = replace_register_names(condition, &map);
            if next != *condition {
                *condition = next;
                folded_agets.push(aoff);
            }
        }
        for off in folded_agets {
            cfg.folded_const_offsets.insert(off);
        }
    }

    /// `switch (v1.value)` → `switch (this.value)` using method-wide register names.
    fn rename_switch_conditions(&self, cfg: &mut MethodCfg) {
        let names = self.method_reg_names.borrow().clone().unwrap_or_default();
        if names.is_empty() {
            return;
        }
        for block in &mut cfg.blocks {
            if let BlockEnd::Switch { condition, .. } = &mut block.end {
                // Already folded to `arr[idx]` / `arr[vN.ordinal()]` — rename regs inside.
                *condition = replace_register_names(condition, &names);
            }
        }
    }

    /// Emit `} else if (…) { … }` chains when the else region is a single nested If;
    /// otherwise emit a plain `} else { … }`. Always closes with `}` at `indent`.
    fn emit_else_chain(
        &self,
        else_branch: &Region,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        base_off: usize,
        encoded: &EncodedMethod,
        code: &CodeItem,
        out: &mut String,
        indent: usize,
        skip_goto_to: Option<BlockId>,
        break_target: Option<BlockId>,
        declared: &mut HashSet<String>,
        global_used_regs: Option<&HashSet<u32>>,
        emit_range: Option<(u32, u32)>,
        class_name: Option<&str>,
    ) -> Result<()> {
        let ind = "    ".repeat(indent);
        let mut current = else_branch;
        loop {
            if let Some((cond, then_b, else_b)) = as_single_if(current) {
                mark_condition_idents_declared(cond, declared);
                writeln!(out, "{}}} else if ({}) {{", ind, shorten_java_names(cond))
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                let _ = self.emit_region(
                    then_b,
                    cfg,
                    instructions,
                    base_off,
                    encoded,
                    code,
                    out,
                    indent + 1,
                    skip_goto_to,
                    break_target,
                    declared,
                    global_used_regs,
                    emit_range,
                    class_name,
                )?;
                if region_is_empty_with_cfg(else_b, cfg) {
                    writeln!(out, "{}}}", ind)
                        .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                    return Ok(());
                }
                current = else_b;
                continue;
            }
            writeln!(out, "{}}} else {{", ind)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            let _ = self.emit_region(
                current,
                cfg,
                instructions,
                base_off,
                encoded,
                code,
                out,
                indent + 1,
                skip_goto_to,
                break_target,
                declared,
                global_used_regs,
                emit_range,
                class_name,
            )?;
            writeln!(out, "{}}}", ind)
                .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            return Ok(());
        }
    }

    /// Predecessor blocks that can reach `target` (for naming values tested by if-*).
    fn is_fall_through_edge(cfg: &MethodCfg, from: BlockId, to: BlockId) -> bool {
        match &cfg.blocks[from].end {
            BlockEnd::FallThrough => cfg.fall_through_block(from) == Some(to),
            BlockEnd::Conditional { fall_through, .. } => *fall_through == to,
            _ => false,
        }
    }

    fn is_conditional_branch_edge(cfg: &MethodCfg, from: BlockId, to: BlockId) -> bool {
        matches!(
            &cfg.blocks[from].end,
            BlockEnd::Conditional { branch_target, .. } if *branch_target == to
        )
    }

    /// Fall-through predecessors only (at most two hops).
    ///
    /// Recursive back-edge walking pulled in unrelated loop-body blocks and
    /// poisoned SSA naming (e.g. `binarySearch` compared against stale `i7`
    /// instead of `target`). We only need the immediate fall-through pred
    /// (and one extra hop for `invoke; move-result` in the prior block).
    fn predecessor_blocks_for_condition_naming(cfg: &MethodCfg, target: BlockId) -> Vec<BlockId> {
        let mut out = Vec::new();
        let mut frontier = vec![target];
        let mut seen = HashSet::from([target]);
        for _ in 0..2 {
            let mut next = Vec::new();
            for &t in &frontier {
                for (from, to) in cfg.successor_edges() {
                    if to != t || seen.contains(&from) {
                        continue;
                    }
                    if !Self::is_fall_through_edge(cfg, from, to) {
                        continue;
                    }
                    seen.insert(from);
                    out.push(from);
                    next.push(from);
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        // Else-if arm: `if-ne` taken target block (e.g. binarySearch `arr[mid] < target`).
        for (from, to) in cfg.successor_edges() {
            if to != target || seen.contains(&from) {
                continue;
            }
            if Self::is_conditional_branch_edge(cfg, from, to) {
                seen.insert(from);
                out.push(from);
            }
        }
        out.sort_by_key(|bid| cfg.blocks[*bid].start_offset);
        out
    }

    /// Instructions whose defs reach an if-* at the end of `block_id` (includes fall-through preds).
    fn block_instruction_seq_for_condition_naming(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        block_id: BlockId,
    ) -> Vec<Instruction> {
        let mut seq = self.block_instruction_seq(cfg, instructions, block_id, None, true, None);
        // `invoke; move-result` in the fall-through predecessor block.
        for pred in Self::predecessor_blocks_for_condition_naming(cfg, block_id) {
            let pred_seq = self.block_instruction_seq(cfg, instructions, pred, None, false, None);
            if pred_seq.iter().any(|i| i.mnemonic().starts_with("invoke")) {
                seq.splice(0..0, pred_seq);
            }
        }
        seq
    }

    /// Rename raw register references (vN) in condition strings.
    /// Prefer the name of the SSA value that reaches the branch in *this* block
    /// (so a reused register does not pick up a later View name like `view0`
    /// when the condition actually tests a boolean `z0`).
    fn rename_condition_registers(
        &self,
        cfg: &mut MethodCfg,
        instructions: &[Instruction],
        encoded: &EncodedMethod,
        code: &CodeItem,
    ) {
        let method_names = self.method_reg_names.borrow().clone().unwrap_or_default();
        let code_insns = code.insns_slice(&*self.dex.data);
        let base_off = code.insns_off;
        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;

        for block_id in 0..cfg.blocks.len() {
            let needs_rename = matches!(&cfg.blocks[block_id].end, BlockEnd::Conditional { .. });
            if !needs_rename {
                continue;
            }

            // Include fall-through predecessor blocks so `invoke; move-result` + `if-nez`
            // in adjacent blocks still names the tested value as `obj1`, not stale `obj0`.
            let seq = self.block_instruction_seq_for_condition_naming(cfg, instructions, block_id);
            let mut local_names: HashMap<u32, String> = HashMap::new();
            let mut local_types: HashMap<u32, String> = HashMap::new();
            if !seq.is_empty() {
                let stmts = self
                    .instructions_to_ir(&seq, base_off, code_insns, Some(instructions))
                    .unwrap_or_default();
                let mut runner = PassRunner::new();
                runner.add(InvokeChainPass);
                runner.add(SsaRenamePass);
                runner.add(CopyPropPass);
                runner.add(ConstructorMergePass);
                runner.add(ExprSimplifyPass);
                runner.add(InlineFilledArrayPass);
                let stmts = runner.run(stmts);
                let types = self.infer_types_for(encoded, code, &stmts);
                let mut name_map =
                    build_var_names_with_regs(&stmts, &types, registers_size, ins_size, is_static);
                self.apply_debug_names_to_name_map(&mut name_map, &types, code, encoded);
                self.apply_signature_param_names(&mut name_map, encoded, code);

                // Highest SSA version per register in this block = value reaching the condition.
                let mut best_ver: HashMap<u32, u32> = HashMap::new();
                for (vid, name) in &name_map {
                    let prev = best_ver.get(&vid.reg).copied().unwrap_or(0);
                    if !local_names.contains_key(&vid.reg) || vid.ver >= prev {
                        local_names.insert(vid.reg, name.clone());
                        best_ver.insert(vid.reg, vid.ver);
                        if let Some(ty) = types.get(vid) {
                            local_types.insert(vid.reg, ty.clone());
                        }
                    }
                }
            }

            // Prefer reaching-def names; fall back to method-wide only when types agree.
            let mut merged = method_names.clone();
            for (reg, name) in &local_names {
                if local_types.get(reg).map(|s| s.as_str()) == Some("boolean") {
                    merged.insert(*reg, name.clone());
                    continue;
                }
                if let Some(method) = method_names.get(reg) {
                    // One method-wide name per register — keep it for conditions so
                    // `int i5 = arr[mid]; if (i5 != target)` stays consistent.
                    if is_synthetic_local_name(name) && is_synthetic_local_name(method) {
                        continue;
                    }
                }
                merged.insert(*reg, name.clone());
            }
            // Drop stale method-wide names when the reaching def has a different type
            // (e.g. method-wide `obj0` for Object reuse, branch tests `obj1` ConnectivityManager).
            if let Some(reg_types) = self.method_reg_types.borrow().as_ref() {
                for (reg, local_ty) in &local_types {
                    let method_ty = reg_types.get(reg).map(|s| s.as_str());
                    if !types_compatible_for_naming(Some(local_ty.as_str()), method_ty)
                        && method_ty.is_some()
                    {
                        if let Some(local_name) = local_names.get(reg) {
                            merged.insert(*reg, local_name.clone());
                        } else {
                            merged.remove(reg);
                        }
                    }
                }
                // Registers tested by this branch: never keep method-wide name when local type differs.
                for reg in condition_regs_in_block(cfg, block_id, instructions) {
                    let Some(local_ty) = local_types.get(&reg) else {
                        continue;
                    };
                    let method_ty = reg_types.get(&reg).map(|s| s.as_str());
                    if method_ty.is_some()
                        && !types_compatible_for_naming(Some(local_ty.as_str()), method_ty)
                    {
                        if let Some(local_name) = local_names.get(&reg) {
                            merged.insert(reg, local_name.clone());
                        } else {
                            merged.remove(&reg);
                        }
                    }
                }
            }

            let cond_regs = condition_regs_in_block(cfg, block_id, instructions);
            if let Some(reg_types) = self.method_reg_types.borrow().as_ref() {
                for reg in &cond_regs {
                    if let Some(mt) = reg_types.get(reg) {
                        local_types.entry(*reg).or_insert_with(|| mt.clone());
                    }
                    if let Some(name) = merged.get(reg) {
                        local_names.entry(*reg).or_insert_with(|| name.clone());
                    }
                }
            }

            if let BlockEnd::Conditional {
                ref mut condition, ..
            } = cfg.blocks[block_id].end
            {
                *condition = replace_register_names(condition, &merged);
                *condition = polish_boolean_condition(condition, &local_names, &local_types);
            }
        }
    }

    /// Collect register numbers that are read (used) in any block, so dead-assign doesn't remove assigns used in other blocks.
    /// Runs the pipeline once over the full method instead of per-block for speed.
    /// Also includes registers only referenced by branch/switch conditions (not present in IR stmts).
    fn method_used_regs(
        &self,
        _cfg: &MethodCfg,
        instructions: &[Instruction],
        base_off: usize,
        code_insns: &[u8],
    ) -> HashSet<u32> {
        let mut runner = PassRunner::new();
        runner.add(InvokeChainPass);
        runner.add(SsaRenamePass);
        runner.add(CopyPropPass);
        runner.add(InlineFilledArrayPass);
        let stmts = self
            .instructions_to_ir(instructions, base_off, code_insns, None)
            .unwrap_or_default();
        let stmts = runner.run(stmts);
        let mut regs = used_regs(&stmts);
        for ins in instructions {
            let m = ins.mnemonic();
            if m.starts_with("if-")
                || m == "packed-switch"
                || m == "sparse-switch"
                || m == "return"
                || m == "return-wide"
                || m == "return-object"
            {
                for reg in regs_mentioned_in_operands(ins.operands()) {
                    regs.insert(reg);
                }
            }
        }
        regs
    }

    /// Load DEX debug local types (descriptors → Java) before IR emission so consts format correctly.
    fn load_method_debug_types(&self, code: &CodeItem) {
        if !self.use_debug_names || code.debug_info_off == 0 {
            *self.method_debug_types.borrow_mut() = None;
            return;
        }
        let Ok(dbg) = self.dex.debug_info_for_code(code) else {
            *self.method_debug_types.borrow_mut() = None;
            return;
        };
        let mut map = HashMap::new();
        for (reg, desc) in &dbg.register_types {
            let java = java::descriptor_to_java(desc);
            if !java.is_empty() {
                map.insert(*reg, java);
            }
        }
        *self.method_debug_types.borrow_mut() = if map.is_empty() { None } else { Some(map) };
    }

    fn debug_type_for_reg(&self, reg: u32) -> Option<String> {
        self.method_debug_types
            .borrow()
            .as_ref()
            .and_then(|m| m.get(&reg).cloned())
    }

    /// Build whole-method register→type and register→name maps for cross-block consistency.
    fn prepare_method_reg_types(
        &self,
        instructions: &[Instruction],
        base_off: usize,
        encoded: &EncodedMethod,
        code: &CodeItem,
        code_insns: &[u8],
        class_name: Option<&str>,
    ) {
        self.load_method_debug_types(code);
        let stmts = self
            .instructions_to_ir(instructions, base_off, code_insns, None)
            .unwrap_or_default();
        // No DeadAssignPass: values only consumed by branch conditions would be dropped otherwise.
        let mut runner = PassRunner::new();
        runner.add(InvokeChainPass);
        runner.add(SsaRenamePass);
        runner.add(CopyPropPass);
        runner.add(ConstructorMergePass);
        runner.add(ExprSimplifyPass);
        runner.add(InlineFilledArrayPass);
        let stmts = runner.run(stmts);
        let types = self.infer_types_for(encoded, code, &stmts);
        let mut by_reg: HashMap<u32, String> = HashMap::new();
        let mut ver_seen: HashMap<u32, u32> = HashMap::new();
        for (vid, ty) in &types {
            let prev_ver = ver_seen.get(&vid.reg).copied().unwrap_or(0);
            let replace = match by_reg.get(&vid.reg) {
                None => true,
                Some(existing) if !existing.ends_with("[]") && ty.ends_with("[]") => true,
                Some(existing) if existing.ends_with("[]") && !ty.ends_with("[]") => false,
                Some(_) if vid.ver >= prev_ver => true,
                _ => false,
            };
            if replace {
                by_reg.insert(vid.reg, ty.clone());
                ver_seen.insert(vid.reg, vid.ver);
            }
        }
        *self.method_reg_types.borrow_mut() = Some(by_reg.clone());

        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;
        let mut name_map =
            build_var_names_with_regs(&stmts, &types, registers_size, ins_size, is_static);
        self.apply_debug_names_to_name_map(&mut name_map, &types, code, encoded);
        self.apply_signature_param_names(&mut name_map, encoded, code);
        // Bake user variable renames into method-wide names (conditions / cross-block).
        self.apply_variable_renames_to_name_map(&mut name_map, class_name, encoded);

        // One stable display name per register, preferring names whose SSA type matches the
        // method-level type (so `permissions` wins over a prior string temp on the same reg).
        let mut names_by_reg: HashMap<u32, String> = HashMap::new();
        let mut used_names: HashSet<String> = HashSet::new();
        let mut regs: Vec<u32> = name_map.keys().map(|v| v.reg).collect();
        regs.sort_unstable();
        regs.dedup();
        for reg in regs {
            let method_ty = by_reg.get(&reg).map(|s| s.as_str());
            let mut best: Option<(i32, String)> = None;
            for (vid, name) in &name_map {
                if vid.reg != reg {
                    continue;
                }
                let ty = types.get(vid).map(|s| s.as_str());
                if !types_compatible_for_naming(ty, method_ty)
                    && method_ty.is_some()
                    && ty.is_some()
                {
                    continue;
                }
                let score = if name == "length" || name == "result" {
                    // Role names — prefer debug locals (`acc`, `sum`, `out`, `n`).
                    5 + vid.ver as i32
                } else if ty.is_some_and(is_primitive_java_type)
                    && !is_synthetic_local_name(name)
                    && types
                        .iter()
                        .any(|(v, t)| v.reg == reg && !is_primitive_java_type(t))
                {
                    // Debug name `host` on a reused int (`const/4 0` for resolveActivity flags).
                    5 + vid.ver as i32
                } else if !is_synthetic_local_name(name) {
                    100 + vid.ver as i32
                } else {
                    10 + vid.ver as i32
                };
                match &best {
                    None => best = Some((score, name.clone())),
                    Some((s, _)) if score > *s => best = Some((score, name.clone())),
                    _ => {}
                }
            }
            if let Some((_, mut name)) = best {
                if used_names.contains(&name) && name != "this" {
                    let mut n = 0u32;
                    let base = name.clone();
                    loop {
                        let candidate = format!("{}_{}", base, n);
                        if !used_names.contains(&candidate) {
                            name = candidate;
                            break;
                        }
                        n += 1;
                    }
                }
                used_names.insert(name.clone());
                names_by_reg.insert(reg, name);
            }
        }
        if !is_static {
            let this_reg = registers_size.saturating_sub(ins_size);
            for reg in registers_holding_this(instructions, this_reg) {
                names_by_reg.insert(reg, "this".to_string());
            }
        }
        // Parameters only referenced by branch conditions may be absent from name_map.
        let sig = self.signature_param_names(encoded, code);
        for (reg, name) in sig {
            names_by_reg.entry(reg).or_insert(name);
        }
        if self.use_debug_names && code.debug_info_off != 0 {
            if let Ok(dbg) = self.dex.debug_info_for_code(code) {
                let param_base = registers_size.saturating_sub(ins_size);
                let param_name_offset = if is_static { 0usize } else { 1usize };
                for (i, pname) in dbg.parameter_names.iter().enumerate() {
                    let Some(name) = pname.as_ref() else { continue };
                    if !is_java_ident(name) {
                        continue;
                    }
                    let reg = param_base + param_name_offset as u32 + i as u32;
                    names_by_reg.insert(reg, name.clone());
                }
            }
        }
        *self.method_reg_names.borrow_mut() = Some(names_by_reg);
    }

    /// Instruction list for a block (for method_used_regs or emit_block_instructions).
    /// When skip_last_instruction is true (e.g. loop header with condition), omit the last instruction.
    /// When emit_range is Some((min, max)), only include instructions with offset in [min, max).
    fn block_instruction_seq(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        block_id: BlockId,
        skip_goto_to: Option<BlockId>,
        skip_last_instruction: bool,
        emit_range: Option<(u32, u32)>,
    ) -> Vec<Instruction> {
        let block = &cfg.blocks[block_id];
        let skip_last = match &block.end {
            BlockEnd::Goto(t) if skip_goto_to == Some(*t) => true,
            _ => false,
        };
        let offs = &block.instruction_offsets;
        let mut seq: Vec<Instruction> = Vec::new();
        let drop_last = skip_last_instruction
            && matches!(
                &block.end,
                BlockEnd::Conditional { .. } | BlockEnd::Switch { .. }
            );
        let skip_switch_ins =
            skip_last_instruction && matches!(&block.end, BlockEnd::Switch { .. });
        let limit = if drop_last && !offs.is_empty() {
            offs.len() - 1
        } else {
            offs.len()
        };
        for (i, &off) in offs.iter().enumerate() {
            if let Some((min_byte, max_byte)) = emit_range {
                if off < min_byte {
                    continue;
                }
                if off >= max_byte {
                    break;
                }
            }
            if i >= limit {
                break;
            }
            let is_last = i == offs.len() - 1;
            if skip_last && is_last {
                break;
            }
            if let Some(ins) = instructions.iter().find(|ins| (ins.offset as u32) == off) {
                if cfg.folded_const_offsets.contains(&off) {
                    continue;
                }
                if skip_switch_ins
                    && (ins.mnemonic() == "packed-switch" || ins.mnemonic() == "sparse-switch")
                {
                    continue;
                }
                seq.push(ins.clone());
            }
        }
        seq
    }

    /// True if block has exactly one instruction that is const/4, const/16, or const.
    fn block_is_single_const_init(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        block_id: BlockId,
    ) -> bool {
        let seq = self.block_instruction_seq(cfg, instructions, block_id, None, false, None);
        if seq.len() != 1 {
            return false;
        }
        let m = seq[0].mnemonic();
        m == "const/4" || m == "const/16" || m == "const"
    }

    /// True if block has exactly one instruction that is an add-int (update). The block may
    /// fall through to a goto-back block (two-block tail) or contain goto itself (single-block tail).
    fn block_is_single_update_and_back_edge(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        block_id: BlockId,
        header: BlockId,
    ) -> bool {
        let block = &cfg.blocks[block_id];
        let seq = self.block_instruction_seq(cfg, instructions, block_id, None, false, None);
        if seq.len() == 1 {
            let m = seq[0].mnemonic();
            let add_like = m.starts_with("add-int") || m.starts_with("add-long");
            if add_like {
                return true;
            }
        }
        if seq.len() == 2 {
            let m = seq[0].mnemonic();
            let add_like = m.starts_with("add-int") || m.starts_with("add-long");
            let goto_header = matches!(&block.end, BlockEnd::Goto(t) if *t == header);
            return add_like && goto_header;
        }
        false
    }

    /// Treat incoming Dalvik parameter registers as already declared so a later
    /// SSA assign (`b = b + 1`) does not emit `int b = ...` after `if (b == 10)`.
    fn seed_declared_from_params(
        &self,
        declared: &mut HashSet<String>,
        code: &CodeItem,
        encoded: &EncodedMethod,
    ) {
        for n in self.signature_param_names(encoded, code).values() {
            if n != "this" {
                declared.insert(n.clone());
            }
        }
        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let param_base = registers_size.saturating_sub(ins_size);
        if let Some(names) = self.method_reg_names.borrow().as_ref() {
            for reg in param_base..registers_size {
                if let Some(n) = names.get(&reg) {
                    if n != "this" {
                        declared.insert(n.clone());
                    }
                }
            }
        }
    }

    /// DEX debug names for each declared parameter (`None` = unnamed).
    /// Prefers `debug_info.parameter_names`, then `DBG_START_LOCAL` on the param register
    /// so the signature matches the body (`grantResults` vs `arr2`).
    fn debug_parameter_display_names(
        &self,
        encoded: &EncodedMethod,
        param_types: &[String],
    ) -> Vec<Option<String>> {
        if !self.use_debug_names || encoded.code_off == 0 {
            return vec![None; param_types.len()];
        }
        let Ok(code) = self.dex.get_code_item(encoded.code_off) else {
            return vec![None; param_types.len()];
        };
        if code.debug_info_off == 0 {
            return vec![None; param_types.len()];
        }
        let Ok(dbg) = self.dex.debug_info_for_code(&code) else {
            return vec![None; param_types.len()];
        };
        let is_static = (encoded.access_flags & 0x8) != 0;
        let param_base = (code.registers_size as u32).saturating_sub(code.ins_size as u32);
        debug_param_names_from_tables(
            &dbg.parameter_names,
            &dbg.register_names,
            param_base,
            is_static,
            param_types,
        )
    }

    /// Dalvik param register → signature display name (`this`, `p0`, `s1`, …).
    fn signature_param_names(
        &self,
        encoded: &EncodedMethod,
        code: &CodeItem,
    ) -> HashMap<u32, String> {
        let Ok(info) = self.dex.get_method_info(encoded.method_idx) else {
            return HashMap::new();
        };
        let is_static = (encoded.access_flags & 0x8) != 0;
        let param_types: Vec<String> = info
            .params
            .iter()
            .map(|p| java::descriptor_to_java(p))
            .collect();
        type_infer::param_names_by_register(
            code.registers_size as u32,
            code.ins_size as u32,
            is_static,
            &param_types,
        )
    }

    /// Keep body names for parameter registers in sync with the method signature.
    /// Temps (`v0`, `x0`, `s0`) are replaced; debug/user names (`email`) are kept.
    fn apply_signature_param_names(
        &self,
        name_map: &mut HashMap<VarId, String>,
        encoded: &EncodedMethod,
        code: &CodeItem,
    ) {
        let sig = self.signature_param_names(encoded, code);
        if sig.is_empty() {
            return;
        }
        for (vid, name) in name_map.iter_mut() {
            let Some(sig_name) = sig.get(&vid.reg) else {
                continue;
            };
            if name.as_str() == "this" {
                continue;
            }
            if sig_name == "this" {
                *name = "this".to_string();
                continue;
            }
            if is_synthetic_local_name(name) || is_signature_style_name(name) {
                *name = sig_name.clone();
            }
        }
    }

    /// Overlay DEX debug_info local/parameter names onto the variable name map.
    /// Applies locals only to SSA versions whose type matches the last definition's type,
    /// so `email` does not overwrite the Activity/`this` receiver used by `findViewById`.
    fn apply_debug_names_to_name_map(
        &self,
        name_map: &mut HashMap<VarId, String>,
        type_map: &HashMap<VarId, String>,
        code: &CodeItem,
        encoded: &EncodedMethod,
    ) {
        if !self.use_debug_names || code.debug_info_off == 0 {
            return;
        }
        let Ok(dbg) = self.dex.debug_info_for_code(code) else {
            return;
        };
        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;
        let param_base = registers_size.saturating_sub(ins_size);
        // Parameter names: skip `this` for instance methods.
        let param_name_offset = if is_static { 0usize } else { 1usize };
        for (i, pname) in dbg.parameter_names.iter().enumerate() {
            let Some(name) = pname.as_ref() else { continue };
            if !is_java_ident(name) {
                continue;
            }
            let reg = param_base + param_name_offset as u32 + i as u32;
            for (var, display) in name_map.iter_mut() {
                if var.reg == reg && display.as_str() != "this" {
                    *display = name.clone();
                }
            }
        }

        let latest_ver_by_reg: HashMap<u32, u32> =
            name_map.keys().fold(HashMap::new(), |mut m, v| {
                m.entry(v.reg)
                    .and_modify(|ver| *ver = (*ver).max(v.ver))
                    .or_insert(v.ver);
                m
            });

        // Names already claimed (params / prior renames) so register-reuse locals do not collide.
        let mut used_names: HashSet<String> = HashSet::new();
        for display in name_map.values() {
            if display.as_str() != "this" && is_java_ident(display) && !is_temp_like_name(display)
            {
                used_names.insert(display.clone());
            }
        }

        // Name every SSA version from debug locals on that register, matching by type so
        // early `double f` is not overwritten by a later `float gettype` on the same reg.
        let mut vars: Vec<VarId> = name_map.keys().copied().collect();
        vars.sort_by_key(|v| (v.reg, v.ver));
        for var in vars {
            let Some(display) = name_map.get_mut(&var) else {
                continue;
            };
            if display.as_str() == "this" {
                continue;
            }
            // Keep meaningful non-temp names already assigned (e.g. params).
            // Role names `result` / `length` are synthetic overlays — allow debug locals
            // (`sum`, `max`, `out`, `n`) to replace them.
            let is_role_overlay = matches!(display.as_str(), "result" | "length");
            if is_java_ident(display) && !is_temp_like_name(display) && !is_role_overlay {
                used_names.insert(display.clone());
                continue;
            }
            let locals = dbg.locals_for_reg(var.reg);
            if locals.is_empty() {
                // Fall back to single last-known name for the latest SSA version only.
                if latest_ver_by_reg.get(&var.reg) != Some(&var.ver) {
                    continue;
                }
                let Some(n) = dbg.name_for_reg(var.reg) else {
                    continue;
                };
                if !is_java_ident(n) || used_names.contains(n) {
                    continue;
                }
                let target_ty = preferred_debug_type_for_reg(var.reg, type_map);
                let this_ty = type_map.get(&var).map(|s| s.as_str());
                if types_compatible_for_naming(this_ty, target_ty) {
                    *display = n.to_string();
                    used_names.insert(n.to_string());
                }
                continue;
            }

            let this_ty = type_map.get(&var).map(|s| s.as_str());
            let mut exact: Option<&str> = None;
            let mut compatible: Option<&str> = None;
            for local in locals {
                if !is_java_ident(&local.name) || used_names.contains(&local.name) {
                    continue;
                }
                let dbg_ty = local
                    .type_desc
                    .as_ref()
                    .map(|d| java::descriptor_to_java(d));
                if !types_compatible_for_naming(this_ty, dbg_ty.as_deref()) {
                    continue;
                }
                if this_ty.is_some() && dbg_ty.as_deref() == this_ty {
                    exact = Some(local.name.as_str());
                    break;
                }
                if compatible.is_none() {
                    compatible = Some(local.name.as_str());
                }
            }
            if let Some(n) = exact.or(compatible) {
                *display = n.to_string();
                used_names.insert(n.to_string());
            }
        }
    }

    /// Apply user-defined variable renames to the name_map (key: ClassName#methodName).
    /// For constructors, also accepts UI keys that use the simple class name instead of `<init>`.
    fn apply_variable_renames_to_name_map(
        &self,
        name_map: &mut HashMap<VarId, String>,
        class_name: Option<&str>,
        encoded: &EncodedMethod,
    ) {
        let (Some(class_name), Some(ref rename_map)) = (class_name, self.rename_map.as_ref())
        else {
            return;
        };
        let Ok(info) = self.dex.get_method_info(encoded.method_idx) else {
            return;
        };
        let method_key = format!("{}#{}", class_name, info.name);
        let var_renames = rename_map.variable.get(&method_key).or_else(|| {
            if info.name == "<init>" {
                let simple = class_name.rsplit('.').next().unwrap_or(class_name);
                rename_map
                    .variable
                    .get(&format!("{}#{}", class_name, simple))
            } else {
                None
            }
        });
        let Some(var_renames) = var_renames else {
            return;
        };
        for (_, name) in name_map.iter_mut() {
            if let Some(new_name) = var_renames.get(name.as_str()) {
                *name = new_name.clone();
            }
        }
    }

    fn emit_block_instructions(
        &self,
        cfg: &MethodCfg,
        instructions: &[Instruction],
        base_off: usize,
        block_id: BlockId,
        skip_goto_to: Option<BlockId>,
        break_target: Option<BlockId>,
        encoded: &EncodedMethod,
        code: &CodeItem,
        out: &mut String,
        indent: usize,
        declared: &mut HashSet<String>,
        global_used_regs: Option<&HashSet<u32>>,
        skip_last_instruction: bool,
        emit_range: Option<(u32, u32)>,
        class_name: Option<&str>,
    ) -> Result<bool> {
        let ind = "    ".repeat(indent);
        let block = &cfg.blocks[block_id];
        let seq = self.block_instruction_seq(
            cfg,
            instructions,
            block_id,
            skip_goto_to,
            skip_last_instruction,
            emit_range,
        );
        let code_insns = code.insns_slice(&*self.dex.data);
        let stmts = self.instructions_to_ir(&seq, base_off, code_insns, Some(instructions))?;
        let stmts = if let Some(used_regs) = global_used_regs {
            let mut runner = PassRunner::new();
            runner.add(InvokeChainPass);
            runner.add(SsaRenamePass);
            runner.add(CopyPropPass);
            runner.add(ConstructorMergePass);
            runner.add(ExprSimplifyPass);
            runner.add(InlineFilledArrayPass);
            run_dead_assign_with_used_regs(runner.run(stmts), used_regs)
        } else {
            self.default_pass_runner().run(stmts)
        };
        let mut type_map = self.infer_types_for(encoded, code, &stmts);
        if let Some(reg_types) = self.method_reg_types.borrow().as_ref() {
            let debug_types = self.method_debug_types.borrow();
            self.enrich_types_for(
                &mut type_map,
                reg_types,
                debug_types.as_ref(),
                &stmts,
            );
        } else if let Some(debug_types) = self.method_debug_types.borrow().as_ref() {
            self.enrich_types_for(
                &mut type_map,
                &HashMap::new(),
                Some(debug_types),
                &stmts,
            );
        }
        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;
        let mut name_map =
            build_var_names_with_regs(&stmts, &type_map, registers_size, ins_size, is_static);
        self.apply_phi_register_names(&mut name_map, &type_map);
        self.apply_debug_names_to_name_map(&mut name_map, &type_map, code, encoded);
        // Prefer method-wide names so loop index / length / temps stay consistent across blocks.
        if let Some(reg_names) = self.method_reg_names.borrow().as_ref() {
            let reg_types = self.method_reg_types.borrow();
            for (vid, name) in name_map.iter_mut() {
                let Some(method_name) = reg_names.get(&vid.reg) else {
                    continue;
                };
                // A register that is only ever a copy of `this` must stay `this`
                // in every block, including loops that did not see the prologue move.
                if method_name == "this" {
                    *name = "this".to_string();
                    continue;
                }
                let ty = type_map.get(vid).map(|s| s.as_str());
                let method_ty = reg_types
                    .as_ref()
                    .and_then(|t| t.get(&vid.reg))
                    .map(|s| s.as_str());
                // Only share method-wide names when types agree. Do not apply a
                // method-wide String name onto a boolean SSA version (or vice versa).
                // `length` is an array-length role — never paint a prior const on the
                // same register (`int length = 1002`).
                if method_name == "length" && name.as_str() != "length" {
                    continue;
                }
                // Do not rename primitive temps to later array/graph locals on the same register.
                if is_synthetic_local_name(name)
                    && !is_synthetic_local_name(method_name)
                    && ty.is_some_and(type_infer::is_primitive_java_type)
                    && method_ty.is_some_and(|t| !type_infer::is_primitive_java_type(t))
                {
                    continue;
                }
                if ty.is_some() && method_ty.is_some() {
                    if types_compatible_for_naming(ty, method_ty) {
                        *name = method_name.clone();
                    }
                } else if ty.is_some_and(type_infer::is_primitive_java_type)
                    && method_ty.is_some_and(|t| t.ends_with("[]"))
                {
                    // Keep `i4` for `const/4 4` — do not paint with later `int[] arr4`.
                    continue;
                } else if ty.is_none() && is_synthetic_local_name(name) {
                    // A use with no type in this block still belongs to the
                    // method-wide name (`j = result` after `int result = j + 1`).
                    *name = method_name.clone();
                } else if ty.is_none() && method_ty.is_none() {
                    *name = method_name.clone();
                }
            }
        }
        self.apply_signature_param_names(&mut name_map, encoded, code);
        // User renames must win over method-wide / debug names.
        self.apply_variable_renames_to_name_map(&mut name_map, class_name, encoded);
        for line in self.codegen_ir_lines(&stmts, Some(&type_map), Some(&name_map), declared) {
            if !line.is_empty() {
                writeln!(out, "{}{}", ind, line)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
            }
        }
        match &block.end {
            BlockEnd::Goto(t) if break_target == Some(*t) => {
                writeln!(out, "{}break;", ind)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                Ok(true)
            }
            BlockEnd::Goto(t) if skip_goto_to == Some(*t) => {
                writeln!(out, "{}continue;", ind)
                    .map_err(|_| DexDecompilerError::Decompilation("write".into()))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn codegen_ir_lines(
        &self,
        stmts: &[IrStmt],
        type_map: Option<&HashMap<VarId, String>>,
        name_map: Option<&HashMap<VarId, String>>,
        declared: &mut HashSet<String>,
    ) -> Vec<String> {
        stmts
            .iter()
            .map(|s| self.codegen_stmt_line(s, type_map, name_map, declared))
            .collect()
    }

    fn codegen_stmt_line(
        &self,
        stmt: &IrStmt,
        type_map: Option<&HashMap<VarId, String>>,
        name_map: Option<&HashMap<VarId, String>>,
        declared: &mut HashSet<String>,
    ) -> String {
        let line = match (stmt, type_map) {
            (IrStmt::Assign { dst, rhs, comment }, Some(types)) if types.get(dst).is_some() => {
                let ty = types.get(dst).unwrap();
                let var = name_map
                    .and_then(|n| n.get(dst).cloned())
                    .unwrap_or_else(|| IrExpr::Var(*dst).to_java());
                let rhs_str = rhs.to_java_with_names(name_map);
                // Handle compound assign markers from ExprSimplifyPass
                if let Some(compound) = rhs_str.strip_prefix("__compound_") {
                    format_compound_line(compound, &var, name_map)
                } else if declared.insert(var.clone()) {
                    format!("{} {} = {};", shorten_type(ty), var, rhs_str)
                } else {
                    format!("{} = {};", var, rhs_str)
                }
            }
            _ => {
                let raw = stmt.to_java_line_with_names(name_map);
                // Handle compound assign in untyped path
                if raw.contains("__compound_") {
                    resolve_compound_in_line(&raw, name_map)
                } else {
                    raw
                }
            }
        };
        shorten_java_names(&line)
    }

    /// Convert a sequence of Dalvik instructions into method IR (raw form).
    /// InvokeChainPass is run by the pipeline to merge invoke+move-result+return.
    /// When `instructions` is a block subset, pass `full_instructions` so bytecode hex is correct for single-instruction blocks.
    fn format_const_wide_rhs(
        &self,
        mnemonic: &str,
        dst_reg: u32,
        bits_str: &str,
        instructions: &[Instruction],
        idx: usize,
    ) -> String {
        let java_ty = self
            .wide_const_java_type(mnemonic, dst_reg, bits_str, instructions, idx)
            .unwrap_or_else(|| "long".to_string());
        format_java_wide_literal(bits_str, &java_ty)
    }

    fn format_const_32_rhs(
        &self,
        dst_reg: u32,
        bits_str: &str,
        instructions: &[Instruction],
        idx: usize,
    ) -> String {
        let as_float = self.const_32_used_as_float(dst_reg, instructions, idx)
            || self.debug_type_for_reg(dst_reg).as_deref() == Some("float");
        if as_float {
            if let Some(bits) = parse_const_bits_u32(bits_str) {
                return format_java_float(f32::from_bits(bits));
            }
            // `const/16 vN, 0` with float debug type → `0.0f`
            if bits_str.trim() == "0" {
                return "0.0f".to_string();
            }
        }
        bits_str.to_string()
    }

    fn wide_const_java_type(
        &self,
        mnemonic: &str,
        dst_reg: u32,
        bits_str: &str,
        instructions: &[Instruction],
        idx: usize,
    ) -> Option<String> {
        if let Some(ty) = self.debug_type_for_reg(dst_reg) {
            if matches!(ty.as_str(), "long" | "double") {
                return Some(ty);
            }
        }
        if let Some(ty) = self.infer_wide_type_from_uses(dst_reg, instructions, idx) {
            return Some(ty);
        }
        if self.is_wide_const_returned(dst_reg, instructions, idx) {
            if let Some(ret_ty) = self.method_return_type.borrow().as_ref() {
                if matches!(ret_ty.as_str(), "long" | "double") {
                    return Some(ret_ty.clone());
                }
            }
        }
        // Prefer the method-level type only when it is long/double (not float/ref from reuse).
        if let Some(reg_types) = self.method_reg_types.borrow().as_ref() {
            if let Some(ty) = reg_types.get(&dst_reg) {
                if matches!(ty.as_str(), "long" | "double") {
                    return Some(ty.clone());
                }
            }
        }
        // `const-wide/high16` immediates are almost always doubles (low 48 bits clear).
        if mnemonic == "const-wide/high16" {
            if let Ok(signed) = bits_str.trim().parse::<i64>() {
                if looks_like_high16_double_bits(signed as u64) {
                    return Some("double".to_string());
                }
            }
        }
        // Small const-wide/16 immediates with debug type absent: prefer long (source often uses long).
        if matches!(mnemonic, "const-wide/16" | "const-wide/32") {
            return Some("long".to_string());
        }
        None
    }

    /// Look ahead for `*-double` / `*-long` uses of a wide const (through `move-wide`).
    fn infer_wide_type_from_uses(
        &self,
        dst_reg: u32,
        instructions: &[Instruction],
        idx: usize,
    ) -> Option<String> {
        let mut reg = dst_reg;
        for ins in instructions.iter().skip(idx + 1).take(64) {
            let m = ins.mnemonic();
            let ops = self.resolve_operands(ins.operands());
            if m.starts_with("move-wide") {
                if let Some((d, s)) = parse_two_regs(&ops) {
                    if s == reg {
                        reg = d;
                        continue;
                    }
                    if d == reg {
                        return None;
                    }
                }
                continue;
            }
            if operand_mentions_reg(&ops, reg) {
                if m == "return-wide" {
                    if let Some(ret_ty) = self.method_return_type.borrow().as_ref() {
                        if matches!(ret_ty.as_str(), "long" | "double") {
                            return Some(ret_ty.clone());
                        }
                    }
                    return None;
                }
                if m.contains("double") {
                    return Some("double".to_string());
                }
                if m.contains("long") {
                    return Some("long".to_string());
                }
            }
            let (_reads, writes) = read_write::instruction_reads_writes(m, ins.operands());
            if writes.contains(&reg) {
                return None;
            }
        }
        None
    }

    fn const_32_used_as_float(
        &self,
        dst_reg: u32,
        instructions: &[Instruction],
        idx: usize,
    ) -> bool {
        let mut reg = dst_reg;
        for ins in instructions.iter().skip(idx + 1).take(32) {
            let m = ins.mnemonic();
            let ops = self.resolve_operands(ins.operands());
            if matches!(m, "move" | "move/from16" | "move/16") {
                if let Some((d, s)) = parse_two_regs(&ops) {
                    if s == reg {
                        reg = d;
                        continue;
                    }
                    if d == reg {
                        return false;
                    }
                }
                continue;
            }
            if operand_mentions_reg(&ops, reg) {
                if m == "float-to-double"
                    || m == "float-to-int"
                    || m == "float-to-long"
                    || m.contains("float")
                {
                    return true;
                }
            }
            let (_reads, writes) = read_write::instruction_reads_writes(m, ins.operands());
            if writes.contains(&reg) {
                return false;
            }
        }
        false
    }

    fn is_wide_const_returned(
        &self,
        dst_reg: u32,
        instructions: &[Instruction],
        idx: usize,
    ) -> bool {
        for ins in instructions.iter().skip(idx + 1) {
            let m = ins.mnemonic();
            if m == "return-wide" {
                let ops = self.resolve_operands(ins.operands());
                return parse_one_reg(&ops) == Some(dst_reg);
            }
            if m != "nop" && !m.starts_with("const-wide") {
                return false;
            }
        }
        false
    }

    fn instructions_to_ir(
        &self,
        instructions: &[Instruction],
        base_off: usize,
        code_insns: &[u8],
        full_instructions: Option<&[Instruction]>,
    ) -> Result<Vec<IrStmt>> {
        #[derive(Clone, Debug)]
        struct PendingInvoke {
            call_expr: IrExpr,
            comment: String,
        }

        let mut out: Vec<IrStmt> = Vec::new();
        let mut pending_invoke: Option<PendingInvoke> = None;

        let flush_pending_invoke =
            |out: &mut Vec<IrStmt>, pending_invoke: &mut Option<PendingInvoke>| {
                if let Some(pi) = pending_invoke.take() {
                    out.push(IrStmt::Expr {
                        expr: pi.call_expr,
                        comment: Some(pi.comment),
                    });
                }
            };

        for (idx, ins) in instructions.iter().enumerate() {
            let m = ins.mnemonic();
            if m.ends_with("-payload") {
                continue;
            }
            let ops = ins.operands();
            let ops_resolved = self.resolve_operands(ops);
            let offset = ins.offset as usize + base_off;
            let raw_hex =
                Self::instruction_raw_hex(instructions, idx, code_insns, full_instructions);
            let ops_display = if m == "goto" {
                format_goto_operands_signed(&raw_hex, &ops)
            } else {
                ops.to_string()
            };
            let comment = format!(
                "// [{}] {:04x} ({}): {} {}",
                idx, offset, raw_hex, m, ops_display
            );

            if pending_invoke.is_some() && !m.starts_with("move-result") {
                flush_pending_invoke(&mut out, &mut pending_invoke);
            }

            if m.starts_with("invoke-") {
                if m.starts_with("invoke-custom") {
                    // Lambda / indy: prefer `(captures) -> { body }` when impl is available.
                    let lambda_expr = self.format_lambda_expression(ops);
                    pending_invoke = Some(PendingInvoke {
                        call_expr: IrExpr::Raw(lambda_expr),
                        comment,
                    });
                    continue;
                }
                let is_super = m.starts_with("invoke-super");
                let is_instance = m.starts_with("invoke-virtual")
                    || m.starts_with("invoke-interface")
                    || m.starts_with("invoke-direct")
                    || is_super;
                if let Some((target, args, param_types)) = parse_invoke_call_parts(&ops_resolved) {
                    let (target, args) = if is_super {
                        to_super_style(&target, &args)
                    } else if is_instance {
                        to_receiver_style(&target, &args)
                    } else {
                        (target, args)
                    };
                    // Dalvik passes long/double as two registers; only the low half is a Java arg.
                    let args = drop_wide_high_half_args(&args, &param_types);
                    pending_invoke = Some(PendingInvoke {
                        call_expr: IrExpr::Call { target, args },
                        comment,
                    });
                } else {
                    out.push(IrStmt::Raw(format!("{}({});", m, ops_resolved)));
                }
                continue;
            }

            // filled-new-array produces a result consumed by move-result-object (like invoke).
            if m == "filled-new-array" || m == "filled-new-array/range" {
                if let Some(expr) = format_filled_new_array_expr(&ops_resolved) {
                    pending_invoke = Some(PendingInvoke {
                        call_expr: IrExpr::Raw(expr),
                        comment,
                    });
                } else {
                    out.push(IrStmt::Raw(format!("{}; /* {} */", ops_resolved, m)));
                }
                continue;
            }

            if m == "move-result" || m == "move-result-wide" || m == "move-result-object" {
                if let Some(pi) = pending_invoke.take() {
                    if let Some(reg) = parse_one_reg(&ops_resolved) {
                        out.push(IrStmt::Expr {
                            expr: pi.call_expr,
                            comment: Some(pi.comment),
                        });
                        out.push(IrStmt::Assign {
                            dst: ir::VarId::new(reg, 0),
                            rhs: IrExpr::PendingResult,
                            comment: Some(comment),
                        });
                        continue;
                    }
                }
                let line = self.instruction_to_java(
                    ins,
                    base_off,
                    code_insns,
                    Some((idx, instructions, full_instructions)),
                )?;
                out.push(IrStmt::Raw(line));
                continue;
            }

            if m == "return-void" {
                flush_pending_invoke(&mut out, &mut pending_invoke);
                out.push(IrStmt::Return {
                    value: None,
                    comment: Some(comment),
                });
                continue;
            }

            if m == "return" || m == "return-wide" || m == "return-object" {
                flush_pending_invoke(&mut out, &mut pending_invoke);
                if let Some(reg) = parse_one_reg(&ops_resolved) {
                    out.push(IrStmt::Return {
                        value: Some(IrExpr::Var(ir::VarId::new(reg, 0))),
                        comment: Some(comment),
                    });
                } else {
                    let line = self.instruction_to_java(
                        ins,
                        base_off,
                        code_insns,
                        Some((idx, instructions, full_instructions)),
                    )?;
                    out.push(IrStmt::Raw(line));
                }
                continue;
            }

            // fill-array-data: Assign so SSA / naming stay consistent with new-array
            if m == "fill-array-data" {
                flush_pending_invoke(&mut out, &mut pending_invoke);
                if let Some((dst_reg, init)) =
                    parse_fill_array_data_assign(ins, code_insns, &ops_resolved)
                {
                    out.push(IrStmt::Assign {
                        dst: ir::VarId::new(dst_reg, 0),
                        rhs: IrExpr::Raw(init),
                        comment: Some(comment),
                    });
                } else {
                    let line = self.instruction_to_java(
                        ins,
                        base_off,
                        code_insns,
                        Some((idx, instructions, full_instructions)),
                    )?;
                    out.push(IrStmt::Raw(line));
                }
                continue;
            }

            // Emit Assign IR for const and binary ops so type inference and naming apply.
            if let Some((dst_reg, rhs_str)) = parse_assign_rhs(m, &ops_resolved) {
                flush_pending_invoke(&mut out, &mut pending_invoke);
                let rhs_str = if m.starts_with("const-wide") {
                    self.format_const_wide_rhs(m, dst_reg, &rhs_str, instructions, idx)
                } else if matches!(m, "const" | "const/high16" | "const/16" | "const/4") {
                    self.format_const_32_rhs(dst_reg, &rhs_str, instructions, idx)
                } else {
                    rhs_str
                };
                out.push(IrStmt::Assign {
                    dst: ir::VarId::new(dst_reg, 0),
                    rhs: IrExpr::Raw(rhs_str),
                    comment: Some(comment),
                });
                continue;
            }

            flush_pending_invoke(&mut out, &mut pending_invoke);
            let line = self.instruction_to_java(
                ins,
                base_off,
                code_insns,
                Some((idx, instructions, full_instructions)),
            )?;
            out.push(IrStmt::Raw(line));
        }

        flush_pending_invoke(&mut out, &mut pending_invoke);
        Ok(out)
    }

    /// Default pass pipeline (jadx-style). Runs after instructions_to_ir, before codegen.
    fn default_pass_runner(&self) -> PassRunner {
        let mut runner = PassRunner::new();
        runner.add(InvokeChainPass);
        runner.add(SsaRenamePass);
        runner.add(CopyPropPass);
        runner.add(ConstructorMergePass);
        runner.add(ExprSimplifyPass);
        runner.add(InlineFilledArrayPass);
        runner.add(DeadAssignPass);
        runner
    }

    /// Map one Dalvik instruction to a Java-like statement (or comment).
    /// When bytecode_ctx is Some((idx, instructions, full_instructions)), comment includes [id] and raw hex (for --show-bytecode).
    fn instruction_to_java(
        &self,
        ins: &Instruction,
        base_off: usize,
        code_insns: &[u8],
        bytecode_ctx: Option<(usize, &[Instruction], Option<&[Instruction]>)>,
    ) -> Result<String> {
        let m = ins.mnemonic();
        if m.ends_with("-payload") {
            return Ok(String::new());
        }
        let ops = ins.operands();
        let ops_resolved = self.resolve_operands(ops);
        let offset = ins.offset as usize + base_off;
        let comment = match bytecode_ctx {
            Some((idx, instructions, full_instructions)) => {
                let raw_hex =
                    Self::instruction_raw_hex(instructions, idx, code_insns, full_instructions);
                let ops_display = if m == "goto" {
                    format_goto_operands_signed(&raw_hex, ops)
                } else {
                    ops.to_string()
                };
                format!(
                    "// [{}] {:04x} ({}): {} {}",
                    idx, offset, raw_hex, m, ops_display
                )
            }
            None => format!("// {:04x}: {} {}", offset, m, ops),
        };
        let stmt = match m {
            "nop" => String::new(),
            "return-void" => "return;".into(),
            "return" => parse_one_reg(&ops_resolved)
                .map(|r| format!("return v{};", r))
                .unwrap_or_default(),
            "return-wide" => parse_one_reg(&ops_resolved)
                .map(|r| format!("return v{};", r))
                .unwrap_or_default(),
            "return-object" => parse_one_reg(&ops_resolved)
                .map(|r| format!("return v{};", r))
                .unwrap_or_default(),
            "move" | "move/from16" | "move/16" => parse_two_regs(&ops_resolved)
                .map(|(d, s)| format!("v{} = v{};", d, s))
                .unwrap_or_default(),
            "move-object" => parse_two_regs(&ops_resolved)
                .map(|(d, s)| format!("v{} = v{};", d, s))
                .unwrap_or_default(),
            "move-result" | "move-result-wide" | "move-result-object" => {
                parse_one_reg(&ops_resolved)
                    .map(|r| format!("v{} = <result>;", r))
                    .unwrap_or_default()
            }
            "const/4" | "const/16" | "const" => {
                parse_const_into_reg(&ops_resolved).unwrap_or_default()
            }
            "const-string" | "const-string/jumbo" => {
                parse_string_ref(&ops_resolved).unwrap_or_default()
            }
            "invoke-virtual" | "invoke-super" | "invoke-direct" | "invoke-static"
            | "invoke-interface" => {
                format!("{}( {} );", m, ops_resolved)
            }
            "invoke-virtual/range"
            | "invoke-super/range"
            | "invoke-direct/range"
            | "invoke-static/range"
            | "invoke-interface/range" => {
                format!("{}( {} );", m, ops_resolved)
            }
            "if-eq" | "if-ne" | "if-lt" | "if-ge" | "if-gt" | "if-le" | "if-eqz" | "if-nez"
            | "if-ltz" | "if-gez" | "if-gtz" | "if-lez" => {
                // No real label is emitted; show only bytecode comment to avoid "goto label" with no label.
                String::new()
            }
            "goto" | "goto/16" | "goto/32" => String::new(),
            "packed-switch" | "sparse-switch" => format_switch(ins, code_insns, &ops_resolved),
            "new-instance" => parse_new_instance(&ops_resolved).unwrap_or_default(),
            "new-array" => format_new_array(&ops_resolved).unwrap_or_default(),
            "filled-new-array" | "filled-new-array/range" => {
                format_filled_new_array_expr(&ops_resolved)
                    .map(|e| format!("{};", e))
                    .unwrap_or_else(|| format!("{}; /* {} */", ops_resolved, m))
            }
            "iget" | "iget-wide" | "iget-object" | "iget-boolean" => format_iget(&ops_resolved),
            "iput" | "iput-wide" | "iput-object" | "iput-boolean" => format_iput(&ops_resolved),
            "sget" | "sget-wide" | "sget-object" => format_sget(&ops_resolved),
            "sput" | "sput-wide" | "sput-object" => format_sput(&ops_resolved),
            "throw" => parse_one_reg(&ops_resolved)
                .map(|r| format!("throw v{};", r))
                .unwrap_or_default(),
            // Binary int ops (2addr): vA, vB → vA = vA op vB
            "add-int/2addr" | "add-long/2addr" => format_binop_2addr(&ops_resolved, "+"),
            "sub-int/2addr" | "sub-long/2addr" => format_binop_2addr(&ops_resolved, "-"),
            "mul-int/2addr" | "mul-long/2addr" => format_binop_2addr(&ops_resolved, "*"),
            "div-int/2addr" | "div-long/2addr" => format_binop_2addr(&ops_resolved, "/"),
            "rem-int/2addr" | "rem-long/2addr" => format_binop_2addr(&ops_resolved, "%"),
            "and-int/2addr" | "and-long/2addr" => format_binop_2addr(&ops_resolved, "&"),
            "or-int/2addr" | "or-long/2addr" => format_binop_2addr(&ops_resolved, "|"),
            "xor-int/2addr" | "xor-long/2addr" => format_binop_2addr(&ops_resolved, "^"),
            "shl-int/2addr" | "shl-long/2addr" => format_binop_2addr(&ops_resolved, "<<"),
            "shr-int/2addr" | "shr-long/2addr" => format_binop_2addr(&ops_resolved, ">>"),
            "ushr-int/2addr" | "ushr-long/2addr" => format_binop_2addr(&ops_resolved, ">>>"),
            // lit8: vA, vB, #CC → vA = vB op lit (or rsub: vA = lit - vB)
            "add-int/lit8" => format_lit8(&ops_resolved, "+"),
            "rsub-int/lit8" => format_lit8_rsub(&ops_resolved),
            "mul-int/lit8" => format_lit8(&ops_resolved, "*"),
            "div-int/lit8" => format_lit8(&ops_resolved, "/"),
            "rem-int/lit8" => format_lit8(&ops_resolved, "%"),
            "and-int/lit8" => format_lit8(&ops_resolved, "&"),
            "or-int/lit8" => format_lit8(&ops_resolved, "|"),
            "xor-int/lit8" => format_lit8(&ops_resolved, "^"),
            "shl-int/lit8" => format_lit8(&ops_resolved, "<<"),
            "shr-int/lit8" => format_lit8(&ops_resolved, ">>"),
            "ushr-int/lit8" => format_lit8(&ops_resolved, ">>>"),
            // lit16: same as lit8 (vA, vB, literal)
            "add-int/lit16" => format_lit8(&ops_resolved, "+"),
            "rsub-int" => format_lit8_rsub(&ops_resolved),
            "mul-int/lit16" => format_lit8(&ops_resolved, "*"),
            "div-int/lit16" => format_lit8(&ops_resolved, "/"),
            "rem-int/lit16" => format_lit8(&ops_resolved, "%"),
            "and-int/lit16" => format_lit8(&ops_resolved, "&"),
            "or-int/lit16" => format_lit8(&ops_resolved, "|"),
            "xor-int/lit16" => format_lit8(&ops_resolved, "^"),
            // Three-register binary (F23x): vA, vB, vC → vA = vB op vC
            "add-int" | "add-long" => format_binop_23x(&ops_resolved, "+"),
            "sub-int" | "sub-long" => format_binop_23x(&ops_resolved, "-"),
            "mul-int" | "mul-long" => format_binop_23x(&ops_resolved, "*"),
            "div-int" | "div-long" => format_binop_23x(&ops_resolved, "/"),
            "rem-int" | "rem-long" => format_binop_23x(&ops_resolved, "%"),
            "and-int" | "and-long" => format_binop_23x(&ops_resolved, "&"),
            "or-int" | "or-long" => format_binop_23x(&ops_resolved, "|"),
            "xor-int" | "xor-long" => format_binop_23x(&ops_resolved, "^"),
            "shl-int" | "shl-long" => format_binop_23x(&ops_resolved, "<<"),
            "shr-int" | "shr-long" => format_binop_23x(&ops_resolved, ">>"),
            "ushr-int" | "ushr-long" => format_binop_23x(&ops_resolved, ">>>"),
            "add-float" | "add-double" => format_binop_23x(&ops_resolved, "+"),
            "sub-float" | "sub-double" => format_binop_23x(&ops_resolved, "-"),
            "mul-float" | "mul-double" => format_binop_23x(&ops_resolved, "*"),
            "div-float" | "div-double" => format_binop_23x(&ops_resolved, "/"),
            "rem-float" | "rem-double" => format_binop_23x(&ops_resolved, "%"),
            // Float/double 2addr: same as int 2addr
            "add-float/2addr" | "add-double/2addr" => format_binop_2addr(&ops_resolved, "+"),
            "sub-float/2addr" | "sub-double/2addr" => format_binop_2addr(&ops_resolved, "-"),
            "mul-float/2addr" | "mul-double/2addr" => format_binop_2addr(&ops_resolved, "*"),
            "div-float/2addr" | "div-double/2addr" => format_binop_2addr(&ops_resolved, "/"),
            "rem-float/2addr" | "rem-double/2addr" => format_binop_2addr(&ops_resolved, "%"),
            // Unary (F12x): vA, vB → vA = op vB
            "neg-int" | "neg-long" | "neg-float" | "neg-double" => format_unary(&ops_resolved, "-"),
            "not-int" | "not-long" => format_unary(&ops_resolved, "~"),
            // Casts (F12x): vA, vB → vA = (type) vB
            "int-to-long" => format_cast(&ops_resolved, "long"),
            "int-to-float" => format_cast(&ops_resolved, "float"),
            "int-to-double" => format_cast(&ops_resolved, "double"),
            "long-to-int" => format_cast(&ops_resolved, "int"),
            "long-to-float" => format_cast(&ops_resolved, "float"),
            "long-to-double" => format_cast(&ops_resolved, "double"),
            "float-to-int" => format_cast(&ops_resolved, "int"),
            "float-to-long" => format_cast(&ops_resolved, "long"),
            "float-to-double" => format_cast(&ops_resolved, "double"),
            "double-to-int" => format_cast(&ops_resolved, "int"),
            "double-to-long" => format_cast(&ops_resolved, "long"),
            "double-to-float" => format_cast(&ops_resolved, "float"),
            "int-to-byte" => format_cast(&ops_resolved, "byte"),
            "int-to-char" => format_cast(&ops_resolved, "char"),
            "int-to-short" => format_cast(&ops_resolved, "short"),
            // Array
            "array-length" => format_array_length(&ops_resolved),
            "aget" | "aget-wide" | "aget-object" | "aget-boolean" | "aget-byte" | "aget-char"
            | "aget-short" => format_aget(&ops_resolved),
            "aput" | "aput-wide" | "aput-object" | "aput-boolean" | "aput-byte" | "aput-char"
            | "aput-short" => format_aput(&ops_resolved),
            // Comparison (F23x): vA, vB, vC → vA = (vB op vC) ? 1 : 0; we emit Java-like comparison
            "cmpl-float" | "cmpg-float" | "cmpl-double" | "cmpg-double" | "cmp-long" => {
                format_cmp(&ops_resolved, m)
            }
            // check-cast vA, type → "vA = (Type) vA;"
            "check-cast" => format_check_cast(&ops_resolved),
            // instance-of vA, vB, type → "vA = vB instanceof Type;"
            "instance-of" => format_instance_of(&ops_resolved),
            // const-class vA, type → "vA = Type.class;"
            "const-class" => format_const_class(&ops_resolved),
            // monitor-enter/exit
            "monitor-enter" => parse_one_reg(&ops_resolved)
                .map(|r| format!("/* monitor-enter(v{}) */", r))
                .unwrap_or_default(),
            "monitor-exit" => parse_one_reg(&ops_resolved)
                .map(|r| format!("/* monitor-exit(v{}) */", r))
                .unwrap_or_default(),
            // fill-array-data: parse payload and emit arr = new int[]{ ... };
            "fill-array-data" => format_fill_array_data(ins, code_insns, &ops_resolved),
            _ => format!("{}; /* {} */", ops_resolved, m),
        };
        if stmt.is_empty() {
            if self.show_bytecode {
                Ok(comment)
            } else {
                Ok(String::new())
            }
        } else if self.show_bytecode {
            Ok(format!("{}  // {}", stmt, comment))
        } else {
            Ok(stmt)
        }
    }

    /// Resolve string@N, type@N, field@N, method@N in operands using DEX indices.
    fn resolve_operands(&self, operands: &str) -> String {
        let parts: Vec<&str> = operands.split(',').map(str::trim).collect();
        parts
            .iter()
            .map(|part| resolve_one(self.dex, part))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Build an `Emulator` for the given method, ready to step through.
    /// If `initial_heap` is non-empty, the emulator starts with these heap objects and `params`
    /// may contain `Value::Ref(i)` for indices into that heap (e.g. for array arguments).
    pub fn build_emulator(
        &self,
        encoded: &EncodedMethod,
        params: Vec<crate::emulator::Value>,
        initial_heap: Vec<crate::emulator::HeapObject>,
    ) -> Result<crate::emulator::Emulator> {
        use crate::emulator::state::{Emulator as Emu, InstructionInfo};

        let code = self
            .dex
            .get_code_item(encoded.code_off)
            .map_err(|e| DexDecompilerError::Decompilation(format!("code_item: {}", e)))?;
        let insns_bytes = code.insns_slice(&*self.dex.data);
        let instructions = decode_all(insns_bytes, 0)
            .map_err(|e| DexDecompilerError::Disassembly(e.to_string()))?;

        let base_off = code.insns_off;
        let ins_info: Vec<InstructionInfo> = instructions
            .iter()
            .enumerate()
            .map(|(i, ins)| InstructionInfo {
                index: i,
                offset: (ins.offset as usize + base_off) as u32,
                mnemonic: ins.mnemonic().to_string(),
                operands: ins.operands().to_string(),
            })
            .collect();

        let resolved: Vec<String> = instructions
            .iter()
            .map(|ins| self.resolve_operands(ins.operands()))
            .collect();

        let registers_size = code.registers_size as u32;
        let ins_size = code.ins_size as u32;
        let is_static = (encoded.access_flags & 0x8) != 0;

        Ok(Emu::new_with_heap(
            ins_info,
            resolved,
            registers_size,
            ins_size,
            is_static,
            params,
            initial_heap,
        ))
    }
}

/// Replace synthetic capture references in inlined run() body: "receiver.val$name" and "val$name" → arg.
fn replace_capture_in_body(body: &str, field_name: &str, arg: &str) -> String {
    let dot_field = format!(".{}", field_name);
    let mut result = body.to_string();
    while let Some(pos) = result.find(&dot_field) {
        let start = result[..pos]
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        result = format!(
            "{}{}{}",
            &result[..start],
            arg,
            &result[pos + dot_field.len()..]
        );
    }
    result = result.replace(field_name, arg);
    result
}

/// After capture replacement: find "var = arg;" lines and replace whole-word "var" with "arg" in body, then remove those assignment lines.
fn replace_capture_assignees_in_body(body: &str, val_replacements: &[(String, String)]) -> String {
    let args: std::collections::HashSet<&str> =
        val_replacements.iter().map(|(_, a)| a.as_str()).collect();
    let mut assignees: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let stmt = line.trim();
        let stmt_clean = if let Some(idx) = stmt.find("  // ") {
            stmt[..idx].trim_end()
        } else {
            stmt
        };
        if let Some(eq) = stmt_clean.find(" = ") {
            let var = stmt_clean[..eq].trim();
            let rhs = stmt_clean[eq + 3..].trim_end_matches(';').trim();
            if args.contains(rhs)
                && !var.is_empty()
                && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                assignees.push((var.to_string(), rhs.to_string()));
            }
        }
    }
    let mut result = body.to_string();
    for (var, arg) in &assignees {
        result = replace_whole_word(&result, var, arg);
    }
    result
        .lines()
        .filter(|line| {
            let stmt = line.trim();
            let stmt_clean = if let Some(idx) = stmt.find("  // ") {
                stmt[..idx].trim_end()
            } else {
                stmt
            };
            if let Some(eq) = stmt_clean.find(" = ") {
                let var = stmt_clean[..eq].trim();
                let rhs = stmt_clean[eq + 3..].trim_end_matches(';').trim();
                if args.contains(rhs) && assignees.iter().any(|(v, a)| v == var && a == rhs) {
                    return false;
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace "synchronized (VAR)" with "synchronized (arg)" so the lock uses the outer capture (first arg = Object).
fn replace_synchronized_lock_with_arg(body: &str, arg: &str) -> String {
    let prefix = "synchronized (";
    let mut result = body.to_string();
    let mut search_from = 0;
    while let Some(rel_start) = result[search_from..].find(prefix) {
        let start = search_from + rel_start;
        let paren_start = start + prefix.len();
        if let Some(paren_end) = result[paren_start..].find(')') {
            let end = paren_start + paren_end;
            result = format!("{}{}{}", &result[..paren_start], arg, &result[end..]);
            search_from = paren_start + arg.len() + 1;
        } else {
            break;
        }
    }
    result
}

/// Remove unreachable exception-handler lines after "return;" (e.g. "var; /* move-exception */", "throw var;", "/* monitor-exit */").
fn strip_unreachable_exception_junk_after_return(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push_str(line);
        if i < lines.len().saturating_sub(1) {
            out.push('\n');
        }
        let stmt = line.trim();
        let stmt_clean = if let Some(idx) = stmt.find("  // ") {
            stmt[..idx].trim_end()
        } else {
            stmt
        };
        if stmt_clean == "return;" {
            let return_indent = line.len() - line.trim_start().len();
            i += 1;
            while i < lines.len() {
                let next = lines[i];
                let next_indent = next.len() - next.trim_start().len();
                if next.trim().is_empty() {
                    i += 1;
                    continue;
                }
                if next_indent <= return_indent
                    && (next.trim().starts_with('}') || next.trim().starts_with("} catch"))
                {
                    break;
                }
                let t = next.trim();
                let drop = t.contains("/* move-exception */")
                    || t.contains("/* monitor-exit(")
                    || (t.starts_with("throw ") && t.ends_with(';'));
                if drop {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        i += 1;
    }
    out
}

/// Replace whole-word occurrences of `from` with `to` (so "v1" does not match inside "v10").
fn replace_whole_word(body: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return body.to_string();
    }
    let mut result = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i..].starts_with(from) {
            let start = i;
            let end = i + from.len();
            let prev_ok = start == 0
                || !body[..start]
                    .chars()
                    .rev()
                    .next()
                    .map_or(false, |c| c.is_ascii_alphanumeric() || c == '_');
            let next_ok = end >= body.len()
                || !body[end..]
                    .chars()
                    .next()
                    .map_or(false, |c| c.is_ascii_alphanumeric() || c == '_');
            if prev_ok && next_ok {
                result.push_str(to);
                i = end;
                continue;
            }
        }
        let ch_len = body[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        result.push_str(&body[i..i + ch_len]);
        i += ch_len;
    }
    result
}

/// For instance calls (virtual/interface/direct/super), transform `Class.method` + `receiver, args`
/// into `receiver.method` + `args`. The class name is dropped in favor of the receiver variable.
fn to_receiver_style(target: &str, args: &str) -> (String, String) {
    let method_name = target.rsplit('.').next().unwrap_or(target);
    let args = args.trim();
    if args.is_empty() {
        return (target.to_string(), args.to_string());
    }
    // Split off the first argument (receiver) — watch for nested parens
    if let Some(comma_pos) = find_first_comma(args) {
        let receiver = args[..comma_pos].trim();
        let rest = args[comma_pos + 1..].trim();
        (format!("{}.{}", receiver, method_name), rest.to_string())
    } else {
        // Single arg = receiver, no remaining args
        (format!("{}.{}", args, method_name), String::new())
    }
}

/// `invoke-super` → `super.method(args)` (drop the `this` receiver).
fn to_super_style(target: &str, args: &str) -> (String, String) {
    let method_name = target.rsplit('.').next().unwrap_or(target);
    let args = args.trim();
    if args.is_empty() {
        return (format!("super.{}", method_name), String::new());
    }
    if let Some(comma_pos) = find_first_comma(args) {
        let rest = args[comma_pos + 1..].trim();
        (format!("super.{}", method_name), rest.to_string())
    } else {
        // Only the receiver — no remaining args
        (format!("super.{}", method_name), String::new())
    }
}

fn find_first_comma(s: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (i, c) in s.chars().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Parse resolved invoke operands string into Java call parts.
/// Input format: \"v0, v1, pkg.Clz.m(java.lang.String, int)\" (method ref last, may contain commas),
/// or just \"pkg.Clz.m()\" for static/direct calls with no register arguments.
/// Output: (\"pkg.Clz.m\", \"v0, v1\", param_java_types) or (\"pkg.Clz.m\", \"\", []) for no-arg.
fn parse_invoke_call_parts(ops_resolved: &str) -> Option<(String, String, Vec<String>)> {
    let inner = ops_resolved.trim();
    if inner.is_empty() {
        return None;
    }
    // Find last comma at depth 0 to split args from method ref.
    let mut depth = 0u32;
    let mut last_comma_at = None;
    for (i, c) in inner.chars().enumerate() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => last_comma_at = Some(i),
            _ => {}
        }
    }
    let (args, method_ref) = if let Some(split_at) = last_comma_at {
        (inner[..split_at].trim(), inner[split_at + 1..].trim())
    } else {
        // No register args — entire operand is the method reference (e.g. Runtime.getRuntime()).
        ("", inner)
    };
    // `invoke-*/range` operands look like `v21 ... v22` (no commas). Expand before
    // receiver peeling so we never emit `System.out ... r.println()`.
    let args = expand_invoke_arg_regs(args);
    if method_ref.is_empty() {
        return None;
    }
    let param_types = parse_java_param_list(method_ref);
    let method_name = method_ref.split('(').next().unwrap_or(method_ref).trim();
    if method_name.is_empty() {
        return None;
    }
    Some((method_name.to_string(), args, param_types))
}

/// Expand `vN ... vM` / `vN .. vM` tokens in an invoke argument list to `vN, …, vM`.
fn expand_invoke_arg_regs(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return String::new();
    }
    if let Some(regs) = expand_reg_range_ellipsis(args) {
        return regs.join(", ");
    }
    let mut out: Vec<String> = Vec::new();
    for part in args.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(regs) = expand_reg_range_ellipsis(part) {
            out.extend(regs);
        } else {
            out.push(part.to_string());
        }
    }
    out.join(", ")
}

/// Param types from a resolved method ref: `pkg.Clz.m(long, int)` → `["long", "int"]`.
fn parse_java_param_list(method_ref: &str) -> Vec<String> {
    let Some(start) = method_ref.find('(') else {
        return Vec::new();
    };
    let Some(end) = method_ref.rfind(')') else {
        return Vec::new();
    };
    if end <= start + 1 {
        return Vec::new();
    }
    let inner = method_ref[start + 1..end].trim();
    if inner.is_empty() {
        return Vec::new();
    }
    split_top_level_args(inner)
}

fn is_wide_java_param(ty: &str) -> bool {
    matches!(ty.trim(), "long" | "double" | "J" | "D")
}

fn looks_like_dalvik_reg(s: &str) -> bool {
    let s = s.trim();
    let rest = if let Some(r) = s.strip_prefix('v') {
        r
    } else if let Some(r) = s.strip_prefix('p') {
        r
    } else {
        return false;
    };
    // v0 / p1 / v3_0 (SSA)
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '_')
}

fn split_top_level_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0u32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '<' => depth = depth.saturating_add(1),
            ')' | ']' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Drop the high half of long/double invoke args (Dalvik register pairs).
/// `args` must already be Java-facing (receiver peeled for instance calls).
fn drop_wide_high_half_args(args: &str, param_types: &[String]) -> String {
    let args = args.trim();
    if args.is_empty() || param_types.is_empty() {
        return args.to_string();
    }
    let parts = split_top_level_args(args);
    if parts.is_empty() {
        return args.to_string();
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    for ty in param_types {
        if i >= parts.len() {
            break;
        }
        out.push(parts[i].clone());
        i += 1;
        if is_wide_java_param(ty) && i < parts.len() && looks_like_dalvik_reg(&parts[i]) {
            i += 1;
        }
    }
    while i < parts.len() {
        out.push(parts[i].clone());
        i += 1;
    }
    out.join(", ")
}

/// Format compound assign marker into proper Java (e.g. `i++` or `n0 += 3`).
fn format_compound_line(
    compound: &str,
    var: &str,
    name_map: Option<&HashMap<VarId, String>>,
) -> String {
    let compound = if let Some(nm) = name_map {
        ir::substitute_names_in_text_pub(compound, nm)
    } else {
        compound.to_string()
    };
    // __compound_vN++ or __compound_vN--
    if compound.ends_with("++") || compound.ends_with("--") {
        let op = &compound[compound.len() - 2..];
        return format!("{}{};", var, op);
    }
    // __compound_vN += expr
    if let Some(rest) = compound.strip_prefix(&format!("{} ", var)) {
        return format!("{} {};", var, rest.trim());
    }
    // fallback: try to match by stripping vN part
    if let Some(idx) = compound.find(" ") {
        let rest = &compound[idx..];
        return format!("{}{};", var, rest.trim_end().trim_end_matches(';'));
    }
    format!("{};", compound)
}

/// Handle compound assign markers in raw/untyped lines.
fn resolve_compound_in_line(line: &str, name_map: Option<&HashMap<VarId, String>>) -> String {
    if let Some(eq_pos) = line.find(" = __compound_") {
        let var = line[..eq_pos].trim();
        let after = line[eq_pos + " = __compound_".len()..]
            .trim_end_matches(';')
            .trim();
        let after = if let Some(nm) = name_map {
            ir::substitute_names_in_text_pub(after, nm)
        } else {
            after.to_string()
        };
        if after.ends_with("++") || after.ends_with("--") {
            let op = &after[after.len() - 2..];
            return format!("{}{};", var, op);
        }
        if let Some(rest) = after.strip_prefix(&format!("{} ", var)) {
            return format!("{} {};", var, rest);
        }
        if let Some(idx) = after.find(' ') {
            return format!("{}{};", var, &after[idx..]);
        }
    }
    line.to_string()
}

/// Shorten common Java types: `java.lang.String` → `String`, `java.lang.Object` → `Object`, etc.
fn shorten_type(ty: &str) -> String {
    shorten_java_names(ty)
}

/// Split a fully qualified class name into (package, simple_class_name).
/// e.g. "androguard.test.TestIfs" → ("androguard.test", "TestIfs"); "TestIfs" → ("", "TestIfs").
fn split_package_and_class(fully_qualified: &str) -> (String, String) {
    match fully_qualified.rfind('.') {
        Some(dot) => (
            fully_qualified[..dot].to_string(),
            fully_qualified[dot + 1..].to_string(),
        ),
        None => (String::new(), fully_qualified.to_string()),
    }
}

/// Annotation type to short name for @Override, @Nullable, etc. e.g. "android.annotation.Override" → "Override".
#[allow(dead_code)]
fn annotation_short_name(java_type: &str) -> &str {
    java_type.rsplit('.').next().unwrap_or(java_type)
}

/// Primitives and void: no import needed.
const PRIMITIVE_OR_VOID: &[&str] = &[
    "void", "boolean", "byte", "short", "char", "int", "long", "float", "double",
];

const ACC_STATIC: u32 = 0x8;
const ACC_FINAL: u32 = 0x10;

/// Pure logic: given super type and static field (type, name, flags), return enum constant names if this is an enum.
/// Used by detect_enum_constants and by tests.
fn enum_constants_from_static_fields(
    class_name: &str,
    super_type: &str,
    static_fields: &[(String, String, u32)],
) -> Vec<String> {
    if super_type.trim() != "Enum" {
        return vec![];
    }
    let mut constants = Vec::new();
    for (field_typ, field_name, access_flags) in static_fields {
        if (access_flags & (ACC_STATIC | ACC_FINAL)) != (ACC_STATIC | ACC_FINAL) {
            continue;
        }
        if field_typ == class_name {
            constants.push(field_name.clone());
        }
    }
    constants
}

/// Detect enum pattern: class extends Enum and has static final fields of its own type (enum constants).
/// Returns the list of constant names in declaration order; empty if not an enum.
fn detect_enum_constants(
    dex: &DexFile,
    _class_def: &ClassDef,
    class_data: Option<&dex_parser::ClassData>,
    class_name: &str,
    super_type: &str,
) -> Vec<String> {
    let Some(ref cd) = class_data else {
        return vec![];
    };
    let mut static_fields: Vec<(String, String, u32)> = Vec::new();
    for f in &cd.static_fields {
        let Ok(fi) = dex.get_field_info(f.field_idx) else {
            continue;
        };
        let field_typ = java::descriptor_to_java(&fi.typ);
        static_fields.push((field_typ, fi.name.to_string(), f.access_flags));
    }
    enum_constants_from_static_fields(class_name, super_type, &static_fields)
}

struct CtorFieldInit {
    field: String,
    expr: String,
    /// Line indexes in the method source to drop when this init is hoisted.
    lines: Vec<usize>,
}

/// Lift field initializers that javac copied into every `super()` constructor
/// back onto the field (`public int[] tab = new int[]{ ... }`).
fn hoist_common_ctor_field_inits(
    methods: &mut [String],
    simple_class: &str,
    instance_fields: &HashSet<String>,
) -> HashMap<String, String> {
    struct Ctor {
        method_idx: usize,
        steps: Vec<CtorFieldInit>,
    }
    let mut ctors = Vec::new();
    for (method_idx, java) in methods.iter().enumerate() {
        if !is_emitted_constructor(java, simple_class) || constructor_delegates_to_this(java) {
            continue;
        }
        ctors.push(Ctor {
            method_idx,
            steps: leading_field_inits(java, instance_fields),
        });
    }
    if ctors.len() < 2 {
        return HashMap::new();
    }
    let mut n = ctors.iter().map(|c| c.steps.len()).min().unwrap_or(0);
    for i in 0..n {
        let field = &ctors[0].steps[i].field;
        let expr = &ctors[0].steps[i].expr;
        if ctors
            .iter()
            .any(|c| c.steps[i].field != *field || c.steps[i].expr != *expr)
        {
            n = i;
            break;
        }
    }
    if n == 0 {
        return HashMap::new();
    }
    let mut inits = HashMap::new();
    for step in &ctors[0].steps[..n] {
        inits.insert(step.field.clone(), step.expr.clone());
    }
    for ctor in &ctors {
        let mut drop_lines: Vec<usize> = ctor.steps[..n]
            .iter()
            .flat_map(|s| s.lines.iter().copied())
            .collect();
        drop_lines.sort_unstable();
        drop_lines.dedup();
        let lines: Vec<&str> = methods[ctor.method_idx].lines().collect();
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate() {
            if drop_lines.binary_search(&i).is_ok() {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        methods[ctor.method_idx] = cleanup_ctor_after_field_hoist(&out);
    }
    inits
}

/// After hoisting `int i = 10; this.test = i` → field init, fold remaining
/// `i = 100; this.value = i` into `this.value = 100` and drop a dead leading
/// `int i = …` that is immediately overwritten.
fn cleanup_ctor_after_field_hoist(java: &str) -> String {
    let mut lines: Vec<String> = java.lines().map(|l| l.to_string()).collect();
    let mut i = 0usize;
    while i + 1 < lines.len() {
        let a = lines[i].trim().to_string();
        let b = lines[i + 1].trim().to_string();
        // `i = 100;` + `this.value = i;` → `this.value = 100;`
        if let Some((name, lit)) = parse_simple_ident_assign(&a) {
            if looks_like_hoistable_ctor_literal(&lit) {
                if let Some((field, rhs)) = parse_this_field_assign(&b) {
                    if rhs == name {
                        let indent = lines[i + 1]
                            .chars()
                            .take_while(|c| c.is_whitespace())
                            .collect::<String>();
                        lines[i + 1] = format!("{indent}this.{field} = {lit};");
                        lines.remove(i);
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    // Drop `int i = 10;` when the next non-empty stmt assigns `i = …`.
    let mut i = 0usize;
    while i + 1 < lines.len() {
        if let Some((_ty, name, _expr)) = parse_typed_assign(lines[i].trim()) {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() {
                if let Some((n2, _)) = parse_simple_ident_assign(lines[j].trim()) {
                    if n2 == name {
                        lines.remove(i);
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    let mut out = lines.join("\n");
    if java.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn parse_simple_ident_assign(line: &str) -> Option<(String, String)> {
    let line = line.strip_suffix(';')?.trim();
    let (lhs, rhs) = line.split_once('=')?;
    let lhs = lhs.trim();
    let rhs = rhs.trim();
    if !is_java_ident(lhs) || rhs.is_empty() || lhs.contains(' ') {
        return None;
    }
    Some((lhs.to_string(), rhs.to_string()))
}

fn looks_like_hoistable_ctor_literal(expr: &str) -> bool {
    let e = expr.trim();
    if e.parse::<i64>().is_ok() {
        return true;
    }
    if e.starts_with("(int) ") {
        let rest = e.strip_prefix("(int) ").unwrap().trim();
        return rest.parse::<i64>().is_ok() || is_java_ident(rest);
    }
    false
}

fn is_emitted_constructor(java: &str, simple: &str) -> bool {
    let Some(first) = java.lines().next() else {
        return false;
    };
    let mut rest = first.trim();
    loop {
        let mut progressed = false;
        for kw in [
            "public",
            "protected",
            "private",
            "final",
            "synthetic",
            "varargs",
            "strictfp",
        ] {
            if let Some(r) = rest.strip_prefix(kw) {
                if r.starts_with(' ') || r.starts_with('\t') {
                    rest = r.trim_start();
                    progressed = true;
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    if rest.starts_with("static ") {
        return false;
    }
    rest.starts_with(&format!("{simple}("))
}

fn constructor_delegates_to_this(java: &str) -> bool {
    java.lines().any(|l| {
        let t = l.trim();
        t.starts_with("this(") && t.ends_with(';')
    })
}

fn leading_field_inits(java: &str, fields: &HashSet<String>) -> Vec<CtorFieldInit> {
    let lines: Vec<&str> = java.lines().collect();
    let mut i = 1usize;
    let mut steps = Vec::new();
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() || (t.starts_with("super(") && t.ends_with(");")) {
            i += 1;
            continue;
        }
        break;
    }
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() || t == "}" {
            break;
        }
        if let Some((_ty, name, expr)) = parse_typed_assign(t) {
            if i + 1 < lines.len() {
                if let Some((field, rhs)) = parse_this_field_assign(lines[i + 1].trim()) {
                    if rhs == name
                        && fields.contains(&field)
                        && !expr_contains_ident(&expr, &name)
                    {
                        // If the temp is only used for this field copy, drop both lines.
                        // If it is reused later (`i = 100; this.value = i`), keep the
                        // declaration and only drop the field assign.
                        let drop = if ident_uses(java, &name) == 2 {
                            vec![i, i + 1]
                        } else {
                            vec![i + 1]
                        };
                        steps.push(CtorFieldInit {
                            field,
                            expr,
                            lines: drop,
                        });
                        i += 2;
                        continue;
                    }
                }
            }
            break;
        }
        if let Some((field, expr)) = parse_this_field_assign(t) {
            if fields.contains(&field) {
                steps.push(CtorFieldInit {
                    field,
                    expr,
                    lines: vec![i],
                });
                i += 1;
                continue;
            }
        }
        break;
    }
    steps
}

fn parse_typed_assign(line: &str) -> Option<(String, String, String)> {
    let line = line.strip_suffix(';')?.trim();
    let (lhs, expr) = line.split_once('=')?;
    let lhs = lhs.trim();
    let expr = expr.trim();
    if lhs.is_empty() || expr.is_empty() || lhs.starts_with("this.") || lhs.contains('(') {
        return None;
    }
    let (ty, name) = lhs.rsplit_once(|c: char| c.is_whitespace())?;
    let ty = ty.trim();
    let name = name.trim();
    if ty.is_empty() || !is_java_ident(name) {
        return None;
    }
    Some((ty.to_string(), name.to_string(), expr.to_string()))
}

fn parse_this_field_assign(line: &str) -> Option<(String, String)> {
    let line = line.strip_suffix(';')?.trim();
    let rest = line.strip_prefix("this.")?;
    let (field, expr) = rest.split_once('=')?;
    let field = field.trim();
    let expr = expr.trim();
    if !is_java_ident(field) || expr.is_empty() {
        return None;
    }
    Some((field.to_string(), expr.to_string()))
}

fn expr_contains_ident(expr: &str, name: &str) -> bool {
    ident_uses(expr, name) > 0
}

fn ident_uses(src: &str, name: &str) -> usize {
    let bytes = src.as_bytes();
    let n = name.as_bytes();
    if n.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut i = 0usize;
    while i + n.len() <= bytes.len() {
        if bytes[i..].starts_with(n) {
            let before = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after_i = i + n.len();
            let after = after_i >= bytes.len() || !is_ident_byte(bytes[after_i]);
            if before && after {
                count += 1;
                i = after_i;
                continue;
            }
        }
        i += 1;
    }
    count
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Prefixes `shorten_java_names` strips, except `java.lang` (imported implicitly).
const IMPORTABLE_SHORT_PREFIXES: &[&str] = &[
    "java.util.",
    "java.io.",
    "android.content.",
    "android.os.",
    "android.app.",
    "android.view.",
    "android.widget.",
];

/// Add `import` lines for types whose package was shortened in the body
/// (`PrintStream` ← `java.io.PrintStream`).
fn insert_used_short_imports(
    dex: &DexFile,
    source: &str,
    class_name: &str,
    package: &str,
) -> String {
    let used = uppercase_idents(source);
    if used.is_empty() {
        return source.to_string();
    }
    let mut by_simple: HashMap<String, Vec<String>> = HashMap::new();
    for idx in 0..dex.header.type_ids_size {
        let Ok(desc) = dex.get_type(idx) else {
            continue;
        };
        let java_ty = java::descriptor_to_java(&desc);
        let base = java_ty.trim_end_matches("[]");
        if base.contains('$') || base == class_name {
            continue;
        }
        let Some(prefix) = IMPORTABLE_SHORT_PREFIXES
            .iter()
            .find(|p| base.starts_with(*p))
        else {
            continue;
        };
        let simple = &base[prefix.len()..];
        if simple.is_empty() || simple.contains('.') {
            continue;
        }
        if !package.is_empty() && base.starts_with(&format!("{package}.")) {
            continue;
        }
        by_simple
            .entry(simple.to_string())
            .or_default()
            .push(base.to_string());
    }
    let mut extra = Vec::new();
    for (simple, mut fqns) in by_simple {
        if !used.contains(&simple) {
            continue;
        }
        fqns.sort();
        fqns.dedup();
        if fqns.len() == 1 {
            extra.push(fqns.remove(0));
        }
    }
    if extra.is_empty() {
        return source.to_string();
    }
    splice_imports(source, &extra)
}

fn uppercase_idents(src: &str) -> HashSet<String> {
    let bytes = src.as_bytes();
    let mut out = HashSet::new();
    let mut i = 0usize;
    let mut in_str = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if b.is_ascii_uppercase() {
            let start = i;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_alphanumeric() || c == b'_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let before_ok = start == 0 || {
                let p = bytes[start - 1];
                !p.is_ascii_alphanumeric() && p != b'_' && p != b'.'
            };
            if before_ok {
                out.insert(src[start..i].to_string());
            }
            continue;
        }
        i += 1;
    }
    out
}

fn splice_imports(source: &str, extra: &[String]) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let class_at = lines
        .iter()
        .position(|l| {
            let t = l.trim_start();
            !t.starts_with("//")
                && !t.starts_with("import ")
                && (t.contains(" class ")
                    || t.starts_with("class ")
                    || t.contains(" interface ")
                    || t.contains(" enum "))
        })
        .unwrap_or(0);
    let mut imports: HashSet<String> = HashSet::new();
    for line in &lines[..class_at] {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("import ") {
            if let Some(fqn) = rest.strip_suffix(';') {
                let fqn = fqn.trim();
                if !fqn.is_empty() {
                    imports.insert(fqn.to_string());
                }
            }
        }
    }
    for fqn in extra {
        imports.insert(fqn.clone());
    }
    let mut sorted: Vec<String> = imports.into_iter().collect();
    sorted.sort();

    let mut out = String::new();
    for line in &lines[..class_at] {
        let t = line.trim();
        if t.is_empty() || t.starts_with("import ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !sorted.is_empty() {
        for fqn in &sorted {
            out.push_str("import ");
            out.push_str(fqn);
            out.push_str(";\n");
        }
        out.push('\n');
    }
    for line in &lines[class_at..] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Collect fully-qualified types used in this class (super, fields, method signatures) for import statements.
/// Excludes java.lang.*, same-package types, primitives, and the current class.
fn collect_class_imports(
    dex: &DexFile,
    class_def: &ClassDef,
    class_data: Option<&dex_parser::ClassData>,
    class_name: &str,
    package: &str,
) -> Result<Vec<String>> {
    let mut fqns: std::collections::HashSet<String> = std::collections::HashSet::new();

    let add_type = |fqns: &mut std::collections::HashSet<String>, ty: &str| {
        let base = ty.trim_end_matches(']').trim_end_matches('[').trim();
        if base.is_empty() || PRIMITIVE_OR_VOID.contains(&base) {
            return;
        }
        if base.starts_with("java.lang.") {
            return;
        }
        if base == class_name {
            return;
        }
        if !package.is_empty() && (base == package || base.starts_with(&format!("{}.", package))) {
            return;
        }
        fqns.insert(base.to_string());
    };

    if class_def.superclass_idx != NO_INDEX {
        if let Ok(s) = dex.get_type(class_def.superclass_idx) {
            let ty = java::descriptor_to_java(&s);
            add_type(&mut fqns, &ty);
        }
    }

    if let Some(cd) = class_data {
        for f in cd.static_fields.iter().chain(cd.instance_fields.iter()) {
            if let Ok(fi) = dex.get_field_info(f.field_idx) {
                let ty = java::descriptor_to_java(&fi.typ);
                add_type(&mut fqns, &ty);
            }
        }
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            if let Ok(info) = dex.get_method_info(m.method_idx) {
                let ret = java::descriptor_to_java(&info.return_type);
                add_type(&mut fqns, &ret);
                for p in &info.params {
                    let ty = java::descriptor_to_java(p);
                    add_type(&mut fqns, &ty);
                }
            }
        }
    }

    let mut list: Vec<String> = fqns.into_iter().collect();
    list.sort();
    Ok(list)
}

/// Target of the `goto` at `try_end_byte` when it jumps strictly past `last_handler_start`.
///
/// Dalvik branch offsets are in 16-bit code units relative to the branch instruction.
/// Operand text is the assembler form (`+09h`, `-08h`).
fn try_exit_merge_byte(
    instructions: &[Instruction],
    try_end_byte: u32,
    last_handler_start: u32,
) -> Option<u32> {
    let ins = instructions.iter().find(|i| i.offset == try_end_byte)?;
    let m = ins.mnemonic();
    if m != "goto" && m != "goto/16" && m != "goto/32" {
        return None;
    }
    let rel = parse_rel_code_units(ins.operands())?;
    let target = (ins.offset as i32).checked_add(rel.checked_mul(2)?)?;
    if target <= last_handler_start as i32 {
        return None;
    }
    Some(target as u32)
}

/// True when instructions in `[handler_start, merge)` do not return, throw, or jump.
fn handler_falls_through(instructions: &[Instruction], handler_start: u32, merge: u32) -> bool {
    let Some(start) = instructions.iter().position(|i| i.offset >= handler_start) else {
        return false;
    };
    let mut saw = false;
    for ins in instructions.iter().skip(start) {
        if ins.offset >= merge {
            return saw;
        }
        saw = true;
        let m = ins.mnemonic();
        if m.starts_with("return") || m == "throw" || m.starts_with("goto") {
            return false;
        }
    }
    false
}

fn parse_rel_code_units(operands: &str) -> Option<i32> {
    let tok = operands.rsplit([',', ' ']).next()?.trim();
    let tok = tok.strip_suffix('h')?;
    if let Some(digits) = tok.strip_prefix('+') {
        i32::from_str_radix(digits, 16).ok()
    } else if let Some(digits) = tok.strip_prefix('-') {
        i32::from_str_radix(digits, 16).ok().map(|n| -n)
    } else {
        i32::from_str_radix(tok, 16).ok()
    }
}

/// Handler `goto` lands on an `if-*` (the loop condition). That is `continue`,
/// unlike a goto into the delayed `j++` move (`foo4`).
fn handler_goto_targets_loop_if(instructions: &[Instruction], addr: u32) -> bool {
    let Some(start) = instructions.iter().position(|ins| ins.offset == addr) else {
        return false;
    };
    for ins in instructions.iter().skip(start).take(8) {
        let m = ins.mnemonic();
        if m.starts_with("goto") {
            let Some(mut rel) = parse_rel_code_units(ins.operands()) else {
                return false;
            };
            // `goto` is an 8-bit offset. Some decoders print `+f0h` for -16.
            if m == "goto" && rel > 127 {
                rel -= 256;
            }
            let target = ins.offset as i32 + rel * 2;
            if target < 0 {
                return false;
            }
            return instructions.iter().any(|other| {
                other.offset == target as u32 && other.mnemonic().starts_with("if-")
            });
        }
        if m.starts_with("return") || m == "throw" {
            return false;
        }
    }
    false
}

fn handler_goto_continues(instructions: &[Instruction], addr: u32) -> bool {
    let Some(start) = instructions.iter().position(|ins| ins.offset == addr) else {
        return false;
    };
    for ins in instructions.iter().skip(start).take(8) {
        let m = ins.mnemonic();
        if m.starts_with("goto") {
            return true;
        }
        if m.starts_with("return") || m == "throw" {
            return false;
        }
    }
    false
}

fn handler_const_literal(instructions: &[Instruction], addr: u32) -> Option<String> {
    let start = instructions.iter().position(|ins| ins.offset == addr)?;
    for ins in instructions.iter().skip(start).take(8) {
        if matches!(ins.mnemonic(), "const/4" | "const/16" | "const") {
            let mut parts = ins.operands.split(',');
            let _reg = parts.next()?;
            let lit = parts.next()?.trim();
            if !lit.is_empty() {
                return Some(lit.to_string());
            }
        }
        if ins.mnemonic().starts_with("goto") || ins.mnemonic() == "throw" {
            break;
        }
    }
    None
}

/// `public int foo() { synchronized (this) { body } }` → `public synchronized int foo() { body }`.
fn promote_this_synchronized(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let Some(sig) = lines.iter().position(|l| {
        let t = l.trim_end();
        t.ends_with('{') && t.contains('(') && !t.contains("synchronized") && !t.trim_start().starts_with("if")
            && !t.trim_start().starts_with("while")
            && !t.trim_start().starts_with("for")
            && !t.trim_start().starts_with("switch")
    }) else {
        return src.to_string();
    };
    let Some(rel) = lines
        .iter()
        .skip(sig + 1)
        .position(|l| !l.trim().is_empty())
    else {
        return src.to_string();
    };
    let sync_i = sig + 1 + rel;
    if lines[sync_i].trim() != "synchronized (this) {" {
        return src.to_string();
    }
    let mut depth = 0i32;
    let mut end = None;
    for (idx, line) in lines.iter().enumerate().skip(sync_i) {
        for c in line.chars() {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
            }
        }
        if depth == 0 {
            end = Some(idx);
            break;
        }
    }
    let Some(end) = end else {
        return src.to_string();
    };
    let method_close = lines.len().saturating_sub(1);
    if lines[end + 1..method_close]
        .iter()
        .any(|l| !l.trim().is_empty())
    {
        return src.to_string();
    }
    let sig_line = insert_synchronized_modifier(lines[sig]);
    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx == sig {
            out.push_str(&sig_line);
        } else if idx == sync_i || idx == end {
            continue;
        } else if idx > sync_i && idx < end {
            out.push_str(line.strip_prefix("    ").unwrap_or(line));
        } else {
            out.push_str(line);
        }
        if idx < lines.len().saturating_sub(1) {
            out.push('\n');
        }
    }
    if src.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn insert_synchronized_modifier(sig: &str) -> String {
    if sig.contains(" static ") {
        sig.replacen(" static ", " static synchronized ", 1)
    } else if let Some(i) = sig.find("public ") {
        let at = i + "public ".len();
        format!("{}synchronized {}", &sig[..at], &sig[at..])
    } else if let Some(i) = sig.find("private ") {
        let at = i + "private ".len();
        format!("{}synchronized {}", &sig[..at], &sig[at..])
    } else if let Some(i) = sig.find("protected ") {
        let at = i + "protected ".len();
        format!("{}synchronized {}", &sig[..at], &sig[at..])
    } else {
        sig.to_string()
    }
}

/// Shorten fully-qualified Java names in a line.
fn shorten_java_names(line: &str) -> String {
    // RDX modification: per-method output must preserve qualified type names.
    // No class-level imports are merged into the native RDX document.
    line.to_owned()
}

/// Negate a condition properly: `a != b` → `a == b`, `a >= 0` → `a < 0`, etc.
/// Also normalizes `!(a >= b)` → `a < b` (and similar).
fn negate_condition(cond: &str) -> String {
    normalize_condition(&raw_negate_condition(cond))
}

fn raw_negate_condition(cond: &str) -> String {
    let cond = cond.trim();
    // !!x / !(!x) style: peel a single leading `!`
    if let Some(rest) = strip_outer_not(cond) {
        return rest.to_string();
    }
    let ops: &[(&str, &str)] = &[
        (" != ", " == "),
        (" == ", " != "),
        (" >= ", " < "),
        (" < ", " >= "),
        (" > ", " <= "),
        (" <= ", " > "),
    ];
    for (op, neg) in ops {
        if let Some(pos) = cond.find(op) {
            // Only flip when this is a simple binary relational (no nested `&&` / `||`).
            if !cond.contains("&&") && !cond.contains("||") {
                return format!("{}{}{}", &cond[..pos], neg, &cond[pos + op.len()..]);
            }
        }
    }
    format!("!({})", cond)
}

/// If `cond` is `!x` or `!(…)`, return the inner expression; otherwise None.
fn strip_outer_not(cond: &str) -> Option<&str> {
    let cond = cond.trim();
    if let Some(rest) = cond.strip_prefix('!') {
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            return Some(inner.trim());
        }
        if is_java_ident(rest) {
            return Some(rest);
        }
    }
    None
}

/// Normalize readable conditions: strip `!(a op b)` into the flipped relation,
/// and collapse `!(!x)` / `!!x` into `x`.
fn normalize_condition(cond: &str) -> String {
    let mut cond = cond.trim().to_string();
    // Collapse nested outer nots: !(!x) → x, !!!x → !x
    for _ in 0..4 {
        let next = if let Some(inner) = strip_outer_not(&cond) {
            if let Some(inner2) = strip_outer_not(inner) {
                inner2.to_string()
            } else if let Some(inner) = cond.strip_prefix("!(").and_then(|s| s.strip_suffix(')')) {
                let inner = inner.trim();
                if !inner.contains("&&") && !inner.contains("||") && !inner.contains('(') {
                    raw_negate_condition(inner)
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            break;
        };
        if next == cond {
            break;
        }
        cond = next;
    }
    let cond = cond.trim();
    if let Some(inner) = cond.strip_prefix("!(").and_then(|s| s.strip_suffix(')')) {
        let inner = inner.trim();
        if !inner.contains("&&") && !inner.contains("||") && !inner.contains('(') {
            return raw_negate_condition(inner);
        }
    }
    cond.to_string()
}

/// Remove move-exception / rethrow lines typical of DEX finally handlers.
fn strip_finally_exception_noise(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // Skip exception rethrow boilerplate often left in catch-all finally handlers.
        if t.starts_with("throw ") || t.contains("move-exception") {
            continue;
        }
        if (t.starts_with("Throwable ") || t.starts_with("Exception "))
            && (t.contains(" = e") || t.ends_with("= e;"))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// For 2-byte goto (format 10t), the offset is 8-bit signed in code units. The decoder often
/// shows it as unsigned (e.g. "+f9h"). Correct to signed so the target is within the method.
fn format_goto_operands_signed(raw_hex: &str, operands: &str) -> String {
    let parts: Vec<&str> = raw_hex.split_whitespace().collect();
    if parts.len() != 2 {
        return operands.to_string();
    }
    let offset_byte = match u8::from_str_radix(parts[1].trim(), 16) {
        Ok(b) => b,
        Err(_) => return operands.to_string(),
    };
    let offset_signed = offset_byte as i8;
    if offset_signed >= 0 {
        return operands.to_string();
    }
    format!("-{:02x}h", (-(offset_signed as i32)) as u8)
}

/// True if the token looks like a branch offset (e.g. "+008h", "-4") rather than a register/value.
fn is_branch_offset(token: &str) -> bool {
    let t = token.trim();
    if t.is_empty() {
        return true;
    }
    let b = t.as_bytes();
    (b[0] == b'+' || b[0] == b'-')
        && t[1..]
            .trim()
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == 'h')
        || (t.starts_with("0x") && t[2..].chars().all(|c| c.is_ascii_hexdigit()))
}

/// Replace raw register references (vN) in a condition string with display names.
/// Word-boundary aware: `v1` won't match inside `v10`.
fn replace_register_names(condition: &str, reg_to_name: &HashMap<u32, String>) -> String {
    let bytes = condition.as_bytes();
    let mut result = String::with_capacity(condition.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'v' {
            let before_ok = i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric();
            if before_ok {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && (bytes[end] as char).is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    let after_ok = end == bytes.len()
                        || !(bytes[end] as char).is_ascii_alphanumeric() && bytes[end] != b'_';
                    if after_ok {
                        if let Ok(reg) = condition[start..end].parse::<u32>() {
                            if let Some(name) = reg_to_name.get(&reg) {
                                result.push_str(name);
                                i = end;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Turn `z0 == 0` / `z0 != 0` into `!z0` / `z0` when the operand is a boolean.
/// Turn `obj == 0` / `obj != 0` into `obj == null` / `obj != null` for reference types.
fn polish_boolean_condition(
    condition: &str,
    local_names: &HashMap<u32, String>,
    local_types: &HashMap<u32, String>,
) -> String {
    let cond = condition.trim();
    let (name, is_eq) = if let Some(n) = cond.strip_suffix(" == 0") {
        (n.trim(), true)
    } else if let Some(n) = cond.strip_suffix(" != 0") {
        (n.trim(), false)
    } else {
        return condition.to_string();
    };
    if !is_java_ident(name) {
        return condition.to_string();
    }
    let ty = local_names.iter().find_map(|(reg, n)| {
        (n == name)
            .then(|| local_types.get(reg).map(|t| t.as_str()))
            .flatten()
    });
    let is_bool = ty == Some("boolean") || looks_like_boolean_temp(name);
    if is_bool {
        return if is_eq {
            format!("!{}", name)
        } else {
            name.to_string()
        };
    }
    let is_primitive_temp = looks_like_primitive_temp(name);
    let is_ref = match ty {
        Some(t) => is_reference_java_type(t),
        None => !is_primitive_temp,
    };
    if is_ref {
        return if is_eq {
            format!("{} == null", name)
        } else {
            format!("{} != null", name)
        };
    }
    condition.to_string()
}

fn looks_like_boolean_temp(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 2 && b[0] == b'z' && b[1..].iter().all(|c| c.is_ascii_digit())
}

fn looks_like_primitive_temp(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 2
        && matches!(b[0], b'i' | b'j' | b'f' | b'd' | b'c' | b'b')
        && b[1..].iter().all(|c| c.is_ascii_digit())
}

fn is_reference_java_type(ty: &str) -> bool {
    !matches!(
        ty.trim(),
        "boolean" | "byte" | "short" | "int" | "long" | "float" | "double" | "char" | "void"
    ) && !ty.trim().is_empty()
}

/// Registers tested by the terminating if-* in a conditional block.
fn condition_regs_in_block(
    cfg: &MethodCfg,
    block_id: BlockId,
    instructions: &[Instruction],
) -> Vec<u32> {
    let offs = &cfg.blocks[block_id].instruction_offsets;
    let Some(&off) = offs.last() else {
        return Vec::new();
    };
    let Some(ins) = instructions.iter().find(|i| i.offset as u32 == off) else {
        return Vec::new();
    };
    let m = ins.mnemonic();
    if m.starts_with("if-") {
        return regs_mentioned_in_operands(ins.operands());
    }
    Vec::new()
}

/// Register numbers mentioned as `vN` / `vN_k` in an operands string.
fn regs_mentioned_in_operands(ops: &str) -> Vec<u32> {
    let bytes = ops.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'v' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            if before_ok {
                i += 1;
                let mut reg: u32 = 0;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    reg = reg * 10 + (bytes[i] - b'0') as u32;
                    i += 1;
                }
                out.push(reg);
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Format Dalvik conditional branch as a Java boolean expression for if/while.
/// Operands may include branch offset (e.g. "v0, +008h"); we use only register/value parts.
fn format_condition(mnemonic: &str, resolved_operands: &str) -> String {
    let parts: Vec<&str> = resolved_operands
        .split(',')
        .map(str::trim)
        .filter(|p| !is_branch_offset(p))
        .collect();
    match mnemonic {
        "if-eq" => {
            if parts.len() >= 2 {
                format!("{} == {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-ne" => {
            if parts.len() >= 2 {
                format!("{} != {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-lt" => {
            if parts.len() >= 2 {
                format!("{} < {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-ge" => {
            if parts.len() >= 2 {
                format!("{} >= {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-gt" => {
            if parts.len() >= 2 {
                format!("{} > {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-le" => {
            if parts.len() >= 2 {
                format!("{} <= {}", parts[0], parts[1])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-eqz" => {
            if !parts.is_empty() {
                format!("{} == 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-nez" => {
            if !parts.is_empty() {
                format!("{} != 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-ltz" => {
            if !parts.is_empty() {
                format!("{} < 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-gez" => {
            if !parts.is_empty() {
                format!("{} >= 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-gtz" => {
            if !parts.is_empty() {
                format!("{} > 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "if-lez" => {
            if !parts.is_empty() {
                format!("{} <= 0", parts[0])
            } else {
                resolved_operands.to_string()
            }
        }
        "packed-switch" | "sparse-switch" => {
            if !parts.is_empty() {
                parts[0].to_string()
            } else {
                resolved_operands.to_string()
            }
        }
        _ => resolved_operands.to_string(),
    }
}

/// True when `segment` is a single safe filesystem path component.
pub fn is_safe_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('/')
        && !segment.contains('\\')
        && !segment.contains('\0')
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Map Java class name to (relative_dir, file_name) for dumping.
/// e.g. "com.example.MyClass" -> ("com/example", "MyClass.java"), "Outer$Inner" -> ("", "Outer$Inner.java").
///
/// Rejects path-traversal segments (`..`, separators). Unsafe names are redirected under
/// `_unsafe/<hex>.java` so dumps never escape the output directory.
pub fn class_name_to_path(class_name: &str) -> (std::path::PathBuf, String) {
    match try_class_name_to_path(class_name) {
        Ok(v) => v,
        Err(_) => {
            let digest = simple_name_digest(class_name);
            (
                std::path::PathBuf::from("_unsafe"),
                format!("{digest}.java"),
            )
        }
    }
}

fn try_class_name_to_path(
    class_name: &str,
) -> std::result::Result<(std::path::PathBuf, String), String> {
    if class_name.is_empty() {
        return Err("empty class name".into());
    }
    let parts: Vec<&str> = class_name.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("empty class name".into());
    }
    if !parts.iter().all(|p| is_safe_path_segment(p)) {
        return Err(format!("unsafe class path: {class_name}"));
    }
    if parts.len() == 1 {
        return Ok((
            std::path::PathBuf::new(),
            format!("{}.java", parts[0]),
        ));
    }
    let simple_name = parts[parts.len() - 1];
    let package_dir = parts[..parts.len() - 1].join("/");
    Ok((
        std::path::PathBuf::from(package_dir),
        format!("{simple_name}.java"),
    ))
}

fn simple_name_digest(s: &str) -> String {
    // Stable, non-crypto fingerprint for fallback filenames (FNV-1a 64-bit).
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

    /// Resolve `base/rel_dir/file_name` and ensure it stays under `base`.
pub fn resolve_under_base(
    base: &std::path::Path,
    rel_dir: &std::path::Path,
    file_name: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    let stem = file_name.strip_suffix(".java").unwrap_or(file_name);
    if !is_safe_path_segment(stem) {
        return Err(format!("unsafe file name: {file_name}"));
    }
    for c in rel_dir.components() {
        match c {
            std::path::Component::Normal(s) => {
                let s = s.to_string_lossy();
                if !is_safe_path_segment(&s) {
                    return Err(format!("unsafe path segment: {s}"));
                }
            }
            std::path::Component::CurDir => {}
            _ => return Err(format!("unsafe relative dir: {}", rel_dir.display())),
        }
    }
    let full_dir = base.join(rel_dir);
    let file_path = full_dir.join(file_name);
    let base_n = normalize_os_path(base).ok_or_else(|| {
        format!("invalid output base: {}", base.display())
    })?;
    let file_n = normalize_os_path(&file_path).ok_or_else(|| {
        format!("path escapes output directory: {}", file_path.display())
    })?;
    if !file_n.starts_with(&base_n) {
        return Err(format!(
            "path escapes output directory: {}",
            file_path.display()
        ));
    }
    Ok(file_path)
}

fn normalize_os_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Some(out)
}

fn write_decompiled_class(
    base_path: &std::path::Path,
    output_class_name: &str,
    class_java: &str,
) -> Result<()> {
    let (rel_dir, file_name) = class_name_to_path(output_class_name);
    let file_path = resolve_under_base(base_path, &rel_dir, &file_name)
        .map_err(DexDecompilerError::Decompilation)?;
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DexDecompilerError::Decompilation(format!(
                "create dir {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    std::fs::write(&file_path, class_java).map_err(|e| {
        DexDecompilerError::Decompilation(format!("write {}: {}", file_path.display(), e))
    })?;
    Ok(())
}

/// Resolve one operand token (e.g. string@5 -> "hello", type@2 -> java.lang.String).
fn resolve_one(dex: &DexFile, part: &str) -> String {
    if let Some(idx_str) = part.strip_prefix("string@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok(s) = dex.get_string(idx) {
                return format!("\"{}\"", escape_java_string(&s));
            }
        }
    }
    if let Some(idx_str) = part.strip_prefix("type@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok(desc) = dex.get_type(idx) {
                return java::descriptor_to_java(&desc);
            }
        }
    }
    if let Some(idx_str) = part.strip_prefix("field@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok(fi) = dex.get_field_info(idx) {
                return format!("{}.{}", java::descriptor_to_java(&fi.class), fi.name);
            }
        }
    }
    if let Some(idx_str) = part.strip_prefix("method@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok(mi) = dex.get_method_info(idx) {
                let params = mi
                    .params
                    .iter()
                    .map(|p| java::descriptor_to_java(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                return format!(
                    "{}.{}({})",
                    java::descriptor_to_java(&mi.class),
                    mi.name,
                    params
                );
            }
        }
    }
    if let Some(idx_str) = part.strip_prefix("callsite@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok(info) = dex.get_call_site(idx) {
                return format_call_site_java(dex, &info);
            }
        }
        return format!("/* invoke-custom/lambda callsite@{} */", idx_str);
    }
    if let Some(idx_str) = part.strip_prefix("proto@") {
        if let Ok(idx) = idx_str.parse::<u32>() {
            if let Ok((ret, params)) =
                dex.protos
                    .get_proto(&*dex.data, &dex.types, &dex.strings, idx)
            {
                let params_j = params
                    .iter()
                    .map(|p| java::descriptor_to_java(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                return format!(
                    "/* proto {}({}) */",
                    java::descriptor_to_java(&ret),
                    params_j
                );
            }
        }
        return format!("/* proto@{} */", idx_str);
    }
    part.to_string()
}

/// Readable stub for a resolved call site (prefer lambda → impl method).
fn format_call_site_java(dex: &dex_parser::DexFile, info: &dex_parser::CallSiteInfo) -> String {
    if let Some(mid) = dex_parser::DexCallSites::impl_method_id(info) {
        if let Ok(mi) = dex.get_method_info(mid as u32) {
            let class = java::descriptor_to_java(&mi.class);
            return format!("/* lambda {} → {}.{} */", info.method_name, class, mi.name);
        }
    }
    format!(
        "/* invoke-custom {} (bootstrap mh@{}) */",
        info.method_name, info.bootstrap_handle_idx
    )
}

/// Emit a Java-ish statement for `invoke-custom` / lambdas (no body inline).
fn format_invoke_custom_stmt(dex: &dex_parser::DexFile, raw_ops: &str) -> String {
    let captures = extract_brace_regs(raw_ops);
    let args = captures.join(", ");
    if let Some(idx) = extract_callsite_idx(raw_ops) {
        if let Ok(info) = dex.get_call_site(idx) {
            if let Some(mid) = dex_parser::DexCallSites::impl_method_id(&info) {
                if let Ok(mi) = dex.get_method_info(mid as u32) {
                    let class = java::descriptor_to_java(&mi.class);
                    return format!(
                        "/* lambda {}.{} */ {}.{}({})",
                        info.method_name, mi.name, class, mi.name, args
                    );
                }
            }
            return format!("/* invoke-custom {} */ ({})", info.method_name, args);
        }
    }
    format!("/* invoke-custom */ {}", raw_ops)
}

fn extract_callsite_idx(ops: &str) -> Option<u32> {
    let idx = ops.find("callsite@")?;
    let rest = &ops[idx + "callsite@".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn extract_brace_regs(ops: &str) -> Vec<String> {
    let start = match ops.find('{') {
        Some(i) => i + 1,
        None => return vec![],
    };
    let end = match ops[start..].find('}') {
        Some(i) => start + i,
        None => return vec![],
    };
    let inner = ops[start..end].trim();
    if inner.is_empty() {
        return vec![];
    }
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn try_lambda_expression_body(body: &str) -> Option<String> {
    let lines: Vec<&str> = body
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();
    if lines.len() != 1 {
        return None;
    }
    let l = lines[0];
    if let Some(rest) = l.strip_prefix("return ") {
        return Some(rest.trim_end_matches(';').trim().to_string());
    }
    None
}

fn replace_ident_in_body(body: &str, from: &str, to: &str) -> String {
    if from == to || from.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if body[i..].starts_with(from) {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after = i + from.len();
            let after_ok = after >= bytes.len() || !is_ident_char(bytes[after]);
            if before_ok && after_ok {
                out.push_str(to);
                i = after;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Escape string for use inside a Java string literal (for tests and reuse).
/// Registers that only ever hold `this` (the incoming `p0` plus `move-object` copies
/// that are never overwritten). Loop blocks often don't see the prologue move, so
/// without this they name `this` as `local0` and later passes turn it into `0`.
fn registers_holding_this(instructions: &[Instruction], this_reg: u32) -> HashSet<u32> {
    let mut holding: HashSet<u32> = HashSet::new();
    holding.insert(this_reg);
    for ins in instructions {
        let m = ins.mnemonic();
        if matches!(
            m,
            "move-object" | "move-object/from16" | "move-object/16"
        ) {
            if let Some((dst, src)) = parse_two_regs(ins.operands()) {
                if holding.contains(&src) {
                    holding.insert(dst);
                } else {
                    holding.remove(&dst);
                }
                continue;
            }
        }
        if let Some(dst) = instruction_writes_register(ins) {
            if dst != this_reg {
                holding.remove(&dst);
            }
        }
    }
    holding
}

/// Destination register of an instruction that defines a value, if any.
fn instruction_writes_register(ins: &Instruction) -> Option<u32> {
    let m = ins.mnemonic();
    if m.starts_with("invoke")
        || m.starts_with("if-")
        || m.starts_with("return")
        || m.starts_with("goto")
        || m.starts_with("monitor")
        || m == "throw"
        || m.starts_with("packed-switch")
        || m.starts_with("sparse-switch")
        || m.starts_with("fill-array")
        || m.ends_with("-payload")
        || m == "nop"
        || m.starts_with("aput")
        || m.starts_with("iput")
        || m.starts_with("sput")
        || m.starts_with("move-result")
        || m == "move-exception"
    {
        // move-result / move-exception do write a register (first operand).
        if m.starts_with("move-result") || m == "move-exception" {
            return parse_one_reg(ins.operands());
        }
        return None;
    }
    parse_one_reg(ins.operands())
}

pub(crate) fn escape_java_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn parse_one_reg(ops: &str) -> Option<u32> {
    let s = ops.trim().strip_prefix('v')?;
    s.split(',').next()?.trim().parse().ok()
}

pub(crate) fn parse_two_regs(ops: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let a = parts[0].strip_prefix('v')?.parse().ok()?;
    let b = parts[1].strip_prefix('v')?.parse().ok()?;
    Some((a, b))
}

/// Parse "vA, vB, literal" (e.g. add-int/lit8). Returns (dest_reg, src_reg, literal_str).
pub(crate) fn parse_two_regs_and_literal(ops: &str) -> Option<(u32, u32, String)> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let a = parts[0].strip_prefix('v')?.parse().ok()?;
    let b = parts[1].strip_prefix('v')?.parse().ok()?;
    let lit = parts[2].to_string();
    Some((a, b, lit))
}

/// Parse "vA, vB, vC" (e.g. add-int). Returns (dest_reg, src1_reg, src2_reg).
pub(crate) fn parse_three_regs(ops: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let a = parts[0].strip_prefix('v')?.parse().ok()?;
    let b = parts[1].strip_prefix('v')?.parse().ok()?;
    let c = parts[2].strip_prefix('v')?.parse().ok()?;
    Some((a, b, c))
}

/// Parse iget/iput operands: "vA, vB, ClassName.fieldName" -> (dest_reg, object_reg, field_name).
/// field_name is the segment after the last dot (Java instance field access).
pub(crate) fn parse_instance_field_operands(ops: &str) -> Option<(u32, u32, String)> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let dest = parts[0].strip_prefix('v')?.parse().ok()?;
    let object_reg = parts[1].strip_prefix('v')?.parse().ok()?;
    let field_ref = parts[2].trim();
    let field_name = field_ref
        .rsplit('.')
        .next()
        .unwrap_or(field_ref)
        .to_string();
    Some((dest, object_reg, field_name))
}

/// Parse sget/sput operands: "vA, ClassName.fieldName" -> (reg, field_ref).
pub(crate) fn parse_static_field_operands(ops: &str) -> Option<(u32, String)> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let reg = parts[0].strip_prefix('v')?.parse().ok()?;
    let field_ref = parts[1].trim().to_string();
    Some((reg, field_ref))
}

/// Format a `const-wide` bit pattern as a readable Java `long` or `double` literal.
///
/// Dex-bytecode prints the operand as a signed decimal of the raw 64-bit pattern
/// (e.g. `-4604930618986332160` for double `-6.0`). Parse as `i64` and reinterpret
/// bits — do not take the absolute value, or IEEE doubles become garbage hex longs.
pub(crate) fn format_java_wide_literal(bits_str: &str, java_type: &str) -> String {
    let trimmed = bits_str.trim();
    let bits = if let Ok(signed) = trimmed.parse::<i64>() {
        signed as u64
    } else if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        let hex = hex.trim_end_matches('L').trim_end_matches('l');
        match u64::from_str_radix(hex, 16) {
            Ok(v) => v,
            Err(_) => return bits_str.to_string(),
        }
    } else if let Ok(u) = trimmed
        .trim_end_matches('L')
        .trim_end_matches('l')
        .parse::<u64>()
    {
        u
    } else {
        return bits_str.to_string();
    };
    let signed = bits as i64;
    match java_type {
        "double" | "java.lang.Double" => format_java_double(f64::from_bits(bits)),
        "float" | "java.lang.Float" => format_java_float(f32::from_bits(bits as u32)),
        _ => format_java_long(signed),
    }
}

/// `const-wide/high16` only sets the top 16 bits; those immediates are almost always doubles.
fn looks_like_high16_double_bits(bits: u64) -> bool {
    bits & 0x0000_FFFF_FFFF_FFFF == 0 && bits != 0
}

fn format_java_double(v: f64) -> String {
    if v.is_nan() {
        return "Double.NaN".to_string();
    }
    if v.is_infinite() {
        return if v.is_sign_positive() {
            "Double.POSITIVE_INFINITY".to_string()
        } else {
            "Double.NEGATIVE_INFINITY".to_string()
        };
    }
    shorten_float_literal(v, &format!("{:.17}", v))
}

fn shorten_float_literal(v: f64, s: &str) -> String {
    let mut best = trim_float_trailing_zeros(s);
    let mut candidate = s.to_string();
    while candidate.contains('.') {
        if let Ok(parsed) = candidate.parse::<f64>() {
            if parsed == v {
                best = trim_float_trailing_zeros(&candidate);
            }
        }
        if !candidate
            .as_bytes()
            .last()
            .is_some_and(|b| b.is_ascii_digit())
        {
            break;
        }
        candidate.pop();
    }
    best
}

fn format_java_float(v: f32) -> String {
    if v.is_nan() {
        return "Float.NaN".to_string();
    }
    if v.is_infinite() {
        return if v.is_sign_positive() {
            "Float.POSITIVE_INFINITY".to_string()
        } else {
            "Float.NEGATIVE_INFINITY".to_string()
        };
    }
    let mut best = trim_float_trailing_zeros(&format!("{:.9}", v));
    let mut candidate = format!("{:.9}", v);
    while candidate.contains('.') {
        if let Ok(parsed) = candidate.parse::<f32>() {
            if parsed == v {
                best = trim_float_trailing_zeros(&candidate);
            }
        }
        if !candidate
            .as_bytes()
            .last()
            .is_some_and(|b| b.is_ascii_digit())
        {
            break;
        }
        candidate.pop();
        if candidate.ends_with('.') {
            break;
        }
    }
    // Prefer short decimal forms that round-trip (4.2f from 4.20f bits).
    for prec in 1..=7 {
        let c = format!("{:.*}", prec, v);
        if let Ok(parsed) = c.parse::<f32>() {
            if parsed == v {
                let t = trim_float_trailing_zeros(&c);
                if t.len() < best.len() {
                    best = t;
                }
            }
        }
    }
    if !best.contains('.') && !best.contains('e') && !best.contains('E') {
        best.push_str(".0");
    }
    format!("{}f", best)
}

fn parse_const_bits_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Ok(v) = s.parse::<i32>() {
        return Some(v as u32);
    }
    s.parse::<u32>().ok()
}

fn operand_mentions_reg(ops: &str, reg: u32) -> bool {
    regs_mentioned_in_operands(ops).contains(&reg)
}

fn format_java_long(v: i64) -> String {
    if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
        return format!("{}L", v);
    }
    format!("0x{:X}L", v as u64)
}

fn trim_float_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let mut out = s.trim_end_matches('0').to_string();
    if out.ends_with('.') {
        out.push('0');
    }
    out
}

pub(crate) fn parse_const_into_reg(ops: &str) -> Option<String> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let reg = parts[0].strip_prefix('v')?;
    let val = parts[1];
    Some(format!("v{} = {};", reg, val))
}

/// Split `vN, <rest>` on the first comma only.
/// Resolved string literals may contain commas (`v0, "a, b"`). Splitting every
/// comma keeps only the prefix and drops the closing quote.
fn split_reg_and_rest(ops: &str) -> Option<(&str, &str)> {
    let comma = ops.find(',')?;
    let head = ops[..comma].trim();
    let rest = ops[comma + 1..].trim();
    if head.is_empty() || rest.is_empty() {
        None
    } else {
        Some((head, rest))
    }
}

/// Parse assignment-like instruction into (dst_reg, rhs_expr_string) for IR Assign.
/// Returns None for non-assignment or unparseable ops.
fn parse_assign_rhs(m: &str, ops: &str) -> Option<(u32, String)> {
    match m {
        "const/4" | "const/16" | "const" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            let val = parts[1].to_string();
            Some((reg, val))
        }
        "const-string" | "const-string/jumbo" => {
            let (reg_tok, val) = split_reg_and_rest(ops)?;
            let reg: u32 = reg_tok.strip_prefix('v')?.parse().ok()?;
            // Rest is already resolved (e.g. "\"hello\"" or "\"a, b\"") by resolve_operands.
            Some((reg, val.to_string()))
        }
        "sub-int/2addr" | "add-int/2addr" | "mul-int/2addr" | "div-int/2addr" | "rem-int/2addr"
        | "and-int/2addr" | "or-int/2addr" | "xor-int/2addr" | "shl-int/2addr"
        | "shr-int/2addr" | "ushr-int/2addr" | "add-long/2addr" | "sub-long/2addr"
        | "mul-long/2addr" | "div-long/2addr" | "rem-long/2addr" | "and-long/2addr"
        | "or-long/2addr" | "xor-long/2addr" | "shl-long/2addr" | "shr-long/2addr"
        | "ushr-long/2addr" | "add-float/2addr" | "sub-float/2addr" | "mul-float/2addr"
        | "div-float/2addr" | "rem-float/2addr" | "add-double/2addr" | "sub-double/2addr"
        | "mul-double/2addr" | "div-double/2addr" | "rem-double/2addr" => {
            let op = match m {
                "sub-int/2addr" | "sub-long/2addr" | "sub-float/2addr" | "sub-double/2addr" => "-",
                "add-int/2addr" | "add-long/2addr" | "add-float/2addr" | "add-double/2addr" => "+",
                "mul-int/2addr" | "mul-long/2addr" | "mul-float/2addr" | "mul-double/2addr" => "*",
                "div-int/2addr" | "div-long/2addr" | "div-float/2addr" | "div-double/2addr" => "/",
                "rem-int/2addr" | "rem-long/2addr" | "rem-float/2addr" | "rem-double/2addr" => "%",
                "and-int/2addr" | "and-long/2addr" => "&",
                "or-int/2addr" | "or-long/2addr" => "|",
                "xor-int/2addr" | "xor-long/2addr" => "^",
                "shl-int/2addr" | "shl-long/2addr" => "<<",
                "shr-int/2addr" | "shr-long/2addr" => ">>",
                "ushr-int/2addr" | "ushr-long/2addr" => ">>>",
                _ => return None,
            };
            parse_two_regs(ops).map(|(a, b)| (a, format!("v{} {} v{}", a, op, b)))
        }
        "add-int/lit8" | "mul-int/lit8" | "div-int/lit8" | "rem-int/lit8" | "and-int/lit8"
        | "or-int/lit8" | "xor-int/lit8" | "shl-int/lit8" | "shr-int/lit8" | "ushr-int/lit8"
        | "add-int/lit16" | "mul-int/lit16" | "div-int/lit16" | "rem-int/lit16"
        | "and-int/lit16" | "or-int/lit16" | "xor-int/lit16" => {
            let op = if m.contains("add") {
                "+"
            } else if m.contains("mul") {
                "*"
            } else if m.contains("div") {
                "/"
            } else if m.contains("rem") {
                "%"
            } else if m.contains("and") {
                "&"
            } else if m.contains("or") {
                "|"
            } else if m.contains("xor") {
                "^"
            } else if m.contains("shl") {
                "<<"
            } else if m.contains("shr") && !m.contains("ushr") {
                ">>"
            } else {
                ">>>"
            };
            parse_two_regs_and_literal(ops).map(|(dest, src, lit)| {
                let lit_trim = lit.trim();
                let rhs = if op == "+" && lit_trim.starts_with('-') && lit_trim.len() > 1 {
                    let magnitude = lit_trim[1..].trim();
                    format!("v{} - {}", src, magnitude)
                } else {
                    format!("v{} {} {}", src, op, lit)
                };
                (dest, rhs)
            })
        }
        "rsub-int/lit8" | "rsub-int" => parse_two_regs_and_literal(ops)
            .map(|(dest, src, lit)| (dest, format!("{} - v{}", lit, src))),
        "add-int" | "add-long" | "sub-int" | "sub-long" | "mul-int" | "mul-long" | "div-int"
        | "div-long" | "rem-int" | "rem-long" | "and-int" | "and-long" | "or-int" | "or-long"
        | "xor-int" | "xor-long" | "shl-int" | "shl-long" | "shr-int" | "shr-long" | "ushr-int"
        | "ushr-long" => {
            let op = if m.contains("add") {
                "+"
            } else if m.contains("sub") {
                "-"
            } else if m.contains("mul") {
                "*"
            } else if m.contains("div") {
                "/"
            } else if m.contains("rem") {
                "%"
            } else if m.contains("and") {
                "&"
            } else if m.contains("or") {
                "|"
            } else if m.contains("xor") {
                "^"
            } else if m.contains("shl") {
                "<<"
            } else if m.contains("ushr") {
                ">>>"
            } else {
                ">>"
            };
            parse_three_regs(ops).map(|(a, b, c)| (a, format!("v{} {} v{}", b, op, c)))
        }
        "neg-int" | "neg-long" | "neg-float" | "neg-double" | "not-int" | "not-long" => {
            let op = if m.starts_with("neg") { "-" } else { "~" };
            parse_two_regs(ops).map(|(a, b)| (a, format!("{}{}", op, format!("v{}", b))))
        }
        "move" | "move/from16" | "move/16" | "move-object" | "move-wide" | "move-wide/from16"
        | "move-wide/16" | "move-object/from16" | "move-object/16" => {
            parse_two_regs(ops).map(|(d, s)| (d, format!("v{}", s)))
        }
        // Int-to-* casts
        "int-to-long" => parse_two_regs(ops).map(|(a, b)| (a, format!("(long) v{}", b))),
        "int-to-float" => parse_two_regs(ops).map(|(a, b)| (a, format!("(float) v{}", b))),
        "int-to-double" => parse_two_regs(ops).map(|(a, b)| (a, format!("(double) v{}", b))),
        "long-to-int" => parse_two_regs(ops).map(|(a, b)| (a, format!("(int) v{}", b))),
        "long-to-float" => parse_two_regs(ops).map(|(a, b)| (a, format!("(float) v{}", b))),
        "long-to-double" => parse_two_regs(ops).map(|(a, b)| (a, format!("(double) v{}", b))),
        "float-to-int" => parse_two_regs(ops).map(|(a, b)| (a, format!("(int) v{}", b))),
        "float-to-long" => parse_two_regs(ops).map(|(a, b)| (a, format!("(long) v{}", b))),
        "float-to-double" => parse_two_regs(ops).map(|(a, b)| (a, format!("(double) v{}", b))),
        "double-to-int" => parse_two_regs(ops).map(|(a, b)| (a, format!("(int) v{}", b))),
        "double-to-long" => parse_two_regs(ops).map(|(a, b)| (a, format!("(long) v{}", b))),
        "double-to-float" => parse_two_regs(ops).map(|(a, b)| (a, format!("(float) v{}", b))),
        "int-to-byte" => parse_two_regs(ops).map(|(a, b)| (a, format!("(byte) v{}", b))),
        "int-to-char" => parse_two_regs(ops).map(|(a, b)| (a, format!("(char) v{}", b))),
        "int-to-short" => parse_two_regs(ops).map(|(a, b)| (a, format!("(short) v{}", b))),
        "new-array" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 3 {
                return None;
            }
            let dst_reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            let size_reg: u32 = parts[1].strip_prefix('v')?.parse().ok()?;
            let type_str = parts[2];
            let element_type = type_str.strip_suffix("[]").unwrap_or(type_str);
            Some((dst_reg, format!("new {}[v{}]", element_type, size_reg)))
        }
        "iget" | "iget-wide" | "iget-object" | "iget-boolean" | "iget-byte" | "iget-char"
        | "iget-short" => {
            if let Some((dest, obj, field)) = parse_instance_field_operands(ops) {
                Some((dest, format!("v{}.{}", obj, field)))
            } else {
                None
            }
        }
        "sget" | "sget-wide" | "sget-object" | "sget-boolean" | "sget-byte" | "sget-char"
        | "sget-short" => {
            if let Some((reg, field_ref)) = parse_static_field_operands(ops) {
                Some((reg, field_ref))
            } else {
                None
            }
        }
        "aget" | "aget-wide" | "aget-object" | "aget-boolean" | "aget-byte" | "aget-char"
        | "aget-short" => parse_three_regs(ops).map(|(a, b, c)| (a, format!("v{}[v{}]", b, c))),
        "array-length" => parse_two_regs(ops).map(|(a, b)| (a, format!("v{}.length", b))),
        "new-instance" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            Some((reg, format!("new {}()", parts[1])))
        }
        "check-cast" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            Some((reg, format!("({}) v{}", parts[1], reg)))
        }
        "instance-of" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 3 {
                return None;
            }
            let dst: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            Some((
                dst,
                format!(
                    "v{} instanceof {}",
                    parts[1].strip_prefix('v').unwrap_or(parts[1]),
                    parts[2]
                ),
            ))
        }
        "const-class" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            Some((reg, format!("{}.class", parts[1])))
        }
        "const-wide/16" | "const-wide/32" | "const-wide" | "const-wide/high16" | "const/high16" => {
            let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
            if parts.len() < 2 {
                return None;
            }
            let reg: u32 = parts[0].strip_prefix('v')?.parse().ok()?;
            Some((reg, parts[1..].join(", ")))
        }
        _ => None,
    }
}

pub(crate) fn parse_string_ref(ops: &str) -> Option<String> {
    let (reg_tok, val) = split_reg_and_rest(ops)?;
    let reg = reg_tok.strip_prefix('v')?;
    Some(format!("v{} = {};", reg, val))
}

pub(crate) fn parse_new_instance(ops: &str) -> Option<String> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let reg = parts[0].strip_prefix('v')?;
    let typ = parts[1];
    Some(format!("v{} = new {}();", reg, typ))
}

/// new-array vA, vB, type → "vA = new ElementType[vB];" (operands may be vN or names).
fn format_new_array(ops: &str) -> Option<String> {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let dst = parts[0];
    let size = parts[1];
    let type_str = parts[2]; // e.g. "boolean[]" or "int[]"
    let element_type = type_str.strip_suffix("[]").unwrap_or(type_str);
    Some(format!("{} = new {}[{}];", dst, element_type, size))
}

/// filled-new-array vA, vB, …, type[] → `new ElementType[]{ vA, vB, … }` (no destination;
/// result is taken by a following move-result-object).
fn format_filled_new_array_expr(ops: &str) -> Option<String> {
    let parts: Vec<&str> = ops
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let type_str = parts.last()?;
    // Last operand is the array type (e.g. java.lang.String[] or [Ljava/lang/String; already resolved).
    let element_type = type_str.strip_suffix("[]").unwrap_or(type_str);
    if element_type.is_empty() || element_type.starts_with('v') || element_type.starts_with('p') {
        return None;
    }
    let elems = &parts[..parts.len() - 1];
    // `filled-new-array/range` arrives as `v0 ... v5` — expand to every register.
    let expanded = expand_filled_array_elem_list(elems);
    Some(format!("new {}[]{{ {} }}", element_type, expanded))
}

/// Expand Dalvik register-range tokens (`v0 ... v3`) into `v0, v1, v2, v3`.
fn expand_filled_array_elem_list(elems: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    for e in elems {
        if let Some(regs) = expand_reg_range_ellipsis(e) {
            out.extend(regs);
        } else {
            out.push((*e).to_string());
        }
    }
    out.join(", ")
}

/// Parse `v0 ... v5` / `v0 .. v5` / `p1...p4` into individual registers.
fn expand_reg_range_ellipsis(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let (left, right) = if let Some(i) = s.find(" ... ") {
        (&s[..i], &s[i + 5..])
    } else if let Some(i) = s.find(" .. ") {
        (&s[..i], &s[i + 4..])
    } else if let Some(i) = s.find("...") {
        (&s[..i], &s[i + 3..])
    } else if let Some(i) = s.find("..") {
        (&s[..i], &s[i + 2..])
    } else {
        return None;
    };
    let left = left.trim();
    let right = right.trim();
    let (pref_l, n_l) = parse_vp_reg(left)?;
    let (pref_r, n_r) = parse_vp_reg(right)?;
    if pref_l != pref_r {
        return None;
    }
    let (lo, hi) = if n_l <= n_r { (n_l, n_r) } else { (n_r, n_l) };
    // Guard against absurd ranges from corrupt operands.
    if hi - lo > 4096 {
        return None;
    }
    Some((lo..=hi).map(|n| format!("{}{}", pref_l, n)).collect())
}

fn parse_vp_reg(s: &str) -> Option<(char, u32)> {
    let s = s.trim();
    let pref = s.chars().next()?;
    if pref != 'v' && pref != 'p' {
        return None;
    }
    let rest = &s[pref.len_utf8()..];
    // Allow SSA `v0_1` → treat as base reg for range (unlikely in range form).
    let num = rest.split('_').next()?;
    let n: u32 = num.parse().ok()?;
    Some((pref, n))
}

/// Format *-int/2addr or *-long/2addr: vA, vB → "vA = vA op vB;"
fn format_binop_2addr(ops: &str, op: &str) -> String {
    parse_two_regs(ops)
        .map(|(a, b)| format!("v{} = v{} {} v{};", a, a, op, b))
        .unwrap_or_default()
}

/// Format *-int/lit8: vA, vB, lit → "vA = vB op lit;" or "vA = vB - N;" when lit is negative and op is "+".
fn format_lit8(ops: &str, op: &str) -> String {
    parse_two_regs_and_literal(ops)
        .map(|(dest, src, lit)| {
            let lit_trim = lit.trim();
            if op == "+" && lit_trim.starts_with('-') && lit_trim.len() > 1 {
                let magnitude = lit_trim[1..].trim();
                format!("v{} = v{} - {};", dest, src, magnitude)
            } else {
                format!("v{} = v{} {} {};", dest, src, op, lit)
            }
        })
        .unwrap_or_default()
}

/// Format rsub-int/lit8 or rsub-int (lit16): vA, vB, lit → "vA = lit - vB;"
fn format_lit8_rsub(ops: &str) -> String {
    parse_two_regs_and_literal(ops)
        .map(|(dest, src, lit)| format!("v{} = {} - v{};", dest, lit, src))
        .unwrap_or_default()
}

/// Format three-register binary (F23x): vA, vB, vC → "vA = vB op vC;"
fn format_binop_23x(ops: &str, op: &str) -> String {
    parse_three_regs(ops)
        .map(|(a, b, c)| format!("v{} = v{} {} v{};", a, b, op, c))
        .unwrap_or_default()
}

/// Format unary (F12x): vA, vB → "vA = op vB;"
fn format_unary(ops: &str, op: &str) -> String {
    parse_two_regs(ops)
        .map(|(a, b)| format!("v{} = {}v{};", a, op, b))
        .unwrap_or_default()
}

/// Format cast (F12x): vA, vB → "vA = (type) vB;"
fn format_cast(ops: &str, target_type: &str) -> String {
    parse_two_regs(ops)
        .map(|(a, b)| format!("v{} = ({}) v{};", a, target_type, b))
        .unwrap_or_default()
}

/// Format iget*: vA, vB, field → "vA = vB.fieldName;"
fn format_iget(ops: &str) -> String {
    parse_instance_field_operands(ops)
        .map(|(dest, obj, field_name)| format!("v{} = v{}.{};", dest, obj, field_name))
        .unwrap_or_default()
}

/// Format iput*: vA (value), vB (object), field → "vB.fieldName = vA;"
fn format_iput(ops: &str) -> String {
    parse_instance_field_operands(ops)
        .map(|(value_reg, object_reg, field_name)| {
            format!("v{}.{} = v{};", object_reg, field_name, value_reg)
        })
        .unwrap_or_default()
}

/// Format sget*: vA, ClassName.fieldName → "vA = ClassName.fieldName;"
fn format_sget(ops: &str) -> String {
    parse_static_field_operands(ops)
        .map(|(reg, field_ref)| format!("v{} = {};", reg, field_ref))
        .unwrap_or_default()
}

/// Format sput*: vA, ClassName.fieldName → "ClassName.fieldName = vA;"
fn format_sput(ops: &str) -> String {
    parse_static_field_operands(ops)
        .map(|(reg, field_ref)| format!("{} = v{};", field_ref, reg))
        .unwrap_or_default()
}

/// Format array-length: vA, vB → "vA = vB.length;"
fn format_array_length(ops: &str) -> String {
    parse_two_regs(ops)
        .map(|(a, b)| format!("v{} = v{}.length;", a, b))
        .unwrap_or_default()
}

/// Format aget: vA, vB, vC → "vA = vB[vC];"
fn format_aget(ops: &str) -> String {
    parse_three_regs(ops)
        .map(|(a, b, c)| format!("v{} = v{}[v{}];", a, b, c))
        .unwrap_or_default()
}

/// Format aput: vA, vB, vC → "vB[vC] = vA;"
fn format_aput(ops: &str) -> String {
    parse_three_regs(ops)
        .map(|(a, b, c)| format!("v{}[v{}] = v{};", b, c, a))
        .unwrap_or_default()
}

/// Format packed-switch / sparse-switch: parse payload and emit "switch (var) { case 0: case 1: ... default: break; }".
/// Branch offset in F31t is in 16-bit code units from the start of the switch instruction to the payload.
const PACKED_SWITCH_ID: u16 = 0x0100;
const SPARSE_SWITCH_ID: u16 = 0x0200;
const FILL_ARRAY_DATA_ID: u16 = 0x0300;

fn format_switch(ins: &Instruction, code_insns: &[u8], ops_resolved: &str) -> String {
    let var = ops_resolved
        .split(',')
        .next()
        .map(str::trim)
        .unwrap_or("v0");
    let ins_off = ins.offset as usize;
    if ins_off + 6 > code_insns.len() {
        return format!("switch ({});  // (payload missing)", var);
    }
    let rel_units = i32::from_le_bytes(
        code_insns[ins_off + 2..ins_off + 6]
            .try_into()
            .unwrap_or([0, 0, 0, 0]),
    );
    // Payload byte offset: F31t branch is in 16-bit units. ART/dex-bytecode use (instruction+2)+rel*2;
    // some DEX use instruction+rel*2. Try both and only consider in-range offsets.
    let try_at = |off: usize| -> Option<(u16, u16)> {
        if off + 4 > code_insns.len() {
            return None;
        }
        let id = u16::from_le_bytes(code_insns[off..off + 2].try_into().unwrap_or([0, 0]));
        let sz = u16::from_le_bytes(code_insns[off + 2..off + 4].try_into().unwrap_or([0, 0]));
        if id == PACKED_SWITCH_ID || id == SPARSE_SWITCH_ID {
            Some((id, sz))
        } else {
            None
        }
    };
    let to_valid = |c: usize| -> Option<usize> {
        if c <= code_insns.len().saturating_sub(4) {
            Some(c)
        } else {
            None
        }
    };
    let cand_a = (ins_off as i32 + rel_units * 2) as usize; // branch from instruction start
    let cand_b = (ins_off as i32 + 2 + rel_units * 2) as usize; // branch from instruction+2
    let mut candidates: Vec<usize> = [
        cand_a,
        cand_b,
        (ins_off as i32 + rel_units) as usize,
        (ins_off as i32 + 2 + rel_units) as usize,
    ]
    .iter()
    .filter_map(|&c| to_valid(c))
    .collect();
    // If no candidate matched, scan a small window (alignment / offset convention)
    if candidates.iter().all(|&c| try_at(c).is_none()) {
        let center = cand_a;
        let start = center.saturating_sub(8);
        let end = (center + 8).min(code_insns.len().saturating_sub(4));
        for off in start..=end {
            if try_at(off).is_some() {
                candidates.push(off);
                break;
            }
        }
    }
    let (payload_off, ident, size) = match candidates
        .iter()
        .find_map(|&off| try_at(off).map(|(id, sz)| (off, id, sz as usize)))
    {
        Some(t) => t,
        None => return format!("switch ({});  // (payload not found)", var),
    };
    let mut cases = Vec::new();
    match ident {
        PACKED_SWITCH_ID => {
            if payload_off + 8 + size * 4 > code_insns.len() {
                return format!("switch ({});  // (packed payload truncated)", var);
            }
            let first_key = i32::from_le_bytes(
                code_insns[payload_off + 4..payload_off + 8]
                    .try_into()
                    .unwrap_or([0, 0, 0, 0]),
            );
            for i in 0..size {
                cases.push((first_key + i as i32).to_string());
            }
        }
        SPARSE_SWITCH_ID => {
            if payload_off + 4 + size * 8 > code_insns.len() {
                return format!("switch ({});  // (sparse payload truncated)", var);
            }
            for i in 0..size {
                let key_off = payload_off + 4 + i * 4;
                let key = i32::from_le_bytes(
                    code_insns[key_off..key_off + 4]
                        .try_into()
                        .unwrap_or([0, 0, 0, 0]),
                );
                cases.push(key.to_string());
            }
        }
        _ => return format!("switch ({});  // (unknown payload)", var),
    }
    let case_str: Vec<String> = cases.iter().map(|k| format!("case {}:", k)).collect();
    format!(
        "switch ({}) {{ {} default: break; }}",
        var,
        case_str.join(" ")
    )
}

/// Parse fill-array-data payload and return an initializer like "arr = new int[]{ 1, 2, 3 };".
fn format_fill_array_data(ins: &Instruction, code_insns: &[u8], ops_resolved: &str) -> String {
    match parse_fill_array_data_assign(ins, code_insns, ops_resolved) {
        Some((reg, init)) => format!("v{} = {};", reg, init),
        None => format!("/* fill-array-data {} */", ops_resolved),
    }
}

/// Element type name for fill-array-data based on payload element width.
fn fill_array_elem_type(elem_width: u16) -> &'static str {
    match elem_width {
        1 => "byte",
        2 => "short",
        4 => "int",
        8 => "long",
        _ => "Object",
    }
}

/// Parse fill-array-data into `(reg, "new int[]{ 1, 2, 3 }")` for IR Assign.
/// Payload: ident 0x0300 (u16), element_width (u16), size (u32), then size*element_width bytes (LE).
fn parse_fill_array_data_assign(
    ins: &Instruction,
    code_insns: &[u8],
    ops_resolved: &str,
) -> Option<(u32, String)> {
    let arr_tok = ops_resolved
        .split(',')
        .next()
        .map(str::trim)
        .unwrap_or("v0");
    let reg: u32 = arr_tok.strip_prefix('v')?.parse().ok()?;
    let ins_off = ins.offset as usize;
    if ins_off + 6 > code_insns.len() {
        return None;
    }
    let rel_units = i32::from_le_bytes(
        code_insns[ins_off + 2..ins_off + 6]
            .try_into()
            .unwrap_or([0, 0, 0, 0]),
    );
    let try_payload = |off: usize| -> Option<(u16, u32)> {
        if off + 8 > code_insns.len() {
            return None;
        }
        let id = u16::from_le_bytes(code_insns[off..off + 2].try_into().unwrap_or([0, 0]));
        if id != FILL_ARRAY_DATA_ID {
            return None;
        }
        let elem_w = u16::from_le_bytes(code_insns[off + 2..off + 4].try_into().unwrap_or([0, 0]));
        let size = u32::from_le_bytes(
            code_insns[off + 4..off + 8]
                .try_into()
                .unwrap_or([0, 0, 0, 0]),
        );
        if elem_w != 1 && elem_w != 2 && elem_w != 4 && elem_w != 8 {
            return None;
        }
        Some((elem_w, size))
    };
    let cand_a = (ins_off as i32 + rel_units * 2) as usize;
    let cand_b = (ins_off as i32 + 2 + rel_units * 2) as usize;
    let (payload_off, elem_width, size) = [cand_a, cand_b]
        .iter()
        .find_map(|&off| try_payload(off).map(|(w, s)| (off, w, s)))?;

    let data_start = payload_off + 8;
    let data_len = (size as usize).saturating_mul(elem_width as usize);
    if data_start + data_len > code_insns.len() {
        return None;
    }

    let mut values: Vec<String> = Vec::new();
    for i in 0..(size as usize) {
        let el_off = data_start + i * (elem_width as usize);
        let val = match elem_width {
            1 => {
                let b = code_insns.get(el_off).copied().unwrap_or(0);
                (b as i8 as i32).to_string()
            }
            2 => {
                if el_off + 2 > code_insns.len() {
                    break;
                }
                let s =
                    i16::from_le_bytes(code_insns[el_off..el_off + 2].try_into().unwrap_or([0, 0]));
                s.to_string()
            }
            4 => {
                if el_off + 4 > code_insns.len() {
                    break;
                }
                let n = i32::from_le_bytes(
                    code_insns[el_off..el_off + 4]
                        .try_into()
                        .unwrap_or([0, 0, 0, 0]),
                );
                n.to_string()
            }
            8 => {
                if el_off + 8 > code_insns.len() {
                    break;
                }
                let n =
                    i64::from_le_bytes(code_insns[el_off..el_off + 8].try_into().unwrap_or([0; 8]));
                n.to_string()
            }
            _ => break,
        };
        values.push(val);
    }
    let ty = fill_array_elem_type(elem_width);
    let init = format!("new {}[]{{ {} }}", ty, values.join(", "));
    Some((reg, init))
}

/// Format cmp (F23x): vA, vB, vC → vA = -1/0/1 comparison result.
fn format_cmp(ops: &str, mnemonic: &str) -> String {
    parse_three_regs(ops)
        .map(|(a, b, c)| {
            // cmpl: less -> -1, greater -> 1, equal -> 0; cmpg: same (NaN handling differs in Dalvik).
            // cmp-long: (vB > vC) ? 1 : ((vB < vC) ? -1 : 0)
            match mnemonic {
                "cmpl-float" | "cmpl-double" | "cmpg-float" | "cmpg-double" => {
                    format!(
                        "v{} = (v{} < v{}) ? -1 : ((v{} > v{}) ? 1 : 0);",
                        a, b, c, b, c
                    )
                }
                "cmp-long" => {
                    format!(
                        "v{} = (v{} > v{}) ? 1 : ((v{} < v{}) ? -1 : 0);",
                        a, b, c, b, c
                    )
                }
                _ => format!(
                    "v{} = (v{} < v{}) ? -1 : ((v{} > v{}) ? 1 : 0);",
                    a, b, c, b, c
                ),
            }
        })
        .unwrap_or_default()
}

/// check-cast: "vN, Type" → "vN = (Type) vN;"
fn format_check_cast(ops: &str) -> String {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() >= 2 {
        let reg = parts[0];
        let ty = parts[1];
        format!("{} = ({}) {};", reg, ty, reg)
    } else {
        String::new()
    }
}

/// instance-of: "vA, vB, Type" → "vA = vB instanceof Type;"
fn format_instance_of(ops: &str) -> String {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() >= 3 {
        format!("{} = {} instanceof {};", parts[0], parts[1], parts[2])
    } else {
        String::new()
    }
}

/// const-class: "vA, Type" → "vA = Type.class;"
fn format_const_class(ops: &str) -> String {
    let parts: Vec<&str> = ops.split(',').map(str::trim).collect();
    if parts.len() >= 2 {
        format!("{} = {}.class;", parts[0], parts[1])
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_exit_goto_caps_handler_at_merge() {
        // goto +05h at byte 0 → target 10. Handler starts at 2 and is only nops.
        let mut code = vec![0u8; 12];
        code[0] = 0x28;
        code[1] = 0x05;
        let ins = decode_all(&code, 0).unwrap();
        assert_eq!(try_exit_merge_byte(&ins, 0, 2), Some(10));
        assert_eq!(try_exit_merge_byte(&ins, 0, 10), None);
        assert!(handler_falls_through(&ins, 2, 10));
        assert!(!handler_falls_through(&ins, 0, 10));
    }

    #[test]
    fn parse_one_reg_valid() {
        assert_eq!(parse_one_reg("v0"), Some(0));
        assert_eq!(parse_one_reg("v1"), Some(1));
        assert_eq!(parse_one_reg("v255"), Some(255));
        assert_eq!(parse_one_reg("  v3  "), Some(3));
        assert_eq!(parse_one_reg("v0, v1"), Some(0)); // first only
    }

    #[test]
    fn parse_one_reg_invalid() {
        assert_eq!(parse_one_reg(""), None);
        assert_eq!(parse_one_reg("0"), None);
        assert_eq!(parse_one_reg("r0"), None);
    }

    #[test]
    fn parse_two_regs_valid() {
        assert_eq!(parse_two_regs("v0, v1"), Some((0, 1)));
        assert_eq!(parse_two_regs("v2, v3"), Some((2, 3)));
        assert_eq!(parse_two_regs("v0, v1, v2"), Some((0, 1)));
    }

    #[test]
    fn parse_two_regs_invalid() {
        assert_eq!(parse_two_regs("v0"), None);
        assert_eq!(parse_two_regs("v0, string@5"), None); // second not v-prefixed
    }

    #[test]
    fn parse_two_regs_and_literal_valid() {
        assert_eq!(
            parse_two_regs_and_literal("v1, v3, 66"),
            Some((1, 3, "66".into()))
        );
        assert_eq!(
            parse_two_regs_and_literal("v0, v1, 26"),
            Some((0, 1, "26".into()))
        );
    }

    #[test]
    fn parse_two_regs_and_literal_invalid() {
        assert_eq!(parse_two_regs_and_literal("v0, v1"), None);
    }

    #[test]
    fn format_binop_2addr_sub_or() {
        assert_eq!(format_binop_2addr("v0, v3", "-"), "v0 = v0 - v3;");
        assert_eq!(format_binop_2addr("v0, v1", "|"), "v0 = v0 | v1;");
    }

    #[test]
    fn format_lit8_add_and() {
        assert_eq!(format_lit8("v1, v3, 66", "+"), "v1 = v3 + 66;");
        assert_eq!(format_lit8("v1, v1, 26", "&"), "v1 = v1 & 26;");
    }

    #[test]
    fn format_lit8_add_negative_simplifies_to_sub() {
        assert_eq!(format_lit8("v0, v1, -3", "+"), "v0 = v1 - 3;");
        assert_eq!(format_lit8("v2, v2, -1", "+"), "v2 = v2 - 1;");
    }

    #[test]
    fn test_format_lit8_rsub() {
        assert_eq!(super::format_lit8_rsub("v0, v1, 23"), "v0 = 23 - v1;");
    }

    #[test]
    fn parse_three_regs_valid() {
        assert_eq!(parse_three_regs("v0, v1, v2"), Some((0, 1, 2)));
        assert_eq!(parse_three_regs("v1, v3, v0"), Some((1, 3, 0)));
    }

    #[test]
    fn format_binop_23x_add() {
        assert_eq!(super::format_binop_23x("v0, v1, v2", "+"), "v0 = v1 + v2;");
    }

    #[test]
    fn format_unary_neg() {
        assert_eq!(super::format_unary("v0, v1", "-"), "v0 = -v1;");
    }

    #[test]
    fn parse_assign_rhs_const_class_and_const4() {
        assert_eq!(
            super::parse_assign_rhs("const-class", "v9, android.content.Context"),
            Some((9, "android.content.Context.class".into()))
        );
        assert_eq!(
            super::parse_assign_rhs("const/4", "v10, 0"),
            Some((10, "0".into()))
        );
        assert_eq!(super::format_aput("v9, v8, v10"), "v8[v10] = v9;");
    }

    #[test]
    fn parse_assign_rhs_move_object16_and_move_wide16() {
        assert_eq!(
            super::parse_assign_rhs("move-object/16", "v0, v3"),
            Some((0, "v3".into()))
        );
        assert_eq!(
            super::parse_assign_rhs("move-wide/16", "v4, v6"),
            Some((4, "v6".into()))
        );
        assert_eq!(
            super::parse_assign_rhs("array-length", "v0, v8"),
            Some((0, "v8.length".into()))
        );
    }

    #[test]
    fn synthetic_local_names_include_x_temps() {
        assert!(is_synthetic_local_name("x0"));
        assert!(is_synthetic_local_name("x12"));
        assert!(is_synthetic_local_name("v0"));
        assert!(is_synthetic_local_name("i0"));
        assert!(!is_synthetic_local_name("email"));
        assert!(!is_synthetic_local_name("grantResults"));
        assert!(!is_synthetic_local_name("length"));
        assert!(is_signature_style_name("p0"));
        assert!(is_signature_style_name("s1"));
        assert!(is_signature_style_name("x2"));
        assert!(!is_signature_style_name("grantResults"));
    }

    #[test]
    fn format_array_length_aget_aput() {
        assert_eq!(super::format_array_length("v0, v1"), "v0 = v1.length;");
        assert_eq!(super::format_aget("v0, v1, v2"), "v0 = v1[v2];");
        assert_eq!(super::format_aput("v0, v1, v2"), "v1[v2] = v0;");
    }

    #[test]
    fn format_filled_new_array_expr_strings() {
        assert_eq!(
            super::format_filled_new_array_expr("v0, v1, java.lang.String[]"),
            Some("new java.lang.String[]{ v0, v1 }".into())
        );
        assert_eq!(
            super::format_filled_new_array_expr("\"a\", \"b\", String[]"),
            Some("new String[]{ \"a\", \"b\" }".into())
        );
    }

    #[test]
    fn format_filled_new_array_expr_expands_range() {
        assert_eq!(
            super::format_filled_new_array_expr("v0 ... v3, java.lang.String[]"),
            Some("new java.lang.String[]{ v0, v1, v2, v3 }".into())
        );
        assert_eq!(
            super::format_filled_new_array_expr("v10 .. v12, int[]"),
            Some("new int[]{ v10, v11, v12 }".into())
        );
        assert_eq!(
            super::expand_reg_range_ellipsis("v2 ... v2"),
            Some(vec!["v2".into()])
        );
    }

    #[test]
    fn format_iget_iput_sget_sput() {
        assert_eq!(
            super::format_iget("v2, v5, android.support.v4.widget.SimpleCursorAdapter.mCursor"),
            "v2 = v5.mCursor;"
        );
        assert_eq!(
            super::format_iput("v3, v1, pkg.Clz.fieldName"),
            "v1.fieldName = v3;"
        );
        assert_eq!(
            super::format_sget("v0, Foo.staticField"),
            "v0 = Foo.staticField;"
        );
        assert_eq!(super::format_sput("v1, Bar.other"), "Bar.other = v1;");
    }

    #[test]
    fn parse_const_into_reg_valid() {
        assert_eq!(parse_const_into_reg("v0, 42"), Some("v0 = 42;".into()));
        assert_eq!(parse_const_into_reg("v1, -1"), Some("v1 = -1;".into()));
    }

    #[test]
    fn parse_const_into_reg_invalid() {
        assert_eq!(parse_const_into_reg("v0"), None);
    }

    #[test]
    fn format_java_wide_literal_double_and_long() {
        assert_eq!(
            format_java_wide_literal("4614256656552045848", "double"),
            "3.141592653589793"
        );
        assert_eq!(
            format_java_wide_literal("1311768467294899695", "long"),
            "0x1234567890ABCDEFL"
        );
    }

    #[test]
    fn format_const_wide_literal_as_double() {
        assert_eq!(
            format_java_wide_literal("4607182418800017408", "double"),
            "1.0"
        );
        assert_eq!(
            format_java_wide_literal("4611686018427387904", "double"),
            "2.0"
        );
        assert_eq!(
            format_java_wide_literal("-4604930618986332160", "double"),
            "-6.0"
        );
        assert_eq!(
            format_java_wide_literal("-4616189618054758400", "double"),
            "-1.0"
        );
    }

    #[test]
    fn format_const_float_bits() {
        assert_eq!(super::format_java_float(f32::from_bits(1082549862)), "4.2f");
        assert_eq!(super::parse_const_bits_u32("1082549862"), Some(1082549862));
    }

    #[test]
    fn parse_string_ref_valid() {
        assert_eq!(
            parse_string_ref("v0, string@5"),
            Some("v0 = string@5;".into())
        );
        assert_eq!(
            parse_string_ref("v1, \"hello\""),
            Some("v1 = \"hello\";".into())
        );
    }

    #[test]
    fn parse_string_literal_keeps_internal_comma() {
        assert_eq!(
            parse_string_ref("v0, \" test_base(500, 3) \""),
            Some("v0 = \" test_base(500, 3) \";".into())
        );
        assert_eq!(
            parse_string_ref("v1, \",\""),
            Some("v1 = \",\";".into())
        );
        assert_eq!(
            super::parse_assign_rhs("const-string", "v2, \" test_base(500, 3) \""),
            Some((2, "\" test_base(500, 3) \"".into()))
        );
        assert_eq!(
            super::parse_assign_rhs("const-string/jumbo", "v3, \",\""),
            Some((3, "\",\"".into()))
        );
    }

    #[test]
    fn parse_new_instance_valid() {
        assert_eq!(
            parse_new_instance("v0, java.lang.Object"),
            Some("v0 = new java.lang.Object();".into())
        );
        assert_eq!(
            parse_new_instance("v1, type@2"),
            Some("v1 = new type@2();".into())
        );
    }

    #[test]
    fn format_new_array_valid() {
        assert_eq!(
            format_new_array("v0, v0, boolean[]"),
            Some("v0 = new boolean[v0];".into())
        );
        assert_eq!(
            format_new_array("v1, v2, int[]"),
            Some("v1 = new int[v2];".into())
        );
    }

    #[test]
    fn escape_java_string_plain() {
        assert_eq!(escape_java_string("hello"), "hello");
    }

    #[test]
    fn escape_java_string_quotes_and_backslash() {
        assert_eq!(escape_java_string("a\"b"), "a\\\"b");
        assert_eq!(escape_java_string("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_java_string_control_chars() {
        assert_eq!(escape_java_string("a\nb"), "a\\nb");
        assert_eq!(escape_java_string("a\rb"), "a\\rb");
        assert_eq!(escape_java_string("a\tb"), "a\\tb");
    }

    #[test]
    fn format_condition_binary() {
        assert_eq!(format_condition("if-eq", "v0, v1"), "v0 == v1");
        assert_eq!(format_condition("if-ne", "v0, v1"), "v0 != v1");
        assert_eq!(format_condition("if-lt", "v0, v1"), "v0 < v1");
        assert_eq!(format_condition("if-ge", "v0, v1"), "v0 >= v1");
        assert_eq!(format_condition("if-gt", "v0, v1"), "v0 > v1");
        assert_eq!(format_condition("if-le", "v0, v1"), "v0 <= v1");
    }

    #[test]
    fn format_condition_unary() {
        assert_eq!(format_condition("if-eqz", "v0"), "v0 == 0");
        assert_eq!(format_condition("if-nez", "v0"), "v0 != 0");
        assert_eq!(format_condition("if-ltz", "v0"), "v0 < 0");
        assert_eq!(format_condition("if-gez", "v0"), "v0 >= 0");
        assert_eq!(format_condition("if-gtz", "v0"), "v0 > 0");
        assert_eq!(format_condition("if-lez", "v0"), "v0 <= 0");
    }

    #[test]
    fn format_condition_ignores_branch_offset() {
        assert_eq!(format_condition("if-eqz", "v0, +008h"), "v0 == 0");
        assert_eq!(format_condition("if-eq", "v0, v1, +00ch"), "v0 == v1");
    }

    #[test]
    fn to_super_style_drops_receiver() {
        assert_eq!(
            to_super_style("android.app.Activity.onCreate", "this, bundle"),
            ("super.onCreate".into(), "bundle".into())
        );
        assert_eq!(
            to_super_style("pkg.Clz.foo", "this"),
            ("super.foo".into(), String::new())
        );
    }

    #[test]
    fn polish_boolean_condition_z_temp() {
        let names = HashMap::new();
        let types = HashMap::new();
        assert_eq!(polish_boolean_condition("z0 == 0", &names, &types), "!z0");
        assert_eq!(polish_boolean_condition("z0 != 0", &names, &types), "z0");
        // Non-boolean temps stay as comparisons
        assert_eq!(
            polish_boolean_condition("i0 == 0", &names, &types),
            "i0 == 0"
        );
        assert_eq!(
            polish_boolean_condition("view0 == 0", &names, &types),
            "view0 == null"
        );
    }

    #[test]
    fn polish_boolean_condition_typed_local() {
        let mut names = HashMap::new();
        names.insert(0u32, "loggedIn".into());
        let mut types = HashMap::new();
        types.insert(0u32, "boolean".into());
        assert_eq!(
            polish_boolean_condition("loggedIn == 0", &names, &types),
            "!loggedIn"
        );
    }

    #[test]
    fn class_name_to_path_default_package() {
        let (dir, file) = super::class_name_to_path("Test");
        assert_eq!(dir, std::path::PathBuf::new());
        assert_eq!(file, "Test.java");
    }

    #[test]
    fn class_name_to_path_with_package() {
        let (dir, file) = super::class_name_to_path("com.example.MyClass");
        assert_eq!(dir, std::path::PathBuf::from("com/example"));
        assert_eq!(file, "MyClass.java");
    }

    #[test]
    fn class_name_to_path_inner_class() {
        let (dir, file) = super::class_name_to_path("com.example.Outer$Inner");
        assert_eq!(dir, std::path::PathBuf::from("com/example"));
        assert_eq!(file, "Outer$Inner.java");
    }

    #[test]
    fn class_name_to_path_rejects_traversal_into_unsafe() {
        let (dir, file) = super::class_name_to_path("com../../tmp.Evil");
        assert_eq!(dir, std::path::PathBuf::from("_unsafe"));
        assert!(file.ends_with(".java"));
        let (dir2, _) = super::class_name_to_path("../../tmp/evil");
        assert_eq!(dir2, std::path::PathBuf::from("_unsafe"));
        let base = std::path::Path::new("/tmp/out");
        let path = super::resolve_under_base(base, &dir, &file).unwrap();
        assert!(path.starts_with(base));
    }

    #[test]
    fn enum_detection_super_not_enum_returns_empty() {
        let r = super::enum_constants_from_static_fields("test.Color", "Object", &[]);
        assert!(r.is_empty());
    }

    #[test]
    fn enum_detection_extends_enum_static_final_self_type_returns_constants() {
        let static_fields = vec![
            ("test.Color".to_string(), "RED".to_string(), 0x18u32),
            ("test.Color".to_string(), "GREEN".to_string(), 0x18u32),
            ("int".to_string(), "other".to_string(), 0x18u32),
        ];
        let r = super::enum_constants_from_static_fields("test.Color", "Enum", &static_fields);
        assert_eq!(r, ["RED", "GREEN"]);
    }

    #[test]
    fn enum_detection_non_static_final_ignored() {
        let static_fields = vec![("test.Color".to_string(), "RED".to_string(), 0x8u32)];
        let r = super::enum_constants_from_static_fields("test.Color", "Enum", &static_fields);
        assert!(r.is_empty());
    }

    #[test]
    fn parse_invoke_expands_range_ellipsis() {
        let (target, args, params) = super::parse_invoke_call_parts(
            "v21 ... v22, java.io.PrintStream.println(java.lang.String)",
        )
        .unwrap();
        assert_eq!(target, "java.io.PrintStream.println");
        assert_eq!(args, "v21, v22");
        assert_eq!(params, vec!["java.lang.String"]);
        let (recv_target, recv_args) = super::to_receiver_style(&target, &args);
        assert_eq!(recv_target, "v21.println");
        assert_eq!(recv_args, "v22");
    }

    #[test]
    fn parse_invoke_expands_single_reg_range() {
        let (target, args, _) =
            super::parse_invoke_call_parts("v24, tests.androguard.TestActivity.pouet2()")
                .unwrap();
        assert_eq!(target, "tests.androguard.TestActivity.pouet2");
        assert_eq!(args, "v24");
        let (recv_target, recv_args) = super::to_receiver_style(&target, &args);
        assert_eq!(recv_target, "v24.pouet2");
        assert_eq!(recv_args, "");
    }

    #[test]
    fn parse_invoke_keeps_param_types() {
        let (target, args, params) =
            super::parse_invoke_call_parts("v1, v3, v4, java.lang.StringBuilder.append(long)")
                .unwrap();
        assert_eq!(target, "java.lang.StringBuilder.append");
        assert_eq!(args, "v1, v3, v4");
        assert_eq!(params, vec!["long"]);
    }

    #[test]
    fn drop_wide_high_half_for_append_long() {
        let (_, args, params) =
            super::parse_invoke_call_parts("v1, v3, v4, java.lang.StringBuilder.append(long)")
                .unwrap();
        let (target, args) = super::to_receiver_style("java.lang.StringBuilder.append", &args);
        assert_eq!(target, "v1.append");
        assert_eq!(super::drop_wide_high_half_args(&args, &params), "v3");
    }

    #[test]
    fn drop_wide_high_half_mixed_params() {
        let args = super::drop_wide_high_half_args(
            "v0, v1, v2, v3",
            &["int".into(), "long".into(), "java.lang.String".into()],
        );
        // v0=int, v1+v2=long (drop v2), v3=String
        assert_eq!(args, "v0, v1, v3");
    }

    #[test]
    fn drop_wide_leaves_non_reg_second_arg() {
        // Don't drop a real expression that happens to follow a long-typed slot mismatch.
        let args =
            super::drop_wide_high_half_args("System.currentTimeMillis(), \"x\"", &["long".into()]);
        assert_eq!(args, "System.currentTimeMillis(), \"x\"");
    }

    #[test]
    fn debug_param_names_prefer_table_then_start_local() {
        let types = vec!["int".into(), "java.lang.String[]".into(), "int[]".into()];
        let mut locals = HashMap::new();
        // instance: slot 0 = this, 1 = requestCode, 2 = permissions, 3 = grantResults
        locals.insert(4, "grantResults".to_string());
        let unnamed = vec![None, None, None];
        let names = debug_param_names_from_tables(&unnamed, &locals, 1, false, &types);
        assert_eq!(names[0], None);
        assert_eq!(names[1], None);
        assert_eq!(names[2], Some("grantResults".into()));

        let table = vec![
            Some("requestCode".into()),
            Some("permissions".into()),
            Some("grantResults".into()),
        ];
        let names = debug_param_names_from_tables(&table, &HashMap::new(), 1, false, &types);
        assert_eq!(
            names,
            vec![
                Some("requestCode".into()),
                Some("permissions".into()),
                Some("grantResults".into())
            ]
        );

        // Table wins over a later START_LOCAL on the same register.
        locals.insert(2, "length".to_string());
        let names = debug_param_names_from_tables(&table, &locals, 1, false, &types);
        assert_eq!(names[0], Some("requestCode".into()));
    }

    #[test]
    fn debug_param_names_skip_this_and_wide_slots() {
        let types = vec!["long".into(), "java.lang.String".into()];
        let mut locals = HashMap::new();
        locals.insert(0, "this".to_string());
        locals.insert(1, "count".to_string());
        locals.insert(3, "name".to_string());
        let names = debug_param_names_from_tables(&[None, None], &locals, 0, false, &types);
        assert_eq!(names[0], Some("count".into()));
        assert_eq!(names[1], Some("name".into()));
    }

    fn fold_cfg(
        offsets: Vec<u32>,
        condition: &str,
        loop_header: bool,
        instructions: Vec<Instruction>,
    ) -> MethodCfg {
        let mut loop_headers = HashSet::new();
        if loop_header {
            loop_headers.insert(0);
        }
        let mut cfg = MethodCfg {
            blocks: vec![cfg::CfgBlock {
                start_offset: 0,
                end_offset: offsets.last().copied().unwrap_or(0) + 4,
                end: BlockEnd::Conditional {
                    condition: condition.to_string(),
                    branch_target: 0,
                    fall_through: 0,
                },
                instruction_offsets: offsets,
            }],
            block_by_start: HashMap::new(),
            loop_headers,
            entry: 0,
            folded_const_offsets: HashSet::new(),
        };
        Decompiler::fold_constants_into_conditions(&mut cfg, &instructions, None);
        cfg
    }

    /// `instance-of v0, v1, Number; if-eqz v0` → `v1 instanceof Number == 0`.
    #[test]
    fn fold_instance_of_into_if_eqz() {
        let ins = vec![
            Instruction::new(0, 4, 0x20, "instance-of", "v0, v1, java.lang.Number".into()),
            Instruction::new(4, 4, 0x38, "if-eqz", "v0, +008h".into()),
        ];
        let cfg = fold_cfg(vec![0, 4], "v0 == 0", false, ins);
        assert_eq!(cond_of(&cfg), "v1 instanceof java.lang.Number == 0");
        assert!(cfg.folded_const_offsets.contains(&0));
    }

    fn cond_of(cfg: &MethodCfg) -> &str {
        match &cfg.blocks[0].end {
            BlockEnd::Conditional { condition, .. } => condition.as_str(),
            other => panic!("expected conditional, got {other:?}"),
        }
    }

    /// `const/16 v0, 1002; if-ne v4, v0` → `v4 != 1002` (OVAA PERMISSIONS_CODE).
    #[test]
    fn fold_const16_permissions_code_into_if() {
        let ins = vec![
            Instruction::new(0, 4, 0x13, "const/16", "v0, 1002".into()),
            Instruction::new(4, 4, 0x33, "if-ne", "v4, v0".into()),
        ];
        let cfg = fold_cfg(vec![0, 4], "v4 != v0", false, ins);
        assert_eq!(cond_of(&cfg), "v4 != 1002");
        assert!(cfg.folded_const_offsets.contains(&0));
    }

    /// D8 copies `requestCode` into v4 before the compare; fold the move so v4 is not left undefined.
    #[test]
    fn fold_move_into_if_ne() {
        let ins = vec![
            Instruction::new(0, 2, 0x01, "move", "v4, v5".into()),
            Instruction::new(2, 4, 0x13, "const/16", "v0, 1002".into()),
            Instruction::new(6, 4, 0x33, "if-ne", "v4, v0".into()),
        ];
        let cfg = fold_cfg(vec![0, 2, 6], "v4 != v0", false, ins);
        assert_eq!(cond_of(&cfg), "v5 != 1002");
        assert!(cfg.folded_const_offsets.contains(&0));
        assert!(cfg.folded_const_offsets.contains(&2));
    }

    /// Loop header `const/4 v0, 0; if-lt v0, v2` must not become `0 < v2` when v0 is incremented.
    #[test]
    fn fold_skips_loop_index_const_on_header() {
        let ins = vec![
            Instruction::new(0, 2, 0x12, "const/4", "v0, 0".into()),
            Instruction::new(2, 4, 0x34, "if-lt", "v0, v2".into()),
            Instruction::new(8, 4, 0xd8, "add-int/lit8", "v0, v0, 1".into()),
        ];
        let cfg = fold_cfg(vec![0, 2], "v0 < v2", true, ins);
        assert_eq!(cond_of(&cfg), "v0 < v2");
        assert!(cfg.folded_const_offsets.is_empty());
    }

    /// Same const on a non-loop if *is* folded (one-shot index).
    #[test]
    fn fold_zero_into_non_loop_if() {
        let ins = vec![
            Instruction::new(0, 2, 0x12, "const/4", "v0, 0".into()),
            Instruction::new(2, 4, 0x34, "if-lt", "v0, v2".into()),
        ];
        let cfg = fold_cfg(vec![0, 2], "v0 < v2", false, ins);
        assert_eq!(cond_of(&cfg), "0 < v2");
        assert!(cfg.folded_const_offsets.contains(&0));
    }

    /// Loop bound `const/16 v2, 100` must fold even on a loop header — v2 is not the
    /// mutated index (`v1++`), so `while (i < i2)` with undefined i2 must not happen.




    /// Loop bound `const/16 v2, 100` must fold even on a loop header — v2 is not the
    /// mutated index (`v1++`), so `while (i < i2)` with undefined i2 must not happen.
    #[test]
    fn fold_loop_bound_const16_into_header_condition() {
        let ins = vec![
            Instruction::new(0, 2, 0x12, "const/4", "v0, 0".into()),
            Instruction::new(2, 2, 0x12, "const/4", "v1, 0".into()),
            Instruction::new(4, 4, 0x13, "const/16", "v2, 100".into()),
            Instruction::new(8, 4, 0x35, "if-ge", "v1, v2".into()),
            Instruction::new(12, 4, 0xd8, "add-int/lit8", "v1, v1, 1".into()),
        ];
        let cfg = fold_cfg(vec![4, 8], "v1 >= v2", true, ins);
        assert_eq!(
            cond_of(&cfg),
            "v1 >= 100",
            "bound const must fold into loop exit condition"
        );
        assert!(
            cfg.folded_const_offsets.contains(&4),
            "const/16 must be marked folded so it is not emitted as i2"
        );
    }

}