use super::{annotation_directory, method, names};
use crate::native_dex::{DexAnnotationSet, DexClass, DexMethod};
use anyhow::{Context, Result, ensure};

const OBJECT: &str = "Ljava/lang/Object;";
const THROWABLE: &str = "Ljava/lang/Throwable;";
const INTRINSICS: &str = "Lkotlin/jvm/internal/Intrinsics;";
const MAX_NAMESPACE_TYPES: usize = 65_536;
const MAX_NAMESPACE_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Proof {
    pub authorization: method::StaticSneakyRethrowAuthorization,
    pub type_parameter: String,
}

fn fresh_type_parameter(class: &DexClass, code_offset: u32) -> Result<String> {
    ensure!(
        class.symbols.types.len() <= MAX_NAMESPACE_TYPES,
        "sneaky-rethrow type namespace budget exceeded"
    );
    let bytes = class
        .symbols
        .types
        .iter()
        .try_fold(0usize, |total, descriptor| {
            total.checked_add(descriptor.len())
        })
        .context("sneaky-rethrow type namespace byte overflow")?;
    ensure!(
        bytes <= MAX_NAMESPACE_BYTES,
        "sneaky-rethrow type namespace byte budget exceeded"
    );
    let collides = |candidate: &str| {
        class.symbols.types.iter().any(|descriptor| {
            descriptor
                .trim_start_matches('[')
                .trim_start_matches('L')
                .trim_end_matches(';')
                .split(['/', '$'])
                .any(|segment| segment == candidate)
        })
    };
    for suffix in 0..64 {
        let candidate = if suffix == 0 {
            format!("RdxSneakyThrowable{code_offset:x}")
        } else {
            format!("RdxSneakyThrowable{code_offset:x}_{suffix}")
        };
        if !collides(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("sneaky-rethrow generated type namespace is exhausted")
}

fn contains_forbidden_annotation(class: &DexClass, set: Option<&DexAnnotationSet>) -> bool {
    set.is_some_and(|set| {
        set.iter().any(|annotation| {
            class
                .symbols
                .types
                .get(annotation.type_idx as usize)
                .is_none_or(|ty| {
                    matches!(
                        ty.as_ref(),
                        "Ldalvik/annotation/Signature;" | "Ljava/lang/SuppressWarnings;"
                    )
                })
        })
    })
}

pub(super) fn prove(
    class: &DexClass,
    root: &DexMethod,
    failure: &method::StaticSneakyRethrowFailure,
) -> Result<Proof> {
    let code = root
        .code
        .as_ref()
        .context("sneaky-rethrow method has no code")?;
    ensure!(
        class.descriptor == root.declaring_type
            && class.access_flags & 7 == 1
            && class.access_flags & !0x7631 == 0
            && class.access_flags & (0x200 | 0x2000 | 0x4000) == 0
            && !class.descriptor.contains('$')
            && root.access_flags == 9
            && root.name.as_ref() != "<clinit>"
            && root.parameters.len() == 1
            && root.parameters[0].as_ref() == OBJECT
            && root.return_type.as_ref() == "V"
            && root.thrown_types.is_empty(),
        "sneaky-rethrow declaration is outside the first static shape"
    );
    ensure!(
        failure.effect == THROWABLE
            && failure.site.declaring_type == root.declaring_type
            && failure.site.method_name == root.name
            && failure.site.parameters == root.parameters
            && failure.site.return_type == root.return_type
            && failure.site.static_method
            && failure.site.code_offset == code.offset
            && failure.site.throw_pc == 14
            && failure.site.throw_register == 1,
        "sneaky-rethrow failure provenance differs"
    );
    ensure!(
        code.registers == 2
            && code.ins == 1
            && code.outs == 2
            && code.tries == 0
            && code.try_regions.is_empty()
            && code.instructions.len() == 15,
        "sneaky-rethrow code header is outside the first shape"
    );
    let words = &code.instructions;
    ensure!(
        words[0] == 0x1020
            && words[2] == 0x0039
            && words[3] == 3
            && words[4] == 0x000e
            && words[5] == 0x011f
            && words[6] == words[1]
            && words[7] == 0x1154
            && words[9] == 0x001a
            && words[11] == 0x2071
            && words[13] == 1
            && words[14] == 0x0127,
        "sneaky-rethrow original instruction/register shape differs"
    );
    let decoded = crate::native_ir::DecodedMethod::decode(code)?;
    ensure!(
        decoded
            .instructions
            .iter()
            .map(|instruction| (instruction.pc, instruction.opcode, instruction.width))
            .eq([
                (0, 0x20, 2),
                (2, 0x39, 2),
                (4, 0x0e, 1),
                (5, 0x1f, 2),
                (7, 0x54, 2),
                (9, 0x1a, 2),
                (11, 0x71, 3),
                (14, 0x27, 1),
            ]),
        "sneaky-rethrow decoded instruction boundaries differ"
    );

    let wrapper = class
        .symbols
        .types
        .get(words[1] as usize)
        .context("sneaky-rethrow wrapper type missing")?;
    ensure!(
        wrapper.starts_with('L') && wrapper.ends_with(';') && wrapper.as_ref() != THROWABLE,
        "sneaky-rethrow wrapper type is invalid"
    );
    let &(field_owner, field_type, field_name) = class
        .symbols
        .fields
        .get(words[8] as usize)
        .context("sneaky-rethrow field reference missing")?;
    let field_owner = class
        .symbols
        .types
        .get(field_owner as usize)
        .context("sneaky-rethrow field owner missing")?;
    let field_type = class
        .symbols
        .types
        .get(field_type as usize)
        .context("sneaky-rethrow field type missing")?;
    let field_name = class
        .symbols
        .strings
        .get(field_name as usize)
        .context("sneaky-rethrow field name missing")?;
    names::member(field_name)?;
    let hierarchy = class
        .symbols
        .hierarchy
        .get()
        .context("sneaky-rethrow hierarchy missing")?;
    ensure!(
        field_owner == wrapper
            && field_type.as_ref() == THROWABLE
            && hierarchy.exact_public_sneaky_readable_field(
                &class.descriptor,
                field_owner,
                field_name,
                THROWABLE,
            ),
        "sneaky-rethrow field declaration or source access differs"
    );
    ensure!(
        class
            .symbols
            .strings
            .get(words[10] as usize)
            .is_some_and(|marker| marker == "e"),
        "sneaky-rethrow null marker differs"
    );
    let &(call_owner, call_proto, call_name) = class
        .symbols
        .methods
        .get(words[12] as usize)
        .context("sneaky-rethrow null-check call missing")?;
    let call_owner = class
        .symbols
        .types
        .get(call_owner as usize)
        .context("sneaky-rethrow null-check owner missing")?;
    let (call_return, call_parameters) = class
        .symbols
        .protos
        .get(call_proto as usize)
        .context("sneaky-rethrow null-check prototype missing")?;
    let call_name = class
        .symbols
        .strings
        .get(call_name as usize)
        .context("sneaky-rethrow null-check name missing")?;
    ensure!(
        call_owner.as_ref() == INTRINSICS
            && call_name == "checkNotNullParameter"
            && call_return.as_ref() == "V"
            && call_parameters
                .iter()
                .map(AsRef::as_ref)
                .eq([OBJECT, "Ljava/lang/String;"])
            && hierarchy.exact_loaded_static_source_call(
                class,
                call_owner,
                call_name,
                call_parameters,
                call_return,
            ),
        "sneaky-rethrow null-check source declaration differs"
    );

    let directory = if class.annotations_offset == 0 {
        None
    } else {
        Some(annotation_directory(class).context("sneaky-rethrow annotation directory missing")?)
    };
    let position = class
        .methods
        .iter()
        .position(|candidate| std::ptr::eq(candidate, root))
        .context("sneaky-rethrow method is not owned by its class")?;
    ensure!(
        !contains_forbidden_annotation(
            class,
            directory.and_then(|directory| directory.class.as_ref()),
        ) && !contains_forbidden_annotation(
            class,
            directory
                .and_then(|directory| directory.methods.get(position))
                .and_then(Option::as_ref),
        ) && directory
            .and_then(|directory| directory.parameters.get(position))
            .is_none_or(|parameters| {
                parameters
                    .iter()
                    .all(|set| !contains_forbidden_annotation(class, set.as_ref()))
            }),
        "sneaky-rethrow existing generic Signature is unsupported"
    );

    let type_parameter = fresh_type_parameter(class, code.offset)?;
    Ok(Proof {
        authorization: method::StaticSneakyRethrowAuthorization {
            site: failure.site.clone(),
            type_parameter: type_parameter.clone(),
        },
        type_parameter,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{
        method as renderer, render_method, render_method_without_same_object_retry,
    };
    use crate::{
        native_dex::{
            DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexField, DexMethod,
            DexSymbols,
        },
        native_hierarchy::TypeHierarchy,
    };
    use std::sync::Arc;

    // The fixture mirrors the independent DEX field and dependency controls.
    #[allow(clippy::too_many_arguments)]
    fn fixture(
        field_flags: u32,
        field_static: bool,
        signature: bool,
        intrinsics_flags: u32,
        intrinsics_throw: Option<&str>,
        ambiguous_allocated_type: bool,
        cyclic_dependency: bool,
        wrapper_descriptor: &str,
    ) -> (DexClass, DexClass, DexClass) {
        let root_owner: Arc<str> = "Lsample/Sneaky;".into();
        let wrapper_owner: Arc<str> = wrapper_descriptor.into();
        let intrinsics_owner: Arc<str> = "Lkotlin/jvm/internal/Intrinsics;".into();
        let mut annotations = std::collections::BTreeMap::new();
        if signature {
            annotations.insert(
                1,
                Arc::new(DexAnnotationDirectory {
                    methods: vec![Some(
                        vec![Arc::new(DexAnnotation {
                            visibility: 2,
                            type_idx: 6,
                            elements: vec![],
                        })]
                        .into(),
                    )],
                    ..Default::default()
                }),
            );
        }
        let symbols = Arc::new(DexSymbols {
            strings: vec![
                "a".into(),
                "checkNotNullParameter".into(),
                "e".into(),
                "l".into(),
                "call".into(),
                "<init>".into(),
            ],
            types: vec![
                root_owner.clone(),
                wrapper_owner.clone(),
                "Ljava/lang/Throwable;".into(),
                "Ljava/lang/Object;".into(),
                "Ljava/lang/String;".into(),
                intrinsics_owner.clone(),
                "Ldalvik/annotation/Signature;".into(),
                intrinsics_throw
                    .unwrap_or("Ljava/lang/NullPointerException;")
                    .into(),
                "Lsample/Caller;".into(),
            ],
            protos: vec![
                (
                    "V".into(),
                    vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                ),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec![]),
            ],
            fields: vec![(1, 2, 0)],
            methods: vec![(5, 0, 1), (0, 1, 3), (7, 2, 5)],
            annotations,
            ..Default::default()
        });
        let root = DexClass {
            symbols: symbols.clone(),
            descriptor: root_owner.clone(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x401,
            annotations_offset: if signature { 1 } else { 0 },
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![DexMethod {
                declaring_type: root_owner,
                name: "l".into(),
                return_type: "V".into(),
                parameters: vec!["Ljava/lang/Object;".into()],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 2,
                    ins: 1,
                    outs: 2,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![
                        0x1020, 1, 0x0039, 3, 0x000e, 0x011f, 1, 0x1154, 0, 0x001a, 2, 0x2071, 0,
                        1, 0x0127,
                    ],
                    offset: 37,
                }),
            }],
        };
        let wrapper = DexClass {
            symbols: symbols.clone(),
            descriptor: wrapper_owner.clone(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![DexField {
                declaring_type: wrapper_owner,
                name: "a".into(),
                field_type: "Ljava/lang/Throwable;".into(),
                access_flags: field_flags,
                is_static: field_static,
            }],
            methods: vec![],
        };
        let intrinsics = DexClass {
            symbols,
            descriptor: intrinsics_owner.clone(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![DexMethod {
                declaring_type: intrinsics_owner,
                name: "checkNotNullParameter".into(),
                return_type: "V".into(),
                parameters: vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                thrown_types: vec![],
                access_flags: if intrinsics_throw.is_some() || cyclic_dependency {
                    9
                } else {
                    intrinsics_flags
                },
                code: if cyclic_dependency {
                    Some(DexCode {
                        registers: 2,
                        ins: 2,
                        outs: 2,
                        tries: 0,
                        try_regions: vec![],
                        instructions: vec![0x2071, 0, 1, 0x000e],
                        offset: 91,
                    })
                } else {
                    intrinsics_throw.map(|_| DexCode {
                        registers: 3,
                        ins: 2,
                        outs: 1,
                        tries: 0,
                        try_regions: vec![],
                        instructions: vec![0x0022, 7, 0x1070, 2, 0, 0x0027],
                        offset: 91,
                    })
                },
            }],
        };
        let caller = DexClass {
            symbols: intrinsics.symbols.clone(),
            descriptor: "Lsample/Caller;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![DexMethod {
                declaring_type: "Lsample/Caller;".into(),
                name: "call".into(),
                return_type: "V".into(),
                parameters: vec!["Ljava/lang/Object;".into()],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![0x1071, 1, 0, 0x000e],
                    offset: 123,
                }),
            }],
        };
        let shadow_a = DexClass {
            symbols: intrinsics.symbols.clone(),
            descriptor: intrinsics_throw
                .unwrap_or("Ljava/lang/NullPointerException;")
                .into(),
            superclass: Some("Ljava/lang/RuntimeException;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let shadow_b = DexClass {
            symbols: intrinsics.symbols.clone(),
            descriptor: intrinsics_throw
                .unwrap_or("Ljava/lang/NullPointerException;")
                .into(),
            superclass: Some("Ljava/lang/Exception;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let mut classes = vec![&root, &wrapper, &intrinsics, &caller];
        if ambiguous_allocated_type {
            classes.extend([&shadow_a, &shadow_b]);
        }
        let hierarchy = Arc::new(TypeHierarchy::from_classes(classes).unwrap());
        root.symbols.hierarchy.set(hierarchy).unwrap();
        (root, wrapper, intrinsics)
    }

    fn baseline_failure(class: &DexClass) -> renderer::StaticSneakyRethrowFailure {
        let error = match renderer::reconstruct("sample.Sneaky", class, &class.methods[0]) {
            Ok(_) => panic!("baseline unexpectedly accepted static sneaky-rethrow fixture"),
            Err(error) => error,
        };
        error
            .downcast_ref::<renderer::StaticSneakyRethrowFailure>()
            .unwrap()
            .clone()
    }

    #[test]
    fn exact_static_shape_gets_only_the_synthesized_generic_header() {
        let (class, _wrapper, _intrinsics) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        let failure = baseline_failure(&class);
        let proof = super::prove(&class, &class.methods[0], &failure).unwrap();
        let effects = renderer::reconstruct_static_sneaky_rethrow_effects_for_test(
            "sample.Sneaky",
            &class,
            &class.methods[0],
            &proof.authorization,
        )
        .unwrap();
        assert!(effects.contains("Ljava/lang/Throwable;"));
        let source = render_method("sample.Sneaky", &class, &class.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("@java.lang.SuppressWarnings(\"unchecked\")"));
        assert!(source.contains("Synthesized generic throws signature"));
        assert!(source.contains("static <RdxSneakyThrowable25 extends java.lang.Throwable> void l(java.lang.Object p0) throws RdxSneakyThrowable25"));
        assert!(source.contains("throw ((RdxSneakyThrowable25) v"));
        assert!(!source.contains("RuntimeException)"));
        assert!(!source.contains("new RuntimeException"));
    }

    #[test]
    fn declaration_header_and_exact_operands_are_required() {
        let (mut checked, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        checked.methods[0]
            .thrown_types
            .push("Ljava/lang/Exception;".into());
        assert!(render_method("sample.Sneaky", &checked, &checked.methods[0]).is_err());

        let (mut instance, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        instance.methods[0].access_flags = 1;
        assert!(render_method("sample.Sneaky", &instance, &instance.methods[0]).is_err());

        for (pc, replacement) in [
            (0, 0x1120),
            (2, 0x0038),
            (7, 0x1054),
            (11, 0x1071),
            (14, 0x0027),
        ] {
            let (mut malformed, _, _) = fixture(
                0x11,
                false,
                false,
                0x109,
                None,
                false,
                false,
                "Lsample/Failure;",
            );
            malformed.methods[0].code.as_mut().unwrap().instructions[pc] = replacement;
            assert!(render_method("sample.Sneaky", &malformed, &malformed.methods[0]).is_err());
        }
    }

    #[test]
    fn readable_field_authority_is_exact_and_public_instance_only() {
        for (flags, is_static) in [(0x12, false), (0x19, true), (0x51, false)] {
            let (class, _, _) = fixture(
                flags,
                is_static,
                false,
                0x109,
                None,
                false,
                false,
                "Lsample/Failure;",
            );
            assert!(render_method("sample.Sneaky", &class, &class.methods[0]).is_err());
        }
    }

    #[test]
    fn generic_metadata_marker_and_call_authority_are_exact() {
        let (signature, _, _) = fixture(
            0x11,
            false,
            true,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        assert!(render_method("sample.Sneaky", &signature, &signature.methods[0]).is_err());

        let (private_call, _, _) = fixture(
            0x11,
            false,
            false,
            0x10a,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        assert!(render_method("sample.Sneaky", &private_call, &private_call.methods[0]).is_err());

        let (mut marker, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        marker.methods[0].code.as_mut().unwrap().instructions[10] = 0;
        assert!(render_method("sample.Sneaky", &marker, &marker.methods[0]).is_err());

        let (direct_unchecked, _, direct_intrinsics) = fixture(
            0x11,
            false,
            false,
            0x109,
            Some("Ljava/lang/NullPointerException;"),
            false,
            false,
            "Lsample/Failure;",
        );
        let hierarchy = direct_unchecked.symbols.hierarchy.get().unwrap();
        assert!(hierarchy.exact_loaded_static_source_call(
            &direct_unchecked,
            "Lkotlin/jvm/internal/Intrinsics;",
            "checkNotNullParameter",
            &["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
            "V",
        ));
        let dependency_source = render_method_without_same_object_retry(
            "kotlin.jvm.internal.Intrinsics",
            &direct_intrinsics,
            &direct_intrinsics.methods[0],
        )
        .unwrap()
        .source;
        assert!(!dependency_source.contains(" throws "));
        assert!(
            render_method(
                "sample.Sneaky",
                &direct_unchecked,
                &direct_unchecked.methods[0],
            )
            .is_ok()
        );

        for (allocated, ambiguous) in [
            ("Ljava/io/IOException;", false),
            ("Ljava/lang/NullPointerException;", true),
        ] {
            let (declined, _, _) = fixture(
                0x11,
                false,
                false,
                0x109,
                Some(allocated),
                ambiguous,
                false,
                "Lsample/Failure;",
            );
            let hierarchy = declined.symbols.hierarchy.get().unwrap();
            assert!(!hierarchy.exact_loaded_static_source_call(
                &declined,
                "Lkotlin/jvm/internal/Intrinsics;",
                "checkNotNullParameter",
                &["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                "V",
            ));
        }

        let (cyclic, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            true,
            "Lsample/Failure;",
        );
        let hierarchy = cyclic.symbols.hierarchy.get().unwrap();
        assert!(!hierarchy.exact_loaded_static_source_call(
            &cyclic,
            "Lkotlin/jvm/internal/Intrinsics;",
            "checkNotNullParameter",
            &["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
            "V",
        ));
    }

    #[test]
    fn generated_type_parameter_is_fresh_and_default_package_access_declines() {
        let (collision, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/RdxSneakyThrowable25;",
        );
        let source = render_method("sample.Sneaky", &collision, &collision.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("<RdxSneakyThrowable25_1 extends java.lang.Throwable>"));
        assert!(source.contains("throws RdxSneakyThrowable25_1"));

        let (default_package, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "LRdxSneakyThrowable25;",
        );
        assert!(
            render_method(
                "sample.Sneaky",
                &default_package,
                &default_package.methods[0],
            )
            .is_err()
        );
    }

    #[test]
    fn authorization_is_bound_to_the_original_terminal_site() {
        let (class, _, _) = fixture(
            0x11,
            false,
            false,
            0x109,
            None,
            false,
            false,
            "Lsample/Failure;",
        );
        let failure = baseline_failure(&class);
        let mut authorization = super::prove(&class, &class.methods[0], &failure)
            .unwrap()
            .authorization;
        authorization.site.throw_pc = 13;
        assert!(
            renderer::reconstruct_static_sneaky_rethrow(
                "sample.Sneaky",
                &class,
                &class.methods[0],
                &authorization,
            )
            .is_err()
        );
    }
}
