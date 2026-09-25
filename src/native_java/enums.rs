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
}
pub(super) struct Plan {
    constants: Vec<(String, String)>,
    array: String,
}
impl Plan {
    pub(super) fn analyze(class: &DexClass) -> Result<Self> {
        ensure!(
            class.access_flags & 0x4000 != 0
                && class.superclass.as_deref() == Some("Ljava/lang/Enum;"),
            "not enum"
        );
        ensure!(
            !class.methods.iter().any(|m| m.name.as_ref() == "<init>")
                && class.fields.iter().all(|f| f.is_static),
            "enum constructor or instance state"
        );
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
                array.is_none() || matches!(insn.opcode, 0x00 | 0x0e),
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
                        class.symbols.types[owner as usize].as_ref() == "Ljava/lang/Enum;"
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
            class.fields.len() == constants.len() + 1
                && class.static_values.is_empty()
                && class.annotations_offset == 0,
            "enum extra static state"
        );
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
        let plan = Self { constants, array };
        ensure!(
            class
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == "values")
                .all(|m| plan.implicit_values(class, m)),
            "custom enum values method"
        );
        ensure!(
            !class.methods.iter().any(|m| m.name.as_ref() == "valueOf"),
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
        Ok(out.finish())
    }
    pub(super) fn render(&self, name: &str, class: &DexClass) -> Result<DecompiledCode> {
        let display = names::qualified(name, '.')?;
        let (package, simple) = display.rsplit_once('.').unwrap_or(("", &display));
        let mut out = Output::default();
        if !package.is_empty() {
            out.push(&format!("package {package};\n\n"));
        }
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
            if matches!(m.name.as_ref(), "<clinit>" | "values") {
                continue;
            }
            append_presented_method(&mut out, class, m, render_method(name, class, m)?)?;
        }
        Ok(finish_class(name, class, out))
    }
}
