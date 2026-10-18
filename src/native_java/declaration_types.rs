//! Shared reverse display-name lookup for allocation declarations.
use crate::native_dex::DexSymbols;
use std::{collections::HashMap, sync::Arc};

fn record(index: &mut HashMap<String, Option<Arc<str>>>, display: String, descriptor: Arc<str>) {
    use std::collections::hash_map::Entry;
    match index.entry(display) {
        Entry::Vacant(entry) => {
            entry.insert(Some(descriptor));
        }
        Entry::Occupied(mut entry) => {
            if entry
                .get()
                .as_deref()
                .is_some_and(|old| old != descriptor.as_ref())
            {
                entry.insert(None);
            }
        }
    }
}

pub(super) fn lookup<'a>(symbols: &'a DexSymbols, display: &str) -> Option<&'a str> {
    symbols
        .declaration_descriptors
        .get_or_init(|| {
            let mut index = HashMap::new();
            let mut seen = std::collections::HashSet::new();
            let descriptors = symbols
                .types
                .iter()
                .cloned()
                .chain(
                    symbols
                        .protos
                        .iter()
                        .flat_map(|(ret, args)| std::iter::once(ret).chain(args))
                        .cloned(),
                )
                .chain(
                    [
                        "Z",
                        "B",
                        "S",
                        "C",
                        "I",
                        "J",
                        "F",
                        "D",
                        "Ljava/lang/String;",
                        "Ljava/lang/Class;",
                        "Ljava/lang/Object;",
                    ]
                    .into_iter()
                    .map(Arc::<str>::from),
                );
            for descriptor in descriptors {
                if seen.insert(Arc::clone(&descriptor))
                    && let Ok(display) = super::java_type(&descriptor)
                {
                    record(&mut index, display, descriptor);
                }
            }
            index
        })
        .get(display)
        .and_then(Option::as_deref)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original_lookup(symbols: &DexSymbols, display: &str) -> Option<String> {
        let candidates: std::collections::HashSet<_> = symbols
            .types
            .iter()
            .map(ToString::to_string)
            .chain(
                symbols
                    .protos
                    .iter()
                    .flat_map(|(ret, args)| std::iter::once(ret).chain(args))
                    .map(ToString::to_string),
            )
            .chain(
                [
                    "Z",
                    "B",
                    "S",
                    "C",
                    "I",
                    "J",
                    "F",
                    "D",
                    "Ljava/lang/String;",
                    "Ljava/lang/Class;",
                    "Ljava/lang/Object;",
                ]
                .into_iter()
                .map(str::to_owned),
            )
            .filter(|descriptor| {
                super::super::java_type(descriptor).is_ok_and(|value| value == display)
            })
            .collect();
        (candidates.len() == 1).then(|| candidates.into_iter().next().unwrap())
    }

    #[test]
    fn cached_lookup_matches_original_scan_over_types_prototypes_and_builtins() {
        let symbols = DexSymbols {
            types: [
                "Lsample/Foo-Bar;",
                "Lsample/_rdx_466f6f2d426172;",
                "Lsample/Foo-Bar;",
                "[I",
                "[[Ljava/lang/String;",
                "Lsample/class;",
                "Lsample/é;",
                "malformed",
                "[V",
                "Lbad.name;",
                "Lbad//Name;",
            ]
            .into_iter()
            .map(Arc::<str>::from)
            .collect(),
            protos: vec![(
                "V".into(),
                vec!["Lsample/ProtoOnly;".into(), "[J".into(), "[I".into()],
            )],
            ..Default::default()
        };
        let displays = symbols
            .types
            .iter()
            .chain(
                symbols
                    .protos
                    .iter()
                    .flat_map(|(ret, args)| std::iter::once(ret).chain(args)),
            )
            .filter_map(|descriptor| super::super::java_type(descriptor).ok())
            .chain(
                [
                    "boolean",
                    "byte",
                    "short",
                    "char",
                    "int",
                    "long",
                    "float",
                    "double",
                    "void",
                    "java.lang.String",
                    "java.lang.Class",
                    "java.lang.Object",
                    "missing",
                    "malformed",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        for display in displays {
            assert_eq!(
                lookup(&symbols, &display),
                original_lookup(&symbols, &display).as_deref(),
                "{display}"
            );
        }
        let alias = super::super::java_type("Lsample/Foo-Bar;").unwrap();
        let reserved = super::super::java_type("Lsample/_rdx_466f6f2d426172;").unwrap();
        assert_ne!(alias, reserved);
        assert_eq!(lookup(&symbols, &alias), Some("Lsample/Foo-Bar;"));
        assert_eq!(
            lookup(&symbols, &reserved),
            Some("Lsample/_rdx_466f6f2d426172;")
        );
    }

    #[test]
    fn repeated_descriptors_stay_unique_and_display_collisions_stay_ambiguous() {
        let mut index = HashMap::new();
        record(&mut index, "Alias".into(), "Lfirst;".into());
        record(&mut index, "Alias".into(), "Lfirst;".into());
        assert_eq!(index["Alias"].as_deref(), Some("Lfirst;"));
        record(&mut index, "Alias".into(), "Lsecond;".into());
        record(&mut index, "Alias".into(), "Lfirst;".into());
        assert_eq!(index["Alias"], None);
    }

    #[test]
    fn includes_proto_only_types_primitives_aliases_and_deduplicates_sources() {
        let symbols = DexSymbols {
            types: vec!["Lsample/Foo-Bar;".into(), "I".into(), "malformed".into()],
            protos: vec![(
                "Lsample/Return;".into(),
                vec!["[Lsample/Arg;".into(), "I".into()],
            )],
            ..Default::default()
        };
        assert_eq!(lookup(&symbols, "sample.Return"), Some("Lsample/Return;"));
        assert_eq!(lookup(&symbols, "sample.Arg[]"), Some("[Lsample/Arg;"));
        assert_eq!(
            lookup(&symbols, "sample._rdx_466f6f2d426172"),
            Some("Lsample/Foo-Bar;")
        );
        for (display, descriptor) in [
            ("int", "I"),
            ("boolean", "Z"),
            ("double", "D"),
            ("java.lang.String", "Ljava/lang/String;"),
        ] {
            assert_eq!(lookup(&symbols, display), Some(descriptor));
        }
        assert_eq!(lookup(&symbols, "void"), None);
        assert_eq!(lookup(&symbols, "malformed"), None);
        let initialized = symbols.declaration_descriptors.get().unwrap();
        lookup(&symbols, "int");
        assert!(std::ptr::eq(
            initialized,
            symbols.declaration_descriptors.get().unwrap()
        ));
    }

    #[test]
    fn concurrent_lookups_share_one_initialized_index() {
        let symbols = DexSymbols {
            types: vec!["Lsample/Shared;".into()],
            ..Default::default()
        };
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let symbols = &symbols;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        assert_eq!(lookup(symbols, "sample.Shared"), Some("Lsample/Shared;"));
                        symbols.declaration_descriptors.get().unwrap()
                    })
                })
                .collect();
            let indexes: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert!(indexes.iter().all(|index| std::ptr::eq(*index, indexes[0])));
        });
    }

    #[test]
    fn independent_symbol_tables_have_independent_indexes() {
        let first = DexSymbols {
            types: vec!["Lsample/First;".into()],
            ..Default::default()
        };
        let second = DexSymbols {
            protos: vec![("V".into(), vec!["Lsample/Second;".into()])],
            ..Default::default()
        };
        assert_eq!(lookup(&first, "sample.First"), Some("Lsample/First;"));
        assert_eq!(lookup(&second, "sample.First"), None);
        assert_eq!(lookup(&second, "sample.Second"), Some("Lsample/Second;"));
        assert_eq!(lookup(&first, "void"), None);
        assert_eq!(lookup(&second, "void"), Some("V"));
    }
}
