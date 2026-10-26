//! Normal acyclic <clinit> exits, including JVM-default blank-final paths.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexClass, DexMethod, DexValue},
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
pub(super) const LABEL: &str = "rdxInitializerExit";
pub(super) struct Assignment {
    pub name: String,
    pub value: &'static str,
    pub label: String,
}
pub(super) struct Plan {
    pub returns: BTreeMap<usize, Vec<Assignment>>,
}
fn default_value(ty: &str, value: Option<&DexValue>) -> Option<&'static str> {
    let valid = match (ty, value) {
        (_, None) => true,
        (t, Some(DexValue::Null)) if t.starts_with('L') || t.starts_with('[') => true,
        ("Z", Some(DexValue::Boolean(false))) => true,
        ("B", Some(DexValue::Byte(0)))
        | ("S", Some(DexValue::Short(0)))
        | ("C", Some(DexValue::Char(0)))
        | ("I", Some(DexValue::Int(0)))
        | ("J", Some(DexValue::Long(0)))
        | ("F", Some(DexValue::Float(0)))
        | ("D", Some(DexValue::Double(0))) => true,
        _ => false,
    };
    if !valid {
        return None;
    }
    match ty {
        "Z" => Some("false"),
        "B" | "S" | "C" | "I" => Some("0"),
        "J" => Some("0L"),
        "F" => Some("0.0f"),
        "D" => Some("0.0d"),
        t if t.starts_with('L') || t.starts_with('[') => Some("null"),
        _ => None,
    }
}
pub(super) fn select(class: &DexClass, method: &DexMethod) -> Option<Plan> {
    let code = method.code.as_ref()?;
    if method.name.as_ref() != "<clinit>"
        || method.declaring_type != class.descriptor
        || method.return_type.as_ref() != "V"
        || !method.parameters.is_empty()
        || method.access_flags & 8 == 0
        || code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 8192
        || class.fields.len() > 4096
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 1024
        || ir
            .instructions
            .iter()
            .any(|i| matches!(i.opcode, 0x1d | 0x1e | 0x0f..=0x11))
    {
        return None;
    }
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut finals = BTreeMap::new();
    let mut stores = BTreeMap::new();
    for block in &cfg.blocks {
        for pc in &block.instructions {
            let i = at.get(pc)?;
            if matches!(i.opcode, 0x67..=0x6d) {
                let index = *code.instructions.get(pc + 1)? as usize;
                let (owner, ty, name) = *class.symbols.fields.get(index)?;
                let owner = class.symbols.types.get(owner as usize)?;
                let ty = class.symbols.types.get(ty as usize)?;
                let name = class.symbols.strings.get(name as usize)?;
                if owner != &class.descriptor {
                    continue;
                }
                let mut matches = class.fields.iter().filter(|f| {
                    f.declaring_type == *owner
                        && f.field_type == *ty
                        && f.name.as_ref() == name
                        && f.is_static
                        && f.access_flags & 8 != 0
                });
                let field = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                if field.access_flags & 0x10 == 0 {
                    continue;
                }
                let key = (name.clone(), ty.to_string());
                let position = class
                    .fields
                    .iter()
                    .filter(|f| f.is_static)
                    .position(|f| std::ptr::eq(f, field))?;
                let value = default_value(ty, class.static_values.get(position))?;
                finals.insert(
                    key.clone(),
                    (
                        super::names::member(name).ok()?,
                        value,
                        format!(
                            "{}.{}:{}",
                            owner
                                .trim_start_matches('L')
                                .trim_end_matches(';')
                                .replace('/', "."),
                            name,
                            ty
                        ),
                    ),
                );
                if finals.len() > 64 {
                    return None;
                }
                stores.insert(*pc, key);
            }
        }
    }
    let mut degree = vec![0usize; cfg.blocks.len()];
    for block in &cfg.blocks {
        for edge in &block.successors {
            if edge.kind != EdgeKind::Normal {
                return None;
            }
            degree[edge.target] += 1;
        }
    }
    let entry = *cfg.block_at.get(&0)?;
    if degree[entry] != 0 {
        return None;
    }
    let mut queue = VecDeque::from([entry]);
    let mut states: Vec<Option<BTreeSet<(String, String)>>> = vec![None; cfg.blocks.len()];
    states[entry] = Some(BTreeSet::new());
    let mut returns = BTreeMap::new();
    let mut visited = 0;
    while let Some(b) = queue.pop_front() {
        visited += 1;
        let mut state = states[b].clone()?;
        for pc in &cfg.blocks[b].instructions {
            if let Some(key) = stores.get(pc)
                && !state.insert(key.clone())
            {
                return None;
            }
            if at.get(pc)?.opcode == 0x0e {
                let mut assignments = vec![];
                for (key, (name, value, label)) in &finals {
                    if !state.contains(key) {
                        assignments.push(Assignment {
                            name: name.clone(),
                            value,
                            label: label.clone(),
                        });
                    }
                }
                returns.insert(*pc, assignments);
            }
        }
        for edge in &cfg.blocks[b].successors {
            match &states[edge.target] {
                Some(previous) if previous != &state => return None,
                Some(_) => {}
                None => states[edge.target] = Some(state.clone()),
            }
            degree[edge.target] = degree[edge.target].checked_sub(1)?;
            if degree[edge.target] == 0 {
                queue.push_back(edge.target);
            }
        }
    }
    // A lone normal exit can precede a throwing arm. It needs the same labelled
    // exit as multiple returns; stripping a terminal return cannot represent it.
    if visited != cfg.blocks.len()
        || returns.is_empty()
        || (returns.len() == 1 && ir.instructions.last()?.opcode == 0x0e)
    {
        return None;
    }
    Some(Plan { returns })
}
