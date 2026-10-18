//! Source-only rethrow of one original handler object; callee headers are irrelevant.
use crate::{
    native_cfg::EdgeKind,
    native_dex::{DexClass, DexMethod},
    native_method::MethodAnalysis,
    native_ssa::DefinitionKind,
};
use std::collections::{BTreeSet, HashMap, HashSet};
fn helper_name(class: &DexClass) -> Option<String> {
    if class.access_flags & (0x200 | 0x2000 | 0x4000) != 0
        || class.methods.len() > 8192
        || !super::source_header_authority(class)
    {
        return None;
    }
    let total = class
        .methods
        .iter()
        .filter_map(|m| m.code.as_ref())
        .try_fold(0usize, |n, c| n.checked_add(c.instructions.len()))?;
    if total > 65536 {
        return None;
    }
    (0..64)
        .map(|n| format!("_rdx_caught_throw_{n}"))
        .find(|candidate| {
            !class
                .methods
                .iter()
                .any(|m| super::names::member(&m.name).ok().as_deref() == Some(candidate))
                && !super::helper_type_names::raw_contains_helper(&class.symbols, candidate)
        })
}
// The alternative changes a throw obligation, never declaration or operand
// validity. Validate own source members independently of their checked headers.
fn own_source_operands(
    class: &DexClass,
    method: &DexMethod,
    analysis: &MethodAnalysis<'_>,
) -> Option<()> {
    let code = method.code.as_ref()?;
    for ins in &analysis.instructions().instructions {
        if matches!(ins.opcode, 0x52..=0x6d) {
            let (owner, ty, name) = *class
                .symbols
                .fields
                .get(*code.instructions.get(ins.pc + 1)? as usize)?;
            let owner = class.symbols.types.get(owner as usize)?;
            if owner != &class.descriptor {
                continue;
            }
            let ty = class.symbols.types.get(ty as usize)?;
            let name = class.symbols.strings.get(name as usize)?;
            let mut fields = class
                .fields
                .iter()
                .filter(|field| field.name.as_ref() == name);
            let field = fields.next()?;
            let static_op = ins.opcode >= 0x60;
            let family = (ins.opcode - if static_op { 0x60 } else { 0x52 }) % 7;
            let typed = match family {
                0 => matches!(ty.as_ref(), "I" | "F"),
                1 => matches!(ty.as_ref(), "J" | "D"),
                2 => ty.starts_with('L') || ty.starts_with('['),
                3 => ty.as_ref() == "Z",
                4 => ty.as_ref() == "B",
                5 => ty.as_ref() == "C",
                6 => ty.as_ref() == "S",
                _ => false,
            };
            if fields.next().is_some()
                || field.declaring_type != class.descriptor
                || field.field_type != *ty
                || !typed
                || field.is_static != static_op
                || (field.access_flags & 8 != 0) != static_op
                || field.access_flags & !0x10df != 0
                || field.access_flags & 0x50 == 0x50
                || (field.access_flags & 7).count_ones() > 1
            {
                return None;
            }
        } else if matches!(ins.opcode, 0x6e..=0x72 | 0x74..=0x78) {
            let (owner, proto, name) = *class
                .symbols
                .methods
                .get(*code.instructions.get(ins.pc + 1)? as usize)?;
            let owner = class.symbols.types.get(owner as usize)?;
            if owner != &class.descriptor {
                continue;
            }
            let name = class.symbols.strings.get(name as usize)?;
            let (ret, args) = class.symbols.protos.get(proto as usize)?;
            let mut methods = class
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == name && m.parameters == *args);
            let target = methods.next()?;
            if methods.next().is_some()
                || target.return_type != *ret
                || !super::ordinary_method_declaration_authority(class, target)
                || (target.access_flags & (0x100 | 0x400) != 0) != target.code.is_none()
                || (target.access_flags & 8 != 0) != matches!(ins.opcode, 0x71 | 0x77)
            {
                return None;
            }
        }
    }
    Some(())
}
fn sites(class: &DexClass, method: &DexMethod) -> Option<BTreeSet<usize>> {
    let code = method.code.as_ref()?;
    if matches!(method.name.as_ref(), "<init>" | "<clinit>")
        || method.declaring_type != class.descriptor
        || code.instructions.len() > 4096
        || code.registers > 256
        || code.try_regions.is_empty()
        || code.try_regions.len() > 16
        || usize::from(code.tries) != code.try_regions.len()
    {
        return None;
    }
    if !code.instructions.iter().any(|w| *w as u8 == 0x27) {
        return None;
    }
    let analysis = MethodAnalysis::build(class, method).ok()?;
    own_source_operands(class, method, &analysis)?;
    let ir = analysis.instructions();
    let cfg = analysis.blocks();
    let ssa = analysis.ssa();
    if cfg.blocks.len() > 1024
        || ssa.definitions.len() > 32768
        || ir
            .instructions
            .iter()
            .any(|i| matches!(i.opcode, 0x1d | 0x1e))
    {
        return None;
    }
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let ssa_at: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    if ssa.graph.blocks.len() > 4096 || ssa.reachable.len() != ssa.graph.blocks.len() {
        return None;
    }
    let mut predecessors = vec![HashSet::new(); ssa.graph.blocks.len()];
    let mut edge_budget = 65536usize;
    for (from, block) in ssa.graph.blocks.iter().enumerate() {
        for edge in &block.successors {
            edge_budget = edge_budget.checked_sub(1)?;
            if ssa.reachable[from] && *ssa.reachable.get(edge.target)? {
                predecessors[edge.target].insert(Some(from));
            }
        }
    }
    predecessors.get_mut(0)?.insert(None);
    let mut roots = HashMap::new();
    for region in &code.try_regions {
        if region.start >= region.end
            || region.end as usize > code.instructions.len()
            || !instructions.contains_key(&(region.start as usize))
            || (!instructions.contains_key(&(region.end as usize))
                && region.end as usize != code.instructions.len())
            || region.catches.is_empty()
            || region.catches.len() > 16
        {
            return None;
        }
        for (index, (ty, handler)) in region.catches.iter().enumerate() {
            if ty.is_none() && index + 1 != region.catches.len() {
                return None;
            }
            if ty
                .as_ref()
                .is_some_and(|ty| super::method::validate_exception_type(class, ty).is_err())
            {
                return None;
            }
            let pc = *handler as usize;
            let raw = instructions.get(&pc)?;
            let read = ssa_at.get(&pc)?;
            if raw.opcode != 0x0d || read.writes.len() != 1 || read.writes[0].words.len() != 1 {
                return None;
            }
            let block = *cfg.block_at.get(&pc)?;
            if cfg.blocks[block].start != pc
                || cfg
                    .blocks
                    .iter()
                    .flat_map(|b| &b.successors)
                    .any(|edge| edge.target == block && edge.kind == EdgeKind::Normal)
            {
                return None;
            }
            roots.insert(read.writes[0].words[0], pc);
        }
    }
    fn origin(
        id: usize,
        analysis: &MethodAnalysis<'_>,
        roots: &HashMap<usize, usize>,
        predecessors: &[HashSet<Option<usize>>],
        active: &mut HashSet<usize>,
        budget: &mut usize,
    ) -> Option<usize> {
        *budget = budget.checked_sub(1)?;
        if let Some(root) = roots.get(&id) {
            return Some(*root);
        }
        if active.len() >= 256 || !active.insert(id) {
            return None;
        }
        let ssa = analysis.ssa();
        let result = match &ssa.definitions.get(id)?.kind {
            DefinitionKind::Instruction { pc, word, .. } if *word == 0 => {
                let raw = analysis
                    .instructions()
                    .instructions
                    .iter()
                    .find(|i| i.pc == *pc)?;
                if !matches!(raw.opcode, 0x07..=0x09) {
                    return None;
                }
                let ins = ssa.instructions.iter().find(|i| i.pc == *pc)?;
                if ins.reads.len() != 1
                    || ins.reads[0].words.len() != 1
                    || ins.writes.len() != 1
                    || ins.writes[0].words.as_slice() != [id]
                {
                    return None;
                }
                origin(
                    ins.reads[0].words[0],
                    analysis,
                    roots,
                    predecessors,
                    active,
                    budget,
                )
            }
            DefinitionKind::Phi { block } => {
                let definition = ssa.definitions.get(id)?;
                let phi = ssa.phis.iter().find(|p| p.result == id)?;
                let actual: HashSet<_> = phi.incoming.iter().map(|(from, _)| *from).collect();
                if phi.incoming.is_empty()
                    || phi.block != *block
                    || phi.register != definition.register
                    || !*ssa.reachable.get(*block)?
                    || actual.len() != phi.incoming.len()
                    || &actual != predecessors.get(*block)?
                {
                    return None;
                }
                let mut root = None;
                for (_, value) in &phi.incoming {
                    let next = origin(*value, analysis, roots, predecessors, active, budget)?;
                    if root.is_some_and(|r| r != next) {
                        return None;
                    }
                    root = Some(next);
                }
                root
            }
            _ => None,
        };
        active.remove(&id);
        result
    }
    let mut selected = BTreeSet::new();
    let mut budget = 100000;
    for raw in ir.instructions.iter().filter(|i| i.opcode == 0x27) {
        let read = ssa_at.get(&raw.pc)?;
        if read.reads.len() != 1 || read.reads[0].words.len() != 1 {
            continue;
        }
        if origin(
            read.reads[0].words[0],
            &analysis,
            &roots,
            &predecessors,
            &mut HashSet::new(),
            &mut budget,
        )
        .is_some()
        {
            selected.insert(raw.pc);
        }
    }
    Some(selected)
}
pub(super) fn for_site(
    class: &DexClass,
    method: &DexMethod,
    pc: usize,
    register: usize,
) -> Option<String> {
    let code = method.code.as_ref()?;
    let raw = *code.instructions.get(pc)?;
    if raw as u8 != 0x27 || usize::from(raw >> 8) != register {
        return None;
    }
    let name = helper_name(class)?;
    sites(class, method)?.contains(&pc).then_some(name)
}
// Inspect emitter-owned tokens, never literal/comment text. This avoids
// class-wide SSA rediscovery and emits no helper for ordinary Java rethrows.
fn referenced(source: &str, name: &str) -> bool {
    let bytes = source.as_bytes();
    let mut pc = 0;
    let mut previous = "";
    while pc < bytes.len() {
        match bytes[pc] {
            b'"' | b'\'' => {
                let quote = bytes[pc];
                pc += 1;
                while pc < bytes.len() {
                    if bytes[pc] == b'\\' {
                        pc = (pc + 2).min(bytes.len());
                    } else if bytes[pc] == quote {
                        pc += 1;
                        break;
                    } else {
                        pc += 1;
                    }
                }
                previous = "";
            }
            b'/' if bytes.get(pc + 1) == Some(&b'/') => {
                pc += 2;
                while pc < bytes.len() && bytes[pc] != b'\n' {
                    pc += 1;
                }
            }
            b'/' if bytes.get(pc + 1) == Some(&b'*') => {
                pc += 2;
                while pc + 1 < bytes.len() && &bytes[pc..pc + 2] != b"*/" {
                    pc += 1;
                }
                pc = (pc + 2).min(bytes.len());
            }
            c if c.is_ascii_alphabetic() || matches!(c, b'_' | b'$') => {
                let start = pc;
                pc += 1;
                while pc < bytes.len()
                    && (bytes[pc].is_ascii_alphanumeric() || matches!(bytes[pc], b'_' | b'$'))
                {
                    pc += 1;
                }
                let token = &source[start..pc];
                if token == name && previous == "throw" {
                    let mut next = pc;
                    while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                        next += 1;
                    }
                    if bytes.get(next) == Some(&b'(') {
                        return true;
                    }
                }
                previous = token;
            }
            c if c.is_ascii_whitespace() => {
                pc += 1;
            }
            _ => {
                pc += 1;
                previous = "";
            }
        }
    }
    false
}
pub(super) fn definitions(class: &DexClass, rendered: &str) -> Vec<String> {
    let Some(name) = helper_name(class) else {
        return vec![];
    };
    if !referenced(rendered, &name) {
        return vec![];
    }
    vec![format!("\n    // Preserve an original handler throwable without changing the caller header.\n    @java.lang.SuppressWarnings(\"unchecked\")\n    private static <E extends java.lang.Throwable> java.lang.RuntimeException {name}(java.lang.Throwable failure) throws E {{\n        throw (E) failure;\n    }}\n")]
}
#[cfg(test)]
mod tests {
    use super::referenced;
    #[test]
    fn helper_presence_uses_only_actual_throw_tokens() {
        let name = "_rdx_caught_throw_0";
        assert!(referenced("throw _rdx_caught_throw_0(value);", name));
        for text in [
            "String s = \"throw _rdx_caught_throw_0(value);\";",
            "// throw _rdx_caught_throw_0(value);",
            "/* throw _rdx_caught_throw_0(value); */",
            "throw Other._rdx_caught_throw_0(value);",
            "throw _rdx_caught_throw_01(value);",
        ] {
            assert!(!referenced(text, name), "{text}");
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use crate::{
        native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
        native_hierarchy::TypeHierarchy,
        native_java,
    };
    use std::sync::Arc;
    #[test]
    fn source_retry_requires_and_consumes_real_precise_rethrow_refusal() {
        let symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Hooks;".into()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let make = |owner: &str, methods| DexClass {
            descriptor: owner.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods,
            symbols: symbols.clone(),
        };
        let root = make(
            "Lsample/Caller;",
            vec![DexMethod {
                declaring_type: "Lsample/Caller;".into(),
                name: "run".into(),
                parameters: vec![],
                return_type: "V".into(),
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 0,
                    outs: 0,
                    tries: 1,
                    try_regions: vec![DexTryRegion {
                        start: 0,
                        end: 3,
                        catches: vec![(None, 4)].into(),
                    }],
                    instructions: vec![0x0071, 0, 0, 0x000e, 0x000d, 0x0027],
                    offset: 0,
                }),
            }],
        );
        let hooks = make(
            "Lsample/Hooks;",
            vec![DexMethod {
                declaring_type: "Lsample/Hooks;".into(),
                name: "touch".into(),
                parameters: vec![],
                return_type: "V".into(),
                thrown_types: vec!["Ljava/io/IOException;".into()],
                access_flags: 0x109,
                code: None,
            }],
        );
        symbols
            .hierarchy
            .set(Arc::new(
                TypeHierarchy::from_classes([&root, &hooks]).unwrap(),
            ))
            .unwrap();
        let baseline = native_java::render_method_without_same_object_retry(
            "sample.Caller",
            &root,
            &root.methods[0],
        )
        .unwrap_err();
        let failure = baseline
            .downcast_ref::<native_java::method::PreciseRethrowFailure>()
            .expect("exact prior refusal");
        assert_eq!(failure.site.throw_pc, 5);
        let output = native_java::render_method("sample.Caller", &root, &root.methods[0]).unwrap();
        assert!(
            output.source.contains("throw _rdx_caught_throw_"),
            "{}",
            output.source
        );
        assert!(!output
            .source
            .lines()
            .find(|line| line.contains(" run("))
            .unwrap()
            .contains("throws"));
    }
}
