//! A source view of DEX anonymous classes that cannot be written as Java
//! anonymous expressions without changing their superclass or interfaces.
//! The original DEX class remains independently navigable. Only exact system
//! metadata may authorize a named nested presentation.

use super::{
    Output, access, annotation_directory, annotations, identifier, java_type, names, presentation,
    readable, render_field, render_header, render_method,
};
use crate::{
    engine::DecompiledCode,
    native_dex::{DexAnnotation, DexClass, DexField, DexMethod, DexValue},
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, HashSet};

const INNER: &str = "Ldalvik/annotation/InnerClass;";
const ENCLOSING_METHOD: &str = "Ldalvik/annotation/EnclosingMethod;";

#[derive(Debug, Clone)]
pub(crate) struct Nested {
    pub(crate) outer: String,
    pub(crate) child: String,
    pub(crate) simple: String,
    pub(crate) access_flags: u32,
    enclosing_name: String,
    enclosing_parameters: Vec<std::sync::Arc<str>>,
    enclosing_return: std::sync::Arc<str>,
}

fn descriptor_name(descriptor: &str) -> Result<String> {
    Ok(descriptor
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(';'))
        .context("Invalid enclosing class descriptor")?
        .replace('/', "."))
}

fn annotation<'a>(class: &'a DexClass, descriptor: &str) -> Option<&'a DexAnnotation> {
    annotation_directory(class)?
        .class
        .as_ref()?
        .iter()
        .find(|annotation| {
            annotation.visibility == 2
                && class
                    .symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .is_some_and(|ty| ty.as_ref() == descriptor)
        })
        .map(AsRef::as_ref)
}

fn element<'a>(
    class: &DexClass,
    annotation: &'a DexAnnotation,
    name: &str,
) -> Option<&'a DexValue> {
    annotation.elements.iter().find_map(|(key, value)| {
        (class.symbols.strings.get(*key as usize)?.as_str() == name).then_some(value)
    })
}

pub(crate) fn metadata(class: &DexClass) -> Result<Option<Nested>> {
    let (Some(inner), Some(enclosing)) = (
        annotation(class, INNER),
        annotation(class, ENCLOSING_METHOD),
    ) else {
        return Ok(None);
    };
    // A named InnerClass is already an ordinary member/local class. A missing
    // or malformed name does not authorize synthesizing a Java member.
    if element(class, inner, "name") != Some(&DexValue::Null) {
        return Ok(None);
    }
    let Some(DexValue::Int(flags)) = element(class, inner, "accessFlags") else {
        return Ok(None);
    };
    let flags = *flags as u32;
    // A static nested view retains the explicit DEX capture constructor. A
    // non-static Java member would add a second hidden enclosing argument.
    if flags & 8 == 0
        || flags & !(7 | 8 | 0x10 | 0x400 | 0x1000) != 0
        || flags & (7 | 0x10 | 0x400) != class.access_flags & (7 | 0x10 | 0x400)
        || class.access_flags & (0x200 | 0x4000) != 0
    {
        return Ok(None);
    }
    let Some(DexValue::Method(method_index)) = element(class, enclosing, "value") else {
        return Ok(None);
    };
    let &(owner_index, proto_index, name_index) = class
        .symbols
        .methods
        .get(*method_index as usize)
        .context("Anonymous enclosing method index")?;
    let owner = class
        .symbols
        .types
        .get(owner_index as usize)
        .context("Anonymous enclosing method owner")?;
    let (ret, args) = class
        .symbols
        .protos
        .get(proto_index as usize)
        .context("Anonymous enclosing method prototype")?;
    let method_name = class
        .symbols
        .strings
        .get(name_index as usize)
        .context("Anonymous enclosing method name")?;
    let prefix = owner
        .strip_suffix(';')
        .context("Invalid anonymous enclosing owner")?;
    let Some(simple) = class
        .descriptor
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('$'))
        .and_then(|suffix| suffix.strip_suffix(';'))
    else {
        return Ok(None);
    };
    if !identifier(simple) || simple.len() > 4096 {
        return Ok(None);
    }
    let outer = descriptor_name(owner)?;
    let child = descriptor_name(&class.descriptor)?;
    if simple == outer.rsplit('.').next().unwrap_or_default() {
        return Ok(None);
    }
    Ok(Some(Nested {
        outer,
        child,
        simple: simple.to_owned(),
        access_flags: flags,
        enclosing_name: method_name.clone(),
        enclosing_parameters: args.clone(),
        enclosing_return: ret.clone(),
    }))
}

pub(crate) fn index(classes: &BTreeMap<String, DexClass>) -> BTreeMap<String, Vec<String>> {
    let mut index = BTreeMap::<String, Vec<String>>::new();
    for (name, class) in classes {
        let Ok(Some(nested)) = metadata(class) else {
            continue;
        };
        let Some(outer) = classes.get(&nested.outer) else {
            continue;
        };
        if nested.child == *name && nested.valid_for(outer) {
            index.entry(nested.outer).or_default().push(name.clone());
        }
    }
    index
}

impl Nested {
    fn valid_for(&self, outer: &DexClass) -> bool {
        descriptor_name(&outer.descriptor).ok().as_deref() == Some(&self.outer)
            && outer.methods.iter().any(|method| {
                method.name.as_ref() == self.enclosing_name
                    && method.parameters == self.enclosing_parameters
                    && method.return_type == self.enclosing_return
            })
            && !outer
                .fields
                .iter()
                .any(|field| field.name.as_ref() == self.simple)
            && !outer
                .methods
                .iter()
                .any(|method| method.name.as_ref() == self.simple)
    }
}

fn indent(code: DecompiledCode) -> DecompiledCode {
    let mut edits = vec![(0, 0, "    ".to_owned())];
    let len = code.source.chars().count();
    for (position, ch) in code.source.chars().enumerate() {
        if ch == '\n' && position + 1 < len {
            edits.push((position + 1, position + 1, "    ".to_owned()));
        }
    }
    readable::apply(code, edits)
}

fn constructor_name(mut code: DecompiledCode, simple: &str) -> Result<DecompiledCode> {
    let Some(definition) = code
        .definitions
        .iter()
        .find(|definition| definition.kind == "method" && definition.name == "<init>")
    else {
        return Ok(code);
    };
    let (start, end) = (definition.start, definition.end);
    ensure!(start < end, "Invalid nested constructor definition");
    code = readable::apply(code, vec![(start, end, simple.to_owned())]);
    Ok(code)
}

fn field_ref_matches(class: &DexClass, index: usize, field: &DexField) -> bool {
    class
        .symbols
        .fields
        .get(index)
        .and_then(|&(owner, ty, name)| {
            Some((
                class.symbols.types.get(owner as usize)?,
                class.symbols.types.get(ty as usize)?,
                class.symbols.strings.get(name as usize)?,
            ))
        })
        .is_some_and(|(owner, ty, name)| {
            owner == &class.descriptor && ty == &field.field_type && name == field.name.as_ref()
        })
}

// Java permits a blank final instance field when every constructor assigns it
// exactly once. Keep this intentionally smaller than general definite
// assignment: straight-line constructors, one exact superclass call, and no
// other method writing the field. The constructor body itself is unchanged.
fn blank_final_capture(class: &DexClass, field: &DexField) -> bool {
    const MAX_METHODS: usize = 128;
    const MAX_WORDS: usize = 512;
    if field.is_static
        || field.access_flags & 0x1010 != 0x1010
        || field.declaring_type != class.descriptor
        || class.methods.len() > MAX_METHODS
        || annotation_directory(class)
            .and_then(|directory| {
                class
                    .fields
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, field))
                    .and_then(|index| directory.fields.get(index))
            })
            .and_then(Option::as_ref)
            .is_some_and(|set| set.iter().any(|annotation| annotation.visibility != 2))
    {
        return false;
    }
    let mut constructors = 0;
    for method in &class.methods {
        let Some(code) = method.code.as_ref() else {
            return false;
        };
        if code.instructions.len() > MAX_WORDS {
            return false;
        }
        if code.ins > code.registers {
            return false;
        }
        let Ok(decoded) = crate::native_ir::DecodedMethod::decode(code) else {
            return false;
        };
        let constructor = method.name.as_ref() == "<init>";
        if constructor {
            constructors += 1;
            if !code.try_regions.is_empty() {
                return false;
            }
        }
        let this_register = usize::from(code.registers - code.ins);
        let mut writes = 0;
        let mut super_calls = 0;
        let mut returns = 0;
        for instruction in &decoded.instructions {
            let pc = instruction.pc;
            // The identity of the incoming receiver is established by the
            // parameter register only while no instruction writes it.
            if constructor {
                let first = code.instructions[pc];
                let destination = match instruction.opcode {
                    0x01..=0x09 => return false,
                    0x12 => Some(usize::from((first >> 8) & 15)),
                    0x13..=0x19 => Some(usize::from(first >> 8)),
                    _ => None,
                };
                if destination.is_some_and(|register| {
                    register == this_register
                        || matches!(instruction.opcode, 0x16..=0x19)
                            && register.checked_add(1) == Some(this_register)
                }) {
                    return false;
                }
            }
            match instruction.opcode {
                0x59..=0x5f => {
                    let Some(&index) = code.instructions.get(pc + 1) else {
                        return false;
                    };
                    let Some(&(owner, ty, name)) = class.symbols.fields.get(index as usize) else {
                        return false;
                    };
                    let (Some(owner), Some(ty), Some(name)) = (
                        class.symbols.types.get(owner as usize),
                        class.symbols.types.get(ty as usize),
                        class.symbols.strings.get(name as usize),
                    ) else {
                        return false;
                    };
                    if name == field.name.as_ref()
                        && (owner != &class.descriptor || ty != &field.field_type)
                    {
                        return false;
                    }
                    if field_ref_matches(class, index as usize, field) {
                        if !constructor
                            || ((code.instructions[pc] >> 12) as usize & 15) != this_register
                        {
                            return false;
                        }
                        writes += 1;
                    }
                }
                0x70 | 0x76 if constructor => {
                    let Some(&index) = code.instructions.get(pc + 1) else {
                        return false;
                    };
                    let Some(&(owner, proto, name)) = class.symbols.methods.get(index as usize)
                    else {
                        return false;
                    };
                    let (Some(owner), Some((ret, _)), Some(name)) = (
                        class.symbols.types.get(owner as usize),
                        class.symbols.protos.get(proto as usize),
                        class.symbols.strings.get(name as usize),
                    ) else {
                        return false;
                    };
                    let receiver = if instruction.opcode == 0x76 {
                        code.instructions.get(pc + 2).copied().map(usize::from)
                    } else {
                        code.instructions
                            .get(pc + 2)
                            .map(|word| usize::from(word & 15))
                    };
                    if Some(owner) != class.superclass.as_ref()
                        || name != "<init>"
                        || ret.as_ref() != "V"
                        || receiver != Some(this_register)
                    {
                        return false;
                    }
                    super_calls += 1;
                }
                0x0e if constructor => returns += 1,
                0x00..=0x09 | 0x12..=0x19 if constructor => {}
                _ if constructor => return false,
                _ => {}
            }
        }
        if constructor
            && (writes != 1
                || super_calls != 1
                || returns != 1
                || decoded
                    .instructions
                    .last()
                    .is_none_or(|instruction| instruction.opcode != 0x0e))
        {
            return false;
        }
    }
    constructors > 0
}

fn render_capture_field(name: &str, class: &DexClass, field: &DexField) -> Result<DecompiledCode> {
    ensure!(
        blank_final_capture(class, field),
        "Blank final capture proof failed"
    );
    let temporary = DexField {
        declaring_type: field.declaring_type.clone(),
        name: field.name.clone(),
        field_type: field.field_type.clone(),
        access_flags: field.access_flags & !0x10,
        is_static: false,
    };
    let code = render_field(name, class, &temporary)?;
    let at = 4 + access(temporary.access_flags)?.chars().count();
    Ok(readable::insert(code, at, "final "))
}

// Moving a pure capture store past super is legal only when that constructor
// cannot observe the child, escape it, or throw. This is a bounded proof over
// loaded DEX; Object.<init>() is the sole platform base case.
fn nonobserving_constructor(
    owner: &str,
    args: &[std::sync::Arc<str>],
    world: Option<&BTreeMap<String, DexClass>>,
    seen: &mut HashSet<String>,
) -> bool {
    if owner == "Ljava/lang/Object;" {
        return args.is_empty()
            && !world.is_some_and(|classes| classes.contains_key("java.lang.Object"));
    }
    if seen.len() >= 8 || !seen.insert(owner.to_owned()) {
        return false;
    }
    let Some(class) = world.and_then(|classes| classes.get(&descriptor_name(owner).ok()?)) else {
        return false;
    };
    let mut constructors = class.methods.iter().filter(|method| {
        method.name.as_ref() == "<init>"
            && method.parameters == args
            && method.return_type.as_ref() == "V"
    });
    let Some(method) = constructors.next() else {
        return false;
    };
    if constructors.next().is_some() {
        return false;
    }
    let Some(code) = method.code.as_ref() else {
        return false;
    };
    if code.instructions.len() > 64 || !code.try_regions.is_empty() || code.ins > code.registers {
        return false;
    }
    let incoming_words = 1 + method
        .parameters
        .iter()
        .map(|ty| {
            if matches!(ty.as_ref(), "J" | "D") {
                2
            } else {
                1
            }
        })
        .sum::<usize>();
    if usize::from(code.ins) != incoming_words {
        return false;
    }
    let Ok(decoded) = crate::native_ir::DecodedMethod::decode(code) else {
        return false;
    };
    let this_register = usize::from(code.registers - code.ins);
    let mut super_seen = false;
    let mut returns = 0;
    for instruction in &decoded.instructions {
        let pc = instruction.pc;
        match instruction.opcode {
            0x70 | 0x76 if !super_seen => {
                let Some(&index) = code.instructions.get(pc + 1) else {
                    return false;
                };
                let Some(&(owner_index, proto_index, name_index)) =
                    class.symbols.methods.get(index as usize)
                else {
                    return false;
                };
                let (Some(invoked_owner), Some((ret, parameters)), Some(name)) = (
                    class.symbols.types.get(owner_index as usize),
                    class.symbols.protos.get(proto_index as usize),
                    class.symbols.strings.get(name_index as usize),
                ) else {
                    return false;
                };
                let receiver = if instruction.opcode == 0x76 {
                    code.instructions.get(pc + 2).copied().map(usize::from)
                } else {
                    code.instructions
                        .get(pc + 2)
                        .map(|word| usize::from(word & 15))
                };
                let expected_words = 1 + parameters
                    .iter()
                    .map(|ty| usize::from(matches!(ty.as_ref(), "J" | "D")) + 1)
                    .sum::<usize>();
                let actual_words = if instruction.opcode == 0x76 {
                    usize::from(code.instructions[pc] >> 8)
                } else {
                    usize::from((code.instructions[pc] >> 12) & 15)
                };
                if Some(invoked_owner) != class.superclass.as_ref()
                    || name != "<init>"
                    || ret.as_ref() != "V"
                    || !parameters.is_empty()
                    || receiver != Some(this_register)
                    || actual_words != expected_words
                    || (instruction.opcode == 0x76
                        && receiver.is_some_and(|start| {
                            start + actual_words > usize::from(code.registers)
                        }))
                    || !nonobserving_constructor(invoked_owner, parameters, world, seen)
                {
                    return false;
                }
                super_seen = true;
            }
            0x59..=0x5f if super_seen => {
                let Some(&index) = code.instructions.get(pc + 1) else {
                    return false;
                };
                let Some(&(owner_index, ty_index, name_index)) =
                    class.symbols.fields.get(index as usize)
                else {
                    return false;
                };
                let (Some(field_owner), Some(ty), Some(name)) = (
                    class.symbols.types.get(owner_index as usize),
                    class.symbols.types.get(ty_index as usize),
                    class.symbols.strings.get(name_index as usize),
                ) else {
                    return false;
                };
                if field_owner != &class.descriptor
                    || usize::from((code.instructions[pc] >> 12) & 15) != this_register
                    || !matches!(
                        (instruction.opcode, ty.as_ref()),
                        (0x59, "I" | "F")
                            | (0x5a, "J" | "D")
                            | (0x5c, "Z")
                            | (0x5d, "B")
                            | (0x5e, "C")
                            | (0x5f, "S")
                    )
                    || !{
                        let source = usize::from((code.instructions[pc] >> 8) & 15);
                        let mut head = this_register + 1;
                        method.parameters.iter().any(|parameter| {
                            let matches = head == source && parameter == ty;
                            head += if matches!(parameter.as_ref(), "J" | "D") {
                                2
                            } else {
                                1
                            };
                            matches
                        })
                    }
                    || !class.fields.iter().any(|field| {
                        field.declaring_type == class.descriptor
                            && field.name.as_ref() == name
                            && field.field_type == *ty
                            && !field.is_static
                    })
                {
                    return false;
                }
            }
            0x0e if super_seen => returns += 1,
            0x00 => {}
            _ => return false,
        }
    }
    super_seen
        && returns == 1
        && decoded
            .instructions
            .last()
            .is_some_and(|instruction| instruction.opcode == 0x0e)
}

fn pre_super_captures(
    class: &DexClass,
    method: &DexMethod,
    world: Option<&BTreeMap<String, DexClass>>,
) -> Result<Vec<String>> {
    let code = method
        .code
        .as_ref()
        .context("Anonymous constructor lacks code")?;
    let decoded = crate::native_ir::DecodedMethod::decode(code)?;
    let this_register = usize::from(code.registers - code.ins);
    let mut fields = Vec::new();
    let mut past_fields = false;
    for instruction in &decoded.instructions {
        let pc = instruction.pc;
        match instruction.opcode {
            0x59..=0x5f if !past_fields => {
                let word = code.instructions[pc];
                ensure!(
                    usize::from((word >> 12) & 15) == this_register,
                    "Pre-super write receiver differs"
                );
                let source = usize::from((word >> 8) & 15);
                ensure!(
                    source > this_register && source < usize::from(code.registers),
                    "Pre-super capture is not an incoming argument"
                );
                let index = usize::from(code.instructions[pc + 1]);
                let field = class
                    .fields
                    .iter()
                    .find(|field| field_ref_matches(class, index, field))
                    .context("Pre-super field is not owned")?;
                ensure!(
                    field.access_flags & 0x1010 == 0x1010 && blank_final_capture(class, field),
                    "Pre-super write is not a proved capture"
                );
                ensure!(
                    !fields.contains(&field.name.to_string()),
                    "Duplicate pre-super capture"
                );
                fields.push(field.name.to_string());
            }
            0x70 | 0x76 => {
                let index = usize::from(code.instructions[pc + 1]);
                let &(owner_index, proto_index, name_index) = class
                    .symbols
                    .methods
                    .get(index)
                    .context("Pre-super constructor reference")?;
                let owner = class
                    .symbols
                    .types
                    .get(owner_index as usize)
                    .context("Pre-super constructor owner")?;
                let (ret, args) = class
                    .symbols
                    .protos
                    .get(proto_index as usize)
                    .context("Pre-super constructor prototype")?;
                let name = class
                    .symbols
                    .strings
                    .get(name_index as usize)
                    .context("Pre-super constructor name")?;
                ensure!(
                    Some(owner) == class.superclass.as_ref()
                        && name == "<init>"
                        && ret.as_ref() == "V",
                    "Pre-super call is not direct superclass constructor"
                );
                if !fields.is_empty() {
                    ensure!(
                        nonobserving_constructor(owner, args, world, &mut HashSet::new()),
                        "Superclass constructor may observe a capture"
                    );
                }
                return Ok(fields);
            }
            0x12..=0x19 => past_fields = true,
            0x00 => {}
            _ => anyhow::bail!("Unproved pre-super constructor effect"),
        }
    }
    anyhow::bail!("Anonymous constructor has no super call")
}

fn line_range(chars: &[char], position: usize) -> Result<(usize, usize)> {
    ensure!(position < chars.len(), "Anonymous source link out of range");
    let start = chars[..position]
        .iter()
        .rposition(|ch| *ch == '\n')
        .map_or(0, |index| index + 1);
    let end = chars[position..]
        .iter()
        .position(|ch| *ch == '\n')
        .map_or(chars.len(), |index| position + index + 1);
    Ok((start, end))
}

fn reorder_capture_lines(
    mut code: DecompiledCode,
    class: &DexClass,
    field_names: &[String],
) -> Result<DecompiledCode> {
    if field_names.is_empty() {
        return Ok(code);
    }
    let chars: Vec<char> = code.source.chars().collect();
    let mut ranges = Vec::new();
    for name in field_names {
        let field = class
            .fields
            .iter()
            .find(|field| field.name.as_ref() == name)
            .context("Capture field missing")?;
        let label = format!(
            "{}.{}:{}",
            descriptor_name(&class.descriptor)?,
            name,
            field.field_type
        );
        let link = code
            .links
            .iter()
            .find(|link| link.label == label)
            .context("Capture assignment link missing")?;
        let range = line_range(&chars, link.start)?;
        let line: String = chars[range.0..range.1].iter().collect();
        ensure!(
            line.trim().starts_with(&format!("this.{name} = "))
                && line.trim_end().ends_with(';')
                && line.matches(';').count() == 1,
            "Capture assignment source is not simple"
        );
        ranges.push(range);
    }
    ensure!(
        ranges.windows(2).all(|pair| pair[0].1 == pair[1].0),
        "Capture assignments are not adjacent"
    );
    let super_link = code
        .links
        .iter()
        .find(|link| {
            link.label.contains(".<init>(")
                && line_range(&chars, link.start).ok().is_some_and(|range| {
                    chars[range.0..range.1]
                        .iter()
                        .collect::<String>()
                        .trim()
                        .starts_with("super(")
                })
        })
        .context("Superclass constructor link missing")?;
    let super_range = line_range(&chars, super_link.start)?;
    ensure!(
        ranges.last().is_some_and(|range| range.1 == super_range.0),
        "Superclass call is not after captures"
    );
    let super_line: String = chars[super_range.0..super_range.1].iter().collect();
    ensure!(
        super_line.trim().starts_with("super(")
            && super_line.trim_end().ends_with(';')
            && super_line.matches(';').count() == 1,
        "Superclass call source is not simple"
    );
    let a = ranges[0].0;
    let b = super_range.0;
    let c = super_range.1;
    let mut source = String::new();
    source.extend(&chars[..a]);
    source.extend(&chars[b..c]);
    source.extend(&chars[a..b]);
    source.extend(&chars[c..]);
    let move_span = |start: &mut usize, end: &mut usize| -> Result<()> {
        if *start >= a && *end <= b {
            *start += c - b;
            *end += c - b;
        } else if *start >= b && *end <= c {
            *start -= b - a;
            *end -= b - a;
        } else {
            ensure!(
                *end <= a || *start >= c,
                "Anonymous source span crosses reordered lines"
            );
        }
        Ok(())
    };
    for link in &mut code.links {
        move_span(&mut link.start, &mut link.end)?;
    }
    for definition in &mut code.definitions {
        move_span(&mut definition.start, &mut definition.end)?;
    }
    code.source = source;
    code.source_hash = crate::engine::source_identity(&code.source);
    Ok(code)
}

pub(crate) fn render_nested(
    outer: &DexClass,
    child: &DexClass,
    nested: &Nested,
    world: Option<&BTreeMap<String, DexClass>>,
) -> Result<DecompiledCode> {
    ensure!(
        nested.valid_for(outer),
        "Anonymous enclosing method mismatch"
    );
    ensure!(
        descriptor_name(&child.descriptor)? == nested.child,
        "Anonymous owner mismatch"
    );
    // Reuse the ordinary header validation and its member invariants, while
    // allowing a separately proved blank final capture declaration below.
    render_header(&nested.child, child)?;
    ensure!(
        child.annotations_offset == 0 || annotation_directory(child).is_some(),
        "Anonymous annotations unresolved"
    );
    ensure!(
        child.access_flags & !0x611 == 0,
        "Unsupported anonymous class modifiers"
    );
    ensure!(
        child.access_flags & 0x410 != 0x410,
        "Anonymous class abstract and final"
    );
    ensure!(
        child.access_flags & 0x400 != 0
            || !child
                .methods
                .iter()
                .any(|method| method.access_flags & 0x400 != 0),
        "Abstract method in concrete anonymous class"
    );
    let mut signatures = HashSet::new();
    ensure!(
        child
            .methods
            .iter()
            .all(|method| signatures.insert((method.name.as_ref(), method.parameters.as_slice()))),
        "Duplicate anonymous Java method signatures"
    );
    let mut fields = HashSet::new();
    ensure!(
        child
            .fields
            .iter()
            .all(|field| fields.insert(field.name.as_ref())),
        "Duplicate anonymous Java fields"
    );
    ensure!(
        child.static_values.len() <= child.fields.iter().filter(|field| field.is_static).count(),
        "Anonymous static values exceed fields"
    );
    ensure!(
        child.static_values_offset == 0 || !child.static_values.is_empty(),
        "Anonymous static values unresolved"
    );
    let mut out = Output::default();
    out.push("\n");
    let class_annotations =
        annotation_directory(child).and_then(|directory| directory.class.as_ref());
    out.append(annotations::lines(child, class_annotations, "    "));
    out.push("    ");
    out.push(access(nested.access_flags)?);
    out.push("static ");
    if nested.access_flags & 0x10 != 0 {
        out.push("final ");
    }
    if nested.access_flags & 0x400 != 0 {
        out.push("abstract ");
    }
    out.push("class ");
    out.definition(&nested.simple, &nested.simple, &nested.child, "class");
    if let Some(parent) = &child.superclass {
        if parent.as_ref() != "Ljava/lang/Object;" {
            let display = java_type(parent)?;
            out.push(" extends ");
            out.reference(
                &display,
                &names::label(parent).context("Nested superclass label")?,
            );
        }
    } else {
        anyhow::bail!("Anonymous child lacks superclass");
    }
    if !child.interfaces.is_empty() {
        out.push(" implements ");
        for (index, interface) in child.interfaces.iter().enumerate() {
            if index != 0 {
                out.push(", ");
            }
            let display = java_type(interface)?;
            out.reference(
                &display,
                &names::label(interface).context("Nested interface label")?,
            );
        }
    }
    out.push(" {\n");
    for field in &child.fields {
        let code = render_field(&nested.child, child, field).or_else(|error| {
            if error.to_string() == "Final field initializer not reconstructed" {
                render_capture_field(&nested.child, child, field)
            } else {
                Err(error)
            }
        })?;
        out.append(indent(code));
    }
    for method in &child.methods {
        let captures = if method.name.as_ref() == "<init>" {
            pre_super_captures(child, method, world)?
        } else {
            Vec::new()
        };
        let mut code = presentation(render_method(&nested.child, child, method)?, child);
        if method.name.as_ref() == "<init>" {
            code = reorder_capture_lines(code, child, &captures)?;
            code = constructor_name(code, &nested.simple)?;
        }
        out.append(indent(code));
    }
    out.push("    }\n");
    Ok(out.finish())
}

pub(super) fn append_proven(
    out: &mut Output,
    outer: &DexClass,
    children: &[&DexClass],
    world: Option<&BTreeMap<String, DexClass>>,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<(String, String)>> {
    const MAX_NESTED: usize = 32;
    const MAX_METHODS: usize = 256;
    const MAX_WORDS: usize = 8192;
    const MAX_SOURCE_CHARS: usize = 200_000;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut methods = 0usize;
    let mut words = 0usize;
    let mut chars = 0usize;
    for child in children.iter().take(MAX_NESTED + 1) {
        ensure!(!cancelled(), "decompilation cancelled");
        if names.len() == MAX_NESTED {
            break;
        }
        methods += child.methods.len();
        words += child
            .methods
            .iter()
            .map(|method| {
                method
                    .code
                    .as_ref()
                    .map_or(0, |code| code.instructions.len())
            })
            .sum::<usize>();
        if methods > MAX_METHODS || words > MAX_WORDS {
            break;
        }
        let Ok(Some(nested)) = metadata(child) else {
            continue;
        };
        if !seen.insert(nested.simple.clone()) {
            continue;
        }
        if let Ok(code) = render_nested(outer, child, &nested, world) {
            chars += code.source.chars().count();
            if chars > MAX_SOURCE_CHARS {
                break;
            }
            out.append(code);
            names.push((nested.child, nested.simple));
        }
    }
    Ok(names)
}

pub(crate) fn shorten_bound_types(
    code: DecompiledCode,
    names: &[(String, String)],
) -> DecompiledCode {
    if names.is_empty() {
        return code;
    }
    let chars: Vec<char> = code.source.chars().collect();
    let mut edits = Vec::new();
    for link in &code.links {
        if let Some((owner, simple)) = names.iter().find(|(owner, _)| {
            owner == &link.label || link.label.starts_with(&format!("{owner}.<init>("))
        }) && link.end <= chars.len()
            && link.start < link.end
            && chars[link.start..link.end].iter().collect::<String>() == *owner
        {
            edits.push((link.start, link.end, simple.clone()));
        }
    }
    edits.sort_unstable_by_key(|edit| edit.0);
    edits.dedup_by_key(|edit| (edit.0, edit.1));
    readable::apply(code, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexAnnotationDirectory, DexCode, DexField, DexMethod, DexSymbols};
    use std::{process::Command, sync::Arc};

    fn render_nested(
        outer: &DexClass,
        child: &DexClass,
        nested: &Nested,
    ) -> Result<DecompiledCode> {
        super::render_nested(outer, child, nested, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn method(
        owner: &str,
        name: &str,
        parameters: &[&str],
        ret: &str,
        access_flags: u32,
        registers: u16,
        ins: u16,
        outs: u16,
        instructions: &[u16],
    ) -> DexMethod {
        DexMethod {
            declaring_type: owner.into(),
            name: name.into(),
            return_type: ret.into(),
            parameters: parameters.iter().map(|ty| Arc::from(*ty)).collect(),
            thrown_types: Vec::new(),
            access_flags,
            code: Some(DexCode {
                registers,
                ins,
                outs,
                tries: 0,
                try_regions: Vec::new(),
                instructions: instructions.into(),
                offset: 0,
            }),
        }
    }

    fn fixture_with_metadata(
        inner_name: DexValue,
        inner_flags: i32,
        enclosing_method: u32,
    ) -> (DexClass, DexClass) {
        const OUTER: &str = "Lsample/Outer;";
        const CHILD: &str = "Lsample/Outer$anon$1;";
        let mut symbols = DexSymbols {
            strings: [
                "<init>",
                "make",
                "run",
                "value",
                "X",
                "name",
                "accessFlags",
                "value",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            types: [
                OUTER,
                CHILD,
                "Ljava/lang/Object;",
                "Ljava/lang/Runnable;",
                "I",
                INNER,
                ENCLOSING_METHOD,
            ]
            .into_iter()
            .map(Arc::from)
            .collect(),
            protos: vec![("V".into(), Vec::new()), ("V".into(), vec!["I".into()])],
            fields: vec![(1, 4, 4)],
            methods: vec![(2, 0, 0), (1, 1, 0), (0, 0, 0)],
            ..Default::default()
        };
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                class: Some(
                    vec![
                        Arc::new(DexAnnotation {
                            visibility: 2,
                            type_idx: 5,
                            elements: vec![(5, inner_name), (6, DexValue::Int(inner_flags))],
                        }),
                        Arc::new(DexAnnotation {
                            visibility: 2,
                            type_idx: 6,
                            elements: vec![(7, DexValue::Method(enclosing_method))],
                        }),
                    ]
                    .into(),
                ),
                ..Default::default()
            }),
        );
        let symbols = Arc::new(symbols);
        let outer = DexClass {
            symbols: Arc::clone(&symbols),
            descriptor: OUTER.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: Vec::new(),
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                method(
                    OUTER,
                    "<init>",
                    &[],
                    "V",
                    0x10001,
                    1,
                    1,
                    1,
                    &[0x1070, 0, 0, 0x000e],
                ),
                method(
                    OUTER,
                    "make",
                    &["I"],
                    CHILD,
                    0x9,
                    2,
                    1,
                    2,
                    &[0x0022, 1, 0x2070, 1, 0x0010, 0x0011],
                ),
            ],
        };
        let child = DexClass {
            symbols,
            descriptor: CHILD.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec!["Ljava/lang/Runnable;".into()],
            access_flags: 0x11,
            annotations_offset: 1,
            static_values_offset: 0,
            static_values: Vec::new(),
            fields: vec![DexField {
                declaring_type: CHILD.into(),
                name: "X".into(),
                field_type: "I".into(),
                access_flags: 0x1010,
                is_static: false,
            }],
            methods: vec![
                method(
                    CHILD,
                    "<init>",
                    &["I"],
                    "V",
                    0x10001,
                    2,
                    2,
                    1,
                    &[0x1070, 0, 0, 0x0159, 0, 0x000e],
                ),
                method(CHILD, "run", &[], "V", 0x11, 1, 1, 0, &[0x000e]),
                method(
                    CHILD,
                    "value",
                    &[],
                    "I",
                    0x11,
                    2,
                    1,
                    0,
                    &[0x1052, 0, 0x000f],
                ),
            ],
        };
        (outer, child)
    }

    fn fixture() -> (DexClass, DexClass) {
        fixture_with_metadata(DexValue::Null, 25, 2)
    }

    fn fixture_with_pure_parent() -> (DexClass, DexClass, DexClass) {
        let (outer, mut child) = fixture();
        let original = &child.symbols;
        let mut symbols = DexSymbols {
            strings: original.strings.clone(),
            types: original.types.clone(),
            protos: original.protos.clone(),
            fields: original.fields.clone(),
            methods: original.methods.clone(),
            annotations: original.annotations.clone(),
            ..Default::default()
        };
        symbols.strings.push("arity".to_owned());
        symbols.types.push("Lsample/Parent;".into());
        symbols.fields.push((7, 4, 8));
        symbols.methods.push((7, 1, 0));
        let symbols = Arc::new(symbols);
        child.symbols = Arc::clone(&symbols);
        child.superclass = Some("Lsample/Parent;".into());
        child.methods[0].code.as_mut().unwrap().instructions =
            vec![0x0159, 0, 0x0112, 0x2070, 3, 0x0010, 0x000e];
        child.methods[0].code.as_mut().unwrap().outs = 2;
        let parent = DexClass {
            symbols,
            descriptor: "Lsample/Parent;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: Vec::new(),
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: Vec::new(),
            fields: vec![DexField {
                declaring_type: "Lsample/Parent;".into(),
                name: "arity".into(),
                field_type: "I".into(),
                access_flags: 0x12,
                is_static: false,
            }],
            methods: vec![method(
                "Lsample/Parent;",
                "<init>",
                &["I"],
                "V",
                1,
                2,
                2,
                1,
                &[0x1070, 0, 0, 0x0159, 1, 0x000e],
            )],
        };
        (outer, child, parent)
    }

    #[test]
    fn metadata_drives_nested_display_and_original_links() {
        let (outer, child) = fixture();
        assert!(annotation(&child, INNER).is_some());
        assert!(annotation(&child, ENCLOSING_METHOD).is_some());
        assert!(metadata(&child).unwrap().is_some());
        let mut classes = BTreeMap::new();
        classes.insert("sample.Outer".to_owned(), outer);
        classes.insert("sample.Outer$anon$1".to_owned(), child);
        assert_eq!(index(&classes)["sample.Outer"], ["sample.Outer$anon$1"]);
        let outer = &classes["sample.Outer"];
        let child = &classes["sample.Outer$anon$1"];
        render_nested(outer, child, &metadata(child).unwrap().unwrap()).unwrap();
        let code = super::super::render_mixed_with_nested("sample.Outer", outer, &[child], None);
        assert!(
            code.source
                .contains("static final class anon$1 implements Runnable"),
            "{}",
            code.source
        );
        assert!(code.source.contains("new anon$1("), "{}", code.source);
        assert!(code.source.contains("final int X;"), "{}", code.source);
        assert!(code.source.contains("int value()"), "{}", code.source);
        assert!(code.source.find("super();").unwrap() < code.source.find("this.X =").unwrap());
        assert!(
            code.links
                .iter()
                .any(|link| link.label == "sample.Outer$anon$1")
        );
        assert!(
            code.definitions
                .iter()
                .any(|definition| definition.kind == "class" && definition.name == "anon$1")
        );
    }

    #[test]
    fn malformed_metadata_and_nonstatic_capture_fail_closed() {
        let (outer, mut child) = fixture();
        let nested = metadata(&child).unwrap().unwrap();
        assert!(nested.valid_for(&outer));
        child.descriptor = "Lsample/Outer$1;".into();
        assert!(metadata(&child).unwrap().is_none());
        let (_, named) = fixture_with_metadata(DexValue::String(0), 25, 2);
        assert!(metadata(&named).unwrap().is_none());
        let (_, nonstatic) = fixture_with_metadata(DexValue::Null, 17, 2);
        assert!(metadata(&nonstatic).unwrap().is_none());
        let (_outer, wrong_method) = fixture_with_metadata(DexValue::Null, 25, 0);
        assert!(metadata(&wrong_method).unwrap().is_none());
        let (outer, child) = fixture();
        let mut classes = BTreeMap::new();
        classes.insert("sample.Other".to_owned(), outer);
        classes.insert("sample.Outer$anon$1".to_owned(), child);
        assert!(index(&classes).is_empty());
        let (_, mut same_name) = fixture();
        same_name.descriptor = "Lsample/Outer$Outer;".into();
        assert!(metadata(&same_name).unwrap().is_none());
    }

    #[test]
    fn blank_final_capture_requires_exact_single_constructor_write() {
        let (outer, mut missing) = fixture();
        missing.methods[0].code.as_mut().unwrap().instructions = vec![0x1070, 0, 0, 0x000e];
        assert!(render_nested(&outer, &missing, &metadata(&missing).unwrap().unwrap()).is_err());

        let (_, mut double) = fixture();
        double.methods[0].code.as_mut().unwrap().instructions =
            vec![0x1070, 0, 0, 0x0159, 0, 0x0159, 0, 0x000e];
        assert!(render_nested(&outer, &double, &metadata(&double).unwrap().unwrap()).is_err());

        let (_, mut wrong_receiver) = fixture();
        wrong_receiver.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x1070, 0, 0, 0x1159, 0, 0x000e];
        assert!(
            render_nested(
                &outer,
                &wrong_receiver,
                &metadata(&wrong_receiver).unwrap().unwrap()
            )
            .is_err()
        );

        let (_, mut overwritten_this) = fixture();
        overwritten_this.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x0012, 0x1070, 0, 0, 0x0159, 0, 0x000e];
        assert!(
            render_nested(
                &outer,
                &overwritten_this,
                &metadata(&overwritten_this).unwrap().unwrap()
            )
            .is_err()
        );

        let (_, mut unknown_field) = fixture();
        unknown_field.methods[0].code.as_mut().unwrap().instructions =
            vec![0x1070, 0, 0, 0x0159, 99, 0x000e];
        assert!(
            render_nested(
                &outer,
                &unknown_field,
                &metadata(&unknown_field).unwrap().unwrap()
            )
            .is_err()
        );

        let (_, mut alias_field) = fixture();
        let original = &alias_field.symbols;
        let mut alias_symbols = DexSymbols {
            strings: original.strings.clone(),
            types: original.types.clone(),
            protos: original.protos.clone(),
            fields: original.fields.clone(),
            methods: original.methods.clone(),
            annotations: original.annotations.clone(),
            ..Default::default()
        };
        alias_symbols.fields.push((0, 4, 4));
        alias_field.symbols = Arc::new(alias_symbols);
        alias_field.methods[0].code.as_mut().unwrap().instructions =
            vec![0x1070, 0, 0, 0x0159, 1, 0x000e];
        assert!(
            render_nested(
                &outer,
                &alias_field,
                &metadata(&alias_field).unwrap().unwrap()
            )
            .is_err()
        );

        let (_, mut cross_method) = fixture();
        cross_method.methods[1].code.as_mut().unwrap().instructions = vec![0x1059, 0, 0x000e];
        cross_method.methods[1].code.as_mut().unwrap().registers = 2;
        assert!(
            render_nested(
                &outer,
                &cross_method,
                &metadata(&cross_method).unwrap().unwrap()
            )
            .is_err()
        );

        let (_, mut duplicate_bridge) = fixture();
        duplicate_bridge.methods.push(method(
            "Lsample/Outer$anon$1;",
            "value",
            &[],
            "Ljava/lang/Object;",
            0x1041,
            1,
            1,
            0,
            &[0x0012, 0x0011],
        ));
        assert!(
            render_nested(
                &outer,
                &duplicate_bridge,
                &metadata(&duplicate_bridge).unwrap().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn pre_super_capture_requires_nonobserving_loaded_parent() {
        let (outer, child, parent) = fixture_with_pure_parent();
        let nested = metadata(&child).unwrap().unwrap();
        assert!(super::render_nested(&outer, &child, &nested, None).is_err());
        let mut world = BTreeMap::new();
        world.insert("sample.Parent".to_owned(), parent);
        let code = super::render_nested(&outer, &child, &nested, Some(&world)).unwrap();
        let super_at = code.source.find("super(0);").unwrap();
        let capture_at = code.source.find("this.X =").unwrap();
        assert!(super_at < capture_at, "{}", code.source);
        assert!(code.links.iter().any(|link| {
            link.label == "sample.Outer$anon$1.X:I"
                && link.start > code.source[..capture_at].chars().count()
        }));

        let mut observing = world.remove("sample.Parent").unwrap();
        observing.methods[0].code.as_mut().unwrap().instructions =
            vec![0x1070, 0, 0, 0x106e, 0, 0, 0x0159, 1, 0x000e];
        world.insert("sample.Parent".to_owned(), observing);
        assert!(super::render_nested(&outer, &child, &nested, Some(&world)).is_err());
        let (_, _, mut undefined_local) = fixture_with_pure_parent();
        undefined_local.methods[0].code.as_mut().unwrap().registers = 3;
        undefined_local.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x1070, 0, 1, 0x1059, 1, 0x000e];
        world.insert("sample.Parent".to_owned(), undefined_local);
        assert!(super::render_nested(&outer, &child, &nested, Some(&world)).is_err());
        let (_, _, pure_parent) = fixture_with_pure_parent();
        world.insert("sample.Parent".to_owned(), pure_parent);
        let (shadow_outer, _) = fixture();
        world.insert("java.lang.Object".to_owned(), shadow_outer);
        let (outer, child, _) = fixture_with_pure_parent();
        assert!(
            super::render_nested(
                &outer,
                &child,
                &metadata(&child).unwrap().unwrap(),
                Some(&world)
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires javac and java"]
    fn nested_capture_compiles_and_retains_distinct_instances() {
        let (outer, child, parent) = fixture_with_pure_parent();
        let mut world = BTreeMap::new();
        world.insert("sample.Parent".to_owned(), parent);
        let code =
            super::super::render_mixed_with_nested("sample.Outer", &outer, &[&child], Some(&world));
        let dir = std::env::temp_dir().join(format!("rdx-anonymous-{}", std::process::id()));
        let package = dir.join("sample");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("Outer.java"), &code.source).unwrap();
        std::fs::write(package.join("Parent.java"), "package sample; class Parent { final int arity; Parent(int arity) { this.arity = arity; } }").unwrap();
        std::fs::write(
            package.join("Check.java"),
            "package sample; public class Check { public static void main(String[] args) { Outer.anon$1 a = Outer.make(7); Outer.anon$1 b = Outer.make(9); if (a == b || a.value() != 7 || b.value() != 9) throw new AssertionError(); a.run(); } }",
        )
        .unwrap();
        let compiled = Command::new("javac")
            .arg("sample/Outer.java")
            .arg("sample/Parent.java")
            .arg("sample/Check.java")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}\n{}",
            code.source,
            String::from_utf8_lossy(&compiled.stderr)
        );
        let ran = Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Check")
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
    }
}
