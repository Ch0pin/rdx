mod native_dex {
    pub use rdx::native_dex::*;
}

#[path = "../src/native_hierarchy.rs"]
mod native_hierarchy;

use native_dex::{DexClass, DexSymbols};
use native_hierarchy::{Relation, TypeHierarchy};
use std::sync::Arc;

fn class(descriptor: &str, superclass: Option<&str>, interfaces: &[&str]) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: Arc::from(descriptor),
        superclass: superclass.map(Arc::from),
        interfaces: interfaces.iter().copied().map(Arc::from).collect(),
        access_flags: 0,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
    }
}

#[test]
fn resolves_transitive_apk_exception_ancestry() {
    let base = class(
        "Lapp/BaseFailure;",
        Some("Ljava/lang/RuntimeException;"),
        &[],
    );
    let leaf = class("Lapp/LeafFailure;", Some("Lapp/BaseFailure;"), &[]);
    let hierarchy = TypeHierarchy::from_classes([&base, &leaf]).unwrap();

    assert_eq!(
        hierarchy.assignable("Lapp/LeafFailure;", "Ljava/lang/Throwable;"),
        Relation::Proven
    );
    assert_eq!(
        hierarchy.assignable("Lapp/LeafFailure;", "Lapp/BaseFailure;"),
        Relation::Proven
    );
    assert_eq!(
        hierarchy.assignable("Lapp/LeafFailure;", "Ljava/lang/Error;"),
        Relation::Disproven
    );
}

#[test]
fn missing_external_parent_keeps_negative_relationship_unknown() {
    let leaf = class("Lapp/LeafFailure;", Some("Lvendor/ExternalFailure;"), &[]);
    let hierarchy = TypeHierarchy::from_classes([&leaf]).unwrap();

    assert_eq!(
        hierarchy.assignable("Lapp/LeafFailure;", "Ljava/lang/Throwable;"),
        Relation::Unknown
    );
}

#[test]
fn follows_interfaces_without_treating_names_as_evidence() {
    let marker = class("Lapp/FailureMarker;", Some("Ljava/lang/Object;"), &[]);
    let failure = class(
        "Lapp/Failure;",
        Some("Ljava/lang/Exception;"),
        &["Lapp/FailureMarker;"],
    );
    let hierarchy = TypeHierarchy::from_classes([&marker, &failure]).unwrap();

    assert_eq!(
        hierarchy.assignable("Lapp/Failure;", "Lapp/FailureMarker;"),
        Relation::Proven
    );
    assert_eq!(
        hierarchy.assignable("Lapp/FailureMarker;", "Ljava/lang/Throwable;"),
        Relation::Disproven
    );
    assert_eq!(
        hierarchy.assignable("Llooks/LikeException;", "Ljava/lang/Throwable;"),
        Relation::Unknown
    );
}

#[test]
fn cycles_are_bounded_and_reported_unknown() {
    let a = class("Lbroken/A;", Some("Lbroken/B;"), &[]);
    let b = class("Lbroken/B;", Some("Lbroken/A;"), &[]);
    let hierarchy = TypeHierarchy::from_classes([&a, &b]).unwrap();

    assert_eq!(
        hierarchy.assignable("Lbroken/A;", "Ljava/lang/Throwable;"),
        Relation::Unknown
    );
    assert_eq!(
        hierarchy.assignable("Lbroken/A;", "Lbroken/B;"),
        Relation::Proven
    );
}

#[test]
fn resolves_custom_checked_exception_through_platform_ancestry() {
    let failure = class(
        "Lapp/DiskFailure;",
        Some("Ljava/io/FileNotFoundException;"),
        &[],
    );
    let hierarchy = TypeHierarchy::from_classes([&failure]).unwrap();

    assert_eq!(
        hierarchy.assignable("Lapp/DiskFailure;", "Ljava/lang/Throwable;"),
        Relation::Proven
    );
    assert_eq!(
        hierarchy.assignable("Lapp/DiskFailure;", "Ljava/io/IOException;"),
        Relation::Proven
    );
}

#[test]
fn long_chain_uses_bounded_heap_traversal() {
    const LENGTH: usize = 20_000;
    let names: Vec<_> = (0..LENGTH)
        .map(|index| format!("Llong/C{index};"))
        .collect();
    let classes: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let parent = index
                .checked_sub(1)
                .map(|parent| names[parent].as_str())
                .unwrap_or("Ljava/lang/RuntimeException;");
            class(name, Some(parent), &[])
        })
        .collect();
    let hierarchy = TypeHierarchy::from_classes(classes.iter()).unwrap();

    assert_eq!(
        hierarchy.assignable(names.last().unwrap(), "Ljava/lang/Throwable;"),
        Relation::Proven
    );
}

#[test]
fn diamond_paths_are_processed_once_and_remain_disproven() {
    let root = class("Ldiamond/Root;", Some("Ljava/lang/Object;"), &[]);
    let left = class("Ldiamond/Left;", Some("Ldiamond/Root;"), &[]);
    let right = class("Ldiamond/Right;", Some("Ldiamond/Root;"), &[]);
    let leaf = class(
        "Ldiamond/Leaf;",
        Some("Ldiamond/Left;"),
        &["Ldiamond/Right;"],
    );
    let hierarchy = TypeHierarchy::from_classes([&root, &left, &right, &leaf]).unwrap();

    assert_eq!(
        hierarchy.assignable("Ldiamond/Leaf;", "Ljava/lang/Throwable;"),
        Relation::Disproven
    );
}

#[test]
fn query_budget_stops_work_and_returns_unknown() {
    let names: Vec<_> = (0..32).map(|index| format!("Lbudget/C{index};")).collect();
    let classes: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let parent = index
                .checked_sub(1)
                .map(|parent| names[parent].as_str())
                .unwrap_or("Ljava/lang/RuntimeException;");
            class(name, Some(parent), &[])
        })
        .collect();
    let hierarchy = TypeHierarchy::from_classes(classes.iter()).unwrap();

    assert_eq!(
        hierarchy.assignable_with_budget(names.last().unwrap(), "Ljava/lang/Throwable;", 8, 8),
        Relation::Unknown
    );
    assert_eq!(
        hierarchy.assignable(names.last().unwrap(), "Ljava/lang/Throwable;"),
        Relation::Proven
    );
}

#[test]
fn relation_cache_is_shared_bounded_and_skips_oversized_keys() {
    let hierarchy = TypeHierarchy::from_classes(std::iter::empty()).unwrap();
    let clone = hierarchy.clone();
    assert_eq!(
        hierarchy.assignable("Ljava/lang/RuntimeException;", "Ljava/lang/Throwable;"),
        Relation::Proven
    );
    assert_eq!(clone.cached_relations(), 1);

    let oversized = format!("L{};", "x".repeat(513));
    assert_eq!(
        hierarchy.assignable("Ljava/lang/Object;", &oversized),
        Relation::Disproven
    );
    assert_eq!(hierarchy.cached_relations(), 1);

    for index in 0..9_000 {
        let target = format!("Lcache/T{index};");
        assert_eq!(
            hierarchy.assignable("Ljava/lang/Object;", &target),
            Relation::Disproven
        );
    }
    assert_eq!(hierarchy.cached_relations(), 8_192);
}

#[test]
fn array_covariance_keeps_primitive_components_invariant() {
    let parent = class("Lapp/Base;", Some("Ljava/lang/Object;"), &[]);
    let child = class("Lapp/Child;", Some("Lapp/Base;"), &[]);
    let h = TypeHierarchy::from_classes([&parent, &child]).unwrap();
    for (source, target) in [
        ("[Lapp/Child;", "[Lapp/Base;"),
        ("[[Lapp/Child;", "[[Lapp/Base;"),
        ("[[I", "[Ljava/lang/Object;"),
        ("[[I", "[Ljava/lang/Cloneable;"),
        ("[[I", "[Ljava/io/Serializable;"),
        ("[I", "Ljava/lang/Object;"),
        ("[I", "Ljava/lang/Cloneable;"),
        ("[J", "Ljava/io/Serializable;"),
    ] {
        assert_eq!(
            h.assignable(source, target),
            Relation::Proven,
            "{source} -> {target}"
        );
    }
    for (source, target) in [
        ("[I", "[J"),
        ("[I", "[Ljava/lang/Object;"),
        ("[Ljava/lang/Object;", "[[I"),
        ("[Lapp/Base;", "[Lapp/Child;"),
        ("Ljava/lang/Object;", "[I"),
        ("[I", "Lapp/Base;"),
    ] {
        assert_eq!(
            h.assignable(source, target),
            Relation::Disproven,
            "{source} -> {target}"
        );
    }
}

#[test]
fn array_external_types_remain_unknown_except_language_guarantees() {
    let h = TypeHierarchy::from_classes(std::iter::empty()).unwrap();
    assert_eq!(
        h.assignable("[Lmissing/A;", "[Lmissing/B;"),
        Relation::Unknown
    );
    assert_eq!(
        h.assignable("[Lmissing/A;", "[Ljava/lang/Object;"),
        Relation::Proven
    );
    assert_eq!(
        h.assignable("Lmissing/A;", "Ljava/lang/Object;"),
        Relation::Proven
    );
    for descriptor in ["[V", "[", "[L;", "[Lbad//Name;", "[Lbad.Name;"] {
        assert_eq!(h.assignable(descriptor, descriptor), Relation::Unknown);
        assert_eq!(
            h.assignable(descriptor, "Ljava/lang/Object;"),
            Relation::Unknown
        );
    }
    let valid = format!("{}I", "[".repeat(255));
    assert_eq!(h.assignable(&valid, "Ljava/lang/Object;"), Relation::Proven);
    let invalid = format!("{}I", "[".repeat(256));
    assert_eq!(
        h.assignable(&invalid, "Ljava/lang/Object;"),
        Relation::Unknown
    );
}

#[test]
fn platform_exception_interfaces_are_not_falsely_disproven() {
    let custom = class("Lapp/Error;", Some("Ljava/lang/RuntimeException;"), &[]);
    let h = TypeHierarchy::from_classes([&custom]).unwrap();
    assert_eq!(
        h.assignable("Lapp/Error;", "Ljava/io/Serializable;"),
        Relation::Proven
    );
    assert_eq!(
        h.assignable("Ljava/sql/SQLException;", "Ljava/lang/Iterable;"),
        Relation::Proven
    );
    assert_eq!(
        h.assignable("Ljava/sql/SQLException;", "Ljava/io/Serializable;"),
        Relation::Proven
    );
    assert_eq!(
        h.assignable("Ljava/io/Serializable;", "Ljava/lang/Throwable;"),
        Relation::Disproven
    );
    assert_eq!(
        h.assignable("Lapp/Error;", "Ljava/lang/Error;"),
        Relation::Disproven
    );
}

#[test]
fn strict_superclass_excludes_interfaces_self_and_unknown_paths() {
    let child = class("LC;", Some("LP;"), &["LI;"]);
    let parent = class("LP;", Some("LG;"), &[]);
    let grand = class("LG;", Some("Ljava/lang/Object;"), &[]);
    let h = TypeHierarchy::from_classes([&child, &parent, &grand]).unwrap();
    assert_eq!(h.strict_superclass("LC;", "LP;"), Relation::Proven);
    assert_eq!(h.strict_superclass("LC;", "LG;"), Relation::Proven);
    assert_eq!(h.strict_superclass("LC;", "LI;"), Relation::Disproven);
    assert_eq!(h.strict_superclass("LC;", "LC;"), Relation::Disproven);
    let h = TypeHierarchy::from_classes([&child]).unwrap();
    assert_eq!(h.strict_superclass("LC;", "LG;"), Relation::Unknown);
    let cycle = class("LP;", Some("LC;"), &[]);
    let h = TypeHierarchy::from_classes([&child, &cycle]).unwrap();
    assert_eq!(h.strict_superclass("LC;", "LG;"), Relation::Unknown);
    let conflict = class("LP;", Some("Lother;"), &[]);
    let h = TypeHierarchy::from_classes([&child, &parent, &conflict]).unwrap();
    assert_eq!(h.strict_superclass("LC;", "LP;"), Relation::Unknown);
}

#[test]
fn missing_constructor_body_cannot_authorize_owner_retarget() {
    let leaf = class("Lsample/Leaf;", Some("Ljava/lang/Object;"), &[]);
    let hierarchy = TypeHierarchy::from_classes([&leaf]).unwrap();
    assert!(!hierarchy.equivalent_noarg_constructor("Lsample/Leaf;", "Ljava/lang/Object;"));
}
