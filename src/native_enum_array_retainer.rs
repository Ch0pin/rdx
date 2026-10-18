//! Loaded, nonpublishing constructors that retain an Enum[] without reading it.
use crate::native_dex::{DexClass, DexMethod};
use crate::native_ir::DecodedMethod;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
const ARRAY: &str = "[Ljava/lang/Enum;";

fn transparent_parent(m: &DexMethod, c: &DexClass, parent: &str) -> bool {
    let Some(code) = &m.code else {
        return false;
    };
    if !m.parameters.is_empty()
        || m.return_type.as_ref() != "V"
        || !m.thrown_types.is_empty()
        || m.access_flags & 5 == 0
        || m.access_flags & (8 | 0x100 | 0x400) != 0
        || code.tries != 0
        || !code.try_regions.is_empty()
        || code.registers != 1
        || code.ins != 1
    {
        return false;
    }
    let w = &code.instructions;
    if w.len() != 4 || w[0] != 0x1070 || w[2] != 0 || w[3] != 0x000e {
        return false;
    }
    let Some(&(owner, proto, name)) = c.symbols.methods.get(w[1] as usize) else {
        return false;
    };
    c.symbols
        .types
        .get(owner as usize)
        .is_some_and(|s| matches!(s.as_ref(), "Ljava/lang/Object;") || s.as_ref() == parent)
        && c.symbols
            .strings
            .get(name as usize)
            .is_some_and(|s| s == "<init>")
        && c.symbols
            .protos
            .get(proto as usize)
            .is_some_and(|(ret, args)| ret.as_ref() == "V" && args.is_empty())
}

pub(crate) fn collect(classes: &[&DexClass]) -> HashSet<Arc<str>> {
    let mut world = HashMap::new();
    let mut duplicates = HashSet::new();
    for c in classes {
        if world.insert(c.descriptor.as_ref(), *c).is_some() {
            duplicates.insert(c.descriptor.as_ref());
        }
    }
    if world.contains_key("Ljava/lang/Object;") {
        return HashSet::new();
    }
    let mut found = HashSet::new();
    let mut work = 0;
    for c in classes {
        if duplicates.contains(c.descriptor.as_ref())
            || c.access_flags & 0x11 != 0x11
            || c.access_flags & (0x200 | 0x400 | 0x4000) != 0
        {
            continue;
        }
        let fields: Vec<_> = c.fields.iter().filter(|f| !f.is_static).collect();
        if fields.len() != 1
            || fields[0].field_type.as_ref() != ARRAY
            || fields[0].access_flags & 0x10 == 0
        {
            continue;
        }
        let constructors: Vec<_> = c
            .methods
            .iter()
            .filter(|m| m.name.as_ref() == "<init>")
            .collect();
        if constructors.len() != 1 {
            continue;
        }
        let m = constructors[0];
        if m.parameters.iter().map(AsRef::as_ref).ne([ARRAY])
            || m.return_type.as_ref() != "V"
            || m.access_flags & 7 != 1
            || m.access_flags & (8 | 0x100 | 0x400) != 0
            || !m.thrown_types.is_empty()
        {
            continue;
        }
        let Some(code) = &m.code else {
            continue;
        };
        if code.registers != 2
            || code.ins != 2
            || code.tries != 0
            || !code.try_regions.is_empty()
            || code.instructions.len() != 6
        {
            continue;
        }
        let mut ancestors = HashSet::new();
        let mut parent = c.superclass.as_deref();
        let mut valid = true;
        while let Some(owner) = parent {
            if !ancestors.insert(owner) || ancestors.len() > 32 || duplicates.contains(owner) {
                valid = false;
                break;
            }
            if owner == "Ljava/lang/Object;" {
                break;
            }
            let Some(p) = world.get(owner) else {
                valid = false;
                break;
            };
            if p.access_flags & 0x200 != 0
                || p.fields.iter().any(|f| !f.is_static)
                || p.methods.iter().any(|m| m.name.as_ref() == "finalize")
            {
                valid = false;
                break;
            }
            let ctors: Vec<_> = p
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == "<init>")
                .collect();
            let Some(next) = p.superclass.as_deref() else {
                valid = false;
                break;
            };
            if !(ctors.is_empty() || ctors.len() == 1 && transparent_parent(ctors[0], p, next)) {
                valid = false;
                break;
            }
            parent = Some(next);
        }
        if !valid
            || !ancestors.contains("Ljava/lang/Object;")
            || c.methods.iter().any(|m| m.name.as_ref() == "finalize")
        {
            continue;
        }
        let Ok(ir) = DecodedMethod::decode(code) else {
            continue;
        };
        if ir.instructions.len() != 3 || ir.instructions.last().is_none_or(|i| i.opcode != 0x0e) {
            continue;
        }
        let mut stores = 0;
        let mut calls = 0;
        for i in &ir.instructions {
            work += 1;
            if work > 100_000 {
                return HashSet::new();
            }
            let w = &code.instructions;
            let pc = i.pc;
            match i.opcode {
                0x5b if w[pc] == 0x015b => {
                    let Some(&(owner, ty, name)) = c.symbols.fields.get(w[pc + 1] as usize) else {
                        valid = false;
                        break;
                    };
                    valid &= c.symbols.types.get(owner as usize) == Some(&c.descriptor)
                        && c.symbols
                            .types
                            .get(ty as usize)
                            .is_some_and(|s| s.as_ref() == ARRAY)
                        && c.symbols
                            .strings
                            .get(name as usize)
                            .is_some_and(|n| n.as_str() == fields[0].name.as_ref());
                    stores += 1;
                }
                0x70 if w[pc] == 0x1070 && w[pc + 2] == 0 => {
                    let Some(&(owner, proto, name)) = c.symbols.methods.get(w[pc + 1] as usize)
                    else {
                        valid = false;
                        break;
                    };
                    valid &= c
                        .symbols
                        .types
                        .get(owner as usize)
                        .is_some_and(|s| ancestors.contains(s.as_ref()))
                        && c.symbols
                            .strings
                            .get(name as usize)
                            .is_some_and(|s| s == "<init>")
                        && c.symbols
                            .protos
                            .get(proto as usize)
                            .is_some_and(|(r, a)| r.as_ref() == "V" && a.is_empty());
                    calls += 1;
                }
                0x0e => {}
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && stores == 1 && calls == 1 {
            found.insert(c.descriptor.clone());
        }
    }
    found
}
