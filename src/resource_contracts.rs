//! Resource argument contracts from pinned SDK annotations and loaded DEX declarations.
//! Unannotated overrides require unchanged argument forwarding to a resource API.
//! Numeric equality and method-name similarity never grant identity.
use crate::{
    native_dex::{DexClass, DexMethod},
    native_ir::{DecodedMethod, PoolKind},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

pub(super) type Slots = Vec<(usize, Vec<&'static str>)>;

pub(super) fn kinds(annotation: &str) -> Option<Vec<&'static str>> {
    Some(match annotation {
        "AnimRes" => vec!["anim"],
        "AnimatorRes" => vec!["animator"],
        "ArrayRes" => vec!["array"],
        "AttrRes" => vec!["attr"],
        "BoolRes" => vec!["bool"],
        "ColorRes" => vec!["color"],
        "DimenRes" => vec!["dimen"],
        "DrawableRes" => vec!["drawable", "mipmap", "color"],
        "FontRes" => vec!["font"],
        "FractionRes" => vec!["fraction"],
        "IdRes" => vec!["id"],
        "IntegerRes" => vec!["integer"],
        "InterpolatorRes" => vec!["interpolator", "anim"],
        "LayoutRes" => vec!["layout"],
        "MenuRes" => vec!["menu"],
        "NavigationRes" => vec!["navigation"],
        "PluralsRes" => vec!["plurals"],
        "RawRes" => vec!["raw"],
        "StringRes" => vec!["string"],
        "StyleRes" => vec!["style"],
        "TransitionRes" => vec!["transition"],
        "XmlRes" => vec!["xml"],
        _ => return None,
    })
}

fn platform(label: &str) -> Slots {
    #[derive(serde::Deserialize)]
    struct Data {
        contracts: BTreeMap<String, Vec<(usize, Vec<String>)>>,
    }
    static DATA: OnceLock<BTreeMap<String, Slots>> = OnceLock::new();
    let data = DATA.get_or_init(|| {
        let data: Data =
            serde_json::from_str(include_str!("../data/android-35-resource-contracts.json"))
                .expect("validated SDK resource contracts");
        data.contracts
            .into_iter()
            .map(|(key, slots)| {
                (
                    key,
                    slots
                        .into_iter()
                        .map(|(slot, names)| {
                            let names = names
                                .iter()
                                .filter_map(|name| match name.as_str() {
                                    "anim" => Some("anim"),
                                    "animator" => Some("animator"),
                                    "array" => Some("array"),
                                    "attr" => Some("attr"),
                                    "bool" => Some("bool"),
                                    "color" => Some("color"),
                                    "dimen" => Some("dimen"),
                                    "drawable" => Some("drawable"),
                                    "mipmap" => Some("mipmap"),
                                    "font" => Some("font"),
                                    "fraction" => Some("fraction"),
                                    "id" => Some("id"),
                                    "integer" => Some("integer"),
                                    "interpolator" => Some("interpolator"),
                                    "layout" => Some("layout"),
                                    "menu" => Some("menu"),
                                    "navigation" => Some("navigation"),
                                    "plurals" => Some("plurals"),
                                    "raw" => Some("raw"),
                                    "string" => Some("string"),
                                    "style" => Some("style"),
                                    "transition" => Some("transition"),
                                    "xml" => Some("xml"),
                                    _ => None,
                                })
                                .collect();
                            (slot, names)
                        })
                        .collect(),
                )
            })
            .collect()
    });
    data.get(label).cloned().unwrap_or_default()
}

fn class_name(descriptor: &str) -> Option<String> {
    Some(
        descriptor
            .strip_prefix('L')?
            .strip_suffix(';')?
            .replace('/', "."),
    )
}

fn signature(method: &DexMethod) -> String {
    format!(
        "{}({}){}",
        method.name,
        method
            .parameters
            .iter()
            .map(|s| s.as_ref())
            .collect::<String>(),
        method.return_type
    )
}

pub(super) struct Resolver<'a> {
    classes: Option<&'a BTreeMap<String, DexClass>>,
    cache: BTreeMap<String, Slots>,
    active: BTreeSet<String>,
    work: usize,
    scan_work: usize,
}
impl<'a> Resolver<'a> {
    pub fn new(classes: Option<&'a BTreeMap<String, DexClass>>) -> Self {
        Self {
            classes,
            cache: BTreeMap::new(),
            active: BTreeSet::new(),
            work: 8192,
            scan_work: 2_000_000,
        }
    }
    pub fn resolve(&mut self, label: &str) -> Slots {
        if let Some(slots) = self.cache.get(label) {
            return slots.clone();
        }
        if self.work == 0 || self.active.len() >= 16 || !self.active.insert(label.into()) {
            return vec![];
        }
        self.work -= 1;
        let slots = self.resolve_inner(label);
        self.active.remove(label);
        self.cache.insert(label.into(), slots.clone());
        slots
    }
    fn resolve_inner(&mut self, label: &str) -> Slots {
        let Some((prefix, _)) = label.split_once('(') else {
            return vec![];
        };
        let Some((owner, raw_name)) = prefix.rsplit_once('.') else {
            return vec![];
        };
        let Some(class) = self.classes.and_then(|classes| classes.get(owner)) else {
            let slots = platform(label);
            return if slots.is_empty() {
                super::usage::arguments(label)
                    .unwrap_or_default()
                    .iter()
                    .map(|(s, k)| (*s, k.to_vec()))
                    .collect()
            } else {
                slots
            };
        };
        let suffix = &label[owner.len() + 1..];
        let mut declarations = class
            .methods
            .iter()
            .enumerate()
            .filter(|(_, m)| m.name.as_ref() == raw_name && signature(m) == suffix);
        if let Some((position, method)) = declarations.next() {
            if declarations.next().is_some() {
                return vec![];
            }
            let annotations = class
                .symbols
                .annotations
                .get(&class.annotations_offset)
                .and_then(|directory| directory.parameters.get(position));
            let mut slots: Slots = Vec::new();
            let mut excluded = BTreeSet::new();
            if let Some(parameters) = annotations {
                for (slot, set) in parameters.iter().enumerate() {
                    if method.parameters.get(slot).map(|p| p.as_ref()) != Some("I") {
                        continue;
                    }
                    for annotation in set.iter().flat_map(|s| s.iter()) {
                        let Some(ty) = class.symbols.types.get(annotation.type_idx as usize) else {
                            continue;
                        };
                        let name = ty
                            .strip_prefix("Landroidx/annotation/")
                            .or_else(|| ty.strip_prefix("Landroid/annotation/"))
                            .or_else(|| ty.strip_prefix("Landroid/support/annotation/"))
                            .and_then(|ty| ty.strip_suffix(';'));
                        if matches!(name, Some("ColorInt" | "IntDef" | "StyleableRes" | "Px")) {
                            excluded.insert(slot);
                        }
                        if let Some(kinds) = name.and_then(kinds) {
                            if let Some((_, prior)) = slots.iter_mut().find(|(s, _)| *s == slot) {
                                prior.retain(|kind| kinds.contains(kind));
                                if prior.is_empty() {
                                    return vec![];
                                }
                            } else {
                                slots.push((slot, kinds));
                            }
                        }
                    }
                }
            }
            slots.retain(|(slot, _)| !excluded.contains(slot));
            if !slots.is_empty() {
                return slots;
            }
            // A declared override can change semantics: inspect its actual uses.
            // Explicit non-resource annotations take precedence over inference.
            if annotations.is_some_and(|p| {
                p.iter()
                    .any(|set| set.as_ref().is_some_and(|set| !set.is_empty()))
            }) {
                return vec![];
            }
            if method.access_flags & 0x400 != 0 && self.active.len() > 1 {
                return self.implementations(class, method);
            }
            return self.forwarded(class, method);
        }
        // Only a unique superclass path grants an inherited method contract.
        // Interfaces are deliberately unresolved until declaration ambiguity is modelled.
        if let Some(parent) = class.superclass.as_deref().and_then(class_name) {
            return self.resolve(&format!("{parent}.{suffix}"));
        }
        vec![]
    }

    fn implementations(&mut self, base: &DexClass, method: &DexMethod) -> Slots {
        let Some(classes) = self.classes else {
            return vec![];
        };
        if classes.len() > 200_000 {
            return vec![];
        }
        let suffix = signature(method);
        let mut candidates = Vec::new();
        for (owner, class) in classes {
            let Some(left) = self.scan_work.checked_sub(1) else {
                return vec![];
            };
            self.scan_work = left;
            for candidate in &class.methods {
                let Some(left) = self.scan_work.checked_sub(1) else {
                    return vec![];
                };
                self.scan_work = left;
                if candidate.name != method.name
                    || candidate.parameters != method.parameters
                    || candidate.return_type != method.return_type
                    || candidate.access_flags & 0x40a != 0
                {
                    continue;
                }
                let mut current = class;
                let mut seen = BTreeSet::new();
                for _ in 0..64 {
                    if !seen.insert(current.descriptor.as_ref()) {
                        break;
                    }
                    if current.superclass.as_deref() == Some(base.descriptor.as_ref()) {
                        candidates.push(format!("{owner}.{suffix}"));
                        break;
                    }
                    let Some(parent) = current
                        .superclass
                        .as_deref()
                        .and_then(class_name)
                        .and_then(|p| classes.get(&p))
                    else {
                        break;
                    };
                    current = parent;
                }
            }
        }
        if candidates.is_empty() || candidates.len() > 32 {
            return vec![];
        }
        let mut common = self.resolve(&candidates[0]);
        for candidate in &candidates[1..] {
            let slots = self.resolve(candidate);
            common.retain_mut(|(slot, kinds)| {
                if let Some((_, other)) = slots.iter().find(|(s, _)| s == slot) {
                    kinds.retain(|k| other.contains(k));
                    !kinds.is_empty()
                } else {
                    false
                }
            });
        }
        common
    }
    fn forwarded(&mut self, class: &DexClass, method: &DexMethod) -> Slots {
        let Some(code) = method.code.as_ref() else {
            return vec![];
        };
        if !method.parameters.iter().any(|p| p.as_ref() == "I")
            || code.tries != 0
            || code.instructions.len() > 512
        {
            return vec![];
        }
        let Ok(decoded) = DecodedMethod::decode(code) else {
            return vec![];
        };
        if decoded
            .instructions
            .iter()
            .any(|i| i.branch_target.is_some() || i.payload_target.is_some())
        {
            return vec![];
        }
        let Some(terminal) = decoded
            .instructions
            .iter()
            .position(|ins| matches!(ins.opcode, 0x0e..=0x11 | 0x27))
        else {
            return vec![];
        };
        let mut aliases = vec![None; code.registers as usize];
        let Some(mut reg) = code.registers.checked_sub(code.ins).map(usize::from) else {
            return vec![];
        };
        if method.access_flags & 8 == 0 {
            reg += 1;
        }
        for (slot, ty) in method.parameters.iter().enumerate() {
            if reg >= aliases.len() {
                return vec![];
            }
            if ty.as_ref() == "I" {
                aliases[reg] = Some(slot);
            }
            reg += if matches!(ty.as_ref(), "J" | "D") {
                2
            } else {
                1
            };
        }
        let mut found: BTreeMap<usize, Vec<&'static str>> = BTreeMap::new();
        let mut invalid = BTreeSet::new();
        for ins in &decoded.instructions[..=terminal] {
            if self.work == 0 {
                return vec![];
            }
            self.work -= 1;
            if matches!(ins.opcode, 0x01..=0x03) {
                if let (Some(read), Some(write)) = (ins.reads.first(), ins.writes.first()) {
                    aliases[write.register as usize] = aliases[read.register as usize];
                }
                continue;
            }
            let mut accepted = BTreeSet::new();
            if ins
                .reads
                .iter()
                .any(|r| aliases[r.register as usize].is_some())
                && let Some(reference) = ins.reference.filter(|r| r.kind == PoolKind::Method)
                && matches!(ins.opcode, 0x6e..=0x72 | 0x74..=0x78)
                && let Some(&(owner, proto, name)) =
                    class.symbols.methods.get(reference.index as usize)
                && let (Some(owner), Some((ret, params)), Some(name)) = (
                    class.symbols.types.get(owner as usize),
                    class.symbols.protos.get(proto as usize),
                    class.symbols.strings.get(name as usize),
                )
            {
                let Some(owner) = class_name(owner) else {
                    return vec![];
                };
                let label = format!(
                    "{}.{}({}){}",
                    owner,
                    name,
                    params.iter().map(|p| p.as_ref()).collect::<String>(),
                    ret
                );
                let slots = self.resolve(&label);
                let mut word = usize::from(!matches!(ins.opcode, 0x71 | 0x77));
                for (slot, ty) in params.iter().enumerate() {
                    if let Some(read) = ins.reads.get(word)
                        && let Some(input) = aliases[read.register as usize]
                        && let Some((_, kinds)) = slots.iter().find(|(s, _)| *s == slot)
                    {
                        accepted.insert(word);
                        if let Some(previous) = found.get_mut(&input) {
                            previous.retain(|kind| kinds.contains(kind));
                        } else {
                            found.insert(input, kinds.clone());
                        }
                    }
                    word += if matches!(ty.as_ref(), "J" | "D") {
                        2
                    } else {
                        1
                    };
                }
            }
            for (word, read) in ins.reads.iter().enumerate() {
                if let Some(input) = aliases[read.register as usize]
                    && !accepted.contains(&word)
                {
                    invalid.insert(input);
                }
            }
            for write in &ins.writes {
                for alias in aliases
                    .iter_mut()
                    .skip(write.register as usize)
                    .take(write.kind.word_count())
                {
                    *alias = None;
                }
            }
        }
        found
            .into_iter()
            .filter(|(slot, kinds)| !invalid.contains(slot) && !kinds.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexAnnotation, DexAnnotationDirectory, DexCode, DexSymbols};
    use std::sync::Arc;

    fn class(
        name: &str,
        parent: &str,
        method_name: &str,
        callee: Option<(&str, &str)>,
    ) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = format!("L{name};").into();
        class.superclass = Some(format!("L{parent};").into());
        class.interfaces.clear();
        class.annotations_offset = 0;
        class.methods.truncate(1);
        let method = &mut class.methods[0];
        method.declaring_type = class.descriptor.clone();
        method.name = method_name.into();
        method.parameters = vec!["I".into()];
        method.return_type = "V".into();
        method.access_flags = 1;
        method.code = callee.map(|_| DexCode {
            registers: 2,
            ins: 2,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x206e, 0, 0x10, 0x000e],
            offset: 0,
        });
        class.symbols = Arc::new(if let Some((owner, name)) = callee {
            DexSymbols {
                types: vec![format!("L{owner};").into()],
                strings: vec![name.into()],
                protos: vec![("V".into(), vec!["I".into()])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            }
        } else {
            DexSymbols::default()
        });
        class
    }
    #[test]
    fn sdk_contracts_distinguish_resources_from_indices_flags_and_overloads() {
        let mut resolver = Resolver::new(None);
        assert_eq!(
            resolver.resolve(
                "android.content.Context.getDrawable(I)Landroid/graphics/drawable/Drawable;"
            ),
            vec![(0, vec!["color", "drawable", "mipmap"])]
        );
        assert_eq!(
            resolver.resolve(
                "android.app.AlertDialog$Builder.setMessage(I)Landroid/app/AlertDialog$Builder;"
            ),
            vec![(0, vec!["string"])]
        );
        for label in [
            "android.content.res.TypedArray.getDrawable(I)Landroid/graphics/drawable/Drawable;",
            "android.app.WallpaperManager.getDrawable(I)Landroid/graphics/drawable/Drawable;",
            "android.widget.TextView.setText(Ljava/lang/CharSequence;)V",
            "custom.Context.getDrawable(I)Landroid/graphics/drawable/Drawable;",
        ] {
            assert!(resolver.resolve(label).is_empty(), "{label}");
        }
    }
    #[test]
    fn inherited_declarations_and_forwarding_resolve_but_semantic_overrides_do_not() {
        let mut inherited = class("Inherited", "android/app/Activity", "unused", None);
        inherited.methods.clear();
        let forward = class(
            "Forward",
            "android/app/Activity",
            "setContentView",
            Some(("android/app/Activity", "setContentView")),
        );
        let hash = class(
            "Hash",
            "android/app/Activity",
            "setContentView",
            Some(("custom/Hash", "hash")),
        );
        let mut transformed = class(
            "Transformed",
            "android/app/Activity",
            "setContentView",
            Some(("android/app/Activity", "setContentView")),
        );
        transformed.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(0..0, [0x01d8, 0x0101]);
        let classes = BTreeMap::from([
            ("Inherited".into(), inherited),
            ("Forward".into(), forward),
            ("Hash".into(), hash),
            ("Transformed".into(), transformed),
        ]);
        let mut resolver = Resolver::new(Some(&classes));
        for name in ["Inherited", "Forward"] {
            assert_eq!(
                resolver.resolve(&format!("{name}.setContentView(I)V")),
                vec![(0, vec!["layout"])]
            );
        }
        for name in ["Hash", "Transformed"] {
            assert!(
                resolver
                    .resolve(&format!("{name}.setContentView(I)V"))
                    .is_empty()
            );
        }
    }
    #[test]
    fn abstract_delegate_requires_agreement_of_concrete_implementations() {
        let mut base = class("Base", "java/lang/Object", "route", None);
        base.methods[0].access_flags = 0x401;
        let forward = class(
            "Forward",
            "java/lang/Object",
            "show",
            Some(("Base", "route")),
        );
        let implementation = class(
            "Implementation",
            "Base",
            "route",
            Some(("android/app/Activity", "setContentView")),
        );
        let mut classes = BTreeMap::from([
            ("Base".into(), base),
            ("Forward".into(), forward),
            ("Implementation".into(), implementation),
        ]);
        assert_eq!(
            Resolver::new(Some(&classes)).resolve("Forward.show(I)V"),
            vec![(0, vec!["layout"])]
        );
        classes.insert(
            "Different".into(),
            class("Different", "Base", "route", Some(("custom/Hash", "hash"))),
        );
        assert!(
            Resolver::new(Some(&classes))
                .resolve("Forward.show(I)V")
                .is_empty()
        );
    }
    #[test]
    fn retained_annotations_identify_custom_parameters_and_conflicts_stop_inference() {
        for (annotation, expected) in [("LayoutRes", true), ("ColorInt", false)] {
            let mut class = class(
                "Annotated",
                "android/app/Activity",
                "setContentView",
                Some(("android/app/Activity", "setContentView")),
            );
            let symbols = Arc::get_mut(&mut class.symbols).unwrap();
            symbols
                .types
                .push(format!("Landroidx/annotation/{annotation};").into());
            symbols.annotations.insert(
                0,
                Arc::new(DexAnnotationDirectory {
                    parameters: vec![vec![Some(
                        vec![Arc::new(DexAnnotation {
                            visibility: 0,
                            type_idx: 1,
                            elements: vec![],
                        })]
                        .into(),
                    )]],
                    ..Default::default()
                }),
            );
            let classes = BTreeMap::from([("Annotated".into(), class)]);
            assert_eq!(
                !Resolver::new(Some(&classes))
                    .resolve("Annotated.setContentView(I)V")
                    .is_empty(),
                expected
            );
        }
    }

    #[test]
    fn unreachable_sinks_and_leading_l_owners_do_not_grant_resource_identity() {
        let mut unreachable = class(
            "Unreachable",
            "android/app/Activity",
            "setContentView",
            Some(("android/app/Activity", "setContentView")),
        );
        unreachable.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .insert(0, 0x000e);
        let misleading = class(
            "Misleading",
            "java/lang/Object",
            "show",
            Some(("Landroid/app/Activity", "setContentView")),
        );
        let classes = BTreeMap::from([
            ("Unreachable".into(), unreachable),
            ("Misleading".into(), misleading),
        ]);
        let mut resolver = Resolver::new(Some(&classes));
        assert!(
            resolver
                .resolve("Unreachable.setContentView(I)V")
                .is_empty()
        );
        assert!(resolver.resolve("Misleading.show(I)V").is_empty());
        assert_eq!(
            class_name("LLandroid/app/Activity;"),
            Some("Landroid.app.Activity".into())
        );
        assert!(class_name("android/app/Activity").is_none());
    }
    #[test]
    fn mixed_resource_and_value_annotations_keep_the_literal() {
        for excluded in ["ColorInt", "IntDef", "StyleableRes", "Px"] {
            let mut class = class(
                "Mixed",
                "android/app/Activity",
                "setContentView",
                Some(("android/app/Activity", "setContentView")),
            );
            let symbols = Arc::get_mut(&mut class.symbols).unwrap();
            symbols.types.extend([
                "Landroidx/annotation/LayoutRes;".into(),
                format!("Landroidx/annotation/{excluded};").into(),
            ]);
            symbols.annotations.insert(
                0,
                Arc::new(DexAnnotationDirectory {
                    parameters: vec![vec![Some(
                        [1, 2]
                            .into_iter()
                            .map(|type_idx| {
                                Arc::new(DexAnnotation {
                                    visibility: 0,
                                    type_idx,
                                    elements: vec![],
                                })
                            })
                            .collect::<Vec<_>>()
                            .into(),
                    )]],
                    ..Default::default()
                }),
            );
            let classes = BTreeMap::from([("Mixed".into(), class)]);
            assert!(
                Resolver::new(Some(&classes))
                    .resolve("Mixed.setContentView(I)V")
                    .is_empty(),
                "{excluded}"
            );
        }
    }
}
