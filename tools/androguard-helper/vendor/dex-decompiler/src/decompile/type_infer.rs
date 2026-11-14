//! Type inference for method IR: seed from params and return, propagate from Assign(Var), Assign(Call), and Raw literals.

use crate::decompile::ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
use crate::java;
use dex_parser::{CodeItem, DexFile, EncodedMethod};
use std::collections::HashMap;

/// Well-known JDK / Android API return types when the method isn't resolved from DEX ids.
fn known_api_return_type(target: &str) -> Option<&'static str> {
    let method = target.rsplit('.').next().unwrap_or(target);
    match method {
        "equals" | "equalsIgnoreCase" | "endsWith" | "startsWith" | "isEmpty" | "contains"
        | "contentEquals" | "matches" | "regionMatches" | "isBlank" => Some("boolean"),
        "length" | "indexOf" | "lastIndexOf" | "hashCode" | "compareTo" | "compareToIgnoreCase" => {
            Some("int")
        }
        "trim" | "toString" | "toLowerCase" | "toUpperCase" | "intern" | "concat" | "replace"
        | "replaceAll" | "replaceFirst" | "substring" | "getScheme" | "getHost" | "getPath"
        | "getQuery" | "getQueryParameter" | "getFragment" | "getAction" | "getDataString" => {
            Some("java.lang.String")
        }
        "getData" | "parse" => {
            // Uri.parse / Intent.getData — heuristic by name only when FQN contains Uri/Intent.
            if target.contains("Uri") {
                Some("android.net.Uri")
            } else if method == "getData" {
                Some("android.net.Uri")
            } else {
                None
            }
        }
        "charAt" => Some("char"),
        "toCharArray" => Some("char[]"),
        "getBytes" => Some("byte[]"),
        "split" => Some("java.lang.String[]"),
        "valueOf" | "copyValueOf" => {
            // String.valueOf vs Integer/Long/… boxing — disambiguate by receiver type.
            let t = target;
            if t.contains("Integer") {
                Some("java.lang.Integer")
            } else if t.contains("Long") {
                Some("java.lang.Long")
            } else if t.contains("Boolean") {
                Some("java.lang.Boolean")
            } else if t.contains("Double") {
                Some("java.lang.Double")
            } else if t.contains("Float") {
                Some("java.lang.Float")
            } else if t.contains("Character") {
                Some("java.lang.Character")
            } else if t.contains("Short") {
                Some("java.lang.Short")
            } else if t.contains("Byte") {
                Some("java.lang.Byte")
            } else {
                Some("java.lang.String")
            }
        }
        _ => None,
    }
}

/// Infer Java type names for SSA variables from method signature and IR.
/// - Seeds parameter registers at the **high** end of the frame
///   (`registers_size - ins_size .. registers_size`) from method params (and `this` for instance methods).
/// - Seeds the return variable with the method return type (if not void).
/// - Propagates: Assign(dst, Var(src)), Assign(dst, Call), Assign(dst, Raw(s)) with literal or first var's type.
pub fn infer_types(
    dex: &DexFile,
    encoded: &EncodedMethod,
    code: &CodeItem,
    stmts: &[IrStmt],
) -> HashMap<VarId, String> {
    let field_types = FieldTypeIndex::build(dex);
    let return_index = MethodReturnIndex::build(dex);
    infer_types_cached(dex, encoded, code, stmts, &field_types, &return_index)
}

/// Same as [`infer_types`], but reuses prebuilt field / method-return indexes
/// (avoids O(fields + methods) rebuild on every CFG block).
pub(crate) fn infer_types_cached(
    dex: &DexFile,
    encoded: &EncodedMethod,
    code: &CodeItem,
    stmts: &[IrStmt],
    field_types: &FieldTypeIndex,
    return_index: &MethodReturnIndex,
) -> HashMap<VarId, String> {
    let mut types: HashMap<VarId, String> = HashMap::new();
    let info = match dex.get_method_info(encoded.method_idx) {
        Ok(i) => i,
        Err(_) => return types,
    };
    let find_method_return_type_java = |target: &str, num_args: usize| -> Option<String> {
        return_index.lookup(target, num_args)
    };
    let ins_size = code.ins_size as u32;
    let registers_size = code.registers_size as u32;
    let is_static = (encoded.access_flags & 0x8) != 0;
    let class_java = java::descriptor_to_java(&info.class);
    let param_types: Vec<String> = info
        .params
        .iter()
        .map(|p| java::descriptor_to_java(p))
        .collect();
    let return_type_java = java::descriptor_to_java(&info.return_type);

    // Seed param registers (version 0) at the high end of the Dalvik frame.
    let param_base = registers_size.saturating_sub(ins_size);
    for i in 0..ins_size {
        let reg = param_base + i;
        let vid = VarId::new(reg, 0);
        if !is_static && i == 0 {
            types.insert(vid, class_java.clone());
        } else {
            let param_idx = if is_static {
                i as usize
            } else {
                (i as usize).saturating_sub(1)
            };
            if param_idx < param_types.len() {
                types.insert(vid, param_types[param_idx].clone());
            }
        }
    }

    // Seed return variable with method return type (so e.g. "result" gets "int").
    if return_type_java != "void" {
        if let Some(return_var) = stmts.iter().find_map(|s| {
            if let IrStmt::Return {
                value: Some(IrExpr::Var(v)),
                ..
            } = s
            {
                Some(*v)
            } else {
                None
            }
        }) {
            types.insert(return_var, return_type_java.clone());
        }
        if return_type_java == "long" || return_type_java == "double" {
            if let Some(return_reg) = stmts.iter().find_map(|s| {
                if let IrStmt::Return {
                    value: Some(IrExpr::Var(v)),
                    ..
                } = s
                {
                    Some(v.reg)
                } else {
                    None
                }
            }) {
                for stmt in stmts {
                    if let IrStmt::Assign {
                        dst,
                        rhs: IrExpr::Raw(s),
                        ..
                    } = stmt
                    {
                        if dst.reg == return_reg && looks_like_wide_bits_literal(s) {
                            types.insert(*dst, return_type_java.clone());
                        }
                    }
                }
            }
        }
    }

    // Propagate: Assign(dst, rhs) from Var, Call, or Raw (literal / first var in text). Fixpoint.
    let max_iters = stmts.len().saturating_mul(2).max(4);
    for _ in 0..max_iters {
        let mut changed = false;
        for stmt in stmts {
            if let IrStmt::Assign { dst, rhs, .. } = stmt {
                if types.contains_key(dst) {
                    continue;
                }
                let ty = match rhs {
                    IrExpr::Var(v) => types.get(v).cloned(),
                    IrExpr::Call { target, args } => {
                        let n = count_args(args);
                        find_method_return_type_java(target, n).or_else(|| {
                            // receiver-style: "v0.method" -> reconstruct "Class.method" from receiver type
                            if let Some(dot) = target.rfind('.') {
                                let receiver_part = &target[..dot];
                                if let Some(vid) = parse_var_id(receiver_part) {
                                    if let Some(class) = types.get(&vid) {
                                        let method = &target[dot + 1..];
                                        let fq = format!("{}.{}", class, method);
                                        return find_method_return_type_java(&fq, n);
                                    }
                                }
                            }
                            None
                        })
                    }
                    IrExpr::Raw(s) => infer_typed_raw(dex, field_types, s, &types),
                    IrExpr::PendingResult => None,
                };
                if let Some(t) = ty {
                    types.insert(*dst, t);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    types
}

/// Fast lookup of invoke return types: `Class.method` → [(arity, java_type), …].
pub(crate) struct MethodReturnIndex {
    by_key: HashMap<String, Vec<(usize, String)>>,
}

impl MethodReturnIndex {
    pub(crate) fn build(dex: &DexFile) -> Self {
        let mut by_key: HashMap<String, Vec<(usize, String)>> = HashMap::new();
        let n = dex.header.method_ids_size;
        for idx in 0..n {
            let Ok(mi) = dex.get_method_info(idx) else {
                continue;
            };
            let key = format!("{}.{}", java::descriptor_to_java(&mi.class), mi.name);
            let ret = java::descriptor_to_java(&mi.return_type);
            by_key
                .entry(key)
                .or_default()
                .push((mi.params.len(), ret));
        }
        Self { by_key }
    }

    fn lookup(&self, target: &str, num_args: usize) -> Option<String> {
        if let Some(t) = known_api_return_type(target) {
            return Some(t.to_string());
        }
        if let Some(entries) = self.by_key.get(target) {
            if let Some((_, ty)) = entries.iter().find(|(n, _)| *n == num_args) {
                return Some(ty.clone());
            }
        }
        // Receiver-style targets (`v0.equals`, `java.lang.String.endsWith`).
        if let Some(dot) = target.rfind('.') {
            let method = &target[dot + 1..];
            if let Some(t) = known_api_return_type(method) {
                return Some(t.to_string());
            }
            if let Some(t) = known_api_return_type(&format!("java.lang.String.{method}")) {
                return Some(t.to_string());
            }
            let fq = format!("java.lang.String.{method}");
            if let Some(entries) = self.by_key.get(&fq) {
                if let Some((_, ty)) = entries.iter().find(|(n, _)| *n == num_args) {
                    return Some(ty.clone());
                }
            }
        }
        None
    }
}

fn looks_like_wide_bits_literal(s: &str) -> bool {
    let s = s.trim();
    let s = s
        .strip_suffix('L')
        .or_else(|| s.strip_suffix('l'))
        .unwrap_or(s);
    let s = s.strip_prefix('-').unwrap_or(s);
    s.chars().all(|c| c.is_ascii_digit()) && s.len() > 10
}

fn raw_looks_like_wide_or_float_literal(s: &str) -> bool {
    let s = s.trim();
    if s.ends_with('f') || s.ends_with('F') {
        return true;
    }
    if s.contains('.') && s.chars().all(|c| c.is_ascii_digit() || ".-+eEfF".contains(c)) {
        return true;
    }
    looks_like_wide_bits_literal(s) || (s.starts_with("0x") || s.starts_with("0X")) && s.ends_with('L')
}

/// Fill missing SSA types from a whole-method register→type map, then re-propagate
/// Assign RHS types (so `aget` can pick up array element types across blocks).
pub fn enrich_types_with_register_map(
    dex: &DexFile,
    type_map: &mut HashMap<VarId, String>,
    reg_types: &HashMap<u32, String>,
    stmts: &[IrStmt],
) {
    enrich_types_with_register_map_and_debug(dex, type_map, reg_types, None, stmts);
}

pub fn enrich_types_with_register_map_and_debug(
    dex: &DexFile,
    type_map: &mut HashMap<VarId, String>,
    reg_types: &HashMap<u32, String>,
    debug_types: Option<&HashMap<u32, String>>,
    stmts: &[IrStmt],
) {
    let field_types = FieldTypeIndex::build(dex);
    enrich_types_with_field_index(
        dex,
        type_map,
        reg_types,
        debug_types,
        stmts,
        &field_types,
    );
}

/// Same as [`enrich_types_with_register_map_and_debug`], but reuses a cached field index
/// (avoids rebuilding owner→field→type for every CFG block).
pub(crate) fn enrich_types_with_field_index(
    dex: &DexFile,
    type_map: &mut HashMap<VarId, String>,
    reg_types: &HashMap<u32, String>,
    debug_types: Option<&HashMap<u32, String>>,
    stmts: &[IrStmt],
    field_types: &FieldTypeIndex,
) {
    for stmt in stmts {
        if let IrStmt::Assign { dst, rhs, .. } = stmt {
            if let Some(dbg) = debug_types.and_then(|m| m.get(&dst.reg)) {
                match rhs {
                    IrExpr::Raw(r)
                        if raw_looks_like_wide_or_float_literal(r)
                            || r.parse::<i64>().is_ok()
                            || r.starts_with("0x")
                            || r.starts_with("0X") =>
                    {
                        // Prefer the literal's own type when it conflicts with a later
                        // debug type on a reused register (`double -5.0` vs `float gettype`).
                        if let Some(lit_ty) = infer_type_from_raw(r, type_map) {
                            if types_compatible_for_naming(Some(lit_ty.as_str()), Some(dbg)) {
                                type_map.insert(*dst, dbg.clone());
                            } else {
                                type_map.insert(*dst, lit_ty);
                            }
                        } else if !type_map.contains_key(dst) {
                            type_map.insert(*dst, dbg.clone());
                        }
                        continue;
                    }
                    _ if !type_map.contains_key(dst) => {
                        type_map.insert(*dst, dbg.clone());
                        continue;
                    }
                    _ => {}
                }
            }
            if !type_map.contains_key(dst) {
                if let IrExpr::Raw(r) = rhs {
                    if let Some(ty) = infer_typed_raw(dex, field_types, r, type_map) {
                        type_map.insert(*dst, ty);
                    }
                }
            }
        }
        for v in vars_in_stmt(stmt) {
            if type_map.contains_key(&v) {
                continue;
            }
            if let Some(ty) = reg_types.get(&v.reg) {
                // Do not paint a wide numeric const with a later reused register's
                // float/reference type (e.g. `float g = -6.0`).
                if let IrStmt::Assign {
                    dst,
                    rhs: IrExpr::Raw(raw),
                    ..
                } = stmt
                {
                    if *dst == v && raw_looks_like_wide_or_float_literal(raw) {
                        if let Some(lit_ty) = infer_type_from_raw(raw, type_map) {
                            if !types_compatible_for_naming(Some(lit_ty.as_str()), Some(ty)) {
                                type_map.insert(v, lit_ty);
                                continue;
                            }
                        }
                    }
                }
                type_map.insert(v, ty.clone());
            }
        }
    }
    let max_iters = stmts.len().saturating_mul(2).max(4);
    for _ in 0..max_iters {
        let mut changed = false;
        for stmt in stmts {
            if let IrStmt::Assign { dst, rhs, .. } = stmt {
                if type_map.contains_key(dst) {
                    continue;
                }
                let ty = match rhs {
                    IrExpr::Var(v) => type_map.get(v).cloned(),
                    IrExpr::Raw(s) => infer_typed_raw(dex, field_types, s, type_map),
                    IrExpr::Call { .. } | IrExpr::PendingResult => None,
                };
                if let Some(t) = ty {
                    type_map.insert(*dst, t);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn vars_in_stmt(stmt: &IrStmt) -> Vec<VarId> {
    match stmt {
        IrStmt::Assign { dst, rhs, .. } => {
            let mut v = var_ids_in_expr(rhs);
            v.push(*dst);
            v
        }
        IrStmt::Expr { expr, .. } => var_ids_in_expr(expr),
        IrStmt::Return { value: Some(e), .. } => var_ids_in_expr(e),
        IrStmt::Return { value: None, .. } => vec![],
        IrStmt::Phi { dst, incomings } => {
            let mut v: Vec<VarId> = incomings.iter().map(|(_, x)| *x).collect();
            v.push(*dst);
            v
        }
        IrStmt::Raw(text) => var_ids_in_text(text),
    }
}

/// owner Java name → field name → Java type.
pub(crate) struct FieldTypeIndex {
    by_owner: HashMap<String, HashMap<String, String>>,
}

impl FieldTypeIndex {
    pub(crate) fn build(dex: &DexFile) -> Self {
        let mut by_owner: HashMap<String, HashMap<String, String>> = HashMap::new();
        let n = dex.header.field_ids_size;
        for idx in 0..n {
            let Ok(fi) = dex.get_field_info(idx) else {
                continue;
            };
            let owner = java::descriptor_to_java(&fi.class);
            let ty = java::descriptor_to_java(&fi.typ);
            by_owner.entry(owner).or_default().insert(fi.name, ty);
        }
        Self { by_owner }
    }

    fn lookup(&self, dex: &DexFile, owner: &str, field: &str) -> Option<String> {
        let mut current = owner.to_string();
        for _ in 0..24 {
            if let Some(ty) = self
                .by_owner
                .get(&current)
                .and_then(|fields| fields.get(field))
            {
                return Some(ty.clone());
            }
            current = superclass_java(dex, &current)?;
        }
        None
    }
}

fn superclass_java(dex: &DexFile, class_java: &str) -> Option<String> {
    if class_java.is_empty() || is_primitive_java_type(class_java) {
        return None;
    }
    let want = format!("L{};", class_java.replace('.', "/"));
    for def in dex.class_defs().flatten() {
        let Ok(desc) = dex.get_type(def.class_idx) else {
            continue;
        };
        if desc != want {
            continue;
        }
        if def.superclass_idx == dex_parser::NO_INDEX {
            return None;
        }
        let Ok(super_desc) = dex.get_type(def.superclass_idx) else {
            return None;
        };
        let super_java = java::descriptor_to_java(&super_desc);
        if super_java.is_empty() || super_java == class_java {
            return None;
        }
        return Some(super_java);
    }
    None
}

fn is_reg_token(s: &str) -> bool {
    let Some(vid) = parse_var_id(s) else {
        return false;
    };
    let expect = if vid.ver == 0 {
        format!("v{}", vid.reg)
    } else {
        format!("v{}_{}", vid.reg, vid.ver)
    };
    s == expect
}

/// `vN.field` / `pkg.Class.field` → the field's declared type, not the receiver register.
fn infer_field_load_type(
    dex: &DexFile,
    fields: &FieldTypeIndex,
    s: &str,
    types: &HashMap<VarId, String>,
) -> Option<String> {
    let s = s.trim();
    if s.is_empty()
        || s.contains('(')
        || s.contains(' ')
        || s.contains('[')
        || s.contains('"')
        || s.ends_with(".length")
        || s.ends_with(".class")
    {
        return None;
    }
    let (recv, field) = s.rsplit_once('.')?;
    if field.is_empty()
        || !field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    {
        return None;
    }
    let owner = if is_reg_token(recv) {
        let vid = parse_var_id(recv)?;
        types
            .get(&vid)
            .cloned()
            .or_else(|| types.get(&VarId::new(vid.reg, 0)).cloned())?
    } else {
        // Reject numeric receivers so `1.0` / `4.2f` are not field loads.
        if !recv.chars().any(|c| c.is_ascii_alphabetic() || c == '_') {
            return None;
        }
        recv.to_string()
    };
    if is_primitive_java_type(&owner) || is_reg_token(&owner) {
        return None;
    }
    fields.lookup(dex, &owner, field)
}

fn infer_typed_raw(
    dex: &DexFile,
    fields: &FieldTypeIndex,
    s: &str,
    types: &HashMap<VarId, String>,
) -> Option<String> {
    if let Some(ty) = infer_field_load_type(dex, fields, s, types) {
        if !is_reg_token(&ty) {
            return Some(ty);
        }
    }
    infer_type_from_raw(s, types).filter(|ty| !is_reg_token(ty))
}

/// Infer type from Raw RHS: string literal, new-array "new Type[size]", new-instance, literal, or first variable reference's type.
fn infer_type_from_raw(s: &str, types: &HashMap<VarId, String>) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // instance-of: `v0 instanceof Type` → boolean (must not be treated as a reference).
    if s.contains(" instanceof ") {
        return Some("boolean".to_string());
    }
    if s.starts_with("new ") {
        // filled-new-array / array literal: "new Type[]{ ... }" → Type[]
        if let Some(brace) = s.find('{') {
            let head = s[4..brace].trim();
            if head.ends_with("[]") {
                return Some(head.to_string());
            }
        }
        // new-array: "new Type[size]" or "new boolean[v0]" -> array type
        if let Some(bracket) = s.find('[') {
            let element_and_brackets = s[4..bracket].trim();
            if !element_and_brackets.is_empty() {
                let ty = element_and_brackets.to_string();
                return Some(if ty.ends_with(']') {
                    ty
                } else {
                    format!("{}[]", ty)
                });
            }
        }
        // new-instance: "new Foo()" or "new Foo(args)" -> type is Foo
        if let Some(paren) = s.find('(') {
            let class = s[4..paren].trim();
            if !class.is_empty() {
                return Some(class.to_string());
            }
        }
    }
    // array-length: "v0.length" / "v0_1.length" → int (must run before falling through to the array's type)
    if let Some(dot) = s.find(".length") {
        let after = s[dot + ".length".len()..].trim_start();
        // Bare `.length` or `.length // comment` — not `.length()` / other members.
        if after.is_empty() || after.starts_with('/') || after.starts_with(';') {
            return Some("int".to_string());
        }
    }
    // Array load: "v0[v1]" / "v0_1[v2_0]" → element type of the array variable
    if let Some(bracket) = s.find('[') {
        if s[bracket..].contains(']') {
            if let Some(array_id) = var_immediately_before(s, bracket) {
                if let Some(array_ty) = types.get(&array_id) {
                    if let Some(elem) = array_ty.strip_suffix("[]") {
                        return Some(elem.to_string());
                    }
                }
            }
        }
    }
    // check-cast: "(Type) expr" -> type is Type
    if s.starts_with('(') {
        if let Some(close) = s.find(") ") {
            let ty = s[1..close].trim();
            if !ty.is_empty()
                && (is_primitive_java_type(ty)
                    || ty
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false))
            {
                return Some(ty.to_string());
            }
        }
    }
    // Java string literal (starts and ends with ")
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Some("java.lang.String".to_string());
    }
    // const-class: "android.content.Context.class" / "Foo.class"
    if let Some(ty) = s.strip_suffix(".class") {
        let ty = ty.trim();
        if !ty.is_empty()
            && ty.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '_'
                    || c == '.'
                    || c == '$'
                    || c == '['
                    || c == ']'
            })
        {
            return Some("java.lang.Class".to_string());
        }
    }
    // Static field / enum constant: com.foo.Bar.BAZ → com.foo.Bar
    // Must not treat float/double literals (`1.0`, `4.2f`) as Class.field.
    if s.contains('.') && !s.contains('(') && !s.contains(' ') && !s.contains('[') {
        if let Some((type_part, field)) = s.rsplit_once('.') {
            let field_ok = !field.is_empty()
                && field
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                && field.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
            let type_ok = !type_part.is_empty()
                && !is_reg_token(type_part)
                && type_part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
                && type_part.chars().any(|c| c.is_ascii_alphabetic() || c == '_');
            if field_ok && type_ok {
                return Some(type_part.to_string());
            }
        }
    }
    // Integer literal (optional minus, digits, optional L)
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && b[i] == b'-' {
        i += 1;
    }
    if i < b.len() && b[i].is_ascii_digit() {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && (b[i] == b'L' || b[i] == b'l') {
            return Some("long".to_string());
        }
        if i == b.len() || b[i] == b' ' || b[i] == b';' {
            return Some("int".to_string());
        }
        if i < b.len() && b[i] == b'.' {
            // float/double
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'f' || b[i] == b'F') {
                return Some("float".to_string());
            }
            return Some("double".to_string());
        }
        if i < b.len() && (b[i] == b'f' || b[i] == b'F') {
            return Some("float".to_string());
        }
    }
    // First variable reference (vN or vN_k) in the string
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'v' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            i += 1;
            let mut reg: u32 = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                reg = reg * 10 + (b[i] - b'0') as u32;
                i += 1;
            }
            let ver = if i + 1 < b.len() && b[i] == b'_' && b[i + 1].is_ascii_digit() {
                i += 1;
                let mut v: u32 = 0;
                while i < b.len() && b[i].is_ascii_digit() {
                    v = v * 10 + (b[i] - b'0') as u32;
                    i += 1;
                }
                v
            } else {
                0
            };
            let vid = VarId::new(reg, ver);
            // `vN.field` is a field load; its type is the field's, not the receiver's.
            if i < b.len() && b[i] == b'.' {
                continue;
            }
            if let Some(t) = types.get(&vid) {
                if !is_reg_token(t) {
                    return Some(t.clone());
                }
            }
            continue;
        }
        i += 1;
    }
    // Integer literal or binary op with literal (e.g. "v3 + 66", "v1_1 & 26") -> assume int
    if s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| "+-*/%&|^<>".contains(c)) {
        return Some("int".to_string());
    }
    None
}

/// Semantic role for a variable (array base, index, length) to improve naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRole {
    /// Variable used as array in aget/aput (e.g. v1 in v1[v2]).
    Array,
    /// Variable used as index in aget/aput (e.g. v2 in v1[v2]). Prefer name "i" or "j".
    Index,
    /// Variable holding result of array-length. Prefer name "length" or "size".
    Length,
}

/// Detect semantic roles from Raw IR strings: "v0 = v1[v2];" -> v1=Array, v2=Index; "v0 = v1.length;" -> v1=Array, v0=Length.
fn collect_semantic_roles(stmts: &[IrStmt]) -> HashMap<VarId, SemanticRole> {
    let mut roles = HashMap::new();
    for s in stmts {
        let (assign_dst, raw) = match s {
            IrStmt::Assign {
                dst,
                rhs: IrExpr::Raw(r),
                ..
            } => (Some(*dst), r.as_str()),
            IrStmt::Raw(r) => (None, r.as_str()),
            _ => continue,
        };
        // "vN = new Type[vM]" → dst is Array, vM is Index (not filled-new-array `new Type[]{ ... }`)
        if let Some(dst) = assign_dst {
            if let Some(new_pos) = raw.find("new ") {
                if let Some(bracket) = raw[new_pos..].find('[') {
                    let abs_bracket = new_pos + bracket;
                    // Skip `new Type[]{ ... }` (filled-new-array); only size form has a non-empty index expr.
                    let after = raw[abs_bracket + 1..].trim_start();
                    if after.starts_with(']') {
                        // `new Type[]{ ... }` or `new Type[]`
                        roles.insert(dst, SemanticRole::Array);
                    } else if raw[abs_bracket..].contains(']') {
                        roles.insert(dst, SemanticRole::Array);
                        if let Some(index_id) = var_between(raw, abs_bracket + 1, raw.len()) {
                            roles.insert(index_id, SemanticRole::Index);
                        }
                    }
                }
            }
            // "vN = { ... }" / "vN = new int[]{ ... }" fill-array initializer → Array
            if raw.trim_start().starts_with('{') || raw.contains("[]{") {
                roles.insert(dst, SemanticRole::Array);
            }
        }
        // "vN = vM[vK];" or "vM[vK] = vN;" or "vN = vM.length;"
        if let Some(bracket) = raw.find('[') {
            if let Some(close) = raw.find(']') {
                if close > bracket {
                    if let Some(array_id) = var_immediately_before(raw, bracket) {
                        roles.insert(array_id, SemanticRole::Array);
                    }
                    if let Some(index_id) = var_between(raw, bracket + 1, close) {
                        roles.insert(index_id, SemanticRole::Index);
                    }
                }
            }
        }
        if raw.contains(".length") {
            if let Some(dot) = raw.find(".length") {
                if let Some(array_id) = var_immediately_before(raw, dot) {
                    roles.insert(array_id, SemanticRole::Array);
                }
                if let Some(dst) = assign_dst {
                    roles.insert(dst, SemanticRole::Length);
                }
            }
        }
    }
    roles
}

/// VarId that ends immediately before position `pos` (e.g. the var before '[' or '.').
fn var_immediately_before(s: &str, pos: usize) -> Option<VarId> {
    let before = s.get(..pos).unwrap_or("").trim_end();
    if before.is_empty() {
        return None;
    }
    let b = before.as_bytes();
    let mut end = before.len();
    while end > 0 && (b[end - 1].is_ascii_digit() || b[end - 1] == b'_') {
        end -= 1;
    }
    if end > 0 && b[end - 1] == b'v' {
        parse_var_id(&before[end - 1..])
    } else {
        None
    }
}

/// First VarId in s[start..end].
fn var_between(s: &str, start: usize, end: usize) -> Option<VarId> {
    let sub = s.get(start..end).unwrap_or("").trim();
    let mut i = 0;
    while i < sub.len() {
        if sub.as_bytes().get(i).map(|&b| b == b'v').unwrap_or(false) {
            if let Some(v) = var_id_at_start(&sub[i..]) {
                return Some(v);
            }
        }
        i += 1;
    }
    None
}

fn var_id_at_start(s: &str) -> Option<VarId> {
    parse_var_id(s)
}

fn parse_var_id(s: &str) -> Option<VarId> {
    let s = s.trim_start();
    if !s.starts_with('v') {
        return None;
    }
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 1 {
        return None;
    }
    let reg: u32 = s[1..i].parse().ok()?;
    let ver = if i < b.len() && b[i] == b'_' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        s[start..i].parse().ok().unwrap_or(0)
    } else {
        0
    };
    Some(VarId::new(reg, ver))
}

/// Build a display name for each variable: "result" for return value, "this"/"p0"/"p1" for params, semantic (array/index/length) or type-based.
#[cfg(test)]
fn build_var_names(
    stmts: &[IrStmt],
    type_map: &HashMap<VarId, String>,
    ins_size: u32,
    is_static: bool,
) -> HashMap<VarId, String> {
    build_var_names_with_regs(stmts, type_map, ins_size, ins_size, is_static)
}

pub fn build_var_names_with_regs(
    stmts: &[IrStmt],
    type_map: &HashMap<VarId, String>,
    registers_size: u32,
    ins_size: u32,
    is_static: bool,
) -> HashMap<VarId, String> {
    let return_var = stmts.iter().find_map(|s| {
        if let IrStmt::Return {
            value: Some(IrExpr::Var(v)),
            ..
        } = s
        {
            Some(*v)
        } else {
            None
        }
    });
    let roles = collect_semantic_roles(stmts);
    let mut names = HashMap::new();
    let mut counters: HashMap<&'static str, u32> = HashMap::new();
    let mut index_counter = 0u32;
    let param_base = registers_size.saturating_sub(ins_size);
    for s in stmts {
        let (def, uses) = match s {
            IrStmt::Assign { dst, rhs, .. } => {
                let uses = var_ids_in_expr(rhs);
                (Some(*dst), uses)
            }
            IrStmt::Expr { expr, .. } => (None, var_ids_in_expr(expr)),
            IrStmt::Return { value: Some(e), .. } => (None, var_ids_in_expr(e)),
            IrStmt::Return { value: None, .. } => (None, vec![]),
            IrStmt::Phi { dst, incomings } => {
                let uses: Vec<VarId> = incomings.iter().map(|(_, v)| *v).collect();
                (Some(*dst), uses)
            }
            IrStmt::Raw(text) => (None, var_ids_in_text(text)),
        };
        for v in &uses {
            if !names.contains_key(v) {
                names.insert(
                    *v,
                    name_for_var(
                        *v,
                        type_map,
                        return_var,
                        param_base,
                        ins_size,
                        is_static,
                        &roles,
                        &mut counters,
                        &mut index_counter,
                    ),
                );
            }
        }
        if let Some(d) = def {
            if !names.contains_key(&d) {
                names.insert(
                    d,
                    name_for_var(
                        d,
                        type_map,
                        return_var,
                        param_base,
                        ins_size,
                        is_static,
                        &roles,
                        &mut counters,
                        &mut index_counter,
                    ),
                );
            }
        }
    }
    unify_names_by_register(&mut names, type_map);
    names
}

/// Prefer one display name for SSA versions of the same Dalvik register **when types match**.
/// Keeps `arr0` shared across untyped/typed array uses, but does **not** collapse
/// Activity → View → TextView → String onto one debug name like `email`.
fn unify_names_by_register(names: &mut HashMap<VarId, String>, type_map: &HashMap<VarId, String>) {
    // Union-find over VarIds that share a register and compatible types.
    let vars: Vec<VarId> = names.keys().copied().collect();
    let mut parent: HashMap<VarId, VarId> = vars.iter().map(|v| (*v, *v)).collect();

    fn find(parent: &mut HashMap<VarId, VarId>, x: VarId) -> VarId {
        let mut root = x;
        while parent.get(&root).copied().unwrap_or(root) != root {
            root = parent[&root];
        }
        let mut cur = x;
        while cur != root {
            let next = parent[&cur];
            parent.insert(cur, root);
            cur = next;
        }
        root
    }
    fn union(parent: &mut HashMap<VarId, VarId>, a: VarId, b: VarId) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }

    for i in 0..vars.len() {
        for j in (i + 1)..vars.len() {
            let a = vars[i];
            let b = vars[j];
            if a.reg != b.reg {
                continue;
            }
            let ta = type_map.get(&a).map(|s| s.as_str());
            let tb = type_map.get(&b).map(|s| s.as_str());
            if !types_compatible_for_naming(ta, tb) {
                continue;
            }
            // `const/16 v0, 1002` then `array-length v0, arr` share a register and `int`
            // type. Do not collapse the constant onto the name `length`.
            // Also keep rem/index SSA versions from inheriting a `.length` name.
            let na = names.get(&a).map(|s| s.as_str()).unwrap_or("");
            let nb = names.get(&b).map(|s| s.as_str()).unwrap_or("");
            let a_len = na == "length" || na.starts_with("length_");
            let b_len = nb == "length" || nb.starts_with("length_");
            if a_len != b_len {
                continue;
            }
            union(&mut parent, a, b);
        }
    }

    // Pick best name per cluster.
    let mut best: HashMap<VarId, String> = HashMap::new();
    for (vid, name) in names.iter() {
        let root = find(&mut parent, *vid);
        let score = name_quality(name, type_map.get(vid).map(|s| s.as_str()));
        match best.get(&root) {
            None => {
                best.insert(root, name.clone());
            }
            Some(cur) => {
                let cur_score = name_quality(cur, None);
                if score > cur_score {
                    best.insert(root, name.clone());
                }
            }
        }
    }
    for (vid, name) in names.iter_mut() {
        let root = find(&mut parent, *vid);
        if let Some(n) = best.get(&root) {
            *name = n.clone();
        }
    }
}

/// Normalize Java type names for comparison (`String` ≡ `java.lang.String`).
fn normalize_type_name(ty: &str) -> String {
    let t = ty.trim();
    match t {
        "String" => "java.lang.String".into(),
        "Object" => "java.lang.Object".into(),
        "CharSequence" => "java.lang.CharSequence".into(),
        "TextView" => "android.widget.TextView".into(),
        "View" => "android.view.View".into(),
        "Activity" => "android.app.Activity".into(),
        "Context" => "android.content.Context".into(),
        other => other.to_string(),
    }
}

/// True when two SSA versions of one register may share a display name.
pub(crate) fn types_compatible_for_naming(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        // Untyped defs must not inherit String/Uri/class names (avoids `host = 0`).
        // Exception: an untyped use may share an array name (`arr0`) with a typed `T[]`.
        // Typed defs may still share a name when the latest version's type is unknown.
        (None, Some(t)) => t.ends_with("[]"),
        (Some(_), None) => true,
        (None, None) => true,
        (Some(x), Some(y)) => normalize_type_name(x) == normalize_type_name(y),
    }
}

pub(crate) fn is_primitive_java_type(t: &str) -> bool {
    matches!(
        normalize_type_name(t).as_str(),
        "int" | "boolean" | "byte" | "short" | "char" | "long" | "float" | "double" | "void"
    )
}

/// Debug names belong to the object local on a register, not a later D8 reuse
/// (`String host` then `const/4 0` as `resolveActivity` flags).
pub(crate) fn preferred_debug_type_for_reg<'a>(
    reg: u32,
    type_map: &'a HashMap<VarId, String>,
) -> Option<&'a str> {
    let mut latest: Option<(u32, &'a str)> = None;
    let mut object_ty: Option<&'a str> = None;
    for (var, ty) in type_map {
        if var.reg != reg {
            continue;
        }
        let t = ty.as_str();
        if latest.map(|(v, _)| var.ver >= v).unwrap_or(true) {
            latest = Some((var.ver, t));
        }
        if !is_primitive_java_type(t) {
            object_ty = Some(t);
        }
    }
    object_ty.or(latest.map(|(_, t)| t))
}

fn name_quality(name: &str, typ: Option<&str>) -> i32 {
    let mut score = 3;
    if name == "this" {
        return 20;
    }
    // Role names lose to debug locals (`sum`/`max`/`out`/`n`) after overlay.
    if name == "result" || name == "length" {
        return 5;
    }
    if name == "i" || name == "j" || name == "k" {
        return 12;
    }
    if name.starts_with("arr") {
        score = 15;
    } else if typ.is_some_and(|t| t.ends_with("[]")) {
        score = 14;
    } else if name.starts_with("local") && name.bytes().skip(5).all(|b| b.is_ascii_digit()) {
        score = 0;
    } else if name.starts_with('v') && name.bytes().skip(1).all(|b| b.is_ascii_digit()) {
        score = 1;
    } else if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        score = 8;
    }
    score
}

fn var_ids_in_expr(expr: &IrExpr) -> Vec<VarId> {
    match expr {
        IrExpr::Var(v) => vec![*v],
        IrExpr::Call { target, args } => {
            let mut v = var_ids_in_text(target);
            v.extend(var_ids_in_text(args));
            v
        }
        IrExpr::PendingResult => vec![],
        IrExpr::Raw(s) => var_ids_in_text(s),
    }
}

/// Collect all VarIds mentioned in text (vN or vN_k).
fn var_ids_in_text(s: &str) -> Vec<VarId> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'v' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            i += 1;
            let mut reg: u32 = 0;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                reg = reg * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            let ver = if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                let mut v: u32 = 0;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    v = v * 10 + (bytes[i] - b'0') as u32;
                    i += 1;
                }
                v
            } else {
                0
            };
            out.push(VarId::new(reg, ver));
            continue;
        }
        i += 1;
    }
    out
}

fn name_for_var(
    v: VarId,
    type_map: &HashMap<VarId, String>,
    return_var: Option<VarId>,
    param_base: u32,
    ins_size: u32,
    is_static: bool,
    roles: &HashMap<VarId, SemanticRole>,
    counters: &mut HashMap<&'static str, u32>,
    index_counter: &mut u32,
) -> String {
    if return_var == Some(v) {
        return "result".to_string();
    }
    // Parameter registers: version 0, reg in [param_base, param_base + ins_size).
    // In Dalvik, parameters occupy the highest registers: v(registers_size - ins_size) .. v(registers_size - 1).
    if v.ver == 0 && ins_size > 0 && v.reg >= param_base && v.reg < param_base + ins_size {
        let param_offset = v.reg - param_base;
        if !is_static && param_offset == 0 {
            return "this".to_string();
        }
        let param_idx = if is_static {
            param_offset
        } else {
            param_offset.saturating_sub(1)
        };
        let typ = type_map.get(&v).map(|s| s.as_str()).unwrap_or("");
        return param_display_name(param_idx as usize, typ);
    }
    if let Some(role) = roles.get(&v) {
        match role {
            SemanticRole::Index => {
                let names = ["i", "j", "k"];
                let idx = (*index_counter as usize).min(names.len() - 1);
                *index_counter = index_counter.saturating_add(1);
                return names[idx].to_string();
            }
            SemanticRole::Length => {
                // D8 often reuses distinct registers for successive array-length
                // temps; give each a unique display name so `data.length` and
                // `key.length` do not collapse to one `length` variable.
                let c = counters.entry("length").or_insert(0);
                let name = if *c == 0 {
                    "length".to_string()
                } else {
                    format!("length_{}", *c)
                };
                *c += 1;
                return name;
            }
            SemanticRole::Array => {
                let c = counters.entry("arr").or_insert(0);
                let name = format!("arr{}", *c);
                *c += 1;
                return name;
            }
        }
    }
    let prefix = type_map.get(&v).map(|t| type_prefix(t)).unwrap_or("local");
    let c = counters.entry(prefix).or_insert(0);
    let name = format!("{}{}", prefix, *c);
    *c += 1;
    name
}

fn type_prefix(ty: &str) -> &'static str {
    match ty {
        "int" | "short" | "byte" => "i",
        "boolean" => "z",
        "long" => "l",
        "java.lang.String" | "String" => "s",
        "float" => "f",
        "double" => "d",
        "char" => "c",
        "java.lang.Object" | "Object" => "obj",
        "java.lang.StringBuilder" | "StringBuilder" => "sb",
        "android.content.Intent" | "Intent" => "intent",
        "android.net.Uri" | "Uri" => "uri",
        "android.os.Bundle" | "Bundle" => "bundle",
        "android.content.Context" | "Context" => "ctx",
        "android.view.View" | "View" => "view",
        "java.lang.Class" | "Class" => "cls",
        _ if ty.ends_with("[]") => "arr",
        _ if ty.ends_with("Exception") => "ex",
        _ if ty.ends_with("List") => "list",
        _ if ty.ends_with("Map") => "map",
        _ if ty.ends_with("Set") => "set",
        _ => "v",
    }
}

/// Prefer type-based param names: `String p0` → keep `p0` in signature sync via
/// [`param_display_name`].
pub fn param_display_name(param_idx: usize, typ: &str) -> String {
    let base = match typ {
        "int" | "short" | "byte" => "i",
        "boolean" => "z",
        "long" => "l",
        "float" => "f",
        "double" => "d",
        "char" => "c",
        "java.lang.String" | "String" => "s",
        "android.content.Intent" | "Intent" => "intent",
        "android.net.Uri" | "Uri" => "uri",
        "android.os.Bundle" | "Bundle" => "bundle",
        "android.content.Context" | "Context" => "ctx",
        "android.view.View" | "View" => "view",
        _ if typ.ends_with("[]") => "arr",
        _ if typ.ends_with("List") => "list",
        _ if typ.ends_with("Map") => "map",
        _ => "p",
    };
    if base == "p" {
        format!("p{}", param_idx)
    } else if param_idx == 0 && matches!(base, "intent" | "bundle" | "ctx" | "view" | "uri") {
        base.to_string()
    } else {
        format!("{}{}", base, param_idx)
    }
}

/// Map Dalvik parameter registers → signature display names (`this`, `p0`, `s1`, …).
/// Accounts for wide (`long`/`double`) params occupying two incoming slots.
pub fn param_names_by_register(
    registers_size: u32,
    ins_size: u32,
    is_static: bool,
    param_types: &[String],
) -> HashMap<u32, String> {
    let param_base = registers_size.saturating_sub(ins_size);
    let mut map = HashMap::new();
    let mut slot = 0u32;
    if !is_static {
        map.insert(param_base, "this".to_string());
        slot = 1;
    }
    let mut param_idx = 0usize;
    while slot < ins_size && param_idx < param_types.len() {
        let ty = param_types[param_idx].as_str();
        map.insert(param_base + slot, param_display_name(param_idx, ty));
        slot += if matches!(ty, "long" | "double" | "J" | "D") {
            2
        } else {
            1
        };
        param_idx += 1;
    }
    map
}

fn count_args(args: &str) -> usize {
    let s = args.trim();
    if s.is_empty() {
        return 0;
    }
    s.split(',').filter(|p| !p.trim().is_empty()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_field_is_not_typed_as_register() {
        use crate::decompile::ir::VarId;
        let mut types = std::collections::HashMap::new();
        types.insert(
            VarId::new(24, 0),
            "tests.androguard.TestActivity".into(),
        );
        assert_eq!(infer_type_from_raw("v24.value", &types), None);
        let mut arith = std::collections::HashMap::new();
        arith.insert(VarId::new(22, 0), "int".into());
        assert_eq!(infer_type_from_raw("v22 * 2", &arith).as_deref(), Some("int"));
        assert_eq!(infer_type_from_raw("1.0", &types).as_deref(), Some("double"));
        assert_eq!(infer_type_from_raw("4.2f", &types).as_deref(), Some("float"));
    }

    #[test]
    fn count_args_empty() {
        assert_eq!(count_args(""), 0);
        assert_eq!(count_args("   "), 0);
    }

    #[test]
    fn count_args_one_or_more() {
        assert_eq!(count_args("v0"), 1);
        assert_eq!(count_args("v0, v1"), 2);
        assert_eq!(count_args(" v0 , v1 "), 2);
    }

    #[test]
    fn build_var_names_result_and_typed() {
        use crate::decompile::ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
        let stmts = vec![
            IrStmt::Assign {
                dst: VarId::new(0, 1),
                rhs: IrExpr::Raw("0".into()),
                comment: None,
            },
            IrStmt::Return {
                value: Some(IrExpr::Var(VarId::new(0, 1))),
                comment: None,
            },
        ];
        let mut type_map = std::collections::HashMap::new();
        type_map.insert(VarId::new(0, 1), "int".into());
        let names = build_var_names(&stmts, &type_map, 0, true);
        assert_eq!(names.get(&VarId::new(0, 1)), Some(&"result".to_string()));
    }

    #[test]
    fn build_var_names_unifies_ssa_versions_array_over_local() {
        use crate::decompile::ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
        // new-array defines v0_1 as int[]; a later use of v0_0 must not stay as local0.
        let stmts = vec![
            IrStmt::Assign {
                dst: VarId::new(0, 1),
                rhs: IrExpr::Raw("new int[v1_1]".into()),
                comment: None,
            },
            IrStmt::Expr {
                expr: IrExpr::Call {
                    target: "java.util.Arrays.sort".into(),
                    args: "v0".into(), // unversioned use
                },
                comment: None,
            },
        ];
        let mut type_map = std::collections::HashMap::new();
        type_map.insert(VarId::new(0, 1), "int[]".into());
        type_map.insert(VarId::new(1, 1), "int".into());
        let names = build_var_names_with_regs(&stmts, &type_map, 4, 0, true);
        let n_typed = names.get(&VarId::new(0, 1)).cloned();
        let n_old = names.get(&VarId::new(0, 0)).cloned();
        assert!(
            n_typed.as_deref() == Some("arr0")
                || n_typed.as_ref().is_some_and(|s| s.starts_with("arr")),
            "typed array version should be arr*: {:?}",
            n_typed
        );
        assert_eq!(
            n_old, n_typed,
            "unversioned use must share the array name, not local0: old={:?} typed={:?}",
            n_old, n_typed
        );
    }

    #[test]
    fn build_var_names_does_not_unify_incompatible_types() {
        use crate::decompile::ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
        // Same register: Activity (this) → View → String must keep distinct names.
        let stmts = vec![
            IrStmt::Assign {
                dst: VarId::new(0, 1),
                rhs: IrExpr::Call {
                    target: "v0_0.findViewById".into(),
                    args: "2131165271".into(),
                },
                comment: None,
            },
            IrStmt::Assign {
                dst: VarId::new(0, 2),
                rhs: IrExpr::Raw("(TextView) v0_1".into()),
                comment: None,
            },
            IrStmt::Assign {
                dst: VarId::new(0, 3),
                rhs: IrExpr::Call {
                    target: "v0_2.getText".into(),
                    args: "".into(),
                },
                comment: None,
            },
            IrStmt::Assign {
                dst: VarId::new(0, 4),
                rhs: IrExpr::Call {
                    target: "v0_3.toString".into(),
                    args: "".into(),
                },
                comment: None,
            },
        ];
        let mut type_map = std::collections::HashMap::new();
        type_map.insert(VarId::new(0, 0), "com.example.MainActivity".into());
        type_map.insert(VarId::new(0, 1), "android.view.View".into());
        type_map.insert(VarId::new(0, 2), "TextView".into());
        type_map.insert(VarId::new(0, 3), "java.lang.CharSequence".into());
        type_map.insert(VarId::new(0, 4), "java.lang.String".into());
        let names = build_var_names_with_regs(&stmts, &type_map, 1, 1, false);
        // Simulate debug name applied only to final String version.
        let mut names = names;
        names.insert(VarId::new(0, 4), "email".into());
        let n0 = names.get(&VarId::new(0, 0)).cloned();
        let n1 = names.get(&VarId::new(0, 1)).cloned();
        let n4 = names.get(&VarId::new(0, 4)).cloned();
        assert_eq!(n0.as_deref(), Some("this"));
        assert_ne!(
            n1, n4,
            "View and String must not share a name: {:?} vs {:?}",
            n1, n4
        );
        assert_ne!(
            n0, n4,
            "this/Activity must not become email: {:?} vs {:?}",
            n0, n4
        );
    }

    #[test]
    fn build_var_names_semantic_array_index_length() {
        use crate::decompile::ir::{Expr as IrExpr, Stmt as IrStmt, VarId};
        // Raw as after SSA: v0_1 = v1_0[v2_0]; -> array v1_0, index v2_0
        let stmts = vec![
            IrStmt::Assign {
                dst: VarId::new(0, 1),
                rhs: IrExpr::Raw("v1_0[v2_0]  // aget".into()),
                comment: None,
            },
            IrStmt::Assign {
                dst: VarId::new(3, 0),
                rhs: IrExpr::Raw("v1_0.length  // array-length".into()),
                comment: None,
            },
        ];
        let type_map = std::collections::HashMap::new();
        let names = build_var_names(&stmts, &type_map, 0, true);
        assert_eq!(names.get(&VarId::new(1, 0)), Some(&"arr0".to_string()));
        assert_eq!(names.get(&VarId::new(2, 0)), Some(&"i".to_string()));
        assert_eq!(names.get(&VarId::new(3, 0)), Some(&"length".to_string()));
    }

    #[test]
    fn param_names_by_register_static_accessor() {
        let names = param_names_by_register(
            6,
            3,
            true,
            &[
                "oversecured.ovaa.activities.LoginActivity".into(),
                "java.lang.String".into(),
                "java.lang.String".into(),
            ],
        );
        assert_eq!(names.get(&3), Some(&"p0".to_string()));
        assert_eq!(names.get(&4), Some(&"s1".to_string()));
        assert_eq!(names.get(&5), Some(&"s2".to_string()));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn param_names_by_register_instance_skips_this_and_wide() {
        let names =
            param_names_by_register(5, 4, false, &["long".into(), "java.lang.String".into()]);
        assert_eq!(names.get(&1), Some(&"this".to_string()));
        assert_eq!(names.get(&2), Some(&"l0".to_string()));
        assert_eq!(names.get(&4), Some(&"s1".to_string()));
        assert!(!names.contains_key(&3));
    }

    /// OVAA `onRequestPermissionsResult(int, String[], int[])` without debug names.
    #[test]
    fn param_names_by_register_on_request_permissions_result() {
        let names = param_names_by_register(
            6,
            4,
            false,
            &["int".into(), "java.lang.String[]".into(), "int[]".into()],
        );
        assert_eq!(names.get(&2), Some(&"this".to_string()));
        assert_eq!(names.get(&3), Some(&"i0".to_string()));
        assert_eq!(names.get(&4), Some(&"arr1".to_string()));
        assert_eq!(names.get(&5), Some(&"arr2".to_string()));
    }

    /// D8 reuses v0 for `const/16 …, 1002` then `array-length`. Both are `int`;
    /// unifying would emit `int length = 1002`.
    #[test]
    fn unify_names_does_not_collapse_const_onto_length() {
        use crate::decompile::ir::VarId;
        let mut names = std::collections::HashMap::new();
        names.insert(VarId::new(0, 1), "i0".into());
        names.insert(VarId::new(0, 2), "length".into());
        let mut types = std::collections::HashMap::new();
        types.insert(VarId::new(0, 1), "int".into());
        types.insert(VarId::new(0, 2), "int".into());
        unify_names_by_register(&mut names, &types);
        assert_eq!(names.get(&VarId::new(0, 1)), Some(&"i0".to_string()));
        assert_eq!(names.get(&VarId::new(0, 2)), Some(&"length".to_string()));
    }

    #[test]
    fn unify_names_still_shares_length_across_ssa_versions() {
        use crate::decompile::ir::VarId;
        let mut names = std::collections::HashMap::new();
        names.insert(VarId::new(0, 1), "length".into());
        names.insert(VarId::new(0, 2), "length".into());
        let mut types = std::collections::HashMap::new();
        types.insert(VarId::new(0, 1), "int".into());
        types.insert(VarId::new(0, 2), "int".into());
        unify_names_by_register(&mut names, &types);
        assert_eq!(names.get(&VarId::new(0, 1)), Some(&"length".to_string()));
        assert_eq!(names.get(&VarId::new(0, 2)), Some(&"length".to_string()));
    }

    #[test]
    fn preferred_debug_type_prefers_string_over_later_int() {
        let mut types = std::collections::HashMap::new();
        types.insert(VarId::new(0, 1), "java.lang.String".into());
        types.insert(VarId::new(0, 2), "int".into());
        assert_eq!(
            preferred_debug_type_for_reg(0, &types),
            Some("java.lang.String")
        );
        assert!(types_compatible_for_naming(
            Some("java.lang.String"),
            preferred_debug_type_for_reg(0, &types)
        ));
        assert!(!types_compatible_for_naming(
            Some("int"),
            preferred_debug_type_for_reg(0, &types)
        ));
    }

    #[test]
    fn substitute_names_prefers_latest_ssa_version() {
        use crate::decompile::ir::{substitute_names_in_text_pub, VarId};
        let mut names = HashMap::new();
        names.insert(VarId::new(3, 1), "obj0".into());
        names.insert(VarId::new(3, 2), "obj1".into());
        let out = substitute_names_in_text_pub("v3 != 0", &names);
        assert_eq!(out, "obj1 != 0");
    }
}
