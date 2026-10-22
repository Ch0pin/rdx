//! An overwritten final-field default is removable only before any observation
//! of an exact, non-finalizable, unpublished receiver.
pub use rdx::native_dex;
use rdx::{
    native_dex::{DexClass, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{fs, process::Command, sync::Arc};

#[path = "fixtures/constructor_dead_final_prefix.rs"]
mod fixture_data;
use fixture_data::fixture;

fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    if class.symbols.hierarchy.get().is_none() {
        class
            .symbols
            .hierarchy
            .set(Arc::new(TypeHierarchy::from_classes([class])?))
            .unwrap();
    }
    native_java::render_method("sample.Child", class, &class.methods[0])
}

fn insert_before_overwrite(class: &mut DexClass, inserted: &[u16]) {
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.splice(10..10, inserted.iter().copied());
    let shift = inserted.len() as u32;
    code.try_regions[0].start += shift;
    code.try_regions[0].end += shift;
    code.try_regions[0].catches =
        vec![(Some("Ljava/lang/RuntimeException;".into()), 16 + shift)].into();
}

#[test]
fn unobserved_overwritten_final_default_retains_both_calls_and_one_assignment() {
    let class = fixture();
    let original = class.methods[0].code.as_ref().unwrap().instructions.clone();
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("this.x =").count(), 1, "{source}");
    assert_eq!(
        source.matches("sample.Effects.step(").count(),
        1,
        "{source}"
    );
    assert_eq!(
        source.matches("sample.Effects.touch()").count(),
        1,
        "{source}"
    );
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
    assert_eq!(
        class.methods[0].code.as_ref().unwrap().instructions,
        original
    );
}

#[test]
fn an_observed_or_published_receiver_retains_the_final_assignment_guard() {
    let mut class = fixture();
    insert_before_overwrite(&mut class, &[0x2152, 0, 0x1071, 5, 1]); // record(this.x)
    assert!(render(&class).is_err());
    let mut class = fixture();
    insert_before_overwrite(&mut class, &[0x1071, 4, 2]); // publish(this)
    assert!(render(&class).is_err());
    let mut class = fixture();
    insert_before_overwrite(&mut class, &[0x2107, 0x1071, 4, 1]); // alias then publish
    assert!(render(&class).is_err());
}

#[test]
fn finalizability_and_exact_field_identity_are_mandatory() {
    let mut class = fixture();
    class.access_flags &= !0x10; // subclass finalizers may observe aborted construction
    assert!(render(&class).is_err());
    let mut class = fixture();
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "finalize".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 4,
        code: None,
    });
    assert!(render(&class).is_err());
    let mut class = fixture();
    class.fields[0].access_flags |= 0x40;
    assert!(render(&class).is_err());
    let mut class = fixture();
    class.fields.push(DexField {
        declaring_type: class.descriptor.clone(),
        name: "x".into(),
        field_type: "I".into(),
        access_flags: 0x11,
        is_static: false,
    });
    assert!(render(&class).is_err());
}

#[test]
fn protected_or_alternate_prefix_entries_keep_original_assignment_checks() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[0].start = 4;
    assert!(render(&class).is_err());
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[15] = 0xf628; // normal continuation returns to step before overwrite
    assert!(render(&class).is_err());
}

#[test]
fn shadowed_object_and_overlong_prefixes_keep_the_original_proof() {
    let class = fixture();
    let shadow = DexClass {
        descriptor: "Ljava/lang/Object;".into(),
        superclass: None,
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    };
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &shadow]).unwrap(),
        ))
        .unwrap();
    assert!(render(&class).is_err());
    let mut class = fixture();
    insert_before_overwrite(&mut class, &[0; 128]);
    assert!(render(&class).is_err());
}

#[test]
fn large_single_assignment_constructor_keeps_the_ordinary_rendering_budget() {
    let large = || {
        let mut class = fixture();
        insert_before_overwrite(&mut class, &[0; 4096]);
        // This fixture exercises the method budget independently of the
        // existing, narrower constructor exception-cleanup proof budget.
        let code = class.methods[0].code.as_mut().unwrap();
        code.tries = 0;
        code.try_regions.clear();
        code.instructions.truncate(4096 + 16);
        class
    };
    let class = large();
    assert!(class.methods[0].code.as_ref().unwrap().instructions.len() > 4096);
    // An actual repeated final still requires the bounded strict flow proof.
    assert!(render(&class).is_err());
    let mut class = large();
    class.methods[0].code.as_mut().unwrap().instructions[4..6].fill(0);
    let source = render(&class).unwrap().source;
    assert_eq!(source.matches("this.x =").count(), 1, "{source}");
    assert_eq!(
        source.matches("sample.Effects.step(").count(),
        1,
        "{source}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_dead_default_preserves_calls_fault_identity_and_final_value() {
    let source = render(&fixture()).unwrap().source;
    let dir =
        std::env::temp_dir().join(format!("rdx-constructor-dead-final-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Child.java"), format!(r#"package sample;
public final class Child {{
 public final int x;
 {source}
 public static void main(String[] args) {{
  for(int value=-2;value<4;value++) for(int mode=0;mode<4;mode++) {{
   Effects.mode=mode;Effects.trace="";Effects.caught=null;
   try {{
    Child result=new Child(value);
    if(mode==1 || mode==3 || result.x!=value+10)throw new AssertionError("final value");
   }} catch(Error actual) {{
    if(mode!=3 || actual!=Effects.unmatched)throw actual;
   }} catch(RuntimeException actual) {{
    if((mode!=1 && mode!=3) || actual!=Effects.fault)throw new AssertionError("fault identity",actual);
   }}
   String expected=mode==1?"step;":mode==2?"step;touch;caught;":"step;touch;";
   if(!Effects.trace.equals(expected))throw new AssertionError(Effects.trace);
   if((mode==2)!=(Effects.caught==Effects.fault))throw new AssertionError("caught identity");
  }}
 }}
}}
class Effects {{
 static int mode;static String trace;static Throwable caught;
 static final RuntimeException fault=new RuntimeException("sentinel");
 static final Error unmatched=new Error("unmatched");
 static int step(int value){{trace+="step;";if(mode==1)throw fault;return value+10;}}
 static void touch(){{trace+="touch;";if(mode==2)throw fault;if(mode==3)throw unmatched;}}
 static void caught(Throwable actual){{trace+="caught;";caught=actual;}}
}}
"#)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Child.java"]),
        ("java", vec!["-cp", ".", "sample.Child"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
