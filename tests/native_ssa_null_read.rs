use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::sync::Arc;
#[allow(dead_code)]
#[path = "../src/native_java/ssa_null_read.rs"]
mod proof;
pub use rdx::{native_ir, native_ssa, native_types};
fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/NullRead;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0238, 4,      // @0 if flag==0 -> @4
                    0x0012, // @2 r0=0
                    0x0228, // @3 goto @5
                    0x0012, // @4 r0=0
                    0x0107, // @5 move-object r1,r0
                    0x0111, // @6 return r1
                ],
            }),
        }],
    }
}
#[test]
fn exact_zero_phi_can_supply_object_move_without_retyping_its_integer_storage() {
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    assert!(proof::at_read(a.instructions(), a.ssa(), &t, 5, 0));
    assert!(!proof::at_read(a.instructions(), a.ssa(), &t, 5, 1));
    native_java::render_method("sample.NullRead", &c, &c.methods[0]).unwrap();
}
#[test]
fn nonzero_actual_integer_undefined_and_wide_inputs_decline() {
    for word in [0x1012, 0x2001, 0, 0x0016] {
        let mut c = fixture();
        let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
        if word == 0x0016 {
            w[3] = 0x0328;
            w[4] = 0x0016;
            w[5] = 0;
            w.insert(6, 0x0107);
        } else {
            w[4] = word;
        }
        let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
        let t = a.infer_types().unwrap();
        assert!(!proof::at_read(
            a.instructions(),
            a.ssa(),
            &t,
            if word == 0x0016 { 6 } else { 5 },
            0
        ));
    }
}
#[test]
fn complete_phi_coverage_and_assignment_domain_are_mandatory() {
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    for shape in 0..4 {
        let mut s = native_ssa::SsaMethod::build(
            c.methods[0].code.as_ref().unwrap(),
            a.instructions(),
            a.blocks(),
        )
        .unwrap();
        let p = s
            .phis
            .iter_mut()
            .find(|p| p.register == 0 && s.graph.blocks[p.block].start == 5)
            .unwrap();
        match shape {
            0 => {
                p.incoming.pop();
            }
            1 => p.incoming.push(p.incoming[0]),
            2 => p.incoming.push((None, p.incoming[0].1)),
            _ => {
                p.incoming[0].1 = p.result;
                p.incoming[1].1 = p.result;
            }
        }
        assert!(!proof::at_read(a.instructions(), &s, &t, 5, 0));
    }
}
