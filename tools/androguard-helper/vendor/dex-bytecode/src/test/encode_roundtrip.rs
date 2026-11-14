//! Encode/decode roundtrip matrix for common formats.

use crate::decoder::decode_one;
use crate::encode::{encode_instruction, IndexResolve};

#[test]
fn encode_decode_matrix() {
    let cases: &[(&str, &str, Option<i32>)] = &[
        ("nop", "", None),
        ("return-void", "", None),
        ("const/4", "v0, 1", None),
        ("const/16", "v1, 100", None),
        ("move", "v0, v1", None),
        ("move-result", "v0", None),
        ("goto", "+02h", Some(2)),
        ("goto/16", "+0010h", Some(0x10)),
        ("if-eqz", "v0, +0004h", Some(4)),
        ("if-eq", "v0, v1, +0004h", Some(4)),
        ("const-string", "v0, string@3", None),
        ("invoke-virtual", "v0, v1, method@2", None),
        ("invoke-static/range", "v0 ... v2, method@1", None),
        ("sget-object", "v0, field@0", None),
        ("new-instance", "v0, type@1", None),
        ("check-cast", "v0, type@1", None),
        ("const/high16", "v0, 0x10000", None),
        ("const-wide/16", "v0, 42", None),
        ("const-wide", "v0, 1234567890", None),
        ("add-int", "v0, v1, v2", None),
        ("add-int/lit8", "v0, v1, 5", None),
        ("invoke-polymorphic", "{v0, v1}, method@2, proto@1", None),
        ("invoke-polymorphic/range", "v0 .. v2 method@2 proto@1", None),
    ];

    for &(mnemonic, operands, branch) in cases {
        let encoded = encode_instruction(mnemonic, operands, &IndexResolve, branch)
            .unwrap_or_else(|e| panic!("encode {mnemonic}: {e}"));
        let decoded = decode_one(&encoded, 0).unwrap_or_else(|e| panic!("decode {mnemonic}: {e}"));
        assert_eq!(
            decoded.mnemonic, mnemonic,
            "mnemonic mismatch for {mnemonic}"
        );
        assert_eq!(
            decoded.length as usize,
            encoded.len(),
            "length mismatch for {mnemonic}"
        );
        let re = encode_instruction(decoded.mnemonic, &decoded.operands, &IndexResolve, branch)
            .unwrap_or_else(|e| panic!("re-encode {mnemonic}: {e}"));
        assert_eq!(re, encoded, "roundtrip bytes for {mnemonic}");
    }
}
