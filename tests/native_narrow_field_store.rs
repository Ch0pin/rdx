use rdx::{
    native_dex::{self, DexField, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(ty: &str, static_field: bool) -> rdx::native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/NarrowStore;".into();
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "store".into();
    method.access_flags = if static_field { 9 } else { 1 };
    method.parameters = vec!["I".into()];
    method.return_type = ty.into();
    let code = method.code.as_mut().unwrap();
    code.registers = if static_field { 1 } else { 2 };
    code.ins = code.registers;
    code.outs = 0;
    let family = match ty {
        "B" => 4,
        "C" => 5,
        "S" => 6,
        _ => panic!(),
    };
    code.instructions = if static_field {
        vec![0x67 + family, 0, 0x60 + family, 0, 0x000f]
    } else {
        vec![0x159 + family, 0, 0x152 + family, 0, 0x010f]
    };
    class.fields = vec![DexField {
        declaring_type: class.descriptor.clone(),
        name: "value".into(),
        field_type: ty.into(),
        access_flags: if static_field { 9 } else { 1 },
        is_static: static_field,
    }];
    class.symbols = Arc::new(DexSymbols {
        types: vec![class.descriptor.clone(), ty.into()],
        strings: vec!["value".into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    class
}

#[test]
fn narrow_field_stores_accept_integral_parameters_in_both_opcode_families() {
    for ty in ["B", "C", "S"] {
        for is_static in [false, true] {
            let c = fixture(ty, is_static);
            let out = native_java::render_method("sample.NarrowStore", &c, &c.methods[0]).unwrap();
            assert!(out.source.contains("p0"), "{}", out.source);
            assert!(
                out.links.iter().any(|l| l.label.contains(".value:")),
                "{:?}",
                out.links
            );
            let mut invalid = fixture(ty, is_static);
            invalid.methods[0].parameters[0] = "Ljava/lang/Object;".into();
            assert!(
                native_java::render_method("sample.NarrowStore", &invalid, &invalid.methods[0])
                    .is_err()
            );
        }
    }
}

#[test]
#[ignore = "requires javac and java; run serially"]
fn jvm_narrow_stores_match_low_bits_for_sign_and_overflow_boundaries() {
    let root = std::env::temp_dir().join(format!("rdx-narrow-store-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    for (ty, java) in [("B", "byte"), ("C", "char"), ("S", "short")] {
        for is_static in [false, true] {
            let c = fixture(ty, is_static);
            let out = native_java::render_method("sample.NarrowStore", &c, &c.methods[0]).unwrap();
            let modifier = if is_static { "static " } else { "" };
            fs::write(root.join("sample/NarrowStore.java"),format!("package sample; public class NarrowStore {{ public {modifier}{java} value; {} public static void main(String[] a) {{ NarrowStore s=new NarrowStore(); for(int n: new int[]{{Integer.MIN_VALUE,-65537,-32769,-129,-128,-1,0,1,127,128,255,256,32767,32768,65535,65536,Integer.MAX_VALUE}}) {{ if(s.store(n)!=({java})n || s.value!=({java})n) throw new AssertionError(n); }} }} }}",out.source)).unwrap();
            let compiled = Command::new("javac")
                .arg(root.join("sample/NarrowStore.java"))
                .output()
                .unwrap();
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let ran = Command::new("java")
                .args(["-cp", root.to_str().unwrap(), "sample.NarrowStore"])
                .output()
                .unwrap();
            assert!(
                ran.status.success(),
                "{}",
                String::from_utf8_lossy(&ran.stderr)
            );
        }
    }
}
