#[path = "../src/native_java/allocation.rs"]
mod allocation;
use allocation::{Allocation, Capture, Event, Expr, Symbol};
fn symbol(text: &str, label: &str) -> Symbol {
    Symbol {
        text: text.into(),
        label: label.into(),
    }
}
fn call(site: usize, name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        site,
        target: Box::new(Expr::Local("source".into())),
        method: symbol(name, name),
        args,
    }
}
#[test]
fn input_cast_preserves_bound_local_validation_and_type_link() {
    let allocation = Allocation {
        site: 1,
        constructor_site: 2,
        ty: symbol("Holder", "sample.Holder"),
        captures: vec![],
        arguments: vec![Expr::Cast {
            ty: symbol("android.content.Context", "android.content.Context"),
            value: Box::new(Expr::Local("this".into())),
        }],
    };
    let events = [
        Event::Allocate {
            site: 1,
            ty: "sample.Holder".into(),
        },
        Event::Construct {
            site: 2,
            ty: "sample.Holder".into(),
        },
    ];
    let rendered = allocation.render_checked(&events, &["this"]).unwrap();
    assert_eq!(
        rendered.expression,
        "new Holder(((android.content.Context) this))"
    );
    assert_eq!(rendered.events, events);
    let link = rendered
        .links
        .iter()
        .find(|link| link.label == "android.content.Context")
        .unwrap();
    assert_eq!(
        rendered
            .expression
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "android.content.Context"
    );
    // Converted spelling is never a substitute for a bound original operand.
    assert!(
        allocation
            .render_checked(&events, &["((android.content.Context) this)"])
            .unwrap_err()
            .to_string()
            .contains("unbound allocation local")
    );
}

#[test]
fn cast_recurses_into_nested_allocation_declarations_and_effects() {
    let inner = Allocation {
        site: 2,
        constructor_site: 4,
        ty: symbol("Child", "sample.Child"),
        captures: vec![Capture {
            ty: "int".into(),
            name: "value".into(),
            expression: call(3, "f", vec![]),
        }],
        arguments: vec![Expr::Capture(0)],
    };
    let allocation = Allocation {
        site: 1,
        constructor_site: 5,
        ty: symbol("Holder", "sample.Holder"),
        captures: vec![],
        arguments: vec![Expr::Cast {
            ty: symbol("Parent", "sample.Parent"),
            value: Box::new(Expr::New(Box::new(inner))),
        }],
    };
    let events = [
        Event::Allocate {
            site: 1,
            ty: "sample.Holder".into(),
        },
        Event::Allocate {
            site: 2,
            ty: "sample.Child".into(),
        },
        Event::Call {
            site: 3,
            method: "f".into(),
        },
        Event::Construct {
            site: 4,
            ty: "sample.Child".into(),
        },
        Event::Construct {
            site: 5,
            ty: "sample.Holder".into(),
        },
    ];
    let rendered = allocation.render_checked(&events, &["source"]).unwrap();
    assert_eq!(rendered.declarations, ["int value;"]);
    assert_eq!(
        rendered.expression,
        "new Holder(((Parent) new Child((value = source.f()))))"
    );
    assert_eq!(rendered.events, events);
}
#[test]
fn nested_allocation_keeps_outer_allocation_before_captured_call() {
    let inner = Allocation {
        site: 2,
        constructor_site: 2,
        ty: symbol("B", "sample.B"),
        captures: vec![Capture {
            ty: "java.lang.String".into(),
            name: "value".into(),
            expression: call(3, "f", vec![]),
        }],
        arguments: vec![Expr::Capture(0)],
    };
    let outer = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![],
        arguments: vec![Expr::New(Box::new(inner))],
    };
    let rendered = outer
        .render_checked(
            &[
                Event::Allocate {
                    site: 1,
                    ty: "sample.A".into(),
                },
                Event::Allocate {
                    site: 2,
                    ty: "sample.B".into(),
                },
                Event::Call {
                    site: 3,
                    method: "f".into(),
                },
                Event::Construct {
                    site: 2,
                    ty: "sample.B".into(),
                },
                Event::Construct {
                    site: 1,
                    ty: "sample.A".into(),
                },
            ],
            &["source"],
        )
        .unwrap();
    assert_eq!(rendered.declarations, ["java.lang.String value;"]);
    assert_eq!(rendered.expression, "new A(new B((value = source.f())))");
    assert_eq!(rendered.links[0].label, "sample.A");
}
#[test]
fn dependency_chain_emits_earlier_capture_inside_later_constructor_argument() {
    let allocation = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![
            Capture {
                ty: "int".into(),
                name: "first".into(),
                expression: call(2, "f", vec![]),
            },
            Capture {
                ty: "int".into(),
                name: "second".into(),
                expression: call(3, "g", vec![Expr::Capture(0)]),
            },
        ],
        arguments: vec![Expr::Capture(1)],
    };
    let rendered = allocation
        .render_checked(
            &[
                Event::Allocate {
                    site: 1,
                    ty: "sample.A".into(),
                },
                Event::Call {
                    site: 2,
                    method: "f".into(),
                },
                Event::Call {
                    site: 3,
                    method: "g".into(),
                },
                Event::Construct {
                    site: 1,
                    ty: "sample.A".into(),
                },
            ],
            &["source"],
        )
        .unwrap();
    assert_eq!(
        rendered.expression,
        "new A((second = source.g((first = source.f()))))"
    );
}
#[test]
fn repeated_callee_sites_cannot_be_reordered() {
    let allocation = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![
            Capture {
                ty: "int".into(),
                name: "first".into(),
                expression: call(20, "same", vec![]),
            },
            Capture {
                ty: "int".into(),
                name: "second".into(),
                expression: call(21, "same", vec![]),
            },
        ],
        arguments: vec![Expr::Capture(0), Expr::Capture(1)],
    };
    assert!(
        allocation
            .render_checked(
                &[
                    Event::Allocate {
                        site: 1,
                        ty: "sample.A".into()
                    },
                    Event::Call {
                        site: 21,
                        method: "same".into()
                    },
                    Event::Call {
                        site: 20,
                        method: "same".into()
                    },
                    Event::Construct {
                        site: 1,
                        ty: "sample.A".into()
                    }
                ],
                &["source"]
            )
            .is_err()
    );
}
#[test]
fn unicode_field_link_uses_character_offsets_and_alias_is_assigned_once() {
    let allocation = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![Capture {
            ty: "int".into(),
            name: "number".into(),
            expression: Expr::FieldRead {
                site: 2,
                receiver: Box::new(Expr::Local("hé".into())),
                field: symbol("値", "sample.Holder.value:I"),
            },
        }],
        arguments: vec![Expr::Capture(0), Expr::Capture(0)],
    };
    let rendered = allocation
        .render_checked(
            &[
                Event::Allocate {
                    site: 1,
                    ty: "sample.A".into(),
                },
                Event::Read {
                    site: 2,
                    field: "sample.Holder.value:I".into(),
                },
                Event::Construct {
                    site: 1,
                    ty: "sample.A".into(),
                },
            ],
            &["hé"],
        )
        .unwrap();
    assert_eq!(rendered.expression, "new A((number = hé.値), number)");
    let link = rendered
        .links
        .iter()
        .find(|link| link.label == "sample.Holder.value:I")
        .unwrap();
    assert_eq!((link.start, link.end), (19, 20));
}
#[test]
fn capture_cycles_reordering_and_unbound_locals_fail_closed() {
    let capture = |name: &str, expression| Capture {
        ty: "int".into(),
        name: name.into(),
        expression,
    };
    let cyclic = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![
            capture("first", Expr::Capture(1)),
            capture("second", Expr::Capture(0)),
        ],
        arguments: vec![Expr::Capture(0)],
    };
    assert!(cyclic.render_checked(&[], &["source"]).is_err());
    let reordered = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![
            capture("first", call(2, "first", vec![])),
            capture("second", call(3, "second", vec![])),
        ],
        arguments: vec![Expr::Capture(1)],
    };
    assert!(reordered.render_checked(&[], &["source"]).is_err());
    let unbound = Allocation {
        site: 1,
        constructor_site: 1,
        ty: symbol("A", "sample.A"),
        captures: vec![],
        arguments: vec![Expr::Local("missing".into())],
    };
    assert!(unbound.render_checked(&[], &["source"]).is_err());
}

#[test]
fn explicit_check_cast_has_ordered_throwing_event() {
    let allocation = Allocation {
        site: 1,
        constructor_site: 3,
        ty: symbol("Holder", "sample.Holder"),
        captures: vec![Capture {
            ty: "Child".into(),
            name: "castValue".into(),
            expression: Expr::CheckCast {
                site: 2,
                ty: symbol("Child", "sample.Child"),
                value: Box::new(Expr::Local("input".into())),
            },
        }],
        arguments: vec![Expr::Capture(0)],
    };
    let events = [
        Event::Allocate {
            site: 1,
            ty: "sample.Holder".into(),
        },
        Event::CheckCast {
            site: 2,
            ty: "sample.Child".into(),
        },
        Event::Construct {
            site: 3,
            ty: "sample.Holder".into(),
        },
    ];
    let rendered = allocation.render_checked(&events, &["input"]).unwrap();
    assert_eq!(
        rendered.expression,
        "new Holder((castValue = ((Child) input)))"
    );
    let mut reordered = events.clone();
    reordered.swap(0, 1);
    assert!(allocation.render_checked(&reordered, &["input"]).is_err());
}

#[test]
fn staged_output_relocates_only_root_allocation_and_keeps_statement_links() {
    let a = Allocation {
        site: 1,
        constructor_site: 4,
        ty: symbol("Holder", "Holder"),
        captures: vec![
            Capture {
                ty: "First".into(),
                name: "first".into(),
                expression: Expr::CheckCast {
                    site: 2,
                    ty: symbol("First", "First"),
                    value: Box::new(Expr::Local("input1".into())),
                },
            },
            Capture {
                ty: "Second".into(),
                name: "second".into(),
                expression: Expr::CheckCast {
                    site: 3,
                    ty: symbol("Second", "Second"),
                    value: Box::new(Expr::Local("input2".into())),
                },
            },
        ],
        arguments: vec![Expr::Capture(1), Expr::Capture(0)],
    };
    let events = [
        Event::Allocate {
            site: 1,
            ty: "Holder".into(),
        },
        Event::CheckCast {
            site: 2,
            ty: "First".into(),
        },
        Event::CheckCast {
            site: 3,
            ty: "Second".into(),
        },
        Event::Construct {
            site: 4,
            ty: "Holder".into(),
        },
    ];
    assert!(a.render_checked(&events, &["input1", "input2"]).is_err());
    let output = a
        .render_staged_checked(&events, &["input1", "input2"])
        .unwrap();
    assert_eq!(
        output.declarations,
        [
            "First first = ((First) input1);",
            "Second second = ((Second) input2);"
        ]
    );
    assert_eq!(output.expression, "new Holder(second, first)");
    assert_eq!(
        output.events,
        [
            events[1].clone(),
            events[2].clone(),
            events[0].clone(),
            events[3].clone()
        ]
    );
    for (line, links) in output.declarations.iter().zip(&output.declaration_links) {
        for link in links {
            assert_eq!(
                line.chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                link.label
            );
        }
    }
    let mut wrong = events.clone();
    wrong.swap(1, 2);
    assert!(
        a.render_staged_checked(&wrong, &["input1", "input2"])
            .is_err()
    );
}

#[test]
fn staged_output_rejects_unused_and_forward_capture_dependencies() {
    let mut a = Allocation {
        site: 1,
        constructor_site: 3,
        ty: symbol("Holder", "Holder"),
        captures: vec![Capture {
            ty: "Child".into(),
            name: "first".into(),
            expression: Expr::CheckCast {
                site: 2,
                ty: symbol("Child", "Child"),
                value: Box::new(Expr::Local("input".into())),
            },
        }],
        arguments: vec![],
    };
    let events = [
        Event::Allocate {
            site: 1,
            ty: "Holder".into(),
        },
        Event::CheckCast {
            site: 2,
            ty: "Child".into(),
        },
        Event::Construct {
            site: 3,
            ty: "Holder".into(),
        },
    ];
    assert!(a.render_staged_checked(&events, &["input"]).is_err());
    a.arguments.push(Expr::Capture(0));
    a.captures[0].expression = Expr::Capture(0);
    assert!(a.render_staged_checked(&events, &["input"]).is_err());
}
