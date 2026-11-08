//! Conservative Retrofit annotation recovery from original DEX control flow.
//!
//! The branch shapes follow Square's RequestFactory.Builder methods
//! parseMethodAnnotation and parseParameterAnnotation (Apache 2.0):
//! https://github.com/square/retrofit/blob/master/retrofit/src/main/java/retrofit2/RequestFactory.java
//! No source code or instruction sequence is copied from upstream.
use crate::{
    native_cfg::instruction_width,
    native_dex::{DexClass, DexMethod, DexSymbols},
    native_engine::disassembly::method_id,
    native_ir::{DecodedMethod, Instruction, PoolKind},
};
fn has_create_invoke(words: &[u16], indices: &BTreeSet<u16>) -> bool {
    let mut pc = 0usize;
    while pc < words.len() {
        let Ok((width, payload)) = instruction_width(words, pc) else {
            return false;
        };
        if width == 0 || pc + width > words.len() {
            return false;
        }
        if !payload
            && matches!(words[pc] as u8,0x6e..=0x72|0x74..=0x78)
            && words
                .get(pc + 1)
                .is_some_and(|index| indices.contains(index))
        {
            return true;
        }
        pc += width;
    }
    false
}
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub(super) struct Role {
    pub name: &'static str,
    pub evidence: Value,
}

pub(super) struct Mappings {
    roles: BTreeMap<String, Role>,
    pub ambiguous: bool,
    pub parser_candidates: usize,
    pub recovery_complete: bool,
    creation: Option<CreationModel>,
}
#[derive(Clone)]
struct CreationModel {
    builder: String,
    retrofit: String,
    base_url_method: String,
    build_method: String,
    create_method: String,
}
#[derive(Clone)]
enum Origin {
    Unknown,
    Builder(usize),
    Retrofit(usize, Option<String>, Option<usize>),
    Text(String),
    Class(String),
}
fn method_matches(
    pool: &DexSymbols,
    i: &Instruction,
    owner: &str,
    name: &str,
    params: &[&str],
    ret: &str,
) -> bool {
    let Some(reference) = i.reference.filter(|r| r.kind == PoolKind::Method) else {
        return false;
    };
    let Some(&(o, p, n)) = pool.methods.get(reference.index as usize) else {
        return false;
    };
    pool.types
        .get(o as usize)
        .is_some_and(|t| t.as_ref() == owner)
        && pool.strings.get(n as usize).is_some_and(|s| s == name)
        && pool.protos.get(p as usize).is_some_and(|(r, ps)| {
            r.as_ref() == ret && ps.iter().map(AsRef::as_ref).eq(params.iter().copied())
        })
}
impl Mappings {
    pub fn get(&self, descriptor: &str) -> Option<&Role> {
        self.roles.get(descriptor)
    }
    pub fn status(&self) -> &'static str {
        if !self.recovery_complete {
            "budget_exhausted"
        } else if self.ambiguous {
            "ambiguous"
        } else if self.parser_candidates == 0 {
            "no_supported_parser"
        } else {
            "supported_roles_recovered"
        }
    }
    pub fn recovered_roles(&self) -> Vec<Value> {
        self.roles.iter().take(64).map(|(descriptor,role)|json!({"original_descriptor":descriptor,"semantic_role":role.name})).collect()
    }
}

fn method_ref<'a>(pool: &'a DexSymbols, i: &Instruction) -> Option<(&'a str, &'a str, &'a str)> {
    if i.reference?.kind != PoolKind::Method {
        return None;
    }
    let &(owner, proto, name) = pool.methods.get(i.reference?.index as usize)?;
    let owner = pool.types.get(owner as usize)?.as_ref();
    let name = pool.strings.get(name as usize)?.as_str();
    let (ret, params) = pool.protos.get(proto as usize)?;
    // Only signatures needed by this detector are represented here.
    let sig = if params.is_empty() && ret.as_ref() == "Ljava/lang/String;" {
        "()String"
    } else if params.is_empty() && ret.as_ref() == "Z" {
        "()bool"
    } else if params
        .iter()
        .map(AsRef::as_ref)
        .eq(["Ljava/lang/String;", "Ljava/lang/String;", "Z"])
        && ret.as_ref() == "V"
    {
        "(String,String,bool)void"
    } else {
        "other"
    };
    Some((owner, name, sig))
}
fn string_ref<'a>(pool: &'a DexSymbols, i: &Instruction) -> Option<&'a str> {
    if !matches!(i.opcode, 0x1a | 0x1b) || i.reference?.kind != PoolKind::String {
        return None;
    }
    pool.strings
        .get(i.reference?.index as usize)
        .map(String::as_str)
}
fn type_ref<'a>(pool: &'a DexSymbols, i: &Instruction) -> Option<&'a str> {
    if i.reference?.kind != PoolKind::Type {
        return None;
    }
    pool.types
        .get(i.reference?.index as usize)
        .map(AsRef::as_ref)
}
fn reg(i: &Instruction, read: bool, n: usize) -> Option<u16> {
    (if read { &i.reads } else { &i.writes })
        .get(n)
        .map(|r| r.register)
}
fn preserved(ins: &[Instruction], start: usize, end: usize, register: u16) -> bool {
    ins[start..end].iter().all(|i| {
        i.writes.iter().all(|w| {
            !(w.register..w.register.saturating_add(w.kind.word_count() as u16)).contains(&register)
        })
    })
}
fn handler_enters_body(code: &crate::native_dex::DexCode, body: &[Instruction]) -> bool {
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return false;
    };
    code.try_regions
        .iter()
        .flat_map(|r| r.catches.iter())
        .any(|(_, pc)| (*pc as usize) >= first.pc && (*pc as usize) <= last.pc)
}
fn field_index(pool: &DexSymbols, i: &Instruction, owner: &str) -> Option<u32> {
    let reference = i.reference.filter(|r| r.kind == PoolKind::Field)?;
    let &(o, t, _) = pool.fields.get(reference.index as usize)?;
    (pool.types.get(o as usize)?.as_ref() == owner && pool.types.get(t as usize)?.as_ref() == "Z")
        .then_some(reference.index)
}
fn query_guard_diagnostic(
    pool: &DexSymbols,
    ins: &[Instruction],
    target: usize,
) -> Option<&'static str> {
    let mut role = None;
    for i in ins.iter().skip(target).take(16) {
        if i.branch_target.is_some() || i.payload_target.is_some() {
            return None;
        }
        if let Some(s) = string_ref(pool, i) {
            let found = match s {
                "A @Url parameter must not come after a @Query." => Some("Query"),
                "A @Url parameter must not come after a @QueryName." => Some("QueryName"),
                "A @Url parameter must not come after a @QueryMap." => Some("QueryMap"),
                _ => None,
            };
            if let Some(found) = found {
                if role.is_some_and(|previous| previous != found) {
                    return None;
                }
                role = Some(found);
            }
        }
        if matches!(i.opcode, 0x0e..=0x11 | 0x27) {
            break;
        }
    }
    role
}
fn insert_query_guard(
    guards: &mut BTreeMap<u32, Option<(&'static str, usize)>>,
    field: u32,
    role: &'static str,
    pc: usize,
) {
    match guards.entry(field) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(Some((role, pc)));
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if entry.get().is_some_and(|(previous, _)| previous != role) {
                entry.insert(None);
            }
        }
    }
}
fn parser_helper(class: &DexClass) -> Option<&DexMethod> {
    class.methods.iter().find(|m| {
        m.return_type.as_ref() == "V"
            && m.parameters.iter().map(AsRef::as_ref).eq([
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Z",
            ])
            && m.code.as_ref().is_some_and(|code| {
                if code.instructions.len() > 4096 {
                    return false;
                }
                let Ok(ir) = DecodedMethod::decode(code) else {
                    return false;
                };
                let strings: BTreeSet<_> = ir
                    .instructions
                    .iter()
                    .filter_map(|i| string_ref(&class.symbols, i))
                    .collect();
                strings.contains("Only one HTTP method is allowed. Found: %s and %s.")
                    && strings
                        .iter()
                        .any(|s| s.starts_with("URL query string ") && s.contains("@Query"))
            })
    })
}
fn branch<'a>(
    pool: &'a DexSymbols,
    ins: &'a [Instruction],
    at: usize,
) -> Option<(&'a str, &'a [Instruction], usize)> {
    let test = ins.get(at)?;
    if test.opcode != 0x20 {
        return None;
    }
    let ty = type_ref(pool, test)?;
    let condition = reg(test, false, 0)?;
    let tested = reg(test, true, 0)?;
    let cond_index = (at + 1..ins.len().min(at + 9)).find(|&n| ins[n].opcode == 0x38)?;
    if ins[at + 1..cond_index].iter().any(|i| {
        !matches!(i.opcode, 0x12..=0x1c)
            || i.branch_target.is_some()
            || i.writes.iter().any(|w| {
                (w.register..w.register.saturating_add(w.kind.word_count() as u16))
                    .contains(&condition)
                    || (w.register..w.register.saturating_add(w.kind.word_count() as u16))
                        .contains(&tested)
            })
    }) {
        return None;
    }
    let cond = ins.get(cond_index)?;
    if reg(cond, true, 0) != Some(condition) {
        return None;
    }
    let end = ins.iter().position(|i| Some(i.pc) == cond.branch_target)?;
    if end <= cond_index + 1 || end > ins.len() {
        return None;
    }
    let body_start = cond_index + 1;
    if ins.iter().enumerate().any(|(n, i)| {
        (n < body_start || n >= end)
            && i.branch_target
                .is_some_and(|pc| pc >= ins[body_start].pc && pc <= ins[end - 1].pc)
    }) {
        return None;
    }
    Some((ty, &ins[body_start..end], tested as usize))
}
fn method_roles(
    class: &DexClass,
    helper: &DexMethod,
    cancel: &AtomicBool,
) -> Result<Vec<(String, Role)>> {
    let mut out = Vec::new();
    for m in &class.methods {
        if !m
            .parameters
            .iter()
            .any(|p| p.as_ref() == "Ljava/lang/annotation/Annotation;")
        {
            continue;
        }
        let Some(code) = &m.code else { continue };
        if m.parameters
            .last()
            .is_none_or(|p| p.as_ref() != "Ljava/lang/annotation/Annotation;")
        {
            continue;
        }
        let Some(tested_register) = code.registers.checked_sub(1) else {
            continue;
        };
        let Ok(ir) = DecodedMethod::decode(code) else {
            continue;
        };
        let ins = &ir.instructions;
        if ins.iter().any(|i| matches!(i.opcode, 0x2b | 0x2c)) {
            continue;
        }
        for at in 0..ins.len() {
            if at % 256 == 0 {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            }
            let Some((ty, body, tested)) = branch(&class.symbols, ins, at) else {
                continue;
            };
            if handler_enters_body(code, body) {
                continue;
            }
            if tested != tested_register as usize {
                continue;
            }
            let mut casts = false;
            let mut value = None;
            let mut verb = None;
            let mut call = None;
            for (bi, i) in body.iter().enumerate() {
                if i.opcode == 0x1f
                    && type_ref(&class.symbols, i) == Some(ty)
                    && reg(i, true, 0) == Some(tested_register)
                {
                    casts = true;
                }
                if let Some((owner, name, sig)) = method_ref(&class.symbols, i) {
                    if owner == ty
                        && name == "value"
                        && sig == "()String"
                        && reg(i, true, 0) == Some(tested_register)
                        && let Some(next) = body.get(bi + 1).filter(|n| n.opcode == 0x0c)
                    {
                        value = reg(next, false, 0).map(|r| (r, bi + 1, i.pc));
                    }
                    if owner == class.descriptor.as_ref()
                        && name == helper.name.as_ref()
                        && sig == "(String,String,bool)void"
                    {
                        call = Some((bi, i));
                    }
                }
                if let Some(s) = string_ref(&class.symbols, i) {
                    let word = match s {
                        "GET" => Some("GET"),
                        "POST" => Some("POST"),
                        "PUT" => Some("PUT"),
                        "PATCH" => Some("PATCH"),
                        "DELETE" => Some("DELETE"),
                        "HEAD" => Some("HEAD"),
                        "OPTIONS" => Some("OPTIONS"),
                        _ => None,
                    };
                    if let Some(word) = word {
                        verb = reg(i, false, 0).map(|r| (word, r, bi, i.pc));
                    }
                }
            }
            let (Some((vr, vi, vpc)), Some((word, hr, hi, hpc)), Some((ci, invoke))) =
                (value, verb, call)
            else {
                continue;
            };
            if !casts
                || vi >= ci
                || hi >= ci
                || body[..ci]
                    .iter()
                    .any(|i| i.branch_target.is_some() || matches!(i.opcode, 0x0e..=0x11 | 0x27))
                || !preserved(body, vi + 1, ci, vr)
                || !preserved(body, hi + 1, ci, hr)
            {
                continue;
            }
            if reg(invoke, true, 0) != code.registers.checked_sub(code.ins)
                || reg(invoke, true, 1) != Some(hr)
                || reg(invoke, true, 2) != Some(vr)
            {
                continue;
            }
            out.push((ty.to_owned(),Role{name:word,evidence:json!({"parser_method_id":method_id(m),"helper_method_id":method_id(helper),"instance_of_pc":ins[at].pc,"member_value_pc":vpc,"verb_literal_pc":hpc,"parse_call_pc":invoke.pc,"proof":"same guarded annotation branch passes its value and literal HTTP verb to parser helper"})}));
        }
    }
    Ok(out)
}
fn parameter_roles(class: &DexClass, cancel: &AtomicBool) -> Result<Vec<(String, Role)>> {
    let mut out = Vec::new();
    for m in &class.methods {
        if !m
            .parameters
            .iter()
            .any(|p| p.as_ref() == "Ljava/lang/annotation/Annotation;")
        {
            continue;
        }
        let Some(code) = &m.code else { continue };
        if m.parameters
            .last()
            .is_none_or(|p| p.as_ref() != "Ljava/lang/annotation/Annotation;")
        {
            continue;
        }
        let Some(tested_register) = code.registers.checked_sub(1) else {
            continue;
        };
        let Ok(ir) = DecodedMethod::decode(code) else {
            continue;
        };
        let ins = &ir.instructions;
        if ins.iter().any(|i| matches!(i.opcode, 0x2b | 0x2c)) {
            continue;
        }
        for at in 0..ins.len() {
            if at % 256 == 0 {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            }
            let Some((ty, body, tested)) = branch(&class.symbols, ins, at) else {
                continue;
            };
            if handler_enters_body(code, body) {
                continue;
            }
            if tested != tested_register as usize {
                continue;
            }
            let strings: BTreeSet<_> = body
                .iter()
                .filter_map(|i| string_ref(&class.symbols, i))
                .collect();
            let url = strings
                .iter()
                .filter(|s| {
                    s.contains("@Url")
                        && (s.contains("cannot") || s.contains("must") || s.contains("Multiple"))
                })
                .count()
                >= 3
                && strings.iter().any(|s| s.starts_with("@Url must be "));
            let header = body.iter().any(|i| {
                method_ref(&class.symbols, i) == Some((ty, "value", "()String"))
                    && reg(i, true, 0) == Some(tested_register)
            }) && body.iter().any(|i| {
                method_ref(&class.symbols, i) == Some((ty, "allowUnsafeNonAsciiValues", "()bool"))
                    && reg(i, true, 0) == Some(tested_register)
            }) && body.iter().any(|i| {
                i.opcode == 0x1f
                    && type_ref(&class.symbols, i) == Some(ty)
                    && reg(i, true, 0) == Some(tested_register)
            });
            let role = if url {
                Some("Url")
            } else if header {
                Some("Header")
            } else {
                None
            };
            if let Some(name) = role {
                out.push((ty.to_owned(),Role{name,evidence:json!({"parser_method_id":method_id(m),"instance_of_pc":ins[at].pc,"branch_end_pc":body.last().map(|i|i.pc),"proof":if url {"guarded annotation branch contains multiple distinctive @Url validation errors"} else {"guarded annotation branch reads value and allowUnsafeNonAsciiValues on the tested annotation"}})}));
            }
        }
    }
    Ok(out)
}
fn query_roles(class: &DexClass, cancel: &AtomicBool) -> Result<Vec<(String, Role)>> {
    let mut out = Vec::new();
    for m in &class.methods {
        if m.parameters
            .last()
            .is_none_or(|p| p.as_ref() != "Ljava/lang/annotation/Annotation;")
        {
            continue;
        }
        let Some(code) = &m.code else { continue };
        let Some(tested) = code.registers.checked_sub(1) else {
            continue;
        };
        let Ok(ir) = DecodedMethod::decode(code) else {
            continue;
        };
        let ins = &ir.instructions;
        if ins.iter().any(|i| matches!(i.opcode, 0x2b | 0x2c)) {
            continue;
        }
        let by_pc: BTreeMap<_, _> = ins.iter().enumerate().map(|(n, i)| (i.pc, n)).collect();
        let mut guards: BTreeMap<u32, Option<(&'static str, usize)>> = BTreeMap::new();
        for at in 0..ins.len() {
            let Some((_, body, seen)) = branch(&class.symbols, ins, at) else {
                continue;
            };
            if seen != tested as usize || handler_enters_body(code, body) {
                continue;
            }
            let strings: BTreeSet<_> = body
                .iter()
                .filter_map(|i| string_ref(&class.symbols, i))
                .collect();
            if !strings.iter().any(|s| s.starts_with("@Url must be "))
                || strings.iter().filter(|s| s.contains("@Url")).count() < 3
            {
                continue;
            }
            for pair in body.windows(2) {
                let (read, cond) = (&pair[0], &pair[1]);
                if read.opcode != 0x55
                    || cond.opcode != 0x39
                    || reg(cond, true, 0) != reg(read, false, 0)
                    || reg(read, true, 0) != code.registers.checked_sub(code.ins)
                {
                    continue;
                }
                let Some(field) = field_index(&class.symbols, read, &class.descriptor) else {
                    continue;
                };
                let Some(target) = cond.branch_target.and_then(|pc| by_pc.get(&pc)).copied() else {
                    continue;
                };
                if let Some(role) = query_guard_diagnostic(&class.symbols, ins, target) {
                    insert_query_guard(&mut guards, field, role, read.pc);
                }
            }
        }
        if guards.is_empty() {
            continue;
        }
        let mut work = 0usize;
        for at in 0..ins.len() {
            if at % 256 == 0 {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            }
            let Some((ty, body, seen)) = branch(&class.symbols, ins, at) else {
                continue;
            };
            if seen != tested as usize || handler_enters_body(code, body) {
                continue;
            }
            for i in body {
                if i.opcode != 0x5c || reg(i, true, 0) != code.registers.checked_sub(code.ins) {
                    continue;
                }
                let Some(field) = field_index(&class.symbols, i, &class.descriptor) else {
                    continue;
                };
                let Some((role, guard_pc)) = guards.get(&field).copied().flatten() else {
                    continue;
                };
                let Some(value_reg) = reg(i, true, 1) else {
                    continue;
                };
                let proof =
                    crate::call_arguments::constant_bits_at(m, i.pc, value_reg, cancel, &mut work);
                match proof {
                    Ok(Some(1)) => {}
                    Err(error) if error.to_string() == "CANCELLED" => return Err(error),
                    _ => continue,
                }
                out.push((ty.to_owned(),Role{name:role,evidence:json!({"parser_method_id":method_id(m),"instance_of_pc":ins[at].pc,"state_field_index":field,"url_guard_field_read_pc":guard_pc,"positive_branch_field_write_pc":i.pc,"proof":"Url validation guard names this parameter category; its guarded annotation branch writes constant true to the same parser state field, proven across CFG predecessors"})}));
            }
        }
    }
    Ok(out)
}
fn creation_model(classes: &[&DexClass], parser: &DexClass) -> Option<CreationModel> {
    let by_type: BTreeMap<_, _> = classes
        .iter()
        .map(|c| (c.descriptor.as_ref(), *c))
        .collect();
    let mut owners = Vec::new();
    for constructor in parser
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == "<init>")
    {
        for ty in &constructor.parameters {
            let Some(owner) = by_type.get(ty.as_ref()) else {
                continue;
            };
            for method in &owner.methods {
                if method
                    .parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .eq(["Ljava/lang/Class;"])
                    && method.return_type.as_ref() == "Ljava/lang/Object;"
                    && method.code.as_ref().is_some_and(|code| {
                        DecodedMethod::decode(code).is_ok_and(|ir| {
                            ir.instructions.iter().any(|i| {
                                method_ref(&owner.symbols, i).is_some_and(|(o, n, _)| {
                                    o == "Ljava/lang/reflect/Proxy;" && n == "newProxyInstance"
                                })
                            })
                        })
                    })
                {
                    owners.push((*owner, method));
                }
            }
        }
    }
    if owners.len() != 1 {
        return None;
    }
    let (owner, create) = owners[0];
    let mut builders = Vec::new();
    for builder in classes {
        let builds: Vec<_> = builder
            .methods
            .iter()
            .filter(|m| {
                m.parameters.is_empty()
                    && m.return_type == owner.descriptor
                    && m.code.as_ref().is_some_and(|code| {
                        DecodedMethod::decode(code).is_ok_and(|ir| {
                            ir.instructions.iter().any(|i| {
                                string_ref(&builder.symbols, i) == Some("Base URL required.")
                            })
                        })
                    })
            })
            .collect();
        let base_urls: Vec<_> = builder
            .methods
            .iter()
            .filter(|m| {
                m.parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .eq(["Ljava/lang/String;"])
                    && m.return_type == builder.descriptor
                    && m.code.as_ref().is_some_and(|code| {
                        DecodedMethod::decode(code).is_ok_and(|ir| {
                            ir.instructions
                                .iter()
                                .any(|i| string_ref(&builder.symbols, i) == Some("baseUrl == null"))
                        })
                    })
            })
            .collect();
        if builds.len() == 1 && base_urls.len() == 1 {
            builders.push((*builder, builds[0], base_urls[0]));
        }
    }
    if builders.len() != 1 {
        return None;
    }
    let (builder, build, base_url) = builders[0];
    Some(CreationModel {
        builder: builder.descriptor.to_string(),
        retrofit: owner.descriptor.to_string(),
        base_url_method: base_url.name.to_string(),
        build_method: build.name.to_string(),
        create_method: create.name.to_string(),
    })
}
#[derive(Default)]
pub(super) struct CreationScan {
    pub items: BTreeMap<String, Vec<Value>>,
    pub candidate_methods: usize,
    pub scanned_methods: usize,
    pub unsupported_methods: usize,
    pub budget_exhausted: bool,
    pub sites_seen: usize,
    pub sites_truncated: usize,
    pub model_resolved: bool,
    retained_bytes: usize,
}
impl CreationScan {
    fn add(&mut self, service: String, item: Value) -> Result<()> {
        self.sites_seen += 1;
        let bytes = serde_json::to_vec(&item)?.len();
        if self.sites_seen <= 500
            && self.retained_bytes.saturating_add(bytes) <= 128_000
            && self.items.get(&service).map_or(0, Vec::len) < 12
        {
            self.retained_bytes += bytes;
            self.items.entry(service).or_default().push(item);
        } else {
            self.sites_truncated += 1;
        }
        Ok(())
    }
}
pub(super) fn creation_links(
    classes: &[&DexClass],
    mappings: &Mappings,
    cancel: &AtomicBool,
) -> Result<CreationScan> {
    let mut result = CreationScan::default();
    let Some(model) = &mappings.creation else {
        return Ok(result);
    };
    result.model_resolved = true;
    let mut remaining = 2_000_000usize;
    let mut table_cache: BTreeMap<usize, BTreeSet<u16>> = BTreeMap::new();
    let mut prefilter_remaining = 80_000_000usize;
    'classes: for class in classes {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let key = Arc::as_ptr(&class.symbols) as usize;
        let create_indices = table_cache.entry(key).or_insert_with(|| {
            class
                .symbols
                .methods
                .iter()
                .enumerate()
                .filter_map(|(n, &(owner, proto, name))| {
                    let (ret, params) = class.symbols.protos.get(proto as usize)?;
                    (class.symbols.types.get(owner as usize)?.as_ref() == model.retrofit
                        && class.symbols.strings.get(name as usize)? == &model.create_method
                        && ret.as_ref() == "Ljava/lang/Object;"
                        && params.iter().map(AsRef::as_ref).eq(["Ljava/lang/Class;"]))
                    .then_some(n as u16)
                })
                .collect()
        });
        if create_indices.is_empty() {
            continue;
        }
        for method in &class.methods {
            let Some(code) = &method.code else { continue };
            if code.instructions.len() > prefilter_remaining {
                result.budget_exhausted = true;
                break 'classes;
            }
            prefilter_remaining -= code.instructions.len();
            if !code
                .instructions
                .iter()
                .any(|word| create_indices.contains(word))
            {
                continue;
            }
            if !has_create_invoke(&code.instructions, create_indices) {
                continue;
            }
            result.candidate_methods += 1;
            if code.instructions.len() > 2048 || !code.try_regions.is_empty() {
                result.unsupported_methods += 1;
                continue;
            }
            if code.instructions.len() > remaining {
                result.budget_exhausted = true;
                break 'classes;
            }
            remaining -= code.instructions.len();
            let Ok(ir) = DecodedMethod::decode(code) else {
                result.unsupported_methods += 1;
                continue;
            };
            if ir
                .instructions
                .iter()
                .any(|i| i.branch_target.is_some() || i.payload_target.is_some())
            {
                result.unsupported_methods += 1;
                continue;
            }
            result.scanned_methods += 1;
            let mut regs = vec![Origin::Unknown; code.registers as usize];
            let mut builders: BTreeMap<usize, (bool, Option<String>, Option<usize>)> =
                BTreeMap::new();
            let mut pending = None;
            for (n, i) in ir.instructions.iter().enumerate() {
                if n % 256 == 0 {
                    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                }
                if matches!(i.opcode, 0x0e..=0x11 | 0x27) {
                    break;
                }
                let prior = |r: Option<u16>| {
                    r.and_then(|r| regs.get(r as usize))
                        .cloned()
                        .unwrap_or(Origin::Unknown)
                };
                let next = match i.opcode {
                    0x07..=0x09 => prior(reg(i, true, 0)),
                    0x0c => pending.take().unwrap_or(Origin::Unknown),
                    0x1a | 0x1b => string_ref(&class.symbols, i)
                        .map(|s| Origin::Text(s.to_owned()))
                        .unwrap_or(Origin::Unknown),
                    0x1c => type_ref(&class.symbols, i)
                        .map(|s| Origin::Class(s.to_owned()))
                        .unwrap_or(Origin::Unknown),
                    0x22 if type_ref(&class.symbols, i) == Some(model.builder.as_str()) => {
                        builders.insert(i.pc, (false, None, None));
                        Origin::Builder(i.pc)
                    }
                    _ => Origin::Unknown,
                };
                let tracked_read = i.reads.iter().any(|r| {
                    matches!(
                        prior(Some(r.register)),
                        Origin::Builder(_) | Origin::Retrofit(..)
                    )
                });
                if matches!(i.opcode,0x6e..=0x72|0x74..=0x78) {
                    pending = None;
                    let receiver = prior(reg(i, true, 0));
                    let constructor = method_ref(&class.symbols, i)
                        .is_some_and(|(o, n, _)| o == model.builder && n == "<init>")
                        && matches!(i.opcode, 0x70 | 0x76);
                    let base = method_matches(
                        &class.symbols,
                        i,
                        &model.builder,
                        &model.base_url_method,
                        &["Ljava/lang/String;"],
                        &model.builder,
                    );
                    let build = method_matches(
                        &class.symbols,
                        i,
                        &model.builder,
                        &model.build_method,
                        &[],
                        &model.retrofit,
                    );
                    let create = method_matches(
                        &class.symbols,
                        i,
                        &model.retrofit,
                        &model.create_method,
                        &["Ljava/lang/Class;"],
                        "Ljava/lang/Object;",
                    );
                    if constructor {
                        if let Origin::Builder(id) = receiver
                            && let Some(b) = builders.get_mut(&id)
                        {
                            b.0 = true;
                        }
                    } else if base {
                        if let Origin::Builder(id) = receiver
                            && let Some(b) = builders.get_mut(&id)
                            && b.0
                        {
                            b.1 = match prior(reg(i, true, 1)) {
                                Origin::Text(s) => Some(s),
                                _ => None,
                            };
                            b.2 = Some(i.pc);
                            // A fluent return is not assumed to alias the
                            // receiver; only its original register is tracked.
                        }
                    } else if build {
                        if let Origin::Builder(id) = receiver
                            && let Some((true, url, base_pc)) = builders.get(&id)
                        {
                            pending = Some(Origin::Retrofit(id, url.clone(), *base_pc));
                        }
                    } else if create {
                        if let Origin::Class(service) = prior(reg(i, true, 1)) {
                            let (url, base_pc, builder_pc, status) = match receiver {
                                Origin::Retrofit(id, Some(url), Some(pc)) => (
                                    Some(url),
                                    Some(pc),
                                    Some(id),
                                    "callsite_same_object_library_model_conditional",
                                ),
                                _ => (None, None, None, "base_url_or_receiver_unresolved"),
                            };
                            result.add(service,json!({"caller_method_id":method_id(method),"dex_entry":class.symbols.origin.get(),"base_url_candidate":url,"base_url_pc":base_pc,"create_pc":i.pc,"builder_allocation_pc":builder_pc,"link_status":status,"callsite_dataflow":if builder_pc.is_some(){"same tracked builder receiver for baseUrl and build; build result receiver for create(service.class)"}else{"constant service Class argument at recovered create call"},"library_model":"conditional_on_signature_and_error_string_fingerprint","model_assumption":"baseUrl method configures this builder and build returns a Retrofit with that configuration"}))?;
                        }
                    } else if tracked_read {
                        break;
                    }
                } else if i.opcode != 0x0c {
                    pending = None;
                    if tracked_read && !matches!(i.opcode, 0x07..=0x09) {
                        break;
                    }
                }
                for w in &i.writes {
                    for word in 0..w.kind.word_count() {
                        if let Some(slot) = regs.get_mut(w.register as usize + word) {
                            *slot = if word == 0 && w.kind.word_count() == 1 {
                                next.clone()
                            } else {
                                Origin::Unknown
                            };
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}
pub(super) fn discover(classes: &[&DexClass], cancel: &AtomicBool) -> Result<Mappings> {
    let mut candidates = Vec::new();
    let mut remaining = 2_000_000usize;
    let mut recovery_complete = true;
    for class in classes {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !class.methods.iter().any(|m| {
            m.parameters.iter().map(AsRef::as_ref).eq([
                "Ljava/lang/String;",
                "Ljava/lang/String;",
                "Z",
            ])
        }) {
            continue;
        }
        let relevant = class.methods.iter().filter(|m| {
            m.parameters
                .iter()
                .any(|p| p.as_ref() == "Ljava/lang/annotation/Annotation;")
                || m.parameters.iter().map(AsRef::as_ref).eq([
                    "Ljava/lang/String;",
                    "Ljava/lang/String;",
                    "Z",
                ])
        });
        let cost = relevant
            .clone()
            .filter_map(|m| m.code.as_ref())
            .map(|c| c.instructions.len())
            .sum::<usize>();
        if cost > remaining
            || relevant
                .filter_map(|m| m.code.as_ref())
                .any(|c| c.instructions.len() > 100_000)
        {
            recovery_complete = false;
            continue;
        }
        remaining -= cost;
        if let Some(helper) = parser_helper(class) {
            let methods = method_roles(class, helper, cancel)?;
            if methods
                .iter()
                .map(|(_, r)| r.name)
                .collect::<BTreeSet<_>>()
                .len()
                >= 3
            {
                candidates.push((*class, helper, methods));
            }
        }
    }
    if candidates.is_empty() || !recovery_complete {
        return Ok(Mappings {
            roles: BTreeMap::new(),
            ambiguous: candidates.len() > 1,
            parser_candidates: candidates.len(),
            recovery_complete,
            creation: None,
        });
    }
    let parser_candidates = candidates.len();
    let creation = if parser_candidates == 1 {
        creation_model(classes, candidates[0].0)
    } else {
        None
    };
    let mut entries = Vec::new();
    for (class, _, methods) in candidates {
        entries.extend(methods);
        entries.extend(parameter_roles(class, cancel)?);
        entries.extend(query_roles(class, cancel)?);
    }
    let mut roles = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for (descriptor, role) in entries {
        if let Some(existing) = roles.get(&descriptor) {
            let existing: &Role = existing;
            if existing.name != role.name {
                conflicts.insert(descriptor.clone());
            }
        } else {
            roles.insert(descriptor, role);
        }
    }
    let ambiguous = !conflicts.is_empty();
    for conflict in conflicts {
        roles.remove(&conflict);
    }
    Ok(Mappings {
        roles,
        ambiguous,
        parser_candidates,
        recovery_complete,
        creation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_ir::{PoolReference, RegisterOperand, ValueKind};
    fn operand(register: u16) -> RegisterOperand {
        RegisterOperand {
            register,
            kind: ValueKind::Reference,
        }
    }
    fn ins(
        pc: usize,
        opcode: u8,
        reads: &[u16],
        writes: &[u16],
        reference: Option<PoolReference>,
        target: Option<usize>,
    ) -> Instruction {
        Instruction {
            pc,
            width: 1,
            opcode,
            reads: reads.iter().copied().map(operand).collect(),
            writes: writes.iter().copied().map(operand).collect(),
            may_throw: false,
            literal: None,
            reference,
            prototype: None,
            branch_target: target,
            payload_target: None,
        }
    }
    #[test]
    fn guarded_branch_allows_safe_constants_and_rejects_wrong_condition_or_external_entry() {
        let pool = DexSymbols {
            types: vec!["Lrenamed/Url;".into()],
            ..Default::default()
        };
        let type_ref = Some(PoolReference {
            kind: PoolKind::Type,
            index: 0,
        });
        let mut code = vec![
            ins(0, 0x20, &[5], &[0], type_ref, None),
            ins(2, 0x1a, &[], &[1], None, None),
            ins(4, 0x12, &[], &[2], None, None),
            ins(5, 0x38, &[0], &[], None, Some(10)),
            ins(7, 0x1f, &[5], &[5], type_ref, None),
            ins(9, 0x0e, &[], &[], None, None),
            ins(10, 0x20, &[5], &[0], type_ref, None),
        ];
        assert_eq!(branch(&pool, &code, 0).map(|x| x.0), Some("Lrenamed/Url;"));
        code[3].reads[0].register = 1;
        assert!(branch(&pool, &code, 0).is_none());
        code[3].reads[0].register = 0;
        code[1].opcode = 0x18;
        code[1].writes[0] = RegisterOperand {
            register: 0,
            kind: ValueKind::Wide64,
        };
        assert!(branch(&pool, &code, 0).is_none());
        code[1].opcode = 0x1a;
        code[1].writes[0] = operand(1);
        code.push(ins(12, 0x28, &[], &[], None, Some(7)));
        assert!(branch(&pool, &code, 0).is_none());
    }
    #[test]
    fn query_guard_rejects_bypassed_diagnostic_conflict_and_false_flag() {
        let pool = DexSymbols {
            strings: vec![
                "A @Url parameter must not come after a @Query.".into(),
                "A @Url parameter must not come after a @QueryMap.".into(),
            ],
            ..Default::default()
        };
        let text = |pc, index| {
            ins(
                pc,
                0x1a,
                &[],
                &[0],
                Some(PoolReference {
                    kind: PoolKind::String,
                    index,
                }),
                None,
            )
        };
        let mut code = vec![text(0, 0), ins(2, 0x27, &[0], &[], None, None)];
        assert_eq!(query_guard_diagnostic(&pool, &code, 0), Some("Query"));
        code.insert(0, ins(0, 0x28, &[], &[], None, Some(2)));
        assert_eq!(query_guard_diagnostic(&pool, &code, 0), None);
        let mut guards = BTreeMap::new();
        insert_query_guard(&mut guards, 7, "Query", 10);
        insert_query_guard(&mut guards, 7, "QueryMap", 20);
        assert_eq!(guards.get(&7), Some(&None));
        let mut class = link_fixture(vec![0x0012, 0x000e]);
        let method = &class.methods[0];
        let mut work = 0;
        assert_eq!(
            crate::call_arguments::constant_bits_at(
                method,
                1,
                0,
                &AtomicBool::new(false),
                &mut work
            )
            .unwrap(),
            Some(0)
        );
        class.methods[0].code.as_mut().unwrap().instructions[0] = 0x0112;
        let method = &class.methods[0];
        let mut work = 0;
        assert_eq!(
            crate::call_arguments::constant_bits_at(
                method,
                1,
                0,
                &AtomicBool::new(false),
                &mut work
            )
            .unwrap(),
            None
        );
    }
    fn link_fixture(words: Vec<u16>) -> DexClass {
        use crate::native_dex::DexCode;
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = "Lsample/Caller;".into();
        class.methods = vec![DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 8,
            code: Some(DexCode {
                registers: 5,
                ins: 0,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }];
        let symbols = DexSymbols {
            strings: vec![
                "https://one.example/".into(),
                "<init>".into(),
                "base".into(),
                "build".into(),
                "create".into(),
                "unknown".into(),
                "https://two.example/".into(),
            ],
            types: vec![
                "Lrenamed/Builder;".into(),
                "Lrenamed/Retrofit;".into(),
                "Lsample/Service;".into(),
                "Lsample/Helper;".into(),
            ],
            protos: vec![
                ("V".into(), vec![]),
                (
                    "Lrenamed/Builder;".into(),
                    vec!["Ljava/lang/String;".into()],
                ),
                ("Lrenamed/Retrofit;".into(), vec![]),
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Ljava/lang/Class;".into()],
                ),
                ("V".into(), vec!["Lrenamed/Builder;".into()]),
                (
                    "Ljava/lang/Object;".into(),
                    vec!["Ljava/lang/String;".into()],
                ),
            ],
            methods: vec![
                (0, 0, 1),
                (0, 1, 2),
                (0, 2, 3),
                (1, 3, 4),
                (3, 4, 5),
                (0, 5, 2),
            ],
            ..Default::default()
        };
        symbols.origin.set("classes2.dex".into()).unwrap();
        class.symbols = std::sync::Arc::new(symbols);
        class
    }
    fn link_model() -> Mappings {
        Mappings {
            roles: BTreeMap::new(),
            ambiguous: false,
            parser_candidates: 1,
            recovery_complete: true,
            creation: Some(CreationModel {
                builder: "Lrenamed/Builder;".into(),
                retrofit: "Lrenamed/Retrofit;".into(),
                base_url_method: "base".into(),
                build_method: "build".into(),
                create_method: "create".into(),
            }),
        }
    }
    fn chain() -> Vec<u16> {
        vec![
            0x0022, 0, 0x1070, 0, 0, 0x011a, 0, 0x206e, 1, 0x0010, 0x040c, 0x106e, 2, 0, 0x020c,
            0x031c, 2, 0x206e, 3, 0x0032, 0x000e,
        ]
    }
    #[test]
    fn creation_links_require_exact_prototypes_and_reject_escape_or_branch() {
        let model = link_model();
        let class = link_fixture(chain());
        let links = creation_links(&[&class], &model, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            links.items["Lsample/Service;"][0]["base_url_candidate"],
            "https://one.example/"
        );
        assert_eq!(links.items["Lsample/Service;"][0]["base_url_pc"], 7);
        let mut escaped = chain();
        escaped.splice(11..11, [0x1071, 4, 0]);
        assert!(
            creation_links(&[&link_fixture(escaped)], &model, &AtomicBool::new(false))
                .unwrap()
                .items
                .is_empty()
        );
        let mut overload = chain();
        overload[8] = 5;
        assert!(
            creation_links(&[&link_fixture(overload)], &model, &AtomicBool::new(false))
                .unwrap()
                .items
                .is_empty()
        );
        let mut branched = chain();
        branched.splice(5..5, [0x0038, 2]);
        assert!(
            creation_links(&[&link_fixture(branched)], &model, &AtomicBool::new(false))
                .unwrap()
                .items
                .is_empty()
        );
    }
    #[test]
    fn creation_link_snapshot_keeps_first_url_with_second_builder_or_later_mutation() {
        let model = link_model();
        let mut later = chain();
        later.splice(15..15, [0x011a, 6, 0x206e, 1, 0x0010, 0x000c]);
        let links =
            creation_links(&[&link_fixture(later)], &model, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            links.items["Lsample/Service;"][0]["base_url_candidate"],
            "https://one.example/"
        );
        assert_eq!(links.items["Lsample/Service;"][0]["base_url_pc"], 7);
        let mut two = chain();
        two.splice(
            15..15,
            [
                0x0422, 0, 0x1070, 0, 0x0004, 0x011a, 6, 0x206e, 1, 0x0014, 0x040c,
            ],
        );
        let links = creation_links(&[&link_fixture(two)], &model, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            links.items["Lsample/Service;"][0]["base_url_candidate"],
            "https://one.example/"
        );
    }
    #[test]
    fn creation_link_resolves_each_dex_string_pool_independently() {
        let first = link_fixture(chain());
        let mut second = link_fixture(chain());
        std::sync::Arc::get_mut(&mut second.symbols)
            .unwrap()
            .strings[0] = "https://other.example/".into();
        let links =
            creation_links(&[&first, &second], &link_model(), &AtomicBool::new(false)).unwrap();
        let urls: Vec<_> = links.items["Lsample/Service;"]
            .iter()
            .map(|v| v["base_url_candidate"].as_str().unwrap())
            .collect();
        assert_eq!(urls, ["https://one.example/", "https://other.example/"]);
    }
    #[test]
    #[ignore = "Set RDX_RETROFIT_ACCEPTANCE_APK to the pinned ServiceNow APK"]
    fn real_apk_recovers_renamed_roles_and_five_url_annotations() {
        use sha2::{Digest, Sha256};
        use std::{fs::File, io::Read};
        let path = std::env::var("RDX_RETROFIT_ACCEPTANCE_APK").expect("APK path");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "7ee97a910577b5c60db29cabaf226c3640ed7599f0e29564f4b8f3b92b6dc454"
        );
        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut classes = Vec::new();
        for n in 0..zip.len() {
            let mut entry = zip.by_index(n).unwrap();
            let filename = entry.name().to_owned();
            if !filename.starts_with("classes") || !filename.ends_with(".dex") {
                continue;
            }
            let mut dex = Vec::new();
            entry.read_to_end(&mut dex).unwrap();
            let parsed = crate::native_dex::parse(&dex).unwrap();
            if let Some(first) = parsed.classes.first() {
                first.symbols.origin.set(filename).unwrap();
            }
            classes.extend(parsed.classes);
        }
        let refs = classes.iter().collect::<Vec<_>>();
        let mappings = discover(&refs, &AtomicBool::new(false)).unwrap();
        assert_eq!(mappings.get("Lkd0/y;").map(|r| r.name), Some("Url"));
        assert_eq!(mappings.get("Lkd0/i;").map(|r| r.name), Some("Header"));
        assert_eq!(mappings.get("Lkd0/f;").map(|r| r.name), Some("GET"));
        assert_eq!(mappings.get("Lkd0/t;").map(|r| r.name), Some("Query"));
        assert_eq!(mappings.get("Lkd0/v;").map(|r| r.name), Some("QueryName"));
        assert_eq!(mappings.get("Lkd0/u;").map(|r| r.name), Some("QueryMap"));
        let mut owners = BTreeSet::new();
        let mut named = BTreeSet::new();
        for class in &classes {
            let Some(dir) = class.symbols.annotations.get(&class.annotations_offset) else {
                continue;
            };
            for (mi, params) in dir.parameters.iter().enumerate() {
                for (pi, set) in params.iter().enumerate() {
                    if let Some(set) = set {
                        for annotation in set.iter() {
                            let ty = &class.symbols.types[annotation.type_idx as usize];
                            if matches!(ty.as_ref(), "Lkd0/t;" | "Lkd0/i;") {
                                let values = crate::annotation_values::elements_value(
                                    &annotation.elements,
                                    &class.symbols,
                                    &mut 4096,
                                )
                                .unwrap();
                                if let Some(value) = values["value"]["value"].as_str() {
                                    named.insert((
                                        method_id(&class.methods[mi]),
                                        pi,
                                        ty.to_string(),
                                        value.to_owned(),
                                    ));
                                }
                            }
                        }
                    }
                    if set.as_ref().is_some_and(|set| {
                        set.iter().any(|a| {
                            class
                                .symbols
                                .types
                                .get(a.type_idx as usize)
                                .is_some_and(|t| t.as_ref() == "Lkd0/y;")
                        })
                    }) {
                        owners.insert((method_id(&class.methods[mi]), pi));
                    }
                }
            }
        }
        assert_eq!(owners.len(), 5);
        assert!(owners.contains(&("wv.j1.a(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Li20/f;)Ljava/lang/Object;".to_owned(),0)));
        let a="wv.j1.a(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Li20/f;)Ljava/lang/Object;".to_owned();
        let b="wv.j1.b(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Li20/f;)Ljava/lang/Object;".to_owned();
        assert!(named.contains(&(a.clone(), 1, "Lkd0/i;".into(), "glide_tdn_tkn".into())));
        assert!(named.contains(&(a, 2, "Lkd0/t;".into(), "sn_mobile_properties".into())));
        assert!(named.contains(&(b, 1, "Lkd0/t;".into(), "url".into())));
        let page_args =
            json!({"projection":"services","package_prefix":"wv.j1","offset":0,"limit":1});
        let first =
            super::super::services(&refs, &page_args, &AtomicBool::new(false), 8_000_000).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        assert!(first["matching_candidates_so_far"].as_u64().unwrap() >= 2);
        assert_eq!(first["next_offset"], 1);
        let mut second_args = page_args;
        second_args["offset"] = first["next_offset"].clone();
        let second =
            super::super::services(&refs, &second_args, &AtomicBool::new(false), 8_000_000)
                .unwrap();
        assert_ne!(
            first["items"][0]["method_id"],
            second["items"][0]["method_id"]
        );
    }
}
