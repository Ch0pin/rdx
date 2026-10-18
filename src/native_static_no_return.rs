//! Positive, bounded proof that an exact loaded static void body cannot return.
//! Exceptions from any instruction also leave the body because handlers are absent.
use crate::native_dex::DexMethod;

pub(super) fn prove(method: &DexMethod) -> bool {
    if method.access_flags & (8 | 0x100 | 0x400) != 8 || method.return_type.as_ref() != "V" {
        return false;
    }
    let Some(code) = &method.code else {
        return false;
    };
    if code.instructions.is_empty()
        || code.instructions.len() > 512
        || code.tries != 0
        || !code.try_regions.is_empty()
    {
        return false;
    }
    let Ok(ir) = crate::native_ir::DecodedMethod::decode(code) else {
        return false;
    };
    if ir.instructions.len() > 256 {
        return false;
    }
    // This constructor rejects payloads and backward edges. Every maximal
    // reachable path therefore terminates, and must end in an explicit throw.
    let Ok(cfg) = crate::native_cfg::ControlFlowGraph::from_decoded(&ir, code.instructions.len())
    else {
        return false;
    };
    let Some(&entry) = cfg.block_at.get(&0) else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    let mut todo = vec![entry];
    let mut terminal = false;
    while let Some(index) = todo.pop() {
        if !seen.insert(index) {
            continue;
        }
        let block = &cfg.blocks[index];
        if block
            .instructions
            .iter()
            .any(|pc| matches!(code.instructions[*pc] as u8, 0x0e..=0x11))
        {
            return false;
        }
        if block.successors.is_empty() {
            if block
                .instructions
                .last()
                .is_none_or(|pc| code.instructions[*pc] as u8 != 0x27)
            {
                return false;
            }
            terminal = true;
        }
        todo.extend(block.successors.iter().map(|edge| edge.target));
    }
    terminal
}

#[cfg(test)]
mod tests {
    use super::*;
    fn method(words: Vec<u16>) -> DexMethod {
        DexMethod {
            declaring_type: "Lsample/Failures;".into(),
            name: "fail".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(crate::native_dex::DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: words,
            }),
        }
    }
    #[test]
    fn requires_all_reachable_paths_to_throw() {
        assert!(prove(&method(vec![0x0012, 0x0027])));
        assert!(prove(&method(vec![
            0x0038, 4, 0x0012, 0x0027, 0x0012, 0x0027
        ])));
        assert!(!prove(&method(vec![0x0038, 4, 0x0012, 0x0027, 0x000e])));
        assert!(!prove(&method(vec![0x0028])));
        assert!(!prove(&method(vec![0x0012])));
        let mut instance = method(vec![0x0012, 0x0027]);
        instance.access_flags = 1;
        assert!(!prove(&instance));
        let mut native = method(vec![0x0012, 0x0027]);
        native.access_flags |= 0x100;
        assert!(!prove(&native));
    }
}
