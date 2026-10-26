//! Source-only private instance forwarding for static DEX invoke-super bridges.
use super::{java_type, names};
use crate::{
    native_calls::BoundCalls,
    native_dex::{DexClass, DexMethod},
    native_ir::DecodedMethod,
};
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct Bridge {
    pub name: String,
    owner: String,
    target: String,
    parameters: Vec<std::sync::Arc<str>>,
    result: std::sync::Arc<str>,
    sites: Vec<(usize, usize)>,
    type_parameter: String,
}

impl Bridge {
    pub fn source(&self) -> Option<String> {
        let params = self
            .parameters
            .iter()
            .enumerate()
            .map(|(i, ty)| Some(format!("{} p{i}", java_type(ty).ok()?)))
            .collect::<Option<Vec<_>>>()?
            .join(", ");
        let args = (0..self.parameters.len())
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let result = java_type(&self.result).ok()?;
        let target = names::member(&self.target).ok()?;
        let ret = if self.result.as_ref() == "V" {
            ""
        } else {
            "return "
        };
        let end = if self.result.as_ref() == "V" {
            " return;"
        } else {
            ""
        };
        Some(format!(
            "\n    // Source forwarding bridge for nonvirtual {}.{} dispatch.\n    @java.lang.SuppressWarnings(\"unchecked\")\n    private <{} extends java.lang.Throwable> {result} {}({params}) throws {} {{\n        try {{ {ret}super.{target}({args});{end} }}\n        catch (java.lang.Throwable failure) {{ throw ({}) failure; }}\n    }}\n",
            self.owner,
            self.target,
            self.type_parameter,
            self.name,
            self.type_parameter,
            self.type_parameter
        ))
    }
}

fn cast_origin(
    class: &DexClass,
    analysis: &crate::native_method::MethodAnalysis<'_>,
    pc: usize,
) -> bool {
    use crate::native_ssa::DefinitionKind;
    use std::collections::HashSet;
    let prove = || -> Option<()> {
        let receiver = analysis
            .calls()
            .calls
            .iter()
            .find(|c| c.pc == pc)?
            .receiver
            .as_ref()?;
        if receiver.words.len() != 1 {
            return None;
        }
        let ssa = analysis.ssa();
        let mut pending = receiver.words.clone();
        let mut seen = HashSet::new();
        let mut rooted = false;
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if seen.len() > 4096 {
                return None;
            }
            match &ssa.definitions.get(id)?.kind {
                DefinitionKind::Instruction { pc, word, .. } if *word == 0 => {
                    let raw = analysis
                        .instructions()
                        .instructions
                        .iter()
                        .find(|i| i.pc == *pc)?;
                    let ins = ssa.instructions.iter().find(|i| i.pc == *pc)?;
                    if ins.writes.len() != 1 || ins.writes[0].words.as_slice() != [id] {
                        return None;
                    }
                    if raw.opcode == 0x1f {
                        if class.symbols.types.get(raw.reference?.index as usize)?
                            != &class.descriptor
                        {
                            return None;
                        }
                        rooted = true;
                    } else {
                        if !matches!(raw.opcode, 0x07..=0x09)
                            || ins.reads.len() != 1
                            || ins.reads[0].words.len() != 1
                        {
                            return None;
                        }
                        pending.push(ins.reads[0].words[0]);
                    }
                }
                DefinitionKind::Phi { block } => {
                    let phi = ssa.phis.iter().find(|p| p.result == id)?;
                    let expected: HashSet<_> = ssa
                        .graph
                        .blocks
                        .iter()
                        .enumerate()
                        .filter_map(|(from, b)| {
                            (ssa.reachable[from] && b.successors.iter().any(|e| e.target == *block))
                                .then_some(Some(from))
                        })
                        .chain((*block == 0).then_some(None))
                        .collect();
                    let actual: HashSet<_> = phi.incoming.iter().map(|(from, _)| *from).collect();
                    if !ssa.reachable[*block]
                        || phi.block != *block
                        || phi.incoming.is_empty()
                        || phi.register != ssa.definitions[id].register
                        || actual != expected
                        || actual.len() != phi.incoming.len()
                    {
                        return None;
                    }
                    pending.extend(phi.incoming.iter().map(|(_, id)| *id));
                }
                _ => return None,
            }
        }
        // Copy cycles carry no new value. All external inputs must end at a
        // successful exact cast; undefined, parameter and non-copy roots fail.
        rooted.then_some(())
    };
    prove().is_some()
}

fn discover(class: &DexClass) -> Option<Vec<Bridge>> {
    if class.access_flags & (0x200 | 0x2000 | 0x4000) != 0
        || class.methods.len() > 8192
        || !super::source_header_authority(class)
    {
        return None;
    }
    let hierarchy = class.symbols.hierarchy.get()?;
    let mut bridges: BTreeMap<u32, Bridge> = BTreeMap::new();
    let mut words = 0usize;
    for (method_index, method) in class
        .methods
        .iter()
        .enumerate()
        .filter(|(_, m)| m.access_flags & 8 != 0)
    {
        let Some(code) = &method.code else {
            continue;
        };
        words = words.checked_add(code.instructions.len())?;
        if words > 65_536 {
            return None;
        }
        if !code
            .instructions
            .iter()
            .any(|w| matches!(*w as u8, 0x6f | 0x75))
        {
            continue;
        }
        let ir = DecodedMethod::decode(code).ok()?;
        let calls = BoundCalls::bind(code, &ir, &class.symbols).ok()?;
        let cfg = crate::native_cfg::ControlFlowGraph::build(code).ok()?;
        let analysis = std::cell::OnceCell::new();
        let mut parameter_register = code.registers.checked_sub(code.ins)?;
        let mut receivers = Vec::new();
        for ty in &method.parameters {
            if ty == &class.descriptor {
                receivers.push(parameter_register);
            }
            parameter_register =
                parameter_register.checked_add(if matches!(ty.as_ref(), "J" | "D") {
                    2
                } else {
                    1
                })?;
        }
        if parameter_register != code.registers {
            return None;
        }
        for instruction in ir
            .instructions
            .iter()
            .filter(|i| matches!(i.opcode, 0x6f | 0x75))
        {
            let call = calls.calls.iter().find(|c| c.pc == instruction.pc)?;
            let receiver = call.receiver.as_ref()?.register;
            let block = &cfg.blocks[*cfg.block_at.get(&instruction.pc)?];
            let last_write = ir.instructions.iter().rev().find(|i| {
                i.pc >= block.start
                    && i.pc < instruction.pc
                    && i.writes.iter().any(|w| {
                        receiver >= w.register
                            && usize::from(receiver - w.register) < w.kind.word_count()
                    })
            });
            let cast_receiver = last_write.is_some_and(|i| {
                i.opcode == 0x1f
                    && i.reference
                        .and_then(|r| class.symbols.types.get(r.index as usize))
                        == Some(&class.descriptor)
            });
            // A successful cast in this basic block establishes the exact
            // bridge receiver type, including array-extracted arguments.
            if !cast_receiver
                && !receivers.contains(&receiver)
                && !analysis
                    .get_or_init(|| crate::native_method::MethodAnalysis::build(class, method).ok())
                    .as_ref()
                    .is_some_and(|analysis| cast_origin(class, analysis, instruction.pc))
            {
                continue;
            }
            let crate::native_calls::CallTarget::Method {
                method_index: target_index,
                declaring_type: owner,
                name,
                prototype_index,
                ..
            } = &call.target
            else {
                return None;
            };
            let (result, parameters) = class.symbols.protos.get(*prototype_index as usize)?;
            if !hierarchy.callable_super_declaration(
                &class.descriptor,
                owner,
                name,
                parameters,
                result,
            ) {
                continue;
            }
            if !bridges.contains_key(target_index) {
                if bridges.len() >= 64 {
                    return None;
                }
                let fresh = |base: &str| {
                    (0..64)
                        .map(|suffix| format!("{base}_{suffix}"))
                        .find(|candidate| {
                            !class.methods.iter().any(|m| {
                                names::member(&m.name).ok().as_deref() == Some(candidate.as_str())
                            }) && !class.symbols.types.iter().any(|t| {
                                t.trim_start_matches('[')
                                    .trim_start_matches('L')
                                    .trim_end_matches(';')
                                    .split(['/', '$'])
                                    .any(|s| s == candidate)
                            })
                        })
                };
                let bridge = Bridge {
                    name: fresh(&format!("_rdx_super_{target_index:x}"))?,
                    owner: java_type(owner).ok()?,
                    target: name.to_string(),
                    parameters: parameters.clone(),
                    result: result.clone(),
                    sites: Vec::new(),
                    type_parameter: fresh(&format!("RdxSuperThrowable{target_index:x}"))?,
                };
                bridge.source()?;
                bridges.insert(*target_index, bridge);
            }
            bridges
                .get_mut(target_index)?
                .sites
                .push((method_index, instruction.pc));
        }
    }
    Some(bridges.into_values().collect())
}

pub(super) fn for_site(class: &DexClass, method: &DexMethod, pc: usize) -> Option<Bridge> {
    let mut matches = class.methods.iter().enumerate().filter(|(_, m)| {
        m.name == method.name
            && m.parameters == method.parameters
            && m.return_type == method.return_type
            && m.access_flags == method.access_flags
            && m.code.as_ref().map(|c| c.offset) == method.code.as_ref().map(|c| c.offset)
    });
    let index = matches.next()?.0;
    if matches.next().is_some() {
        return None;
    }
    discover(class)?
        .into_iter()
        .find(|p| p.sites.contains(&(index, pc)))
}

pub(super) fn definitions(class: &DexClass) -> Vec<String> {
    discover(class)
        .unwrap_or_default()
        .iter()
        .filter_map(Bridge::source)
        .collect()
}
