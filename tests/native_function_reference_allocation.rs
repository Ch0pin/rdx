use rdx::{
    native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexField, DexMethod, DexSymbols,
        DexTryRegion, DexValue,
    },
    native_hierarchy::TypeHierarchy,
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};

const CALLER: &str = "Lsample/Caller;";
const ADAPTER: &str = "Lsample/Caller$Callback;";
const PARENT: &str = "Lkotlin/jvm/internal/FunctionReferenceImpl;";
const ROUTER: &str = "Lsample/Router;";
const EVENTS: &str = "Lsample/NavigationEvents;";
const OBJECT: &str = "Ljava/lang/Object;";
const PARAMETERS: [&str; 6] = [
    "I",
    OBJECT,
    "Ljava/lang/Class;",
    "Ljava/lang/String;",
    "Ljava/lang/String;",
    "I",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    SameEnclosingMethod,
    DifferentEnclosingMethod,
    MissingMetadata,
    DuplicateMetadata,
    Field,
    OtherParent,
    OtherPrototype,
    ProtectedParentConstructor,
    AliasEscape,
    AliasOverwrite,
    PartialHandler,
}

#[allow(clippy::too_many_arguments)]
fn method(
    owner: &str,
    name: &str,
    ret: &str,
    args: &[&str],
    flags: u32,
    registers: u16,
    ins: u16,
    outs: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: args.iter().copied().map(Arc::from).collect(),
        thrown_types: vec![],
        access_flags: flags,
        code: Some(DexCode {
            registers,
            ins,
            outs,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}

fn fixture(shape: Shape) -> Vec<DexClass> {
    let params = PARAMETERS.into_iter().map(Arc::from).collect::<Vec<_>>();
    let mut symbols = DexSymbols {
        types: [
            ADAPTER,
            ROUTER,
            EVENTS,
            CALLER,
            PARENT,
            OBJECT,
            "Ldalvik/annotation/InnerClass;",
            "Ldalvik/annotation/EnclosingMethod;",
        ]
        .into_iter()
        .map(Arc::from)
        .collect(),
        strings: [
            "<init>",
            "getNavigationEvents",
            "onNavigate",
            "onNavigate()V",
            "observe",
            "run",
            "name",
            "accessFlags",
            "value",
            "consume",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        protos: vec![
            ("V".into(), params),
            (EVENTS.into(), vec![]),
            (
                "V".into(),
                vec![ADAPTER.into(), ADAPTER.into(), EVENTS.into()],
            ),
            ("V".into(), vec![]),
            (ADAPTER.into(), vec![ROUTER.into()]),
            ("V".into(), vec![OBJECT.into()]),
        ],
        methods: vec![
            (4, 0, 0),
            (1, 1, 1),
            (3, 2, 4),
            (5, 3, 0),
            (3, 4, 5),
            (3, 3, 0),
            (3, 5, 9),
        ],
        ..Default::default()
    };
    if shape == Shape::OtherPrototype {
        symbols.protos[0].1[5] = "Z".into();
    }
    let enclosing = Arc::new(DexAnnotation {
        visibility: 2,
        type_idx: 7,
        elements: vec![(
            8,
            DexValue::Method(if shape == Shape::DifferentEnclosingMethod {
                5
            } else {
                4
            }),
        )],
    });
    let mut annotations = vec![
        Arc::new(DexAnnotation {
            visibility: 2,
            type_idx: 6,
            elements: vec![(6, DexValue::Null), (7, DexValue::Int(0x1019))],
        }),
        enclosing.clone(),
    ];
    if shape == Shape::DuplicateMetadata {
        annotations.push(enclosing);
    }
    symbols.annotations.insert(
        1,
        Arc::new(DexAnnotationDirectory {
            class: Some(annotations.into()),
            ..Default::default()
        }),
    );
    let symbols = Arc::new(symbols);
    let class = |descriptor: &str, parent: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: symbols.clone(),
        methods,
    };

    let mut words = vec![
        0x0722, 0,      // new-instance v7, Caller$Callback
        0x7007, // move-object v0, v7
    ];
    match shape {
        Shape::AliasEscape => words.extend([0x1071, 6, 0]), // consume(v0)
        Shape::AliasOverwrite => words.push(0x8007),        // move-object v0, v8
        _ => {}
    }
    words.extend([
        0x106e, 1, 8,      // Router.getNavigationEvents(v8)
        0x020c, // move-result-object v2
        0x051a, 3,      // const-string v5, "onNavigate()V"
        0x0612, // const/4 v6, 0
        0x2112, // const/4 v1, 2
        0x031c, 1, // const-class v3, Router
        0x041a, 2, // const-string v4, "onNavigate"
        0x0776, 0, 0, // invoke-direct/range {v0 .. v6}, FunctionReferenceImpl.<init>
        0x3071, 2, 0x0270, // observe(v0, v7, v2)
        0x0711, // return-object v7
    ]);
    let mut run = method(CALLER, "run", ADAPTER, &[ROUTER], 9, 9, 1, 7, words);
    if shape == Shape::PartialHandler {
        let code = run.code.as_mut().unwrap();
        let handler = u32::try_from(code.instructions.len()).unwrap();
        code.instructions.extend([0x000d, 0x0027]);
        code.tries = 1;
        code.try_regions = vec![DexTryRegion {
            start: 3,
            end: 7,
            catches: vec![(None, handler)].into(),
        }];
    }
    let caller = class(
        CALLER,
        OBJECT,
        vec![
            run,
            method(
                CALLER,
                "<init>",
                "V",
                &[],
                0x10001,
                1,
                1,
                1,
                vec![0x1070, 3, 0, 0x000e],
            ),
            method(
                CALLER,
                "observe",
                "V",
                &[ADAPTER, ADAPTER, EVENTS],
                9,
                3,
                3,
                0,
                vec![0x000e],
            ),
        ],
    );
    let mut adapter = class(
        ADAPTER,
        if shape == Shape::OtherParent {
            ROUTER
        } else {
            PARENT
        },
        vec![method(
            ADAPTER,
            "invoke",
            OBJECT,
            &[OBJECT, OBJECT],
            0x11,
            3,
            3,
            0,
            vec![0x0111],
        )],
    );
    adapter.access_flags = 0x1011;
    adapter.annotations_offset = u32::from(shape != Shape::MissingMetadata);
    adapter.interfaces = vec!["Lkotlin/jvm/functions/Function2;".into()];
    if shape == Shape::Field {
        adapter.fields.push(DexField {
            declaring_type: ADAPTER.into(),
            name: "capture".into(),
            field_type: OBJECT.into(),
            access_flags: 1,
            is_static: false,
        });
    }
    let parent = class(
        PARENT,
        OBJECT,
        vec![method(
            PARENT,
            "<init>",
            "V",
            &PARAMETERS,
            if shape == Shape::ProtectedParentConstructor {
                0x10004
            } else {
                0x10001
            },
            7,
            7,
            1,
            vec![0x1070, 3, 0, 0x000e],
        )],
    );
    let router = class(
        ROUTER,
        OBJECT,
        vec![method(
            ROUTER,
            "getNavigationEvents",
            EVENTS,
            &[],
            1,
            2,
            1,
            0,
            vec![0x0012, 0x0011],
        )],
    );
    let classes = vec![
        caller,
        adapter,
        parent,
        router,
        class(EVENTS, OBJECT, vec![]),
    ];
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes(classes.iter()).unwrap(),
        ))
        .unwrap();
    classes
}

#[test]
fn erased_function_reference_constructor_keeps_order_links_and_aliases() {
    for shape in [Shape::SameEnclosingMethod, Shape::DifferentEnclosingMethod] {
        let classes = fixture(shape);
        let caller = &classes[0];
        let analysis = MethodAnalysis::build(caller, &caller.methods[0]).unwrap();
        assert!(analysis.constructors().unwrap().bindings[0].owner_retarget_required);
        let output =
            native_java::render_method("sample.Caller", caller, &caller.methods[0]).unwrap();
        let ordered = [
            "new sample.Caller$Callback(",
            "p0.getNavigationEvents()",
            "\"onNavigate()V\"",
            "sample.Router.class",
            "\"onNavigate\"",
            "observe(",
        ];
        let positions = ordered.map(|term| {
            output
                .source
                .find(term)
                .unwrap_or_else(|| panic!("missing {term:?}: {}", output.source))
        });
        assert!(
            positions.windows(2).all(|pos| pos[0] < pos[1]),
            "{}",
            output.source
        );
        assert_eq!(output.source.matches("getNavigationEvents()").count(), 1);
        let returned = output
            .source
            .split_once("return ")
            .unwrap()
            .1
            .split_once(';')
            .unwrap()
            .0;
        assert!(
            output
                .source
                .contains(&format!("observe({returned}, {returned}, "))
        );
        let ctor_link = output.links.iter().find(|link| link.label ==
            "kotlin.jvm.internal.FunctionReferenceImpl.<init>(ILjava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
        ).unwrap();
        let spelling = output
            .source
            .chars()
            .skip(ctor_link.start)
            .take(ctor_link.end - ctor_link.start)
            .collect::<String>();
        assert_eq!(spelling, "sample.Caller$Callback");
        let adapter = native_java::render("sample.Caller$Callback", &classes[1]).unwrap();
        assert!(
            adapter.source.contains(
                "class Caller$Callback extends FunctionReferenceImpl implements Function2"
            )
        );
        assert!(adapter.source.contains("super(p0, p1, p2, p3, p4, p5)"));
    }
}

#[test]
fn incomplete_function_reference_proof_keeps_dex_fallback() {
    for shape in [
        Shape::MissingMetadata,
        Shape::DuplicateMetadata,
        Shape::Field,
        Shape::OtherParent,
        Shape::OtherPrototype,
        Shape::ProtectedParentConstructor,
        Shape::AliasEscape,
        Shape::AliasOverwrite,
        Shape::PartialHandler,
    ] {
        let classes = fixture(shape);
        let caller = &classes[0];
        assert!(
            caller
                .symbols
                .hierarchy
                .get()
                .unwrap()
                .recovered_constructors(ADAPTER)
                .is_empty(),
            "{shape:?}"
        );
        assert!(
            native_java::render_method("sample.Caller", caller, &caller.methods[0]).is_err(),
            "{shape:?}"
        );
    }
}

#[test]
#[ignore = "requires a JDK with switch expression support"]
fn generated_java_preserves_effect_failures_and_object_identity() {
    let classes = fixture(Shape::DifferentEnclosingMethod);
    let caller = native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
        .unwrap()
        .source;
    let adapter = native_java::render("sample.Caller$Callback", &classes[1])
        .unwrap()
        .source;
    let caller = format!(
        r#"package sample;
public class Caller {{
    static final StringBuilder trace = new StringBuilder();
    static final RuntimeException getterFailure = new IllegalStateException("getter");
    static final RuntimeException ctorFailure = new IllegalStateException("constructor");
    static final NavigationEvents events = new NavigationEvents();
    static int mode;
    static Caller$Callback observed;
    public static void onAllocate() {{
        trace.append('A');
        if (mode == 3) throw new IllegalStateException("allocation");
    }}
    public static Class expectedOwner() {{ return Router.class; }}
    public static void onConstruct(Object receiver) {{
        trace.append('C');
        if (receiver != events) throw new AssertionError("constructor argument identity");
        if (mode == 2) throw ctorFailure;
    }}
    static void observe(Caller$Callback first, Caller$Callback second, NavigationEvents value) {{
        trace.append('O');
        if (first != second || value != events) throw new AssertionError("saved aliases");
        observed = first;
    }}
    {caller}
    public static void main(String[] args) {{
        mode = Integer.parseInt(args[0]);
        Caller$Callback returned = null;
        Throwable failure = null;
        try {{ returned = run(new Router()); }} catch (Throwable thrown) {{ failure = thrown; }}
        boolean expected = switch (mode) {{
            case 0 -> failure == null && returned != null && returned == observed && trace.toString().equals("AGCO");
            case 1 -> failure == getterFailure && trace.toString().equals("AG");
            case 2 -> failure == ctorFailure && trace.toString().equals("AGC");
            case 3 -> failure instanceof ExceptionInInitializerError &&
                      failure.getCause().getMessage().equals("allocation") && trace.toString().equals("A");
            default -> false;
        }};
        if (!expected) throw new AssertionError("trace: " + trace, failure);
        System.out.println(trace);
    }}
}}
class NavigationEvents {{}}
class Router {{
    public NavigationEvents getNavigationEvents() {{
        Caller.trace.append('G');
        if (Caller.mode == 1) throw Caller.getterFailure;
        return Caller.events;
    }}
}}
"#
    );
    let files = [
        ("Caller.java", caller.as_str()),
        ("Caller$Callback.java", adapter.as_str()),
        (
            "Function2.java",
            "package kotlin.jvm.functions; public interface Function2 { Object invoke(Object a, Object b); }",
        ),
        (
            "FunctionReferenceImpl.java",
            r#"package kotlin.jvm.internal; public class FunctionReferenceImpl {
                static { sample.Caller.onAllocate(); }
                public FunctionReferenceImpl(int arity, Object receiver, Class owner, String name, String sig, int flags) {
                    if (arity != 2 || owner != sample.Caller.expectedOwner() || !name.equals("onNavigate")
                        || !sig.equals("onNavigate()V") || flags != 0) throw new AssertionError("constructor arguments");
                    sample.Caller.onConstruct(receiver);
                }
            }"#,
        ),
    ];
    let work = tempfile::tempdir().unwrap();
    let mut javac = Command::new("javac");
    javac.current_dir(work.path()).args(["-d", "."]);
    for (path, source) in files {
        fs::write(work.path().join(path), source).unwrap();
        javac.arg(path);
    }
    let compiled = javac.output().unwrap();
    assert!(
        compiled.status.success(),
        "javac failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    for (mode, trace) in [("0", "AGCO"), ("1", "AG"), ("2", "AGC"), ("3", "A")] {
        let run = Command::new("java")
            .current_dir(work.path())
            .args(["-cp", ".", "sample.Caller", mode])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "mode {mode}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(String::from_utf8(run.stdout).unwrap().trim(), trace);
    }
}
