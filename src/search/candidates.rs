//! Query-local DEX hints. Ordering only: every class remains searchable.
use rdx::{
    engine::NativeEngine,
    native_dex::{DexAnnotationSet, DexClass, DexSymbols, DexValue},
};
use regex::Regex;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

struct Plan {
    strings: Vec<bool>,
    types: Vec<bool>,
    protos: Vec<bool>,
    fields: Vec<bool>,
    methods: Vec<bool>,
}

fn yes(values: &[bool], index: usize) -> bool {
    values.get(index).copied().unwrap_or(false)
}

impl Plan {
    fn new(symbols: &DexSymbols, matcher: &Regex, cancel: &AtomicBool) -> Option<Self> {
        let mut strings = Vec::with_capacity(symbols.strings.len());
        for value in &symbols.strings {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            strings.push(matcher.is_match(value));
        }
        let types: Vec<_> = symbols
            .types
            .iter()
            .map(|s| matcher.is_match(s) || matcher.is_match(&s.replace('/', ".")))
            .collect();
        let protos: Vec<_> = symbols
            .protos
            .iter()
            .map(|(ret, args)| matcher.is_match(ret) || args.iter().any(|a| matcher.is_match(a)))
            .collect();
        let fields = symbols
            .fields
            .iter()
            .map(|&(owner, ty, name)| {
                yes(&strings, name as usize)
                    || yes(&types, owner as usize)
                    || yes(&types, ty as usize)
            })
            .collect();
        let methods = symbols
            .methods
            .iter()
            .map(|&(owner, proto, name)| {
                yes(&strings, name as usize)
                    || yes(&types, owner as usize)
                    || yes(&protos, proto as usize)
            })
            .collect();
        (!cancel.load(Ordering::Relaxed)).then_some(Self {
            strings,
            types,
            protos,
            fields,
            methods,
        })
    }

    fn value(&self, value: &DexValue) -> bool {
        match value {
            DexValue::String(i) => yes(&self.strings, *i as usize),
            DexValue::Type(i) => yes(&self.types, *i as usize),
            DexValue::Field(i) | DexValue::Enum(i) => yes(&self.fields, *i as usize),
            DexValue::Method(i) => yes(&self.methods, *i as usize),
            DexValue::MethodType(i) => yes(&self.protos, *i as usize),
            DexValue::Array(values) => values.iter().any(|v| self.value(v)),
            DexValue::Annotation { type_idx, elements } => {
                yes(&self.types, *type_idx as usize)
                    || elements
                        .iter()
                        .any(|(name, v)| yes(&self.strings, *name as usize) || self.value(v))
            }
            _ => false,
        }
    }

    fn annotations(&self, set: &DexAnnotationSet) -> bool {
        set.iter().any(|a| {
            yes(&self.types, a.type_idx as usize)
                || a.elements
                    .iter()
                    .any(|(n, v)| yes(&self.strings, *n as usize) || self.value(v))
        })
    }

    fn class_matches(&self, class: &DexClass, matcher: &Regex, cancel: &AtomicBool) -> bool {
        let text = |s: &str| matcher.is_match(s) || matcher.is_match(&s.replace('/', "."));
        if text(&class.descriptor)
            || class.superclass.as_ref().is_some_and(|s| text(s))
            || class.interfaces.iter().any(|s| text(s))
            || class
                .fields
                .iter()
                .any(|f| text(&f.name) || text(&f.field_type))
            || class.static_values.iter().any(|v| self.value(v))
        {
            return true;
        }
        if let Some(a) = class.symbols.annotations.get(&class.annotations_offset)
            && (a
                .class
                .iter()
                .chain(a.fields.iter().flatten())
                .chain(a.methods.iter().flatten())
                .any(|set| self.annotations(set))
                || a.parameters
                    .iter()
                    .flatten()
                    .flatten()
                    .any(|set| self.annotations(set)))
        {
            return true;
        }
        for method in &class.methods {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            if text(&method.name)
                || text(&method.return_type)
                || method
                    .parameters
                    .iter()
                    .chain(&method.thrown_types)
                    .any(|s| text(s))
            {
                return true;
            }
            let Some(code) = &method.code else { continue };
            let words = &code.instructions;
            let mut pc = 0;
            while pc < words.len() {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                let Ok((width, payload)) = rdx::native_cfg::instruction_width(words, pc) else {
                    break;
                };
                if !payload {
                    let op = words[pc] as u8;
                    let index = words.get(pc + 1).copied().map(usize::from);
                    let matched = match (op, index) {
                        (0x1a, Some(i)) => yes(&self.strings, i),
                        (0x1b, Some(lo)) => words
                            .get(pc + 2)
                            .is_some_and(|hi| yes(&self.strings, lo | (usize::from(*hi) << 16))),
                        (0x1c | 0x1f | 0x20 | 0x22..=0x25, Some(i)) => yes(&self.types, i),
                        (0x52..=0x6d, Some(i)) => yes(&self.fields, i),
                        (0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb, Some(i)) => {
                            yes(&self.methods, i)
                                || (matches!(op, 0xfa | 0xfb)
                                    && words
                                        .get(pc + 3)
                                        .is_some_and(|p| yes(&self.protos, usize::from(*p))))
                        }
                        (0xff, Some(i)) => yes(&self.protos, i),
                        _ => false,
                    };
                    if matched {
                        return true;
                    }
                }
                pc += width;
            }
        }
        false
    }
}

fn stable_prioritize<T>(classes: &mut Vec<T>, hints: Vec<bool>) {
    let mut ranked: Vec<_> = classes.drain(..).zip(hints).collect();
    ranked.sort_by_key(|(_, hint)| !*hint);
    classes.extend(ranked.into_iter().map(|(class, _)| class));
}

pub(super) fn prioritize(
    engine: &mut NativeEngine,
    classes: &mut Vec<&String>,
    matcher: &Regex,
    cancel: &AtomicBool,
    interactive: &mut impl FnMut(&mut NativeEngine),
) {
    let mut plans = HashMap::new();
    let mut hints = Vec::with_capacity(classes.len());
    for (index, name) in classes.iter().enumerate() {
        if index % 128 == 0 {
            interactive(engine);
        }
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some(class) = engine.dex_class(name) else {
            hints.push(false);
            continue;
        };
        let key = std::sync::Arc::as_ptr(&class.symbols) as usize;
        if let std::collections::hash_map::Entry::Vacant(entry) = plans.entry(key) {
            let Some(plan) = Plan::new(&class.symbols, matcher, cancel) else {
                return;
            };
            entry.insert(plan);
        }
        hints.push(plans[&key].class_matches(class, matcher, cancel));
    }
    if !cancel.load(Ordering::Relaxed) {
        stable_prioritize(classes, hints);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdx::native_dex::{DexCode, DexMethod};
    use std::sync::Arc;

    fn fixture(instructions: Vec<u16>) -> DexClass {
        DexClass {
            symbols: Arc::new(DexSymbols {
                strings: vec![
                    "needleString".into(),
                    "needleField".into(),
                    "needleMethod".into(),
                    "Kelvin".into(),
                ],
                types: vec!["Lsample/Owner;".into(), "Lsample/needleType;".into()],
                protos: vec![("V".into(), vec![])],
                fields: vec![(0, 0, 1)],
                methods: vec![(0, 0, 2)],
                ..Default::default()
            }),
            descriptor: "Lsample/Caller;".into(),
            superclass: None,
            interfaces: vec![],
            access_flags: 0,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![DexMethod {
                declaring_type: "Lsample/Caller;".into(),
                name: "call".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 0,
                code: Some(DexCode {
                    registers: 1,
                    ins: 0,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    instructions,
                    offset: 0,
                }),
            }],
        }
    }

    #[test]
    fn instruction_hints_resolve_each_reference_kind_and_unicode() {
        let cancel = AtomicBool::new(false);
        let absent = Regex::new("absent").unwrap();
        for (instructions, query) in [
            (vec![0x1a, 0, 0x0e], "needleString"),
            (vec![0x1b, 0, 0, 0x0e], "needleString"),
            (vec![0x1c, 1, 0x0e], "needleType"),
            (vec![0x52, 0, 0x0e], "needleField"),
            (vec![0x71, 0, 0, 0x0e], "needleMethod"),
            (vec![0x1a, 3, 0x0e], "(?i)kelvin"),
        ] {
            let class = fixture(instructions);
            let matcher = Regex::new(query).unwrap();
            let plan = Plan::new(&class.symbols, &matcher, &cancel).unwrap();
            assert!(plan.class_matches(&class, &matcher, &cancel), "{query}");
            assert!(
                !Plan::new(&class.symbols, &absent, &cancel)
                    .unwrap()
                    .class_matches(&class, &absent, &cancel)
            );
        }
    }

    #[test]
    fn payload_contents_are_skipped_and_malformed_owners_survive_ranking() {
        let cancel = AtomicBool::new(false);
        let matcher = Regex::new("needleString").unwrap();
        // Array data happens to contain const-string words; these are not instructions.
        let payload = fixture(vec![0x0300, 2, 2, 0, 0x1a, 0]);
        let malformed = fixture(vec![0x0300]);
        let hit = fixture(vec![0x1a, 0]);
        let mut classes = vec![&payload, &malformed, &hit];
        let hints = classes
            .iter()
            .map(|class| {
                Plan::new(&class.symbols, &matcher, &cancel)
                    .unwrap()
                    .class_matches(class, &matcher, &cancel)
            })
            .collect();
        stable_prioritize(&mut classes, hints);
        assert!(std::ptr::eq(classes[0], &hit));
        assert!(std::ptr::eq(classes[1], &payload));
        assert!(std::ptr::eq(classes[2], &malformed));
    }

    #[test]
    fn ranking_is_stable_and_preserves_exhaustive_fallback() {
        let mut classes = vec!["fallback-a", "hit-a", "malformed", "hit-b", "fallback-b"];
        stable_prioritize(&mut classes, vec![false, true, false, true, false]);
        assert_eq!(
            classes,
            ["hit-a", "hit-b", "fallback-a", "malformed", "fallback-b"]
        );
    }

    #[test]
    fn plans_are_query_local_and_use_unicode_regex_casefold() {
        let symbols = DexSymbols {
            strings: vec!["parseUri".into(), "Kelvin".into(), "ſearch".into()],
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        for (query, expected) in [
            ("(?i)parseuri", vec![true, false, false]),
            ("(?i)kelvin", vec![false, true, false]),
            ("(?i)search", vec![false, false, true]),
            ("absent", vec![false; 3]),
        ] {
            assert_eq!(
                Plan::new(&symbols, &Regex::new(query).unwrap(), &cancel)
                    .unwrap()
                    .strings,
                expected
            );
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(Plan::new(&symbols, &Regex::new("parseUri").unwrap(), &cancel).is_none());
    }
}
