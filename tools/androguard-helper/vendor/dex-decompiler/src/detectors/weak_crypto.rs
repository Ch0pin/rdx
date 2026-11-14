//! Weak crypto: SecretKeySpec / weak Cipher / MessageDigest algorithms,
//! plus insufficient key lengths (RSA ≤1024, AES ≤128) on key generators.

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, VulnFinding};

const KEY_SPEC: &[&str] = &[
    "SecretKeySpec.<init>",
    "IvParameterSpec.<init>",
    "DESKeySpec.<init>",
    "PBEKeySpec.<init>",
];

const ALGO_APIS: &[&str] = &[
    "Cipher.getInstance",
    "MessageDigest.getInstance",
    "SecureRandom.setSeed",
];

const RSA_INIT_APIS: &[&str] = &[
    "KeyPairGenerator.initialize",
    "KeyPairGeneratorSpec.Builder.setKeySize",
    "KeyGenParameterSpec.Builder.setKeySize",
    "setKeySize",
];

const AES_INIT_APIS: &[&str] = &[
    "KeyGenerator.init",
    "KeyGenParameterSpec.Builder.setKeySize",
    "setKeySize",
];

fn method_mentions_weak_algo(owned: &ValueFlowAnalysisOwned) -> bool {
    owned.insn_at.values().any(|s| {
        let u = s.to_uppercase();
        u.contains("\"DES\"")
            || u.contains("\"DESEDE\"")
            || u.contains("\"AES/ECB")
            || u.contains("\"RC4\"")
            || u.contains("\"MD5\"")
            || u.contains("\"SHA-1\"")
            || u.contains("\"SHA1\"")
    })
}

fn insn_blob(owned: &ValueFlowAnalysisOwned) -> String {
    owned
        .insn_at
        .values()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn invoke_blob(owned: &ValueFlowAnalysisOwned) -> String {
    owned
        .invoke_method_map
        .values()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// True if a `const*` literal in the method is exactly one of `sizes`.
fn has_const_size(blob: &str, sizes: &[u32]) -> bool {
    for line in blob.lines() {
        let t = line.trim();
        if !(t.starts_with("const/") || t.starts_with("const ")) {
            continue;
        }
        // e.g. "const/16 v2, 1024"
        if let Some(rhs) = t.rsplit(',').next() {
            let n = rhs.trim().parse::<u32>().ok();
            if let Some(n) = n {
                if sizes.contains(&n) {
                    return true;
                }
            }
        }
    }
    false
}

fn mentions_rsa(blob: &str) -> bool {
    let u = blob.to_ascii_uppercase();
    u.contains("RSA") || u.contains("KEY_ALGORITHM_RSA")
}

fn mentions_aes(blob: &str) -> bool {
    let u = blob.to_ascii_uppercase();
    u.contains("\"AES\"") || u.contains("KEY_ALGORITHM_AES") || u.contains(", \"AES")
}

pub fn scan_weak_crypto(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut findings = invoke_scan(owned, class_name, method_name, "weak_crypto", KEY_SPEC);
    if method_mentions_weak_algo(owned) {
        findings.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "weak_crypto",
            ALGO_APIS,
        ));
    }
    findings.extend(scan_insufficient_key_length(owned, class_name, method_name));
    findings
}

/// RSA key size ≤1024 or AES key size ≤128 co-located with key-init APIs (MASTG-DEMO-0012).
pub fn scan_insufficient_key_length(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let insns = insn_blob(owned);
    let invokes = invoke_blob(owned);
    let mut out = Vec::new();

    let has_kpg = invokes.contains("KeyPairGenerator");
    let has_kg = invokes.contains("KeyGenerator");
    let has_set_key_size = invokes.contains("setKeySize");

    // RSA ≤1024 bits (512/768/1024) with KeyPairGenerator.initialize / setKeySize
    if (has_kpg || (has_set_key_size && mentions_rsa(&insns)))
        && mentions_rsa(&insns)
        && has_const_size(&insns, &[512, 768, 1024])
    {
        out.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "insufficient_key_length",
            RSA_INIT_APIS,
        ));
    }

    // AES ≤128 bits with KeyGenerator.init / setKeySize (MASTG treats 128 as insufficient here)
    if (has_kg || (has_set_key_size && mentions_aes(&insns)))
        && mentions_aes(&insns)
        && has_const_size(&insns, &[64, 128])
    {
        out.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "insufficient_key_length",
            AES_INIT_APIS,
        ));
    }

    // Dedup by sink_offset within this detector
    out.sort_by_key(|f| f.sink_offset);
    out.dedup_by_key(|f| f.sink_offset);
    out
}
