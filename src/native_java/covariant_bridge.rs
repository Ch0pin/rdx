//! Proof that a DEX bridge method is the compiler's covariant-return forward
//! to a sibling declaration with the same name and parameters.
//!
//! `javac` and R8 emit `ACC_BRIDGE | ACC_SYNTHETIC` methods whose body is
//! exactly `return this.<name>(<parameters>)` against the override that
//! narrows an inherited return type. Java source never contains such a method,
//! so a proven bridge folds out of class presentations and does not count as a
//! duplicate Java signature. Standalone method rendering still shows it.
use crate::{
    native_dex::{DexClass, DexMethod},
    native_ir::{DecodedMethod, PoolKind},
};
use std::collections::BTreeMap;

pub(crate) const REASON: &str = "proven_covariant_bridge";

fn reference_type(ty: &str) -> bool {
    ty.starts_with('L') || ty.starts_with('[')
}

/// The sibling declaration that `method` forwards to when `method` is a proven
/// covariant-return bridge; `None` for every other method.
pub(crate) fn target<'a>(class: &'a DexClass, method: &DexMethod) -> Option<&'a DexMethod> {
    if method.access_flags & 0x1040 != 0x1040
        || method.access_flags & 8 != 0
        || method.name.starts_with('<')
        || method.declaring_type != class.descriptor
        || !reference_type(&method.return_type)
    {
        return None;
    }
    let code = method.code.as_ref()?;
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 8 {
        return None;
    }
    let mut siblings = class.methods.iter().filter(|m| {
        !std::ptr::eq(*m, method)
            && m.access_flags & 0x40 == 0
            && m.name == method.name
            && m.parameters == method.parameters
    });
    let target = siblings.next()?;
    if siblings.next().is_some()
        || target.declaring_type != class.descriptor
        || target.access_flags & 8 != 0
        || target.return_type == method.return_type
        || !reference_type(&target.return_type)
    {
        return None;
    }
    let decoded = DecodedMethod::decode(code).ok()?;
    let [invoke, result, ret] = decoded.instructions.as_slice() else {
        return None;
    };
    if !matches!(invoke.opcode, 0x6e | 0x74) || result.opcode != 0x0c || ret.opcode != 0x11 {
        return None;
    }
    let reference = invoke.reference.filter(|r| r.kind == PoolKind::Method)?;
    let (owner, proto, name) = *class.symbols.methods.get(reference.index as usize)?;
    let owner = class.symbols.types.get(usize::from(owner))?;
    let name = class.symbols.strings.get(name as usize)?;
    let (proto_return, proto_parameters) = class.symbols.protos.get(usize::from(proto))?;
    if *owner != class.descriptor
        || name != method.name.as_ref()
        || *proto_return != target.return_type
        || *proto_parameters != method.parameters
    {
        return None;
    }
    let registers = usize::from(code.registers);
    let first = registers.checked_sub(usize::from(code.ins))?;
    let forwards_every_input = invoke
        .reads
        .iter()
        .map(|operand| usize::from(operand.register))
        .eq(first..registers);
    let [written] = result.writes.as_slice() else {
        return None;
    };
    let [returned] = ret.reads.as_slice() else {
        return None;
    };
    (forwards_every_input && written.register == returned.register).then_some(target)
}

pub(crate) fn folded(class: &DexClass, method: &DexMethod) -> bool {
    target(class, method).is_some()
}

/// Class methods that Java source declares: every method except proven bridges.
pub(crate) fn declared<'a>(class: &'a DexClass) -> impl Iterator<Item = &'a DexMethod> + 'a {
    class.methods.iter().filter(move |m| !folded(class, m))
}

/// Coverage identities of the proven bridges with the reason they are absent
/// from the class presentation.
pub(crate) fn folded_methods(name: &str, class: &DexClass) -> BTreeMap<String, String> {
    class
        .methods
        .iter()
        .filter(|m| folded(class, m))
        .map(|m| {
            (
                crate::source_coverage::method_id(name, m),
                REASON.to_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::target;
    use crate::native_dex::{DexClass, DexCode, DexMethod, DexSymbols};
    use std::sync::Arc;

    fn fixture() -> DexClass {
        let symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Box;".into()],
            strings: vec!["get".into()],
            protos: vec![
                ("Lsample/Item;".into(), vec![]),
                ("Ljava/lang/Object;".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 0)],
            ..Default::default()
        });
        let method = |return_type: &str, flags, words: &[u16]| DexMethod {
            declaring_type: "Lsample/Box;".into(),
            name: "get".into(),
            return_type: return_type.into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: flags,
            code: Some(DexCode {
                registers: 1,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words.to_vec(),
                offset: 0,
            }),
        };
        DexClass {
            descriptor: "Lsample/Box;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![
                method("Lsample/Item;", 1, &[0x0012, 0x0011]),
                method(
                    "Ljava/lang/Object;",
                    0x1041,
                    &[0x106e, 0, 0x0000, 0x000c, 0x0011],
                ),
            ],
            symbols,
        }
    }

    #[test]
    fn proven_bridge_names_its_target_and_folds_in_coverage() {
        let class = fixture();
        assert!(target(&class, &class.methods[0]).is_none());
        assert!(std::ptr::eq(
            target(&class, &class.methods[1]).unwrap(),
            &class.methods[0]
        ));
        let (code, coverage) = super::super::render_mixed_reported_with_nested(
            "sample.Box",
            &class,
            &[],
            None,
            &|| false,
        )
        .unwrap();
        assert!(code.source.contains("Item get()"), "{}", code.source);
        assert!(!code.source.contains("Object get()"), "{}", code.source);
        assert_eq!(coverage["counts"]["decompiled"], 2);
        assert_eq!(coverage["methods"][1]["representation"], "source_sugar");
        assert_eq!(coverage["methods"][1]["reason"], super::REASON);
        assert_eq!(coverage["java_complete"], true);
    }

    #[test]
    fn a_failing_sibling_keeps_the_bridge_visible_in_the_skeleton() {
        let mut class = fixture();
        // An unsupported opcode in the covariant override fails its body.
        class.methods[0].code.as_mut().unwrap().instructions = vec![0x00ff, 0x0011];
        let (code, coverage) = super::super::render_mixed_reported_with_nested(
            "sample.Box",
            &class,
            &[],
            None,
            &|| false,
        )
        .unwrap();
        assert!(code.source.contains("Object get()"), "{}", code.source);
        assert_eq!(coverage["counts"]["dex-fallback"], 1);
        assert_eq!(coverage["methods"][1]["representation"], "java");
    }
}
