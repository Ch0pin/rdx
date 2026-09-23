//! Bounded duplicate-cleanup recognition guided by JADX MarkFinallyVisitor
//! (28ff15e4ae69950aebea110a13e5ab895d234dfc). Original exception dispatch is
//! verified before emitting a terminal try/finally enclosed in one typed catch.
use super::*;
use crate::native_dex::DexCode;
use crate::native_ir::DecodedMethod;

pub(super) fn reconstruct(
    class: &DexClass,
    method: &DexMethod,
    regs: Vec<Option<Value>>,
) -> Result<MethodBody> {
    let code = method.code.as_ref().context("missing cleanup code")?;
    ensure!(
        code.try_regions.len() <= 16,
        "cleanup region budget exceeded"
    );
    ensure!(
        method.return_type.as_ref() == "V",
        "nested cleanup requires void return"
    );
    let words = &code.instructions;
    let catchall = code
        .try_regions
        .iter()
        .find(|r| r.catches.len() == 1 && r.catches[0].0.is_none())
        .context("nested regions lack a cleanup handler")?;
    let inner_start = code
        .try_regions
        .iter()
        .filter(|r| r.catches == catchall.catches)
        .map(|r| r.start as usize)
        .min()
        .unwrap();
    let handler = catchall.catches[0].1 as usize;
    let outer = code
        .try_regions
        .iter()
        .find(|r| {
            r.start == 0
                && r.end as usize == inner_start
                && r.catches.len() == 1
                && r.catches[0].0.is_some()
        })
        .context("cleanup lacks outer prefix catch")?;
    let (ty, outer_handler) = &outer.catches[0];
    let ty = ty.as_ref().unwrap();
    let outer_handler = *outer_handler as usize;
    ensure!(
        inner_start < handler && handler + 5 == outer_handler && outer_handler < words.len(),
        "cleanup handler layout unsupported"
    );
    ensure!(
        words[handler] as u8 == 0x0d
            && words[handler + 4] as u8 == 0x27
            && words[handler] >> 8 == words[handler + 4] >> 8,
        "cleanup does not rethrow original exception"
    );
    ensure!(
        words[outer_handler] as u8 == 0x0d,
        "outer catch must capture exception"
    );
    let ir = DecodedMethod::decode(code)?;
    let cleanup_pc = handler + 1;
    let cleanup = ir
        .instructions
        .iter()
        .find(|i| i.pc == cleanup_pc)
        .context("cleanup instruction")?;
    ensure!(
        cleanup.width == 3 && matches!(cleanup.opcode, 0x6e..=0x72 | 0x74..=0x78),
        "cleanup must be one invocation"
    );
    let (_, proto, _) = *class
        .symbols
        .methods
        .get(words[cleanup_pc + 1] as usize)
        .context("cleanup method")?;
    ensure!(
        class
            .symbols
            .protos
            .get(proto as usize)
            .context("cleanup prototype")?
            .0
            .as_ref()
            == "V",
        "cleanup result unsupported"
    );
    let copies: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| {
            i.pc >= inner_start
                && i.pc < handler
                && i.width == 3
                && words[i.pc..i.pc + 3] == words[cleanup_pc..cleanup_pc + 3]
        })
        .collect();
    ensure!(copies.len() == 1, "cleanup requires one exact normal copy");
    let normal_cleanup = copies[0].pc;
    ensure!(
        words.get(normal_cleanup + 3) == Some(&0x000e),
        "normal cleanup must precede void return"
    );
    let original = Graph::with_handlers(words, &[handler, outer_handler])?;
    ensure!(
        code.try_regions.iter().all(|r| {
            [r.start as usize, r.end as usize]
                .iter()
                .all(|&pc| pc < words.len() && original.widths[pc] != 0)
        }),
        "cleanup try boundary is not an instruction boundary"
    );
    let mut pending = vec![inner_start];
    let mut visited = std::collections::HashSet::new();
    while let Some(pc) = pending.pop() {
        if pc == normal_cleanup || !visited.insert(pc) {
            continue;
        }
        ensure!(
            pc >= inner_start && pc < handler && original.widths[pc] != 0,
            "normal path escapes cleanup body"
        );
        match words[pc] as u8 {
            0x0e..=0x11 => bail!("normal return bypasses cleanup"),
            0x27 => {}
            0x28..=0x2a => pending.push(original.targets[pc].context("cleanup jump")?),
            _ => {
                ensure!(
                    original.switches[pc].is_none(),
                    "cleanup switch unsupported"
                );
                pending.push(pc + original.widths[pc]);
                pending.extend(original.targets[pc]);
            }
        }
    }
    let caught_reg = (words[handler] >> 8) as usize;
    for read in &cleanup.reads {
        let register = usize::from(read.register);
        ensure!(register != caught_reg, "cleanup observes caught exception");
        ensure!(
            !ir.instructions
                .iter()
                .filter(|i| i.pc >= inner_start && i.pc < handler)
                .flat_map(|i| &i.writes)
                .any(|w| usize::from(w.register) <= register
                    && register < usize::from(w.register) + w.kind.word_count()),
            "cleanup input changes in protected body"
        );
    }
    // Every original throwing instruction must dispatch to exactly the handler
    // represented by its Java region. Nonthrowing gaps do not expand catches.
    for insn in ir
        .instructions
        .iter()
        .filter(|i| i.pc < outer_handler && i.may_throw)
    {
        let expected = if insn.pc < inner_start || insn.pc == normal_cleanup || insn.pc > handler {
            &outer.catches
        } else {
            &catchall.catches
        };
        let covering: Vec<_> = code
            .try_regions
            .iter()
            .filter(|r| r.start as usize <= insn.pc && insn.pc < r.end as usize)
            .collect();
        ensure!(
            covering.len() == 1 && &covering[0].catches == expected,
            "cleanup exception dispatch mismatch"
        );
    }
    ensure!(
        code.try_regions.iter().all(|r| r.start < r.end
            && r.end as usize <= outer_handler
            && (r.catches == outer.catches || r.catches == catchall.catches)),
        "unrelated nested exception region"
    );
    let declared_throw = ir
        .instructions
        .iter()
        .filter(|i| i.pc < handler && matches!(i.opcode, 0x6e..=0x72 | 0x74..=0x78))
        .any(|i| {
            let Some(&(owner, proto, name)) = class.symbols.methods.get(words[i.pc + 1] as usize)
            else {
                return false;
            };
            let (Some(owner), Some((ret, args)), Some(name)) = (
                class.symbols.types.get(owner as usize),
                class.symbols.protos.get(proto as usize),
                class.symbols.strings.get(name as usize),
            ) else {
                return false;
            };
            throwing::known_call_throws(owner, name, args, ret, ty)
        });
    throwing::validate_catch_type(class, ty, declared_throw)?;
    // Keep all original offsets and operands; only the proven normal cleanup
    // copy becomes nops because the finally block now executes it on every exit.
    let mut instructions = words.clone();
    instructions[normal_cleanup..normal_cleanup + 3].fill(0);
    let lowered = DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: vec![ty.clone()],
        access_flags: method.access_flags,
        code: Some(DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: 0,
            try_regions: vec![],
            instructions,
            offset: code.offset,
        }),
    };
    let lowered_code = lowered.code.as_ref().unwrap();
    let mut graph = Graph::with_handlers(&lowered_code.instructions, &[handler, outer_handler])?;
    graph.live = super::super::liveness::analyze(lowered_code);
    ensure!(
        graph.loops.is_empty() && graph.switches.iter().all(Option::is_none),
        "complex cleanup control flow unsupported"
    );
    for (pc, target) in graph
        .targets
        .iter()
        .enumerate()
        .filter_map(|(pc, t)| t.map(|t| (pc, t)))
    {
        ensure!(
            (pc < inner_start && target < inner_start)
                || (pc >= inner_start
                    && pc < handler
                    && target >= inner_start
                    && target < handler
                    && !(normal_cleanup..normal_cleanup + 3).contains(&target))
                || (pc > outer_handler && target > outer_handler),
            "branch crosses cleanup region"
        );
    }
    let mut out = Output::default();
    out.line("try {", &[]);
    out.indent += 1;
    let (inner_regs, terminal) = render(
        class,
        &lowered,
        &graph,
        0,
        inner_start,
        regs,
        &mut out,
        0,
        true,
        None,
        None,
    )?;
    ensure!(!terminal, "cleanup prefix terminates");
    out.line("try {", &[]);
    out.indent += 1;
    let (_, terminal) = render(
        class,
        &lowered,
        &graph,
        inner_start,
        handler,
        inner_regs.clone(),
        &mut out,
        1,
        true,
        None,
        None,
    )?;
    ensure!(terminal, "cleanup body must terminate");
    out.indent -= 1;
    out.line("} finally {", &[]);
    out.indent += 1;
    let (_, terminal) = render(
        class,
        &lowered,
        &graph,
        cleanup_pc,
        cleanup_pc + 3,
        inner_regs,
        &mut out,
        1,
        true,
        None,
        None,
    )?;
    ensure!(!terminal, "cleanup unexpectedly terminates");
    out.indent -= 1;
    out.line("}", &[]);
    out.indent -= 1;
    let display = java_type(ty)?;
    let caught = format!("caught{}", out.sequence);
    out.sequence += 1;
    out.line(
        &format!("}} catch ({display} {caught}) {{"),
        &[(9, display.chars().count(), class_label(ty).unwrap())],
    );
    out.indent += 1;
    let mut handler_regs = vec![None; code.registers as usize];
    assign(
        &mut handler_regs,
        (words[outer_handler] >> 8) as usize,
        Value {
            text: caught.clone(),
            ty: ty.to_string(),
            literal: None,
            wide_literal: None,
        },
    )?;
    graph.caught_values.borrow_mut().insert(caught);
    let (_, terminal) = render(
        class,
        &lowered,
        &graph,
        outer_handler + 1,
        words.len(),
        handler_regs,
        &mut out,
        1,
        true,
        None,
        None,
    )?;
    ensure!(terminal, "outer cleanup catch must terminate");
    out.indent -= 1;
    out.line("}", &[]);
    let mut body = MethodBody {
        text: out.text,
        links: out.links,
    };
    cleanup::inline_receivers(&mut body, &out.receiver_locals);
    Ok(body)
}
