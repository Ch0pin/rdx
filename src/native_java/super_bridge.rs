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
            if !receivers.contains(&call.receiver.as_ref()?.register) {
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
