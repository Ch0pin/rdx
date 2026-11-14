//! Static inventory / misuse detectors for MASTG resilience & storage lessons
//! that Frida demos often only observe at runtime (device lock, StrictMode,
//! plaintext prefs, multipurpose keys, anti-Frida maps, emulator checks,
//! hardcoded secrets into Cipher).

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, VulnFinding};

fn insn_blob(owned: &ValueFlowAnalysisOwned) -> String {
    owned
        .insn_at
        .values()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn invoke_blob(owned: &ValueFlowAnalysisOwned) -> String {
    owned
        .invoke_method_map
        .values()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

const SECRET_MARKERS: &[&str] = &[
    "ghp_",
    "akia",
    "sk-",
    "api_key",
    "apikey",
    "secret",
    "password",
    "token",
    "private key",
    "begin private key",
    "presShared",
    "github",
    "aws",
];

fn has_secret_marker(blob: &str) -> bool {
    SECRET_MARKERS.iter().any(|m| blob.contains(&m.to_ascii_lowercase()))
}

/// KeyguardManager.isDeviceSecure / BiometricManager.canAuthenticate inventory.
pub fn scan_device_lock_api(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    invoke_scan(
        owned,
        class_name,
        method_name,
        "device_lock_api_check",
        &[
            "KeyguardManager.isDeviceSecure",
            "isDeviceSecure",
            "BiometricManager.canAuthenticate",
            "canAuthenticate",
        ],
    )
}

/// StrictMode.setVmPolicy / setThreadPolicy (debug leak detection surface).
pub fn scan_strict_mode(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    invoke_scan(
        owned,
        class_name,
        method_name,
        "strict_mode_policy",
        &[
            "StrictMode.setVmPolicy",
            "StrictMode.setThreadPolicy",
            "setVmPolicy",
            "setThreadPolicy",
            "VmPolicy.Builder",
            "ThreadPolicy.Builder",
        ],
    )
}

/// SharedPreferences putString/putStringSet co-located with secret-like constants.
pub fn scan_prefs_plaintext_secret(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let invokes = invoke_blob(owned);
    // EncryptedSharedPreferences is the secure counterpart (MASTG-DEMO-0060).
    if invokes.contains("EncryptedSharedPreferences") {
        return Vec::new();
    }
    let has_put = invokes.contains("putString")
        || invokes.contains("SharedPreferences")
        || owned.insn_at.values().any(|s| {
            let l = s.to_ascii_lowercase();
            l.contains("putstring") || l.contains("putstringset")
        });
    if !has_put {
        return Vec::new();
    }
    let blob = insn_blob(owned);
    if !has_secret_marker(&blob) {
        return Vec::new();
    }
    // If every put is clearly preceded by Cipher in the same method, still flag:
    // demo 0059 mixes encrypted + plaintext puts; presence of plaintext secret
    // markers + putString is enough (Cipher co-occurrence does not clear it).
    invoke_scan(
        owned,
        class_name,
        method_name,
        "prefs_plaintext_secret",
        &[
            "putString",
            "putStringSet",
            "SharedPreferences.Editor.putString",
            "edit",
        ],
    )
}

/// KeyGenParameterSpec combining sign/verify with encrypt/decrypt purposes.
pub fn scan_keystore_multipurpose(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let invokes = invoke_blob(owned);
    let has_kg = invokes.contains("KeyGenParameterSpec")
        || invokes.contains("KeyPairGenerator")
        || insn_blob(owned).contains("keygenparameterspec");
    if !has_kg {
        return Vec::new();
    }
    // Strong static signal: same Builder configures both signature and encryption paddings
    // (MASTG-DEMO-0071/0072 — PURPOSE_SIGN|VERIFY|ENCRYPT|DECRYPT collapsed to int 15).
    let both_paddings = invokes.contains("setSignaturePaddings")
        && invokes.contains("setEncryptionPaddings");
    let cipher_and_sign = invokes.contains("Cipher.") && invokes.contains("Signature.");
    let blob = insn_blob(owned);
    let purpose_strings = (blob.contains("purpose_sign") || blob.contains("purpose_verify"))
        && (blob.contains("purpose_encrypt") || blob.contains("purpose_decrypt"));
    if !(both_paddings || cipher_and_sign || purpose_strings) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "keystore_multipurpose",
        &[
            "KeyGenParameterSpec.Builder",
            "KeyGenParameterSpec$Builder",
            "setSignaturePaddings",
            "setEncryptionPaddings",
            "KeyPairGenerator.initialize",
            "KeyPairGenerator.getInstance",
            "Cipher.getInstance",
            "Signature.getInstance",
        ],
    )
}

/// /proc/self/maps scan looking for frida/gadget (anti-instrumentation).
pub fn scan_anti_frida_maps(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let blob = insn_blob(owned);
    let has_maps = blob.contains("/proc/self/maps") || blob.contains("proc/self/maps");
    let has_frida = blob.contains("frida") || blob.contains("gadget");
    if !(has_maps && has_frida) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "anti_frida_maps",
        &[
            "FileReader",
            "BufferedReader",
            "FileInputStream",
            "Process.killProcess",
            "System.exit",
            "contains",
        ],
    )
}

/// Emulator fingerprinting via Build.* and known emulator package/string markers.
pub fn scan_emulator_detection(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let blob = insn_blob(owned);
    let invokes = invoke_blob(owned);
    let build_reads = [
        "Build.FINGERPRINT",
        "Build.MODEL",
        "Build.MANUFACTURER",
        "Build.PRODUCT",
        "Build.HARDWARE",
        "Build.BOARD",
        "Build.BRAND",
        "Build.DEVICE",
    ]
    .iter()
    .any(|b| blob.contains(&b.to_ascii_lowercase()) || invokes.contains(b));
    let emu_markers = [
        "genymotion",
        "goldfish",
        "ranchu",
        "emulator",
        "google_sdk",
        "sdk_gphone",
        "vbox86",
        "ttvm",
        "nox",
        "bluestacks",
        "microvirt",
        "andy",
        "droid4x",
    ]
    .iter()
    .any(|m| blob.contains(m));
    if !(build_reads && emu_markers) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "emulator_detection",
        &[
            "Build.FINGERPRINT",
            "getPackageInfo",
            "queryIntentActivities",
            "getRunningServices",
            "getLine1Number",
            "getNetworkOperatorName",
            "contains",
            "equals",
        ],
    )
}

/// Hardcoded secret-like const co-located with Cipher.doFinal (hookable plaintext).
pub fn scan_hardcoded_crypto_secret(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let invokes = invoke_blob(owned);
    if !invokes.contains("Cipher") && !invokes.contains("doFinal") {
        return Vec::new();
    }
    let blob = insn_blob(owned);
    if !has_secret_marker(&blob) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "hardcoded_crypto_secret",
        &["Cipher.doFinal", "doFinal", "Cipher.getInstance", "Cipher.init"],
    )
}

/// Bundle for `run_all_detectors`.
pub fn scan_resilience_static(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut out = Vec::new();
    out.extend(scan_device_lock_api(owned, class_name, method_name));
    out.extend(scan_strict_mode(owned, class_name, method_name));
    out.extend(scan_prefs_plaintext_secret(owned, class_name, method_name));
    out.extend(scan_keystore_multipurpose(owned, class_name, method_name));
    out.extend(scan_anti_frida_maps(owned, class_name, method_name));
    out.extend(scan_emulator_detection(owned, class_name, method_name));
    out.extend(scan_hardcoded_crypto_secret(owned, class_name, method_name));
    out
}
