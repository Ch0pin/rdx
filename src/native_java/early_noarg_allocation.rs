//! A fresh, non-finalizable Object-root allocation around one independent call.
//! The allocation remains first. Only a proven effect-free default constructor
//! moves forward; the uninitialized identity cannot be read or overwritten.
use super::{Graph, Output, Value, allocation_lowering::Lowered};
use crate::native_dex::{DexClass, DexMethod};
use anyhow::{Context, Result, ensure};

#[allow(clippy::too_many_arguments)]
pub(super) fn lower(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    words: &[u16],
    pc: usize,
    stop: usize,
    inputs: &[Option<Value>],
    output: &Output,
) -> Result<Lowered> {
    let end = pc.checked_add(9).context("allocation window overflow")?;
    let code = method.code.as_ref().context("allocation code missing")?;
    ensure!(
        end <= stop && end <= words.len() && code.instructions.get(pc..end) == words.get(pc..end),
        "allocation window differs"
    );
    let dst = usize::from(words[pc] >> 8);
    ensure!(
        words[pc] as u8 == 0x22
            && matches!(words[pc + 2], 0x106e | 0x1072)
            && words[pc + 5] as u8 == 0x0c
            && words[pc + 6] == 0x1070
            && usize::from(words[pc + 8]) == dst,
        "allocation sequence differs"
    );
    let receiver = usize::from(words[pc + 4]);
    let result = usize::from(words[pc + 5] >> 8);
    ensure!(
        receiver < 16 && receiver != dst && result != dst && dst < inputs.len(),
        "fresh identity escapes or is overwritten"
    );
    ensure!(
        graph
            .targets
            .iter()
            .flatten()
            .all(|target| *target <= pc || *target >= end)
            && graph.switches.iter().all(Option::is_none)
            && !code
                .try_regions
                .iter()
                .any(|r| (r.start as usize) < end && pc < r.end as usize
                    || r.catches
                        .iter()
                        .any(|(_, h)| pc <= *h as usize && (*h as usize) < end)),
        "allocation has alternate entry or protected effects"
    );
    let ty = class
        .symbols
        .types
        .get(words[pc + 1] as usize)
        .context("allocation type")?;
    let hierarchy = class
        .symbols
        .hierarchy
        .get()
        .context("allocation hierarchy")?;
    ensure!(
        hierarchy.early_fresh_noarg_constructor(ty),
        "constructor has observable or unknown effects"
    );
    let &(owner, proto, name) = class
        .symbols
        .methods
        .get(words[pc + 7] as usize)
        .context("constructor symbol")?;
    let (ret, args) = class
        .symbols
        .protos
        .get(proto as usize)
        .context("constructor prototype")?;
    ensure!(
        class
            .symbols
            .types
            .get(owner as usize)
            .is_some_and(|s| s.as_ref() == "Ljava/lang/Object;")
            && class
                .symbols
                .strings
                .get(name as usize)
                .is_some_and(|s| s == "<init>")
            && ret.as_ref() == "V"
            && args.is_empty(),
        "constructor is not Object initialization"
    );
    let &(_, call_proto, call_name) = class
        .symbols
        .methods
        .get(words[pc + 3] as usize)
        .context("independent call symbol")?;
    let (call_ret, call_args) = class
        .symbols
        .protos
        .get(call_proto as usize)
        .context("independent call prototype")?;
    ensure!(
        call_args.is_empty()
            && super::reference(call_ret)
            && class
                .symbols
                .strings
                .get(call_name as usize)
                .is_some_and(|s| !s.starts_with('<')),
        "independent call shape differs"
    );
    let display = super::java_type(ty)?;
    let label = super::class_label(ty).context("allocation label")?;
    let mut out = output.empty_fork();
    out.sequence = output.sequence;
    out.indent = output.indent;
    let value = out.local(
        ty,
        &format!("new {display}()"),
        &[(4, display.chars().count(), label)],
    )?;
    let mut regs = inputs.to_vec();
    super::assign(&mut regs, dst, value)?;
    let (regs, terminal) = super::render(
        class,
        method,
        graph,
        pc + 2,
        pc + 6,
        regs,
        &mut out,
        1,
        true,
        None,
        None,
    )?;
    ensure!(!terminal, "independent call terminates allocation window");
    Ok(Lowered {
        regs,
        out,
        next_pc: end,
        internal_handler_regions: vec![],
    })
}
