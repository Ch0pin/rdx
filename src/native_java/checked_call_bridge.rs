//! Source-only static forwarding keeps checked Throwable identity without
//! changing a caller's override declaration. Original argument order is retained.
use super::{java_type, names};
use crate::{
    native_calls::BoundCalls,
    native_dex::{DexClass, DexMethod},
    native_hierarchy::Relation,
    native_ir::DecodedMethod,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(super) struct Bridge {
    pub name: String,
    owner: Arc<str>,
    target: String,
    parameters: Vec<Arc<str>>,
    result: Arc<str>,
    type_parameter: String,
    sites: Vec<(usize, usize)>,
}
impl Bridge {
    fn source(&self) -> Option<String> {
        let owner = java_type(&self.owner).ok()?;
        let target = names::member(&self.target).ok()?;
        let result = java_type(&self.result).ok()?;
        let parameters = self
            .parameters
            .iter()
            .enumerate()
            .map(|(i, ty)| Some(format!("{} p{i}", java_type(ty).ok()?)))
            .collect::<Option<Vec<_>>>()?
            .join(", ");
        let arguments = (0..self.parameters.len())
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(", ");
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
            "\n    // Source forwarding preserves the original checked throwable identity.\n    @java.lang.SuppressWarnings(\"unchecked\")\n    private static <{} extends java.lang.Throwable> {result} {}({parameters}) throws {} {{\n        try {{ {ret}{owner}.{target}({arguments});{end} }}\n        catch (java.lang.Throwable failure) {{ throw ({}) failure; }}\n    }}\n",
            self.type_parameter, self.name, self.type_parameter, self.type_parameter
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
    for (method_index, method) in class.methods.iter().enumerate() {
        let Some(code) = &method.code else { continue };
        words = words.checked_add(code.instructions.len())?;
        if words > 65_536 {
            return None;
        }
        if !code
            .instructions
            .iter()
            .any(|w| matches!(*w as u8, 0x71 | 0x77))
        {
            continue;
        }
        let ir = DecodedMethod::decode(code).ok()?;
        let bound = BoundCalls::bind(code, &ir, &class.symbols).ok()?;
        for call in bound.calls.iter().filter(|call| {
            matches!(
                ir.instructions
                    .iter()
                    .find(|i| i.pc == call.pc)
                    .map(|i| i.opcode),
                Some(0x71 | 0x77)
            )
        }) {
            let crate::native_calls::CallTarget::Method {
                method_index: target_index,
                declaring_type: owner,
                name,
                prototype_index,
                ..
            } = &call.target
            else {
                continue;
            };
            let (result, parameters) = class.symbols.protos.get(*prototype_index as usize)?;
            let Some(declaration) = hierarchy.exact_platform_source_declaration(
                &class.descriptor,
                owner,
                name,
                parameters,
                result,
                true,
            ) else {
                continue;
            };
            let checked: Vec<_> = declaration
                .thrown_types
                .iter()
                .filter(|ty| {
                    hierarchy.assignable(ty, "Ljava/lang/RuntimeException;") != Relation::Proven
                        && hierarchy.assignable(ty, "Ljava/lang/Error;") != Relation::Proven
                })
                .collect();
            if !checked.iter().any(|ty| {
                let caught = code.try_regions.iter().any(|region| {
                    region.start as usize <= call.pc
                        && call.pc < region.end as usize
                        && region.catches.iter().any(|(catch, _)| {
                            catch.as_ref().is_none_or(|catch| {
                                hierarchy.assignable(ty, catch) == Relation::Proven
                            })
                        })
                });
                !caught
                    && !method
                        .thrown_types
                        .iter()
                        .any(|declared| hierarchy.assignable(ty, declared) == Relation::Proven)
                    && !hierarchy.permits_inferred_checked_throw(class, method, ty)
                    && !super::inherited_override_exception(class, method)
                        .into_iter()
                        .any(|parent| hierarchy.assignable(ty, parent) == Relation::Proven)
            }) {
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
                    name: fresh(&format!("_rdx_checked_{target_index:x}"))?,
                    owner: owner.clone(),
                    target: name.to_string(),
                    parameters: parameters.clone(),
                    result: result.clone(),
                    type_parameter: fresh(&format!("RdxCallThrowable{target_index:x}"))?,
                    sites: Vec::new(),
                };
                bridge.source()?;
                bridges.insert(*target_index, bridge);
            }
            bridges
                .get_mut(target_index)?
                .sites
                .push((method_index, call.pc));
        }
    }
    Some(bridges.into_values().collect())
}
pub(super) fn for_site(class: &DexClass, method: &DexMethod, pc: usize) -> Option<Bridge> {
    // Rendering asks about every static call. Resolve only the exact pool entry
    // first; class-wide decoding is needed solely for uncovered checked calls.
    // discover remains the single authority for helper names and helper-set limits.
    let code = method.code.as_ref()?;
    if !matches!(*code.instructions.get(pc)? as u8, 0x71 | 0x77) {
        return None;
    }
    let target = *code.instructions.get(pc.checked_add(1)?)? as usize;
    let (owner, proto, name) = *class.symbols.methods.get(target)?;
    let owner = class.symbols.types.get(owner as usize)?;
    let name = class.symbols.strings.get(name as usize)?;
    let (result, parameters) = class.symbols.protos.get(proto as usize)?;
    let hierarchy = class.symbols.hierarchy.get()?;
    let declaration = hierarchy.exact_platform_source_declaration(
        &class.descriptor,
        owner,
        name,
        parameters,
        result,
        true,
    )?;
    let needs_bridge = declaration.thrown_types.iter().any(|ty| {
        hierarchy.assignable(ty, "Ljava/lang/RuntimeException;") != Relation::Proven
            && hierarchy.assignable(ty, "Ljava/lang/Error;") != Relation::Proven
            && !method
                .thrown_types
                .iter()
                .any(|declared| hierarchy.assignable(ty, declared) == Relation::Proven)
            && !code.try_regions.iter().any(|region| {
                region.start as usize <= pc
                    && pc < region.end as usize
                    && region.catches.iter().any(|(catch, _)| {
                        catch
                            .as_ref()
                            .is_none_or(|catch| hierarchy.assignable(ty, catch) == Relation::Proven)
                    })
            })
            && !hierarchy.permits_inferred_checked_throw(class, method, ty)
            && !super::inherited_override_exception(class, method)
                .into_iter()
                .any(|parent| hierarchy.assignable(ty, parent) == Relation::Proven)
    });
    if !needs_bridge {
        return None;
    }
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
