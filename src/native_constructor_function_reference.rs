//! Restore erased forwarding constructors for static anonymous Kotlin function references.
use super::{RecoveredConstructor, complete_layered_method_validation, forwarding_target};
use crate::{
    native_calls::{BoundCalls, CallKind, CallTarget},
    native_cfg::ControlFlowGraph,
    native_dex::{DexClass, DexCode, DexMethod},
    native_ir::{DecodedMethod, Instruction, PoolKind},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const PARENT: &str = "Lkotlin/jvm/internal/FunctionReferenceImpl;";
const FUNCTION: &str = "Lkotlin/jvm/functions/Function2;";
const SUSPEND: &str = "Lkotlin/coroutines/jvm/internal/SuspendFunction;";
const OBJECT: &str = "Ljava/lang/Object;";
const PARAMETERS: [&str; 6] = [
    "I",
    OBJECT,
    "Ljava/lang/Class;",
    "Ljava/lang/String;",
    "Ljava/lang/String;",
    "I",
];
const MAX_WINDOW: usize = 32;
const MAX_METHOD_WORDS: usize = 4096;
const MAX_AUDITED_WORDS: usize = 2_000_000;

pub(crate) fn recovered_function_reference(class: &DexClass) -> bool {
    class.access_flags == 0x1011
        && class.superclass.as_deref() == Some(PARENT)
        && class.symbols.hierarchy.get().is_some_and(|hierarchy| {
            hierarchy
                .recovered_constructors(&class.descriptor)
                .iter()
                .any(|constructor| {
                    constructor.invoked_owner.as_ref() == PARENT
                        && constructor
                            .parameters
                            .iter()
                            .map(AsRef::as_ref)
                            .eq(PARAMETERS)
                        && constructor.strict_allocation_order
                })
        })
}

struct Candidate<'a> {
    enclosing: &'a str,
    constructor: RecoveredConstructor,
    observed: bool,
    invalid: bool,
}

pub(super) struct Recovery<'a> {
    candidates: HashMap<&'a str, Candidate<'a>>,
    remaining_words: Option<usize>,
}

impl<'a> Recovery<'a> {
    pub(super) fn new(
        classes: &[&'a DexClass],
        owners: &HashMap<&str, &'a DexClass>,
        duplicates: &HashSet<&str>,
    ) -> Self {
        let mut recovery = Self {
            candidates: HashMap::new(),
            remaining_words: Some(MAX_AUDITED_WORDS),
        };
        let Some(parent) = owners.get(PARENT).copied() else {
            return recovery;
        };
        if duplicates.contains(PARENT)
            || parent.access_flags != 1
            || !crate::native_java::source_header_authority(parent)
        {
            return recovery;
        }
        let parameters: Vec<Arc<str>> = PARAMETERS.iter().copied().map(Arc::from).collect();
        for &class in classes {
            let name = class.descriptor.as_ref();
            if class.access_flags != 0x1011
                || class.superclass.as_deref() != Some(PARENT)
                || !class.fields.is_empty()
                || duplicates.contains(name)
                || !matches!(
                    class
                        .interfaces
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    [FUNCTION] | [FUNCTION, SUSPEND] | [SUSPEND, FUNCTION]
                )
            {
                continue;
            }
            let [invoke] = class.methods.as_slice() else {
                continue;
            };
            if invoke.name.as_ref() != "invoke"
                || invoke.declaring_type != class.descriptor
                || invoke.access_flags != 0x11
                || invoke.return_type.as_ref() != OBJECT
                || !invoke
                    .parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .eq([OBJECT, OBJECT])
                || invoke.code.is_none()
            {
                continue;
            }
            let Some((outer, _)) = name.rsplit_once('$') else {
                continue;
            };
            let outer = format!("{outer};");
            let Some(enclosing) = owners.get(outer.as_str()).copied() else {
                continue;
            };
            if duplicates.contains(outer.as_str())
                || !crate::native_java::static_anonymous_enclosing_authority(class, enclosing)
            {
                continue;
            }
            let Some(target) = forwarding_target(class, parent, &parameters) else {
                continue;
            };
            // The caller names the parent constructor directly. Verify it is
            // public even when a hypothetical subclass super(...) could reach
            // a protected overload.
            if target.access_flags != 0x10001 {
                continue;
            }
            recovery.candidates.insert(
                name,
                Candidate {
                    enclosing: &enclosing.descriptor,
                    constructor: RecoveredConstructor {
                        source_bridge: false,
                        access_flags: 1,
                        parent: parent.descriptor.clone(),
                        invoked_owner: parent.descriptor.clone(),
                        parameters: parameters.clone(),
                        thrown_types: target.thrown_types.clone(),
                        strict_allocation_order: true,
                        observed_allocation: true,
                        observed_super: false,
                    },
                    observed: false,
                    invalid: false,
                },
            );
        }
        recovery
    }

    pub(super) fn owners(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.candidates.keys().copied()
    }

    pub(super) fn contains(&self, owner: &str) -> bool {
        self.candidates.contains_key(owner)
    }

    pub(super) fn invalidate(&mut self, owner: &str) {
        if let Some(candidate) = self.candidates.get_mut(owner) {
            candidate.invalid = true;
        }
    }

    pub(super) fn observe(&mut self, class: &DexClass, method: &DexMethod, ir: &DecodedMethod) {
        if self.candidates.is_empty() || self.remaining_words.is_none() {
            return;
        }
        let sites: Vec<_> = ir
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                let reference = instruction.reference?;
                if instruction.opcode != 0x22 || reference.kind != PoolKind::Type {
                    return None;
                }
                let owner = class.symbols.types.get(reference.index as usize)?;
                self.contains(owner).then_some((index, owner))
            })
            .collect();
        if sites.is_empty() {
            return;
        }
        let code = method.code.as_ref().expect("decoded method has DEX code");
        let work = code
            .instructions
            .len()
            .saturating_add(sites.len().saturating_mul(MAX_WINDOW));
        self.remaining_words = self
            .remaining_words
            .and_then(|remaining| remaining.checked_sub(work));
        if self.remaining_words.is_none() {
            return;
        }
        let proof = (code.instructions.len() <= MAX_METHOD_WORDS)
            .then(|| complete_layered_method_validation(class, method, ir))
            .flatten();
        for (index, owner) in sites {
            let candidate = self
                .candidates
                .get_mut(owner.as_ref())
                .expect("known candidate");
            let accepted = candidate.enclosing == class.descriptor.as_ref()
                && proof
                    .as_ref()
                    .is_some_and(|(cfg, bound)| exact_site(index, code, ir, cfg, bound));
            candidate.observed |= accepted;
            candidate.invalid |= !accepted;
        }
    }

    pub(super) fn commit(
        self,
        output: &mut HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>>,
    ) {
        if self.remaining_words.is_none() {
            return;
        }
        for (owner, candidate) in self.candidates {
            if candidate.observed && !candidate.invalid {
                output
                    .entry(owner.into())
                    .or_default()
                    .insert(PARAMETERS.concat(), candidate.constructor);
            }
        }
    }
}

fn register_words(
    operands: &[crate::native_ir::RegisterOperand],
) -> impl Iterator<Item = usize> + '_ {
    operands.iter().flat_map(|operand| {
        let start = usize::from(operand.register);
        start..start + operand.kind.word_count()
    })
}

fn exact_site(
    index: usize,
    code: &DexCode,
    ir: &DecodedMethod,
    cfg: &ControlFlowGraph,
    bound: &BoundCalls,
) -> bool {
    let allocation = &ir.instructions[index];
    let Some(block) = cfg.block_at.get(&allocation.pc) else {
        return false;
    };
    let [destination] = allocation.writes.as_slice() else {
        return false;
    };
    let mut aliases = HashSet::from([usize::from(destination.register)]);
    let mut next_pc = allocation.pc + allocation.width;
    for instruction in ir.instructions.iter().skip(index + 1).take(MAX_WINDOW) {
        if instruction.pc != next_pc || cfg.block_at.get(&instruction.pc) != Some(block) {
            return false;
        }
        next_pc += instruction.width;
        let reads_alias = register_words(&instruction.reads).any(|r| aliases.contains(&r));
        let copies_alias = reads_alias && matches!(instruction.opcode, 0x07..=0x09);
        if reads_alias && !copies_alias {
            return exact_initializer(instruction, bound, &aliases)
                && code.try_regions.iter().all(|region| {
                    let start = region.start as usize;
                    let end = region.end as usize;
                    end <= allocation.pc
                        || start >= next_pc
                        || (start <= allocation.pc && end >= next_pc)
                });
        }
        for word in register_words(&instruction.writes) {
            aliases.remove(&word);
            if copies_alias {
                aliases.insert(word);
            }
        }
        if aliases.is_empty() {
            return false;
        }
    }
    false
}

fn exact_initializer(
    instruction: &Instruction,
    bound: &BoundCalls,
    aliases: &HashSet<usize>,
) -> bool {
    let Ok(call) = bound
        .calls
        .binary_search_by_key(&instruction.pc, |call| call.pc)
    else {
        return false;
    };
    let call = &bound.calls[call];
    call.kind == CallKind::Direct
        && matches!(&call.target, CallTarget::Method { declaring_type, name, .. }
            if declaring_type.as_ref() == PARENT && name.as_ref() == "<init>")
        && call.return_type.as_ref() == "V"
        && call
            .receiver
            .as_ref()
            .is_some_and(|receiver| aliases.contains(&usize::from(receiver.register)))
        && call
            .arguments
            .iter()
            .map(|arg| arg.descriptor.as_ref())
            .eq(PARAMETERS)
        && call
            .arguments
            .iter()
            .all(|arg| !aliases.contains(&usize::from(arg.register)))
}
