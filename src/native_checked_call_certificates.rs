//! Sparse source-call facts for protected calls when readability metadata is exhausted.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Key {
    owner: Arc<str>,
    name: Arc<str>,
    args: Vec<Arc<str>>,
    ret: Arc<str>,
    static_call: bool,
}
#[derive(Debug)]
pub(super) enum Certificate {
    Present {
        method: LoadedCallMethod,
        owner_flags: u32,
    },
    Absent,
    Unknown,
}
#[derive(Debug, Default)]
pub(super) struct Certificates {
    facts: Vec<(Key, Certificate)>,
}
impl Certificates {
    pub(super) fn get(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        static_call: bool,
    ) -> Option<&Certificate> {
        self.facts
            .binary_search_by(|(key, _)| {
                key.owner
                    .as_ref()
                    .cmp(owner)
                    .then_with(|| key.name.as_ref().cmp(name))
                    .then_with(|| key.args.as_slice().cmp(args))
                    .then_with(|| key.ret.as_ref().cmp(ret))
                    .then_with(|| key.static_call.cmp(&static_call))
            })
            .ok()
            .map(|index| &self.facts[index].1)
    }
}

#[derive(Clone, Copy)]
struct Limits {
    seeds: usize,
    words: usize,
    traversal: usize,
    declarations: usize,
    ranges: usize,
    classifier_words: usize,
    symbol_bytes: usize,
}
const LIMITS: Limits = Limits {
    seeds: 65_536,
    words: 8_000_000,
    traversal: 1_000_000,
    declarations: 4_000_000,
    ranges: 8_000_000,
    classifier_words: 8_000_000,
    symbol_bytes: 32_000_000,
};
pub(super) fn build(hierarchy: &TypeHierarchy, classes: &[&DexClass]) -> Certificates {
    build_with_limits(hierarchy, classes, LIMITS)
}
fn build_with_limits(
    hierarchy: &TypeHierarchy,
    classes: &[&DexClass],
    limits: Limits,
) -> Certificates {
    if !hierarchy.loaded_call_families.is_empty() {
        return Certificates::default();
    }
    let owners: HashMap<_, _> = classes
        .iter()
        .map(|c| (c.descriptor.as_ref(), *c))
        .collect();
    let mut seeds = HashSet::new();
    let mut word_work = 0usize;
    let mut range_work = 0usize;
    let mut symbol_work = 0usize;
    for class in classes {
        for method in &class.methods {
            let Some(code) = &method.code else { continue };
            if code.try_regions.is_empty() {
                continue;
            }
            if code.tries as usize != code.try_regions.len() || code.ins > code.registers {
                return Certificates::default();
            }
            word_work = word_work.saturating_add(code.instructions.len());
            if word_work > limits.words {
                return Certificates::default();
            }
            let mut pc = 0usize;
            let mut boundaries = HashSet::new();
            let mut payload_starts = HashSet::new();
            while pc < code.instructions.len() {
                boundaries.insert(pc);
                let Ok((width, payload)) =
                    crate::native_cfg::instruction_width(&code.instructions, pc)
                else {
                    return Certificates::default();
                };
                if width == 0
                    || pc
                        .checked_add(width)
                        .is_none_or(|end| end > code.instructions.len())
                {
                    return Certificates::default();
                }
                if payload {
                    payload_starts.insert(pc);
                }
                let op = code.instructions[pc] & 0xff;
                if !payload && matches!(op, 0x6e..=0x72 | 0x74..=0x78) {
                    let Some(index) = code.instructions.get(pc + 1) else {
                        return Certificates::default();
                    };
                    let Some(&(oi, pi, ni)) = class.symbols.methods.get(usize::from(*index)) else {
                        return Certificates::default();
                    };
                    let (Some(owner), Some((ret, args)), Some(name)) = (
                        class.symbols.types.get(usize::from(oi)),
                        class.symbols.protos.get(usize::from(pi)),
                        class.symbols.strings.get(ni as usize),
                    ) else {
                        return Certificates::default();
                    };
                    symbol_work = symbol_work
                        .saturating_add(owner.len())
                        .saturating_add(name.len())
                        .saturating_add(ret.len())
                        .saturating_add(args.iter().map(|ty| ty.len()).sum::<usize>());
                    if symbol_work > limits.symbol_bytes {
                        return Certificates::default();
                    }
                    if owner.len() > 512
                        || name.len() > 512
                        || ret.len() > 512
                        || args.len() > 255
                        || args.iter().any(|ty| ty.len() > 512)
                        || !(valid_class(owner) || valid_array(owner))
                        || !(ret.as_ref() == "V"
                            || primitive(ret)
                            || valid_class(ret)
                            || valid_array(ret))
                        || args
                            .iter()
                            .any(|ty| !(primitive(ty) || valid_class(ty) || valid_array(ty)))
                    {
                        return Certificates::default();
                    }
                    let mut protected = false;
                    for region in &code.try_regions {
                        range_work = range_work.saturating_add(1);
                        if range_work > limits.ranges {
                            return Certificates::default();
                        }
                        if region.start as usize <= pc && pc < region.end as usize {
                            protected = true;
                            break;
                        }
                    }
                    if protected {
                        seeds.insert(Key {
                            owner: owner.clone(),
                            name: Arc::from(name.as_str()),
                            args: args.clone(),
                            ret: ret.clone(),
                            static_call: matches!(op, 0x71 | 0x77),
                        });
                        if seeds.len() > limits.seeds {
                            return Certificates::default();
                        }
                    }
                }
                pc += width;
            }
            boundaries.insert(pc);
            range_work = range_work.saturating_add(
                code.try_regions
                    .iter()
                    .map(|r| 1usize.saturating_add(r.catches.len()))
                    .sum::<usize>(),
            );
            if range_work > limits.ranges {
                return Certificates::default();
            }
            if code.try_regions.iter().any(|r| {
                r.start >= r.end
                    || !boundaries.contains(&(r.start as usize))
                    || !boundaries.contains(&(r.end as usize))
                    || r.catches.iter().any(|(ty, handler)| {
                        !boundaries.contains(&(*handler as usize))
                            || payload_starts.contains(&(*handler as usize))
                            || *handler as usize == code.instructions.len()
                            || ty.as_ref().is_some_and(|ty| !valid_class(ty))
                    })
            }) {
                return Certificates::default();
            }
        }
    }
    // Include exact inherited/interface lookup dependencies; source projections
    // outside this bounded seed set remain unknown at the consuming lookup.
    let mut pending: Vec<_> = seeds.iter().cloned().collect();
    pending.sort();
    let mut traversal = 0usize;
    let mut at = 0;
    while at < pending.len() {
        let key = pending[at].clone();
        at += 1;
        traversal += 1;
        if traversal > limits.traversal {
            return Certificates::default();
        }
        let Some(entry) = hierarchy.entries.get(key.owner.as_ref()) else {
            continue;
        };
        if key.name.as_ref() == "<init>" {
            continue;
        }
        let parents = entry
            .superclass
            .iter()
            .chain(entry.interfaces.iter().filter(|_| !key.static_call));
        for parent in parents {
            traversal = traversal.saturating_add(1);
            if traversal > limits.traversal {
                return Certificates::default();
            }
            if !hierarchy.loaded_owners.contains(parent.as_ref()) {
                continue;
            }
            let mut next = key.clone();
            next.owner = parent.clone();
            if seeds.insert(next.clone()) {
                pending.push(next);
            }
            if seeds.len() > limits.seeds {
                return Certificates::default();
            }
        }
    }
    pending.sort();
    let mut facts = Vec::with_capacity(pending.len());
    let mut declaration_work = 0usize;
    let mut classifier_work = 0usize;
    for key in pending {
        let Some(class) = owners.get(key.owner.as_ref()) else {
            continue;
        };
        declaration_work = declaration_work.saturating_add(class.methods.len().saturating_mul(2));
        if declaration_work > limits.declarations {
            return Certificates::default();
        }
        let certificate = if hierarchy.ambiguous.contains(key.owner.as_ref())
            || hierarchy
                .override_ambiguous_owners
                .contains(key.owner.as_ref())
            || (class.annotations_offset != 0
                && !class
                    .symbols
                    .annotations
                    .contains_key(&class.annotations_offset))
            || class
                .methods
                .iter()
                .any(|m| m.declaring_type != class.descriptor)
        {
            Certificate::Unknown
        } else {
            let mut matches = class
                .methods
                .iter()
                .filter(|m| m.name == key.name && m.parameters == key.args);
            match matches.next() {
                None => Certificate::Absent,
                Some(_) if matches.next().is_some() => Certificate::Unknown,
                Some(method) => {
                    if method.thrown_types.len() > 255
                        || method
                            .thrown_types
                            .iter()
                            .any(|ty| ty.len() > 512 || !valid_class(ty))
                    {
                        return Certificates::default();
                    }
                    if checked_inference_eligible(class, method) {
                        classifier_work = classifier_work.saturating_add(
                            method
                                .code
                                .as_ref()
                                .map_or(0, |code| code.instructions.len().saturating_mul(2)),
                        );
                        if classifier_work > limits.classifier_words {
                            return Certificates::default();
                        }
                    }
                    if future_headers::signature(method) {
                        classifier_work = classifier_work.saturating_add(
                            method
                                .code
                                .as_ref()
                                .map_or(0, |code| code.instructions.len().saturating_mul(3)),
                        );
                        if classifier_work > limits.classifier_words {
                            return Certificates::default();
                        }
                    }
                    let may_infer_checked = method_may_infer_checked(class, method);
                    Certificate::Present {
                        method: LoadedCallMethod {
                            static_no_return: false,
                            // Sparse checked-call certificates do not establish the
                            // complete Java source declaration required by the
                            // static sneaky-rethrow proof.
                            declared_throws_empty: false,
                            generic_method: true,
                            legal_source_declaration: false,
                            parameters: method.parameters.clone(),
                            return_type: method.return_type.clone(),
                            access_flags: method.access_flags,
                            annotated: false,
                            may_infer_checked,
                            no_new_checked_header: false,
                            future_checked_effects_closed: future_headers::checked_effects_closed(
                                method,
                            ),
                        },
                        owner_flags: class.access_flags,
                    }
                }
            }
        };
        facts.push((key, certificate));
    }
    Certificates { facts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexMethod, DexSymbols, DexTryRegion};
    fn fixture(checked: bool) -> (DexClass, DexClass) {
        let symbols = Arc::new(DexSymbols {
            types: vec![Arc::from("Lsample/Api;")],
            strings: vec!["noop".into()],
            protos: vec![(Arc::from("V"), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let target = DexMethod {
            declaring_type: Arc::from("Lsample/Api;"),
            name: Arc::from("noop"),
            return_type: Arc::from("V"),
            parameters: vec![],
            thrown_types: if checked {
                vec![Arc::from("Ljava/io/IOException;")]
            } else {
                vec![]
            },
            access_flags: 9,
            code: Some(DexCode {
                registers: 0,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 0,
            }),
        };
        let run = DexMethod {
            declaring_type: Arc::from("Lsample/Calls;"),
            name: Arc::from("run"),
            return_type: Arc::from("V"),
            parameters: vec![],
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
        };
        let class = |descriptor: &str, method| DexClass {
            symbols: symbols.clone(),
            descriptor: Arc::from(descriptor),
            superclass: Some(Arc::from(OBJECT)),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![method],
        };
        (class("Lsample/Api;", target), class("Lsample/Calls;", run))
    }
    fn exhausted(classes: &[&DexClass]) -> TypeHierarchy {
        let mut h = TypeHierarchy::from_classes(classes.iter().copied()).unwrap();
        h.loaded_call_families = Arc::new(HashMap::new());
        h.checked_call_certificates = Arc::new(build(&h, classes));
        h
    }
    #[test]
    fn sparse_exact_contract_survives_readability_exhaustion() {
        for checked in [false, true] {
            let (api, calls) = fixture(checked);
            let normal = TypeHierarchy::from_classes([&api, &calls]).unwrap();
            let sparse = exhausted(&[&api, &calls]);
            assert_eq!(
                normal.call_checked_effects("Lsample/Api;", "noop", &[], "V", true),
                sparse.call_checked_effects("Lsample/Api;", "noop", &[], "V", true)
            );
            assert_eq!(
                sparse.call_checked_effects("Lsample/Api;", "other", &[], "V", true),
                Some(vec![Arc::from("Ljava/lang/Throwable;")])
            );
        }
    }
    #[test]
    fn sparse_duplicate_prototype_and_dispatch_remain_unknown() {
        let (mut api, calls) = fixture(false);
        let (mut duplicate, _) = fixture(false);
        duplicate.methods[0].return_type = Arc::from("Ljava/lang/Object;");
        duplicate.methods[0].access_flags = 0x109;
        duplicate.methods[0].code = None;
        api.methods.extend(duplicate.methods);
        let h = exhausted(&[&api, &calls]);
        assert_eq!(
            h.call_checked_effects("Lsample/Api;", "noop", &[], "V", true),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
        let (api, calls) = fixture(false);
        let h = exhausted(&[&api, &calls]);
        assert_eq!(
            h.call_checked_effects("Lsample/Api;", "noop", &[], "V", false),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
    }
    #[test]
    fn sparse_malformed_call_and_handler_metadata_fail_closed() {
        for variant in 0..3 {
            let (api, mut calls) = fixture(false);
            let code = calls.methods[0].code.as_mut().unwrap();
            match variant {
                0 => code.instructions[1] = u16::MAX,
                1 => code.try_regions[0].catches = vec![(None, 2)].into(),
                _ => code.instructions.truncate(2),
            };
            let h = exhausted(&[&api, &calls]);
            assert_eq!(
                h.call_checked_effects("Lsample/Api;", "noop", &[], "V", true),
                Some(vec![Arc::from("Ljava/lang/Throwable;")])
            );
        }
    }
    #[test]
    fn sparse_inherited_contract_and_empty_shadow_keep_original_authority() {
        let (mut api, mut calls) = fixture(false);
        let (mut base, _) = fixture(true);
        base.descriptor = Arc::from("Lsample/Base;");
        base.methods[0].declaring_type = base.descriptor.clone();
        base.methods[0].access_flags = 1;
        base.methods[0].code.as_mut().unwrap().registers = 1;
        base.methods[0].code.as_mut().unwrap().ins = 1;
        api.superclass = Some(base.descriptor.clone());
        calls.methods[0].parameters = vec![api.descriptor.clone()];
        calls.methods[0].code.as_mut().unwrap().ins = 1;
        calls.methods[0].code.as_mut().unwrap().outs = 1;
        calls.methods[0].code.as_mut().unwrap().instructions[0] = 0x106e;
        let own = api.methods.pop().unwrap();
        let h = exhausted(&[&api, &base, &calls]);
        assert_eq!(
            h.call_checked_effects("Lsample/Api;", "noop", &[], "V", false),
            Some(vec![Arc::from("Ljava/io/IOException;")])
        );
        api.methods.push(own);
        api.methods[0].access_flags = 1;
        api.methods[0].code.as_mut().unwrap().registers = 1;
        api.methods[0].code.as_mut().unwrap().ins = 1;
        let h = exhausted(&[&api, &base, &calls]);
        assert_eq!(
            h.call_checked_effects("Lsample/Api;", "noop", &[], "V", false),
            Some(vec![])
        );
    }
    #[test]
    fn sparse_exact_budget_boundaries_publish_only_complete_facts() {
        let (api, calls) = fixture(false);
        let mut h = TypeHierarchy::from_classes([&api, &calls]).unwrap();
        h.loaded_call_families = Arc::new(HashMap::new());
        let exact = Limits {
            seeds: 1,
            words: 6,
            traversal: 2,
            declarations: 2,
            ranges: 3,
            classifier_words: 2,
            symbol_bytes: 17,
        };
        assert!(
            build_with_limits(&h, &[&api, &calls], exact)
                .get("Lsample/Api;", "noop", &[], "V", true)
                .is_some()
        );
        for limits in [
            Limits { seeds: 0, ..exact },
            Limits { words: 5, ..exact },
            Limits {
                traversal: 1,
                ..exact
            },
            Limits {
                declarations: 1,
                ..exact
            },
            Limits { ranges: 2, ..exact },
            Limits {
                classifier_words: 1,
                ..exact
            },
            Limits {
                symbol_bytes: 16,
                ..exact
            },
        ] {
            assert!(
                build_with_limits(&h, &[&api, &calls], limits)
                    .get("Lsample/Api;", "noop", &[], "V", true)
                    .is_none()
            );
        }
    }
    #[test]
    fn sparse_conflicting_loaded_sdk_shadow_and_exact_return_stay_unknown() {
        let (mut api, mut calls) = fixture(false);
        let symbols = Arc::new(DexSymbols {
            types: vec![Arc::from("Ljava/lang/Class;")],
            strings: vec!["forName".into()],
            protos: vec![(
                Arc::from("Ljava/lang/Class;"),
                vec![Arc::from("Ljava/lang/String;")],
            )],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        api.symbols = symbols.clone();
        api.descriptor = Arc::from("Ljava/lang/Class;");
        let target = &mut api.methods[0];
        target.declaring_type = api.descriptor.clone();
        target.name = Arc::from("forName");
        target.parameters = vec![Arc::from("Ljava/lang/String;")];
        target.return_type = Arc::from("Ljava/lang/Class;");
        target.code = None;
        target.access_flags = 0x109;
        calls.symbols = symbols;
        calls.methods[0].parameters = vec![Arc::from("Ljava/lang/String;")];
        let code = calls.methods[0].code.as_mut().unwrap();
        code.registers = 2;
        code.ins = 1;
        code.outs = 1;
        code.instructions = vec![0x1071, 0, 1, 0x000c, 0x000e, 0x000d, 0x0027];
        code.try_regions[0].end = 4;
        code.try_regions[0].catches = vec![(None, 5)].into();
        let h = exhausted(&[&api, &calls]);
        let args = [Arc::from("Ljava/lang/String;")];
        assert_eq!(
            h.call_checked_effects(
                "Ljava/lang/Class;",
                "forName",
                &args,
                "Ljava/lang/Class;",
                true
            ),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
        assert_eq!(
            h.call_checked_effects(
                "Ljava/lang/Class;",
                "forName",
                &args,
                "Ljava/lang/Object;",
                true
            ),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
        let (mut duplicate, _) = fixture(false);
        duplicate.descriptor = api.descriptor.clone();
        duplicate.methods[0].declaring_type = api.descriptor.clone();
        let h = exhausted(&[&api, &duplicate, &calls]);
        assert_eq!(
            h.call_checked_effects(
                "Ljava/lang/Class;",
                "forName",
                &args,
                "Ljava/lang/Class;",
                true
            ),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
    }
    #[test]
    fn sparse_unseeded_allocated_constructor_projection_remains_unknown() {
        let (mut api, mut calls) = fixture(true);
        api.access_flags = 1;
        let symbols = Arc::new(DexSymbols {
            types: vec![Arc::from("Lsample/Api;"), Arc::from("Lsample/Child;")],
            strings: vec!["<init>".into()],
            protos: vec![(Arc::from("V"), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        api.symbols = symbols.clone();
        api.methods[0].name = Arc::from("<init>");
        api.methods[0].access_flags = 0x10001;
        api.methods[0].code = None;
        calls.symbols = symbols;
        let code = calls.methods[0].code.as_mut().unwrap();
        code.outs = 1;
        code.instructions = vec![0x0022, 1, 0x1070, 0, 0, 0x000e, 0x000d, 0x0027];
        code.try_regions[0].end = 5;
        code.try_regions[0].catches = vec![(None, 6)].into();
        let (mut child, _) = fixture(false);
        child.descriptor = Arc::from("Lsample/Child;");
        child.superclass = Some(api.descriptor.clone());
        child.methods.clear();
        let h = exhausted(&[&api, &child, &calls]);
        assert_eq!(
            h.call_checked_effects("Lsample/Api;", "<init>", &[], "V", false),
            Some(vec![Arc::from("Ljava/io/IOException;")])
        );
        assert_eq!(
            h.call_checked_effects("Lsample/Child;", "<init>", &[], "V", false),
            Some(vec![Arc::from("Ljava/lang/Throwable;")])
        );
    }
    #[test]
    fn repeated_wide_arity_calls_use_one_seed_with_bounded_symbol_work() {
        let (mut api, mut calls) = fixture(false);
        let args = vec![Arc::from("Ljava/lang/String;"); 255];
        let symbols = Arc::new(DexSymbols {
            types: vec![api.descriptor.clone()],
            strings: vec!["noop".into()],
            protos: vec![(Arc::from("V"), args.clone())],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        api.symbols = symbols.clone();
        api.methods[0].parameters = args.clone();
        api.methods[0].access_flags = 0x109;
        api.methods[0].code = None;
        calls.symbols = symbols;
        calls.methods[0].parameters = args.clone();
        let code = calls.methods[0].code.as_mut().unwrap();
        code.registers = 256;
        code.ins = 255;
        code.outs = 255;
        code.instructions = (0..1024)
            .flat_map(|_| [0xff77, 0, 1])
            .chain([0x000e, 0xff0d, 0xff27])
            .collect();
        code.try_regions[0].end = 3072;
        code.try_regions[0].catches = vec![(None, 3073)].into();
        let mut h = TypeHierarchy::from_classes([&api, &calls]).unwrap();
        h.loaded_call_families = Arc::new(HashMap::new());
        let certs = build_with_limits(&h, &[&api, &calls], Limits { seeds: 1, ..LIMITS });
        assert_eq!(certs.facts.len(), 1);
        assert!(matches!(
            certs.get("Lsample/Api;", "noop", &args, "V", true),
            Some(Certificate::Present { .. })
        ));
        assert!(
            build_with_limits(
                &h,
                &[&api, &calls],
                Limits {
                    seeds: 1,
                    symbol_bytes: 4096,
                    ..LIMITS
                }
            )
            .facts
            .is_empty()
        );
    }
}
