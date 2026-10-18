//! Exact raw type-component collisions for checked-throw helper names.
use crate::native_dex::DexSymbols;

fn raw_contains(symbols: &DexSymbols, candidate: &str) -> bool {
    symbols
        .types
        .iter()
        .any(|ty| ty.split(['/', '$', ';']).any(|part| part == candidate))
}

pub(super) fn raw_contains_helper(symbols: &DexSymbols, candidate: &str) -> bool {
    if !candidate.starts_with("_rdx_") {
        return raw_contains(symbols, candidate);
    }
    symbols
        .raw_helper_type_components
        .get_or_init(|| {
            symbols
                .types
                .iter()
                .flat_map(|ty| ty.split(['/', '$', ';']))
                .filter(|part| part.starts_with("_rdx_"))
                .map(str::to_owned)
                .collect()
        })
        .contains(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_components_match_original_raw_split_exactly() {
        let symbols = DexSymbols {
            types: [
                "Lsample/_rdx_checked_throw_0;",
                "Lsample/Outer$_rdx_caught_throw_1;",
                "[Lsample/_rdx_array;",
                "[[Lsample/Outer$_rdx_nested;",
                "L_rdx_default_package;",
                "[_rdx_array_prefix;",
                "_rdx_raw;",
                "L_rdx_before_slash/_rdx_package/Type;",
                "Lsample/_rdx_duplicate;",
                "Lsample/_rdx_duplicate;",
                "",
                ";;",
                "I",
                "Lsample/Normal;",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
            ..Default::default()
        };
        for candidate in [
            "_rdx_checked_throw_0",
            "_rdx_caught_throw_1",
            "_rdx_array",
            "_rdx_nested",
            "_rdx_default_package",
            "_rdx_array_prefix",
            "_rdx_raw",
            "_rdx_package",
            "_rdx_duplicate",
            "_rdx_missing",
            "",
            "I",
            "Normal",
            "Lsample",
            "L_rdx_default_package",
            "[_rdx_array_prefix",
        ] {
            assert_eq!(
                raw_contains_helper(&symbols, candidate),
                raw_contains(&symbols, candidate),
                "{candidate:?}"
            );
        }
        assert!(!raw_contains_helper(&symbols, "_rdx_default_package"));
        assert!(!raw_contains_helper(&symbols, "_rdx_array_prefix"));
        assert_eq!(symbols.raw_helper_type_components.get().unwrap().len(), 7);
    }

    #[test]
    fn empty_tables_and_separate_tables_keep_exact_identity() {
        let empty = DexSymbols::default();
        assert!(!raw_contains_helper(&empty, "_rdx_missing"));
        assert!(empty.raw_helper_type_components.get().unwrap().is_empty());
        assert!(!raw_contains_helper(&empty, ""));
        let first = DexSymbols {
            types: vec!["Lsample/_rdx_first;".into()],
            ..Default::default()
        };
        let second = DexSymbols {
            types: vec!["Lsample/_rdx_second;".into()],
            ..Default::default()
        };
        assert!(raw_contains_helper(&first, "_rdx_first"));
        assert!(!raw_contains_helper(&second, "_rdx_first"));
        assert!(raw_contains_helper(&second, "_rdx_second"));
    }

    #[test]
    fn concurrent_lookups_share_one_component_set() {
        let symbols = DexSymbols {
            types: vec!["Lsample/_rdx_shared;".into()],
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
                        assert!(raw_contains_helper(symbols, "_rdx_shared"));
                        symbols.raw_helper_type_components.get().unwrap()
                    })
                })
                .collect();
            let sets: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert!(sets.iter().all(|set| std::ptr::eq(*set, sets[0])));
        });
    }
}
