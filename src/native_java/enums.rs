//! Exact recovery of fieldless enum constants with erased Enum constructors.
use super::*;
use crate::native_ir::DecodedMethod;
use std::collections::HashSet;

#[derive(Clone)]
enum Atom {
    Empty,
    Text(String),
    Int(i32),
    New(usize),
    Constant(usize),
    Array(Vec<Option<usize>>),
    Entries,
}
pub(super) struct Plan {
    constants: Vec<(String, String)>,
    array: String,
    entries: Option<String>,
}
impl Plan {
    fn kotlin_constructor(class: &DexClass) -> Result<()> {
        let constructors = class
            .methods
            .iter()
            .filter(|m| m.name.as_ref() == "<init>")
            .collect::<Vec<_>>();
        ensure!(constructors.len() == 1, "enum constructor count");
        let m = constructors[0];
        ensure!(
            m.access_flags & 7 == 2
                && m.access_flags & !(7 | 0x10000 | 0x1000) == 0
                && m.return_type.as_ref() == "V"
                && m.parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .eq(["Ljava/lang/String;", "I"]),
            "enum constructor signature"
        );
        let code = m
            .code
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("enum constructor code"))?;
        let w = &code.instructions;
        ensure!(
            code.try_regions.is_empty()
                && code.registers == 3
                && code.ins == 3
                && w.len() == 4
                && w[0] == 0x3070
                && w[2] == 0x0210
                && w[3] == 0x000e
                && (w[1] as usize) < class.symbols.methods.len(),
            "enum constructor body"
        );
        let &(owner, proto, name) = &class.symbols.methods[w[1] as usize];
        ensure!(
            class.symbols.types[owner as usize].as_ref() == "Ljava/lang/Enum;"
                && class.symbols.strings[name as usize] == "<init>"
                && class.symbols.protos[proto as usize].0.as_ref() == "V"
                && class.symbols.protos[proto as usize]
                    .1
                    .iter()
                    .map(AsRef::as_ref)
                    .eq(["Ljava/lang/String;", "I"]),
            "enum constructor super call"
        );
        Ok(())
    }

    fn kotlin_values_helper(
        class: &DexClass,
        constants: &[(String, String)],
    ) -> Result<Vec<Option<usize>>> {
        let helpers = class
            .methods
            .iter()
            .filter(|m| m.name.as_ref() == "$values")
            .collect::<Vec<_>>();
        ensure!(helpers.len() == 1, "enum values helper count");
        let m = helpers[0];
        ensure!(
            m.access_flags & (7 | 8 | 16 | 0x1000) == 2 | 8 | 16 | 0x1000
                && m.access_flags & !(7 | 8 | 16 | 0x1000) == 0
                && m.parameters.is_empty()
                && m.return_type.as_ref() == format!("[{}", class.descriptor),
            "enum values helper signature"
        );
        let code = m
            .code
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("enum values helper code"))?;
        ensure!(code.try_regions.is_empty(), "enum values helper handlers");
        let ir = DecodedMethod::decode(code)?;
        let mut regs = vec![None; code.registers as usize];
        let mut values = None;
        for (position, insn) in ir.instructions.iter().enumerate() {
            let pc = insn.pc;
            let w = code.instructions[pc];
            let a = (w >> 8) as usize;
            match insn.opcode {
                0x62 if values.is_none() => {
                    let &(owner, ty, name) = class
                        .symbols
                        .fields
                        .get(code.instructions[pc + 1] as usize)
                        .ok_or_else(|| anyhow::anyhow!("enum helper field"))?;
                    let field = &class.symbols.strings[name as usize];
                    ensure!(
                        class.symbols.types[owner as usize] == class.descriptor
                            && class.symbols.types[ty as usize] == class.descriptor,
                        "enum helper field type"
                    );
                    regs[a] = Some(
                        constants
                            .iter()
                            .position(|(_, f)| f == field)
                            .ok_or_else(|| anyhow::anyhow!("enum helper constant"))?,
                    );
                }
                0x24 if values.is_none() => {
                    ensure!(
                        class.symbols.types[code.instructions[pc + 1] as usize].as_ref()
                            == format!("[{}", class.descriptor),
                        "enum helper array type"
                    );
                    let p = code.instructions[pc + 2];
                    let rr = [
                        (p & 15) as usize,
                        ((p >> 4) & 15) as usize,
                        ((p >> 8) & 15) as usize,
                        ((p >> 12) & 15) as usize,
                        a & 15,
                    ];
                    let count = a >> 4;
                    ensure!(count == constants.len() && count <= 5, "enum helper count");
                    let selected = rr[..count].iter().map(|&r| regs[r]).collect::<Vec<_>>();
                    ensure!(
                        selected.iter().copied().eq((0..count).map(Some)),
                        "enum helper order"
                    );
                    values = Some(selected);
                }
                0x0c if values.is_some() => ensure!(a < regs.len(), "enum helper result register"),
                0x11 if values.is_some() => ensure!(
                    position + 1 == ir.instructions.len(),
                    "enum helper return position"
                ),
                _ => bail!("enum helper instruction"),
            }
        }
        let values = values.ok_or_else(|| anyhow::anyhow!("enum helper array missing"))?;
        ensure!(
            ir.instructions.len() == constants.len() + 3
                && ir.instructions[ir.instructions.len() - 2].opcode == 0x0c
                && ir.instructions.last().is_some_and(|i| i.opcode == 0x11)
                && code.instructions[ir.instructions.last().unwrap().pc] >> 8
                    == code.instructions[ir.instructions[ir.instructions.len() - 2].pc] >> 8,
            "enum helper result"
        );
        Ok(values)
    }

    fn standard_value_of(class: &DexClass, m: &DexMethod) -> bool {
        if m.access_flags != 9
            || !m.thrown_types.is_empty()
            || m.parameters
                .iter()
                .map(AsRef::as_ref)
                .ne(["Ljava/lang/String;"])
            || m.return_type != class.descriptor
        {
            return false;
        }
        let Some(code) = &m.code else {
            return false;
        };
        let w = &code.instructions;
        if !code.try_regions.is_empty()
            || code.registers != 2
            || code.ins != 1
            || w.len() != 9
            || w[0] != 0x001c
            || w[2] != 0x2071
            || w[4] != 0x0010
            || w[5] != 0x010c
            || w[6] != 0x011f
            || w[8] != 0x0111
        {
            return false;
        }
        let Some(&(owner, proto, name)) = class.symbols.methods.get(w[3] as usize) else {
            return false;
        };
        class.symbols.types.get(w[1] as usize) == Some(&class.descriptor)
            && class.symbols.types.get(w[7] as usize) == Some(&class.descriptor)
            && class.symbols.types[owner as usize].as_ref() == "Ljava/lang/Enum;"
            && class.symbols.strings[name as usize] == "valueOf"
            && class.symbols.protos[proto as usize].0.as_ref() == "Ljava/lang/Enum;"
            && class.symbols.protos[proto as usize]
                .1
                .iter()
                .map(AsRef::as_ref)
                .eq(["Ljava/lang/Class;", "Ljava/lang/String;"])
            && code.outs == 2
    }
    pub(super) fn analyze(class: &DexClass) -> Result<Self> {
        ensure!(
            class.access_flags & 0x4000 != 0
                && class.superclass.as_deref() == Some("Ljava/lang/Enum;"),
            "not enum"
        );
        let kotlin = class.fields.iter().any(|f| f.name.as_ref() == "$ENTRIES");
        ensure!(
            class.fields.iter().all(|f| f.is_static),
            "enum instance state"
        );
        if kotlin {
            Self::kotlin_constructor(class)?;
        } else {
            ensure!(
                !class.methods.iter().any(|m| m.name.as_ref() == "<init>"),
                "enum constructor"
            );
        }
        ensure!(
            class.access_flags & !0x5011 == 0
                && !class.methods.iter().any(|m| m.access_flags & 0x400 != 0),
            "unsupported enum modifiers"
        );
        let clinit = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == "<clinit>")
            .ok_or_else(|| anyhow::anyhow!("enum initializer missing"))?;
        let code = clinit
            .code
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("enum code missing"))?;
        ensure!(code.try_regions.is_empty(), "enum initializer handlers");
        let ir = DecodedMethod::decode(code)?;
        let mut regs = vec![Atom::Empty; code.registers as usize];
        let mut constants: Vec<(String, String)> = vec![];
        let mut pending = None;
        let mut array = None;
        let mut entries = None;
        let mut helper_calls = 0usize;
        let mut entries_calls = 0usize;
        let mut constructed = HashSet::new();
        let mut allocations = 0usize;
        for (index, insn) in ir.instructions.iter().enumerate() {
            ensure!(
                pending.is_none() || insn.opcode == 0x0c,
                "enum result is not adjacent"
            );
            let pc = insn.pc;
            let w = code.instructions[pc];
            let a = (w >> 8) as usize;
            ensure!(
                array.is_none()
                    || matches!(insn.opcode, 0x00 | 0x0e)
                    || (kotlin && matches!(insn.opcode, 0x1f | 0x71 | 0x0c | 0x69)),
                "enum instructions after values publication"
            );
            match insn.opcode {
                0x00 => ensure!(w == 0, "enum payload"),
                0x12 => regs[a & 15] = Atom::Int(((w as i16) >> 12) as i32),
                0x13 => regs[a] = Atom::Int(code.instructions[pc + 1] as i16 as i32),
                0x14 => {
                    regs[a] = Atom::Int(
                        (u32::from(code.instructions[pc + 1])
                            | u32::from(code.instructions[pc + 2]) << 16)
                            as i32,
                    )
                }
                0x1a => {
                    regs[a] = Atom::Text(
                        class
                            .symbols
                            .strings
                            .get(code.instructions[pc + 1] as usize)
                            .ok_or_else(|| anyhow::anyhow!("enum string"))?
                            .clone(),
                    )
                }
                0x22 => {
                    ensure!(array.is_none(), "enum allocation after values array");
                    allocations += 1;
                    ensure!(
                        class.symbols.types.get(code.instructions[pc + 1] as usize)
                            == Some(&class.descriptor),
                        "enum subclass allocation"
                    );
                    regs[a] = Atom::New(pc);
                }
                0x70 => {
                    ensure!(a >> 4 == 3, "enum constructor arguments");
                    let packed = code.instructions[pc + 2];
                    let rr = [
                        (packed & 15) as usize,
                        ((packed >> 4) & 15) as usize,
                        ((packed >> 8) & 15) as usize,
                    ];
                    let &(owner, proto, name) = class
                        .symbols
                        .methods
                        .get(code.instructions[pc + 1] as usize)
                        .ok_or_else(|| anyhow::anyhow!("enum method"))?;
                    ensure!(
                        ((!kotlin
                            && class.symbols.types[owner as usize].as_ref() == "Ljava/lang/Enum;")
                            || (kotlin && class.symbols.types[owner as usize] == class.descriptor))
                            && class.symbols.strings[name as usize] == "<init>"
                            && class.symbols.protos[proto as usize].0.as_ref() == "V"
                            && class.symbols.protos[proto as usize]
                                .1
                                .iter()
                                .map(AsRef::as_ref)
                                .eq(["Ljava/lang/String;", "I"]),
                        "enum constructor target"
                    );
                    let Atom::New(site) = regs[rr[0]] else {
                        bail!("enum receiver")
                    };
                    let Atom::Text(ref label) = regs[rr[1]] else {
                        bail!("enum name")
                    };
                    let Atom::Int(ordinal) = regs[rr[2]] else {
                        bail!("enum ordinal")
                    };
                    ensure!(
                        ordinal == constants.len() as i32 && constructed.insert(site),
                        "enum ordinal or allocation order"
                    );
                    ensure!(
                        names::member(label)? == *label
                            && !constants.iter().any(|(n, _)| n == label),
                        "enum constant identifier"
                    );
                    constants.push((label.clone(), String::new()));
                    regs[rr[0]] = Atom::Constant(ordinal as usize);
                }
                0x69 => {
                    let &(owner, ty, name) = class
                        .symbols
                        .fields
                        .get(code.instructions[pc + 1] as usize)
                        .ok_or_else(|| anyhow::anyhow!("enum field"))?;
                    ensure!(
                        class.symbols.types[owner as usize] == class.descriptor,
                        "enum external store"
                    );
                    let field = &class.symbols.strings[name as usize];
                    match &regs[a] {
                        Atom::Constant(ordinal) => {
                            ensure!(
                                class.symbols.types[ty as usize] == class.descriptor
                                    && constants[*ordinal].1.is_empty(),
                                "enum duplicate field store"
                            );
                            ensure!(
                                class.fields.iter().any(|f| f.name.as_ref() == field
                                    && f.access_flags & 0x4019 == 0x4019),
                                "enum constant field flags"
                            );
                            constants[*ordinal].1 = field.clone();
                        }
                        Atom::Array(values) => {
                            ensure!(
                                class.symbols.types[ty as usize].as_ref()
                                    == format!("[{}", class.descriptor)
                                    && values.iter().copied().eq((0..constants.len()).map(Some))
                                    && array.is_none(),
                                "enum values array"
                            );
                            array = Some(field.clone());
                        }
                        Atom::Entries if kotlin => {
                            ensure!(
                                array.is_some()
                                    && entries.is_none()
                                    && field == "$ENTRIES"
                                    && class.symbols.types[ty as usize].as_ref()
                                        == "Lkotlin/enums/EnumEntries;",
                                "enum entries store"
                            );
                            entries = Some(field.clone());
                        }
                        _ => bail!("enum nonconstant store"),
                    }
                }
                0x23 => {
                    ensure!(
                        class.symbols.types[code.instructions[pc + 1] as usize].as_ref()
                            == format!("[{}", class.descriptor),
                        "enum array type"
                    );
                    let Atom::Int(size) = regs[a >> 4] else {
                        bail!("enum array size")
                    };
                    ensure!((0..=4096).contains(&size), "enum array size budget");
                    regs[a & 15] = Atom::Array(vec![None; size as usize]);
                }
                0x4d => {
                    let p = code.instructions[pc + 1];
                    let array = (p & 255) as usize;
                    let index = (p >> 8) as usize;
                    let Atom::Int(index) = regs[index] else {
                        bail!("enum array index")
                    };
                    let Atom::Constant(value) = regs[a] else {
                        bail!("enum array value")
                    };
                    let Atom::Array(ref mut values) = regs[array] else {
                        bail!("enum array receiver")
                    };
                    let slot = values
                        .get_mut(index as usize)
                        .ok_or_else(|| anyhow::anyhow!("enum array bounds"))?;
                    *slot = Some(value);
                }
                0x24 | 0x25 => {
                    ensure!(
                        class.symbols.types[code.instructions[pc + 1] as usize].as_ref()
                            == format!("[{}", class.descriptor),
                        "enum array type"
                    );
                    let p = code.instructions[pc + 2];
                    let rr = [
                        (p & 15) as usize,
                        ((p >> 4) & 15) as usize,
                        ((p >> 8) & 15) as usize,
                        ((p >> 12) & 15) as usize,
                        a & 15,
                    ];
                    let selected = if insn.opcode == 0x25 {
                        (p as usize..p as usize + a).collect::<Vec<_>>()
                    } else {
                        let count = a >> 4;
                        ensure!(count <= 5, "enum array count");
                        rr[..count].to_vec()
                    };
                    let mut values = vec![];
                    for r in selected {
                        let Atom::Constant(i) = regs[r] else {
                            bail!("enum array entry")
                        };
                        values.push(Some(i));
                    }
                    pending = Some(Atom::Array(values));
                }
                0x0c => {
                    regs[a] = pending
                        .take()
                        .ok_or_else(|| anyhow::anyhow!("enum array result"))?
                }
                0x1f if kotlin => {
                    ensure!(
                        array.is_some()
                            && class.symbols.types[code.instructions[pc + 1] as usize].as_ref()
                                == "[Ljava/lang/Enum;"
                            && matches!(regs[a], Atom::Array(_)),
                        "enum entries array cast"
                    );
                }
                0x71 if kotlin => {
                    let &(owner, proto, name) = class
                        .symbols
                        .methods
                        .get(code.instructions[pc + 1] as usize)
                        .ok_or_else(|| anyhow::anyhow!("enum Kotlin method"))?;
                    let owner = class.symbols.types[owner as usize].as_ref();
                    let method = &class.symbols.strings[name as usize];
                    let signature = &class.symbols.protos[proto as usize];
                    if owner == class.descriptor.as_ref() && method == "$values" {
                        ensure!(
                            array.is_none()
                                && helper_calls == 0
                                && a == 0
                                && code.instructions[pc + 2] == 0
                                && signature.0.as_ref() == format!("[{}", class.descriptor)
                                && signature.1.is_empty(),
                            "enum helper invocation"
                        );
                        helper_calls += 1;
                        pending = Some(Atom::Array(Self::kotlin_values_helper(class, &constants)?));
                    } else {
                        ensure!(
                            owner == "Lkotlin/enums/EnumEntriesKt;"
                                && method == "enumEntries"
                                && signature.0.as_ref() == "Lkotlin/enums/EnumEntries;"
                                && signature
                                    .1
                                    .iter()
                                    .map(AsRef::as_ref)
                                    .eq(["[Ljava/lang/Enum;"])
                                && array.is_some()
                                && entries.is_none()
                                && entries_calls == 0
                                && a == 0x10
                                && code.instructions[pc + 2] == 0
                                && matches!(&regs[(code.instructions[pc + 2] & 15) as usize], Atom::Array(values)
                                    if values.iter().copied().eq((0..constants.len()).map(Some))),
                            "enum entries invocation"
                        );
                        entries_calls += 1;
                        pending = Some(Atom::Entries);
                    }
                }
                0x0e => ensure!(index + 1 == ir.instructions.len(), "enum premature return"),
                _ => bail!("unsupported enum initializer instruction"),
            }
        }
        ensure!(
            matches!(ir.instructions.last().map(|i| i.opcode), Some(0x0e))
                && !constants.is_empty()
                && constants.iter().all(|(_, f)| !f.is_empty()),
            "enum incomplete initialization"
        );
        let array = array.ok_or_else(|| anyhow::anyhow!("enum array missing"))?;
        ensure!(
            constructed.len() == allocations
                && constants
                    .iter()
                    .map(|(_, f)| f)
                    .collect::<HashSet<_>>()
                    .len()
                    == constants.len(),
            "enum unused allocation or repeated constant field"
        );
        ensure!(
            class.fields.len() == constants.len() + 1 + usize::from(kotlin)
                && class.static_values.is_empty()
                && (class.annotations_offset == 0
                    || (kotlin && annotation_directory(class).is_some())),
            "enum extra static state"
        );
        ensure!(kotlin == entries.is_some(), "enum entries missing");
        ensure!(
            !kotlin || (helper_calls == 1 && entries_calls == 1),
            "enum Kotlin helper calls"
        );
        if let Some(ref entries) = entries {
            ensure!(
                class
                    .fields
                    .iter()
                    .find(|f| f.name.as_ref() == entries)
                    .is_some_and(|f| f.field_type.as_ref() == "Lkotlin/enums/EnumEntries;"
                        && f.access_flags == 0x101a),
                "enum entries field flags"
            );
        }
        if kotlin {
            if let Some(directory) = annotation_directory(class) {
                ensure!(
                    directory.fields.iter().all(Option::is_none),
                    "enum suppressed field annotations"
                );
                for (index, method) in class.methods.iter().enumerate() {
                    if matches!(
                        method.name.as_ref(),
                        "<clinit>" | "<init>" | "$values" | "valueOf" | "values"
                    ) {
                        let annotation = directory.methods.get(index).and_then(Option::as_ref);
                        ensure!(
                            annotation.is_none_or(|set| set.iter().all(|a| a.visibility == 2
                                && class.symbols.types.get(a.type_idx as usize).is_some_and(
                                    |t| t.as_ref() == "Ldalvik/annotation/Signature;"
                                ))),
                            "enum suppressed method annotations"
                        );
                        ensure!(
                            directory
                                .parameters
                                .get(index)
                                .is_none_or(|sets| sets.iter().all(Option::is_none)),
                            "enum suppressed parameter annotations"
                        );
                    }
                }
            }
            for method in class
                .methods
                .iter()
                .filter(|m| m.name.as_ref() != "<clinit>")
            {
                if let Some(code) = &method.code {
                    for insn in DecodedMethod::decode(code)?.instructions {
                        if matches!(insn.opcode, 0x6e..=0x78) {
                            let &(owner, _, name) = class
                                .symbols
                                .methods
                                .get(code.instructions[insn.pc + 1] as usize)
                                .ok_or_else(|| anyhow::anyhow!("enum helper use"))?;
                            ensure!(
                                class.symbols.types[owner as usize] != class.descriptor
                                    || class.symbols.strings[name as usize] != "$values",
                                "enum helper has external use"
                            );
                        }
                    }
                }
            }
        }
        ensure!(
            class
                .fields
                .iter()
                .find(|f| f.name.as_ref() == array)
                .is_some_and(|f| f.access_flags & (7 | 8 | 16) == 26
                    && f.access_flags & !(7 | 8 | 16 | 0x1000) == 0),
            "enum array field flags"
        );
        for (name, field) in &constants {
            ensure!(
                name == field || !class.fields.iter().any(|f| f.name.as_ref() == name),
                "enum name collides with alias field"
            );
        }
        // Java emits one implicit enum backing array. Only hide the DEX array
        // when no nonstandard method can observe or mutate that storage.
        for method in class
            .methods
            .iter()
            .filter(|m| !matches!(m.name.as_ref(), "<clinit>" | "values"))
        {
            if let Some(code) = &method.code {
                for instruction in DecodedMethod::decode(code)?.instructions {
                    if matches!(instruction.opcode, 0x52..=0x6d) {
                        let index = code.instructions[instruction.pc + 1] as usize;
                        let &(owner, _, field) = class
                            .symbols
                            .fields
                            .get(index)
                            .ok_or_else(|| anyhow::anyhow!("enum field reference"))?;
                        ensure!(
                            class.symbols.types[owner as usize] != class.descriptor
                                || class.symbols.strings[field as usize] != array,
                            "enum backing array has nonstandard uses"
                        );
                    }
                }
            }
        }
        let plan = Self {
            constants,
            array,
            entries,
        };
        ensure!(
            class
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == "values")
                .all(|m| plan.implicit_values(class, m)),
            "custom enum values method"
        );
        ensure!(
            class
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == "valueOf")
                .all(|m| Self::standard_value_of(class, m)),
            "custom enum valueOf method"
        );
        Ok(plan)
    }
    fn implicit_values(&self, class: &DexClass, m: &DexMethod) -> bool {
        if m.access_flags & 9 != 9
            || !m.parameters.is_empty()
            || m.return_type.as_ref() != format!("[{}", class.descriptor)
        {
            return false;
        }
        let Some(code) = &m.code else {
            return false;
        };
        let w = &code.instructions;

        if !code.try_regions.is_empty()
            || w.len() != 9
            || w[0] as u8 != 0x62
            || w[2] as u8 != 0x6e
            || w[5] as u8 != 0x0c
            || w[6] as u8 != 0x1f
            || w[8] as u8 != 0x11
        {
            return false;
        }
        let source = (w[0] >> 8) as usize;
        let result = (w[5] >> 8) as usize;
        if w[2] >> 8 != 0x10
            || (w[4] & 15) as usize != source
            || w[4] >> 4 != 0
            || (w[6] >> 8) as usize != result
            || (w[8] >> 8) as usize != result
            || class.symbols.types.get(w[7] as usize) != Some(&m.return_type)
        {
            return false;
        }
        self.values_shape(class, m)
    }
    fn values_shape(&self, class: &DexClass, m: &DexMethod) -> bool {
        let code = m.code.as_ref().unwrap();
        let w = &code.instructions;
        let Some(&(owner, _, field)) = class.symbols.fields.get(w[1] as usize) else {
            return false;
        };
        let Some(&(array, proto, method)) = class.symbols.methods.get(w[3] as usize) else {
            return false;
        };
        class.symbols.types[owner as usize] == class.descriptor
            && class.symbols.strings[field as usize] == self.array
            && (class.symbols.types[array as usize].as_ref() == format!("[{}", class.descriptor)
                || class.symbols.types[array as usize].as_ref() == "Ljava/lang/Object;")
            && class.symbols.strings[method as usize] == "clone"
            && class.symbols.protos[proto as usize].1.is_empty()
            && class.symbols.protos[proto as usize].0.as_ref() == "Ljava/lang/Object;"
    }
    pub(super) fn initializer(&self, name: &str, class: &DexClass) -> Result<DecompiledCode> {
        let mut out = Output::default();
        for (i, (constant, field)) in self.constants.iter().enumerate() {
            out.push("    ");
            out.definition(
                constant,
                field,
                &format!("{name}.{field}:{}", class.descriptor),
                "field",
            );
            out.push(if i + 1 == self.constants.len() {
                ";\n"
            } else {
                ",\n"
            });
        }
        for (constant, field) in &self.constants {
            if constant != field {
                out.push("    public static final ");
                out.push(&java_type(&class.descriptor)?);
                out.push(" ");
                out.definition(
                    &names::member(field)?,
                    field,
                    &format!("{name}.{field}:{}", class.descriptor),
                    "field",
                );
                out.push(&format!(" = {constant};\n"));
            }
        }
        if self.entries.is_some() {
            out.push("    private static final kotlin.enums.EnumEntries ");
            out.definition(
                "$ENTRIES",
                "$ENTRIES",
                &format!("{name}.$ENTRIES:Lkotlin/enums/EnumEntries;"),
                "field",
            );
            out.push(" = kotlin.enums.EnumEntriesKt.enumEntries(values());\n");
        }
        Ok(out.finish())
    }
    pub(super) fn render(&self, name: &str, class: &DexClass) -> Result<DecompiledCode> {
        let display = names::qualified(name, '.')?;
        let (package, simple) = display.rsplit_once('.').unwrap_or(("", &display));
        let mut out = Output::default();
        if !package.is_empty() {
            out.push(&format!("package {package};\n\n"));
        }
        let class_annotations =
            annotation_directory(class).and_then(|directory| directory.class.as_ref());
        out.append(annotations::lines(class, class_annotations, ""));
        out.push(access(class.access_flags)?);
        out.push("enum ");
        out.definition(simple, name, name, "class");
        if !class.interfaces.is_empty() {
            out.push(" implements ");
            for (i, t) in class.interfaces.iter().enumerate() {
                if i > 0 {
                    out.push(", ");
                }
                out.reference(&java_type(t)?, &names::label(t).unwrap());
            }
        }
        out.push(" {\n");
        out.append(self.initializer(name, class)?);
        for m in &class.methods {
            if matches!(m.name.as_ref(), "<clinit>" | "values" | "valueOf")
                || (self.entries.is_some() && matches!(m.name.as_ref(), "<init>" | "$values"))
            {
                continue;
            }
            append_presented_method(&mut out, class, m, render_method(name, class, m)?)?;
        }
        Ok(finish_class(name, class, out))
    }
}
