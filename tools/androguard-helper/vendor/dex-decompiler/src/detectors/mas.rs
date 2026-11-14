//! OWASP MAS enrichment for vuln findings (MASWE → MASVS → MASTG KNOW/BEST).
//!
//! Attached natively on [`crate::detectors::VulnFinding`] so WASM / androhunt / CLI
//! consumers share the same mapping (parity with droid2web `web/maswe.js`).
//!
//! Control URLs: <https://mas.owasp.org/MASVS/controls/MASVS-XXXX-N/>
//! Weaknesses: <https://mas.owasp.org/MASWE/>

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// One MAS catalog link (MASWE / MASVS control / MASTG KNOW or BEST).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasLink {
    pub id: String,
    pub title: String,
    /// Family or category, e.g. `MASVS-PLATFORM`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub family: String,
    pub url: String,
}

/// Resolved MAS chain for a finding.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MasEnrichment {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maswe: Vec<MasLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masvs: Vec<MasLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_know: Vec<MasLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_best: Vec<MasLink>,
}

fn maswe_url(id: &str, family: &str) -> String {
    format!("https://mas.owasp.org/MASWE/{family}/{id}/")
}
fn masvs_control_url(id: &str) -> String {
    format!("https://mas.owasp.org/MASVS/controls/{id}/")
}
fn mastg_url(id: &str) -> String {
    format!("https://mas.owasp.org/{id}")
}

fn maswe_catalog() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("MASWE-0001", "Sensitive Data Stored Unencrypted in Private Storage", "MASVS-STORAGE"),
        ("MASWE-0002", "Sensitive Data Stored Unencrypted Outside of Private Storage", "MASVS-STORAGE"),
        ("MASWE-0003", "Cryptographic Keys Stored Outside of Platform Keystore", "MASVS-STORAGE"),
        ("MASWE-0004", "Sensitive Data Hardcoded in the App Package", "MASVS-STORAGE"),
        ("MASWE-0005", "Insertion of Sensitive Data into Logs", "MASVS-STORAGE"),
        ("MASWE-0006", "Sensitive Data Not Excluded From Backup", "MASVS-STORAGE"),
        ("MASWE-0007", "Improper Encryption", "MASVS-CRYPTO"),
        ("MASWE-0008", "Improper Hashing", "MASVS-CRYPTO"),
        ("MASWE-0009", "Improper Use of Message Authentication Code (MAC)", "MASVS-CRYPTO"),
        ("MASWE-0010", "Improper Generation of Cryptographic Signatures", "MASVS-CRYPTO"),
        ("MASWE-0011", "Improper Verification of Cryptographic Signature", "MASVS-CRYPTO"),
        ("MASWE-0012", "Improper Random Number Generation", "MASVS-CRYPTO"),
        ("MASWE-0013", "Improper Cryptographic Key Generation", "MASVS-CRYPTO"),
        ("MASWE-0014", "Improper Cryptographic Key Derivation", "MASVS-CRYPTO"),
        ("MASWE-0015", "Cryptographic Key Rotation Not Implemented", "MASVS-CRYPTO"),
        ("MASWE-0016", "Cryptographic Key Access Not Restricted", "MASVS-CRYPTO"),
        ("MASWE-0017", "Device Secure Lock Not Enforced", "MASVS-CRYPTO"),
        ("MASWE-0018", "Lack of Authentication or Authorization on App Components", "MASVS-AUTH"),
        ("MASWE-0019", "Lack of Auto-fill Support for Credential Providers", "MASVS-AUTH"),
        ("MASWE-0020", "Local Authentication Can Be Bypassed", "MASVS-AUTH"),
        ("MASWE-0021", "Fallback to Non-biometric Credentials Allowed for Sensitive Transactions", "MASVS-AUTH"),
        ("MASWE-0022", "Crypto Keys Not Invalidated on New Biometric Enrollment", "MASVS-AUTH"),
        ("MASWE-0023", "Step-Up Authentication Not Implemented for Sensitive Actions", "MASVS-AUTH"),
        ("MASWE-0024", "Sensitive Data Accessible After Session Termination", "MASVS-AUTH"),
        ("MASWE-0025", "Lack of Non-Repudiation for Critical Actions", "MASVS-AUTH"),
        ("MASWE-0026", "Network Traffic Not Encrypted", "MASVS-NETWORK"),
        ("MASWE-0027", "Insecure Certificate Validation", "MASVS-NETWORK"),
        ("MASWE-0028", "Insecure Identity Pinning", "MASVS-NETWORK"),
        ("MASWE-0029", "Insecure Deep Links", "MASVS-PLATFORM"),
        ("MASWE-0030", "Improper Use of the Clipboard", "MASVS-PLATFORM"),
        ("MASWE-0031", "Allowing Untrusted App Extensions", "MASVS-PLATFORM"),
        ("MASWE-0032", "Insecure Intents", "MASVS-PLATFORM"),
        ("MASWE-0033", "Sensitive Native Functionality Exposed in WebViews", "MASVS-PLATFORM"),
        ("MASWE-0034", "WebViews Allow Access to Local Resources with Untrusted Content", "MASVS-PLATFORM"),
        ("MASWE-0035", "WebViews Loading Untrusted Content", "MASVS-PLATFORM"),
        ("MASWE-0036", "Unnecessary Exposure of Sensitive Data via the User Interface", "MASVS-PLATFORM"),
        ("MASWE-0037", "Unnecessary Exposure of Sensitive Data via Notifications", "MASVS-PLATFORM"),
        ("MASWE-0038", "Insufficient Protection of Sensitive Data from Screenshots or Screen Recordings", "MASVS-PLATFORM"),
        ("MASWE-0039", "App Vulnerable to Overlay Attacks", "MASVS-PLATFORM"),
        ("MASWE-0040", "Sensitive Data Leaked via Accessibility Services", "MASVS-PLATFORM"),
        ("MASWE-0041", "Running on a Recent Platform Version Not Ensured", "MASVS-CODE"),
        ("MASWE-0042", "Latest Platform Version Not Targeted", "MASVS-CODE"),
        ("MASWE-0043", "Enforced Updating Not Implemented", "MASVS-CODE"),
        ("MASWE-0044", "Dependencies with Known Vulnerabilities", "MASVS-CODE"),
        ("MASWE-0045", "Compiler-Provided Security Features Not Used", "MASVS-CODE"),
        ("MASWE-0046", "Use of Deprecated APIs or Functionality", "MASVS-CODE"),
        ("MASWE-0047", "Using Non-Standard APIs for Security-Critical Functionality", "MASVS-CODE"),
        ("MASWE-0048", "Malicious Code Included in the App", "MASVS-CODE"),
        ("MASWE-0049", "Unsafe Dynamic Code Loading", "MASVS-CODE"),
        ("MASWE-0050", "Unsafe Handling of Untrusted Data", "MASVS-CODE"),
        ("MASWE-0051", "Root/Jailbreak Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0052", "App Virtualization Environment Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0053", "Emulated or Virtual Device Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0054", "Device Attestation Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0055", "Malware Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0056", "App Attestation Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0057", "App Resources Integrity Not Verified", "MASVS-RESILIENCE"),
        ("MASWE-0058", "Runtime Code Integrity Not Verified", "MASVS-RESILIENCE"),
        ("MASWE-0059", "Code Obfuscation Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0060", "Resource Obfuscation Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0061", "Debug Artifacts Not Removed", "MASVS-RESILIENCE"),
        ("MASWE-0062", "No Application-Level Payload Encryption", "MASVS-RESILIENCE"),
        ("MASWE-0063", "Debug Mechanisms Not Disabled", "MASVS-RESILIENCE"),
        ("MASWE-0064", "Debugger Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0065", "Dynamic Analysis Tools Detection Not Implemented", "MASVS-RESILIENCE"),
        ("MASWE-0066", "Inadequate Permission Management", "MASVS-PRIVACY"),
        ("MASWE-0067", "Lack of Anonymization or Pseudonymisation Measures", "MASVS-PRIVACY"),
        ("MASWE-0068", "Incorrect Use of Identifiers for User Tracking", "MASVS-PRIVACY"),
        ("MASWE-0069", "Usage of Non-Privacy-Preserving Functionality", "MASVS-PRIVACY"),
        ("MASWE-0070", "Inadequate Awareness for Privacy Relevant Actions", "MASVS-PRIVACY"),
        ("MASWE-0071", "Inadequate Defaults for Privacy Relevant Actions", "MASVS-PRIVACY"),
        ("MASWE-0072", "Inadequate Privacy Policy", "MASVS-PRIVACY"),
        ("MASWE-0073", "Inadequate Data Collection Declarations", "MASVS-PRIVACY"),
        ("MASWE-0074", "Inadequate Tracking Domains Declarations", "MASVS-PRIVACY"),
        ("MASWE-0075", "Non-Reproducible Builds", "MASVS-PRIVACY"),
        ("MASWE-0076", "Lack of Proper Data Management Controls", "MASVS-PRIVACY"),
        ("MASWE-0077", "Inadequate Data Visibility Controls", "MASVS-PRIVACY"),
        ("MASWE-0078", "Inadequate or Ambiguous User Consent Mechanisms", "MASVS-PRIVACY"),
    ]
}

fn know_catalog() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("MASTG-KNOW-0001", "Biometric Authentication", "MASVS-AUTH"),
        ("MASTG-KNOW-0002", "FingerprintManager", "MASVS-AUTH"),
        ("MASTG-KNOW-0003", "App Signing", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0004", "Third-Party Libraries", "MASVS-CODE"),
        ("MASTG-KNOW-0005", "Memory Corruption Bugs", "MASVS-CODE"),
        ("MASTG-KNOW-0006", "Binary Protection Mechanisms", "MASVS-CODE"),
        ("MASTG-KNOW-0007", "Debuggable Apps", "MASVS-CODE"),
        ("MASTG-KNOW-0008", "Debugging Information and Debug Symbols", "MASVS-CODE"),
        ("MASTG-KNOW-0009", "StrictMode", "MASVS-CODE"),
        ("MASTG-KNOW-0010", "Exception Handling", "MASVS-CODE"),
        ("MASTG-KNOW-0011", "Security Provider", "MASVS-CRYPTO"),
        ("MASTG-KNOW-0012", "Key Generation", "MASVS-CRYPTO"),
        ("MASTG-KNOW-0013", "Random Number Generation", "MASVS-CRYPTO"),
        ("MASTG-KNOW-0014", "Android Network Security Configuration", "MASVS-NETWORK"),
        ("MASTG-KNOW-0015", "Certificate Pinning", "MASVS-NETWORK"),
        ("MASTG-KNOW-0017", "App Permissions", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0018", "WebViews", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0019", "Deep Links", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0020", "Inter-Process Communication (IPC) Mechanisms", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0021", "Object Serialization", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0022", "Overlay Attacks", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0023", "Enforced Updating", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0024", "Pending Intents", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0025", "Explicit vs Implicit Intents", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0026", "Third-party Services Embedded in the App", "MASVS-STORAGE"),
        ("MASTG-KNOW-0027", "Root Detection", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0028", "Anti-Debugging", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0029", "File Integrity Checks", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0030", "Reverse Engineering Tool Detection", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0031", "Emulator Detection", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0032", "Runtime Integrity Verification", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0033", "Obfuscation", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0034", "Device Binding", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0035", "Google Play Integrity API", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0036", "Shared Preferences", "MASVS-STORAGE"),
        ("MASTG-KNOW-0037", "SQLite Database", "MASVS-STORAGE"),
        ("MASTG-KNOW-0038", "SQLCipher Database", "MASVS-STORAGE"),
        ("MASTG-KNOW-0039", "Firebase Real-time Databases", "MASVS-STORAGE"),
        ("MASTG-KNOW-0040", "Realm Databases", "MASVS-STORAGE"),
        ("MASTG-KNOW-0041", "Internal Storage", "MASVS-STORAGE"),
        ("MASTG-KNOW-0042", "External Storage", "MASVS-STORAGE"),
        ("MASTG-KNOW-0043", "Android KeyStore", "MASVS-STORAGE"),
        ("MASTG-KNOW-0044", "Key Attestation", "MASVS-STORAGE"),
        ("MASTG-KNOW-0045", "Secure Key Import into Keystore", "MASVS-STORAGE"),
        ("MASTG-KNOW-0047", "Cryptographic Key Storage", "MASVS-STORAGE"),
        ("MASTG-KNOW-0048", "KeyChain", "MASVS-STORAGE"),
        ("MASTG-KNOW-0049", "Logs", "MASVS-STORAGE"),
        ("MASTG-KNOW-0050", "Backups", "MASVS-STORAGE"),
        ("MASTG-KNOW-0051", "Process Memory", "MASVS-STORAGE"),
        ("MASTG-KNOW-0052", "User Interface Components", "MASVS-STORAGE"),
        ("MASTG-KNOW-0053", "Screenshots", "MASVS-STORAGE"),
        ("MASTG-KNOW-0054", "App Notifications", "MASVS-STORAGE"),
        ("MASTG-KNOW-0055", "Keyboard Cache", "MASVS-STORAGE"),
        ("MASTG-KNOW-0117", "Android ContentProvider", "MASVS-CODE"),
        ("MASTG-KNOW-0118", "Runtime Application Self-Protection (RASP)", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0132", "Android Activities", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0133", "Android Services", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0134", "Android Broadcast Receivers", "MASVS-PLATFORM"),
        ("MASTG-KNOW-0135", "Virtual Devices Detection", "MASVS-RESILIENCE"),
        ("MASTG-KNOW-0138", "URI Schemes in Android Intent Results", "MASVS-CODE"),
        ("MASTG-KNOW-0142", "Android DataStore", "MASVS-STORAGE"),
    ]
}

fn best_catalog() -> &'static [(&'static str, &'static str)] {
    &[
        ("MASTG-BEST-0001", "Use Secure Random Number Generator APIs"),
        ("MASTG-BEST-0002", "Remove Logging Code"),
        ("MASTG-BEST-0004", "Exclude Sensitive Data from Backups"),
        ("MASTG-BEST-0005", "Use Secure Encryption Modes"),
        ("MASTG-BEST-0007", "Debuggable Flag Disabled in the AndroidManifest"),
        ("MASTG-BEST-0008", "Debugging Disabled for WebViews"),
        ("MASTG-BEST-0009", "Use Secure Encryption Algorithms"),
        ("MASTG-BEST-0010", "Use Up-to-Date minSdkVersion"),
        ("MASTG-BEST-0011", "Securely Load File Content in a WebView"),
        ("MASTG-BEST-0012", "Disable JavaScript in WebViews"),
        ("MASTG-BEST-0013", "Disable Content Provider Access in WebViews"),
        ("MASTG-BEST-0014", "Preventing Screenshots and Screen Recording"),
        ("MASTG-BEST-0016", "Use SECURE_FLAG to Prevent Screenshots and Screen Recording"),
        ("MASTG-BEST-0019", "Use Non-Caching Input Types for Sensitive Fields"),
        ("MASTG-BEST-0020", "Update the GMS Security Provider"),
        ("MASTG-BEST-0022", "Disable Verbose and Debug Logging in Production Builds"),
        ("MASTG-BEST-0023", "Exclude Sensitive Information from Backups"),
        ("MASTG-BEST-0024", "Store Data Encrypted in App Sandbox Directory"),
        ("MASTG-BEST-0026", "Preventing Keyboard Caching for Sensitive Text Inputs"),
        ("MASTG-BEST-0027", "Preventing Sensitive Data Exposure in Notifications"),
        ("MASTG-BEST-0029", "Implementing Resilience and RASP Signals"),
        ("MASTG-BEST-0030", "Implementing Root Detection"),
        ("MASTG-BEST-0031", "Enforce Strong Biometrics for Sensitive Operations"),
        ("MASTG-BEST-0036", "Use Cryptographic Binding for Biometric Authentication"),
        ("MASTG-BEST-0037", "Invalidate Biometric Keys on Enrollment Changes"),
        ("MASTG-BEST-0038", "Require Explicit User Confirmation for Biometric Authentication"),
        ("MASTG-BEST-0039", "Prevent SQL Injection in ContentProviders"),
        ("MASTG-BEST-0040", "Preventing Overlay Attacks"),
    ]
}

fn maswe_to_masvs(id: &str) -> &'static [&'static str] {
    match id {
        "MASWE-0001" => &["MASVS-STORAGE-1", "MASVS-STORAGE-2", "MASVS-CRYPTO-2"],
        "MASWE-0002" => &["MASVS-STORAGE-1", "MASVS-STORAGE-2"],
        "MASWE-0003" => &["MASVS-STORAGE-1", "MASVS-CRYPTO-2"],
        "MASWE-0004" => &["MASVS-STORAGE-1"],
        "MASWE-0005" => &["MASVS-STORAGE-2"],
        "MASWE-0006" => &["MASVS-STORAGE-2"],
        "MASWE-0007" => &["MASVS-CRYPTO-1", "MASVS-CRYPTO-2"],
        "MASWE-0008" => &["MASVS-CRYPTO-1"],
        "MASWE-0009" => &["MASVS-CRYPTO-1", "MASVS-CRYPTO-2"],
        "MASWE-0010" => &["MASVS-CRYPTO-1", "MASVS-CRYPTO-2"],
        "MASWE-0011" => &["MASVS-CRYPTO-1"],
        "MASWE-0012" => &["MASVS-CRYPTO-1"],
        "MASWE-0013" => &["MASVS-CRYPTO-2"],
        "MASWE-0014" => &["MASVS-CRYPTO-2"],
        "MASWE-0015" => &["MASVS-CRYPTO-2"],
        "MASWE-0016" => &["MASVS-CRYPTO-2", "MASVS-AUTH-2", "MASVS-AUTH-3"],
        "MASWE-0017" => &["MASVS-CRYPTO-2"],
        "MASWE-0018" => &["MASVS-AUTH-1", "MASVS-PLATFORM-1", "MASVS-STORAGE-2"],
        "MASWE-0019" => &["MASVS-AUTH-1", "MASVS-AUTH-3"],
        "MASWE-0020" => &["MASVS-AUTH-2", "MASVS-CRYPTO-2"],
        "MASWE-0021" => &["MASVS-AUTH-2"],
        "MASWE-0022" => &["MASVS-AUTH-2", "MASVS-CRYPTO-2"],
        "MASWE-0023" => &["MASVS-AUTH-3", "MASVS-PLATFORM-3"],
        "MASWE-0024" => &["MASVS-AUTH-3"],
        "MASWE-0025" => &["MASVS-AUTH-3"],
        "MASWE-0026" => &["MASVS-NETWORK-1"],
        "MASWE-0027" => &["MASVS-NETWORK-1"],
        "MASWE-0028" => &["MASVS-NETWORK-2"],
        "MASWE-0029" => &["MASVS-PLATFORM-1", "MASVS-STORAGE-2", "MASVS-CODE-4"],
        "MASWE-0030" => &["MASVS-PLATFORM-1", "MASVS-STORAGE-2"],
        "MASWE-0031" => &["MASVS-PLATFORM-1", "MASVS-STORAGE-2"],
        "MASWE-0032" => &["MASVS-PLATFORM-1", "MASVS-STORAGE-2"],
        "MASWE-0033" => &["MASVS-PLATFORM-2", "MASVS-STORAGE-2"],
        "MASWE-0034" => &["MASVS-PLATFORM-2", "MASVS-STORAGE-2", "MASVS-CODE-4"],
        "MASWE-0035" => &["MASVS-PLATFORM-2", "MASVS-CODE-4"],
        "MASWE-0036" => &["MASVS-PLATFORM-3", "MASVS-STORAGE-2"],
        "MASWE-0037" => &["MASVS-PLATFORM-3", "MASVS-STORAGE-2"],
        "MASWE-0038" => &["MASVS-PLATFORM-3", "MASVS-STORAGE-2"],
        "MASWE-0039" => &["MASVS-PLATFORM-3", "MASVS-CODE-1"],
        "MASWE-0040" => &["MASVS-PLATFORM-3", "MASVS-STORAGE-2"],
        "MASWE-0041" => &["MASVS-CODE-1"],
        "MASWE-0042" => &["MASVS-CODE-1"],
        "MASWE-0043" => &["MASVS-CODE-2"],
        "MASWE-0044" => &["MASVS-CODE-3"],
        "MASWE-0045" => &["MASVS-CODE-3", "MASVS-CODE-4"],
        "MASWE-0046" => &["MASVS-CODE-3", "MASVS-CRYPTO-2"],
        "MASWE-0047" => &["MASVS-CODE-3", "MASVS-AUTH-1", "MASVS-CRYPTO-1", "MASVS-NETWORK-1"],
        "MASWE-0048" => &["MASVS-CODE-3"],
        "MASWE-0049" => &["MASVS-CODE-4"],
        "MASWE-0050" => &["MASVS-CODE-4"],
        "MASWE-0051" => &["MASVS-RESILIENCE-1", "MASVS-RESILIENCE-4"],
        "MASWE-0052" => &["MASVS-RESILIENCE-1"],
        "MASWE-0053" => &["MASVS-RESILIENCE-1", "MASVS-RESILIENCE-4"],
        "MASWE-0054" => &["MASVS-RESILIENCE-1"],
        "MASWE-0055" => &["MASVS-RESILIENCE-2"],
        "MASWE-0056" => &["MASVS-RESILIENCE-2"],
        "MASWE-0057" => &["MASVS-RESILIENCE-2", "MASVS-CODE-4"],
        "MASWE-0058" => &["MASVS-RESILIENCE-2"],
        "MASWE-0059" => &["MASVS-RESILIENCE-3"],
        "MASWE-0060" => &["MASVS-RESILIENCE-3"],
        "MASWE-0061" => &["MASVS-RESILIENCE-3"],
        "MASWE-0062" => &["MASVS-RESILIENCE-3", "MASVS-NETWORK-1"],
        "MASWE-0063" => &["MASVS-RESILIENCE-4", "MASVS-PLATFORM-2"],
        "MASWE-0064" => &["MASVS-RESILIENCE-4"],
        "MASWE-0065" => &["MASVS-RESILIENCE-4"],
        "MASWE-0066" => &["MASVS-PRIVACY-1"],
        "MASWE-0067" => &["MASVS-PRIVACY-2"],
        "MASWE-0068" => &["MASVS-PRIVACY-2"],
        "MASWE-0069" => &["MASVS-PRIVACY-2"],
        "MASWE-0070" => &["MASVS-PRIVACY-2"],
        "MASWE-0071" => &["MASVS-PRIVACY-2"],
        "MASWE-0072" => &["MASVS-PRIVACY-3"],
        "MASWE-0073" => &["MASVS-PRIVACY-3", "MASVS-PRIVACY-1"],
        "MASWE-0074" => &["MASVS-PRIVACY-3"],
        "MASWE-0075" => &["MASVS-PRIVACY-3"],
        "MASWE-0076" => &["MASVS-PRIVACY-4"],
        "MASWE-0077" => &["MASVS-PRIVACY-4"],
        "MASWE-0078" => &["MASVS-PRIVACY-4"],
        _ => &[],
    }
}

fn maswe_to_know(id: &str) -> &'static [&'static str] {
    match id {
        "MASWE-0001" => &["MASTG-KNOW-0036", "MASTG-KNOW-0041", "MASTG-KNOW-0142"],
        "MASWE-0002" => &["MASTG-KNOW-0042"],
        "MASWE-0003" => &["MASTG-KNOW-0043", "MASTG-KNOW-0047"],
        "MASWE-0004" => &["MASTG-KNOW-0047", "MASTG-KNOW-0012"],
        "MASWE-0005" => &["MASTG-KNOW-0049"],
        "MASWE-0006" => &["MASTG-KNOW-0050"],
        "MASWE-0007" => &["MASTG-KNOW-0012", "MASTG-KNOW-0011"],
        "MASWE-0009" => &["MASTG-KNOW-0012"],
        "MASWE-0012" => &["MASTG-KNOW-0013"],
        "MASWE-0013" => &["MASTG-KNOW-0012", "MASTG-KNOW-0043"],
        "MASWE-0016" => &["MASTG-KNOW-0043", "MASTG-KNOW-0001"],
        "MASWE-0017" => &["MASTG-KNOW-0001"],
        "MASWE-0018" => &["MASTG-KNOW-0020", "MASTG-KNOW-0117", "MASTG-KNOW-0132"],
        "MASWE-0020" => &["MASTG-KNOW-0001"],
        "MASWE-0021" => &["MASTG-KNOW-0001"],
        "MASWE-0022" => &["MASTG-KNOW-0001", "MASTG-KNOW-0043"],
        "MASWE-0027" => &["MASTG-KNOW-0014", "MASTG-KNOW-0015"],
        "MASWE-0028" => &["MASTG-KNOW-0015"],
        "MASWE-0029" => &["MASTG-KNOW-0019"],
        "MASWE-0032" => &["MASTG-KNOW-0020", "MASTG-KNOW-0025", "MASTG-KNOW-0024"],
        "MASWE-0033" => &["MASTG-KNOW-0018"],
        "MASWE-0034" => &["MASTG-KNOW-0018"],
        "MASWE-0035" => &["MASTG-KNOW-0018"],
        "MASWE-0036" => &["MASTG-KNOW-0052", "MASTG-KNOW-0055"],
        "MASWE-0037" => &["MASTG-KNOW-0054"],
        "MASWE-0038" => &["MASTG-KNOW-0053"],
        "MASWE-0039" => &["MASTG-KNOW-0022"],
        "MASWE-0041" => &["MASTG-KNOW-0023"],
        "MASWE-0042" => &["MASTG-KNOW-0023"],
        "MASWE-0049" => &["MASTG-KNOW-0005"],
        "MASWE-0050" => &["MASTG-KNOW-0021", "MASTG-KNOW-0117"],
        "MASWE-0051" => &["MASTG-KNOW-0027"],
        "MASWE-0053" => &["MASTG-KNOW-0031", "MASTG-KNOW-0135"],
        "MASWE-0058" => &["MASTG-KNOW-0032", "MASTG-KNOW-0118"],
        "MASWE-0061" => &["MASTG-KNOW-0007", "MASTG-KNOW-0008"],
        "MASWE-0063" => &["MASTG-KNOW-0007", "MASTG-KNOW-0009"],
        "MASWE-0064" => &["MASTG-KNOW-0028"],
        "MASWE-0066" => &["MASTG-KNOW-0017"],
        _ => &[],
    }
}

fn maswe_to_best(id: &str) -> &'static [&'static str] {
    match id {
        "MASWE-0001" => &["MASTG-BEST-0024"],
        "MASWE-0002" => &["MASTG-BEST-0024"],
        "MASWE-0005" => &["MASTG-BEST-0002", "MASTG-BEST-0022"],
        "MASWE-0006" => &["MASTG-BEST-0004", "MASTG-BEST-0023"],
        "MASWE-0007" => &["MASTG-BEST-0005", "MASTG-BEST-0009"],
        "MASWE-0009" => &["MASTG-BEST-0005"],
        "MASWE-0012" => &["MASTG-BEST-0001"],
        "MASWE-0013" => &["MASTG-BEST-0009"],
        "MASWE-0016" => &["MASTG-BEST-0031", "MASTG-BEST-0036"],
        "MASWE-0017" => &["MASTG-BEST-0031"],
        "MASWE-0020" => &["MASTG-BEST-0036"],
        "MASWE-0021" => &["MASTG-BEST-0031"],
        "MASWE-0022" => &["MASTG-BEST-0037"],
        "MASWE-0033" => &["MASTG-BEST-0012"],
        "MASWE-0034" => &["MASTG-BEST-0011", "MASTG-BEST-0013"],
        "MASWE-0035" => &["MASTG-BEST-0008", "MASTG-BEST-0012"],
        "MASWE-0036" => &["MASTG-BEST-0019", "MASTG-BEST-0026"],
        "MASWE-0037" => &["MASTG-BEST-0027"],
        "MASWE-0038" => &["MASTG-BEST-0014", "MASTG-BEST-0016"],
        "MASWE-0039" => &["MASTG-BEST-0040"],
        "MASWE-0041" => &["MASTG-BEST-0010"],
        "MASWE-0042" => &["MASTG-BEST-0010"],
        "MASWE-0050" => &["MASTG-BEST-0039"],
        "MASWE-0051" => &["MASTG-BEST-0030"],
        "MASWE-0053" => &["MASTG-BEST-0029"],
        "MASWE-0058" => &["MASTG-BEST-0029"],
        "MASWE-0061" => &["MASTG-BEST-0007"],
        "MASWE-0063" => &["MASTG-BEST-0007"],
        "MASWE-0064" => &["MASTG-BEST-0029"],
        _ => &[],
    }
}

fn category_maswe(category: &str) -> &'static [&'static str] {
    let cat = category
        .strip_prefix("semgrep:")
        .or_else(|| category.strip_prefix("taint:"))
        .unwrap_or(category);
    match cat {
        "biometric_without_crypto" => &["MASWE-0020"],
        "broadcast_intent_redirect" => &["MASWE-0032"],
        "command_receiver" => &["MASWE-0018"],
        "credential_broadcast" => &["MASWE-0032"],
        "custom_tabs_intent_url" | "CustomTabsLaunch" => &["MASWE-0035"],
        "deeplink_webview_js_bridge" => &["MASWE-0029", "MASWE-0033"],
        "exported_receiver_intent_redirect" => &["MASWE-0032"],
        "hardcoded_secrets_review" => &["MASWE-0004"],
        "icc_extra_flow" => &["MASWE-0032"],
        "implicit_intent_launch" => &["MASWE-0032"],
        "implicit_intent_sensitive" => &["MASWE-0032"],
        "insecure_logging" | "Logging" => &["MASWE-0005"],
        "intent_parse_uri_redirect" => &["MASWE-0032"],
        "intent_redirect_grant_smuggle" => &["MASWE-0032"],
        "intent_redirect_nested" | "NestedIntent" => &["MASWE-0032"],
        "intent_redirect_no_sanitizer" => &["MASWE-0032"],
        "intent_spoofing" => &["MASWE-0032"],
        "ipc_intent_validation" => &["MASWE-0032", "MASWE-0018"],
        "keystore_no_user_auth" => &["MASWE-0016"],
        "logging_pii" => &["MASWE-0005"],
        "path_traversal" | "FileWrite" | "FileRead" => &["MASWE-0050"],
        "pending_intent" => &["MASWE-0032"],
        "pending_intent_pitracker" => &["MASWE-0032"],
        "pick_file_theft" => &["MASWE-0002"],
        "pinning_bypass" => &["MASWE-0028"],
        "provider_path_traversal" => &["MASWE-0050"],
        "provider_sql_injection" => &["MASWE-0050"],
        "rce_dynamic_loading" | "CodeExecution" => &["MASWE-0049"],
        "rce_process_exec" => &["MASWE-0049"],
        "reflection_rce" => &["MASWE-0049"],
        "room_sql_injection" => &["MASWE-0050"],
        "sensitive_broadcast" => &["MASWE-0032"],
        "sql_injection" | "SqlQuery" => &["MASWE-0050"],
        "sqlcipher_hardcoded_passphrase" => &["MASWE-0003", "MASWE-0007"],
        "ssl_bypass_webview_colocate" => &["MASWE-0027", "MASWE-0035"],
        "ssl_trust_all" | "SslBypass" => &["MASWE-0027"],
        "sticky_ordered_broadcast" => &["MASWE-0032"],
        "tracker_fingerprint_api" => &["MASWE-0068"],
        "unsafe_deserialization" => &["MASWE-0050"],
        "uri_permission_grant" | "UriGrant" => &["MASWE-0032"],
        "weak_crypto" => &["MASWE-0007"],
        "insufficient_key_length" => &["MASWE-0013", "MASWE-0007"],
        "insecure_random" | "non_random_source" => &["MASWE-0012"],
        "external_storage_write" => &["MASWE-0002"],
        "explicit_security_provider" => &["MASWE-0007"],
        "notification_sensitive" => &["MASWE-0037"],
        "safebrowsing_disabled" => &["MASWE-0035"],
        "flag_secure" => &["MASWE-0038"],
        "debugger_check" | "tracerpid_check" => &["MASWE-0064"],
        "root_detection" | "native_root_detection" => &["MASWE-0051"],
        "deeplink_query_unvalidated" => &["MASWE-0029"],
        "overlay_protection_api" | "system_alert_window" => &["MASWE-0039"],
        "network_pii" => &["MASWE-0067", "MASWE-0026"],
        "ui_password_cache" | "compose_password_visible" => &["MASWE-0036"],
        "ssl_socket_no_hostname" => &["MASWE-0027"],
        "sdk_int_check" => &["MASWE-0041"],
        "storage_integrity_hmac" => &["MASWE-0009"],
        "allow_backup" => &["MASWE-0006"],
        "exported_custom_action" => &["MASWE-0032"],
        "manifest_debuggable" => &["MASWE-0061"],
        "dangerous_permission" => &["MASWE-0066"],
        "network_security_config_user_ca" => &["MASWE-0027"],
        "device_lock_api_check" => &["MASWE-0017"],
        "strict_mode_policy" => &["MASWE-0063"],
        "prefs_plaintext_secret" => &["MASWE-0001"],
        "keystore_multipurpose" => &["MASWE-0016"],
        "anti_frida_maps" => &["MASWE-0065"],
        "emulator_detection" => &["MASWE-0053"],
        "hardcoded_crypto_secret" => &["MASWE-0004", "MASWE-0003"],
        "webview_cookie_exfil" | "Cookie" => &["MASWE-0033"],
        "webview_file_access" => &["MASWE-0034"],
        "webview_javascript_interface" | "ExecuteJavascript" | "JavascriptInterface" => &["MASWE-0033"],
        "webview_js_bridge_user_url" => &["MASWE-0033", "MASWE-0035"],
        "webview_js_bridge_file_url" => &["MASWE-0033", "MASWE-0034"],
        "webview_postmessage" => &["MASWE-0033", "MASWE-0035"],
        "webview_resource_response_file" => &["MASWE-0034", "MASWE-0050"],
        "webview_unsafe_url" | "LoadUrl" => &["MASWE-0035"],
        "webview_url_override" => &["MASWE-0035"],
        "webview_weak_host_check" => &["MASWE-0035"],
        "activity_result_contracts" => &["MASWE-0032"],
        "activity_result_grant_smuggle" => &["MASWE-0032"],
        "aidl_stub_as_interface" => &["MASWE-0032", "MASWE-0018"],
        "binder_intent_control" => &["MASWE-0032"],
        "deeplink_webview_path_traversal" => &["MASWE-0029", "MASWE-0033", "MASWE-0050"],
        "dynamic_register_receiver" => &["MASWE-0032"],
        "intent_url_network_fetch" => &["MASWE-0029", "MASWE-0026"],
        "jni_taint_import_bridge" => &["MASWE-0049"],
        "logcat_external_storage" => &["MASWE-0005", "MASWE-0002"],
        "rce_package_context" => &["MASWE-0049"],
        "slice_provider_api" => &["MASWE-0018"],
        "uri_permission_grant_flow" | "uri_permission_result_forward"
        | "uri_permission_setresult_passthrough" => &["MASWE-0032"],
        "world_readable_storage" => &["MASWE-0001", "MASWE-0002"],
        "zip_slip" => &["MASWE-0050"],
        "LaunchingComponent" | "StartActivity" | "StartService" | "SendBroadcast" => &["MASWE-0032"],
        "Network" => &["MASWE-0026"],
        _ => &[],
    }
}

type Hint = (Regex, &'static [&'static str]);

fn maswe_hints() -> &'static [Hint] {
    static CELL: OnceLock<Vec<Hint>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mk = |pat: &str, ids: &'static [&'static str]| -> Option<Hint> {
            Regex::new(pat).ok().map(|r| (r, ids))
        };
        [
            mk(r"(?i)backup|allowbackup|fullbackup|dataextraction|backup.?rules", &["MASWE-0006"]),
            mk(r"(?i)logcat|logging_pii|insecure_logging|android\\.util\\.log|\blogging\b|sensitive.?data.?in.?log|Log\.[devwi]", &["MASWE-0005"]),
            mk(r"(?i)hardcoded.?secret|hardcoded.?crypto|secretkeyspec|hardcoded.?aes|hardcoded.?key", &["MASWE-0004", "MASWE-0003"]),
            // Prefer API/path tokens over bare "MediaStore" so method names like mastgTestMediaStore do not match.
            mk(r"(?i)shared.?storage|external.?storage|MediaStore\.|getExternal(?:Files|Storage|Cache)|scoped.?storage", &["MASWE-0002"]),
            mk(r"(?i)shared.?prefer|datastore|sqlite|sqlcipher|internal.?storage|openfileoutput|app.?sandbox|sandbox.?storage", &["MASWE-0001"]),
            mk(r"(?i)broken.?encrypt|encryption.?mode|encryption.?algorithm|weak_crypto|insufficient.?key|cipher\\.getinstance|ecb", &["MASWE-0007"]),
            mk(r"(?i)hmac|mac.?valid", &["MASWE-0009"]),
            mk(r"(?i)non.?random|random.?apis|math\\.random|java\\.util\\.random|securerandom|insufficient.?entropy", &["MASWE-0012"]),
            mk(r"(?i)key.?generation|key.?length|keygenparameterspec|asymmetric.?key", &["MASWE-0013"]),
            mk(r"(?i)security.?provider|providers?\\.get", &["MASWE-0007"]),
            mk(r"(?i)keystore|keychain|user.?auth|setuserauthentication", &["MASWE-0016", "MASWE-0003"]),
            mk(r"(?i)biometric.?device.?credential|device.?credential.?fallback", &["MASWE-0021"]),
            mk(r"(?i)biometric.?invalidat|invalidatedbybiometric", &["MASWE-0022"]),
            mk(r"(?i)biometric|passcode|local.?auth|event.?bound|no.?confirmation|validity.?duration", &["MASWE-0020", "MASWE-0016"]),
            mk(r"(?i)ssl.?trust|trust.?all|checkservertrusted|hostname.?verif|onreceivedsslerror|trust.?anchor|cert(?:ificate)?.?pinning|ssl.?pinning|pinning.?bypass|network.?security|cleartext", &["MASWE-0027", "MASWE-0028"]),
            mk(r"(?i)deeplink|deep.?link|autoverify|custom.?scheme|intent.?filter", &["MASWE-0029"]),
            mk(r"(?i)pending.?intent", &["MASWE-0032"]),
            mk(r"(?i)implicit.?intent|intent.?leak|intent.?redirect|intent.?spoof|icc_|ipc_intent|broadcast.?receiver|sendbroadcast|sticky.?broadcast|ordered.?broadcast|sensitive.?broadcast|credential.?broadcast", &["MASWE-0032"]),
            mk(r"(?i)content.?provider|provider.?exported|fileprovider|sql.?inject", &["MASWE-0018", "MASWE-0050"]),
            mk(r"(?i)javascript.?interface|js.?bridge|addjavascriptinterface|webview.?bridges", &["MASWE-0033"]),
            mk(r"(?i)webview.?file|allowfileaccess|setAllowFileAccess|allowcontentaccess|setAllowContentAccess|WebResourceResponse|shouldInterceptRequest", &["MASWE-0034"]),
            // Inventory WebViewClient / loadUrl → loading-untrusted-content weakness only (not local-file MASWE-0034).
            mk(r"(?i)webviewclient|shouldOverrideUrlLoading|setWebViewClient|\bloadUrl\b|safebrowsing", &["MASWE-0035"]),
            mk(r"(?i)\bWebView\b", &["MASWE-0035"]),
            mk(r"(?i)keyboard.?cache|input.?type|textpassword|textnonsuggestion|input.?field", &["MASWE-0036"]),
            mk(r"(?i)NotificationManager|NotificationCompat|notification_sensitive|sensitive.?data.?in.?notification|post.?notification|android\.permission\.POST_NOTIFICATIONS", &["MASWE-0037"]),
            mk(r"(?i)flag.?secure|screenshot|setsecure|recents.?screenshot", &["MASWE-0038"]),
            mk(r"(?i)overlay.?attack|system.?alert.?window|hideoverlay|filtertouches|tapjack|draw.?over", &["MASWE-0039"]),
            mk(r"(?i)minsdk|sdk.?version|target.?sdk", &["MASWE-0041", "MASWE-0042"]),
            mk(r"(?i)debuggable|strictmode", &["MASWE-0061", "MASWE-0063"]),
            mk(r"(?i)debugger|tracerpid|ptrace|anti.?debug", &["MASWE-0064"]),
            mk(r"(?i)root.?detect|su.?binary|test.?keys", &["MASWE-0051"]),
            mk(r"(?i)emulator|virtual.?device", &["MASWE-0053"]),
            mk(r"(?i)deserial|object.?input|serializable", &["MASWE-0050"]),
            mk(r"(?i)rce_dynamic|dexclassloader|pathclassloader|dynamic.?code|reflection_rce|\\bRuntime\\.exec\\b", &["MASWE-0049"]),
            mk(r"(?i)path.?traversal|zip.?slip|uri.?permission|uri.?grant", &["MASWE-0050", "MASWE-0018"]),
            mk(r"(?i)dangerous.?permission|uses.?permission|runtime.?permission|app.?permission|permission.?protect|detect-dangerous-android-permissions", &["MASWE-0066"]),
        ]
        .into_iter()
        .flatten()
        .collect()
    })
}

fn know_hints() -> &'static [Hint] {
    static CELL: OnceLock<Vec<Hint>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mk = |pat: &str, ids: &'static [&'static str]| -> Option<Hint> {
            Regex::new(pat).ok().map(|r| (r, ids))
        };
        [
            mk(r"(?i)biometric|fingerprint|local.?auth|device.?credential|passcode", &["MASTG-KNOW-0001"]),
            mk(r"(?i)debuggable|allow.?debug", &["MASTG-KNOW-0007"]),
            mk(r"(?i)debugger|tracerpid|ptrace|anti.?debug", &["MASTG-KNOW-0008", "MASTG-KNOW-0028"]),
            mk(r"(?i)strictmode", &["MASTG-KNOW-0009"]),
            mk(r"(?i)key.?gen|keygen|asymmetric.?key|key.?length|keystore|keychain|crypto.?key|hardcoded.?crypto", &["MASTG-KNOW-0012", "MASTG-KNOW-0043", "MASTG-KNOW-0047"]),
            mk(r"(?i)non.?random|random.?apis|math\\.random|java\\.util\\.random|securerandom|insufficient.?entropy", &["MASTG-KNOW-0013"]),
            mk(r"(?i)network.?security|cleartext|trust.?anchor|insecure.?trust", &["MASTG-KNOW-0014"]),
            mk(r"(?i)hostname.?verif|ssl.?error|checkservertrusted|certificate.?pin|ssl.?socket|cert(?:ificate)?.?pinning|ssl.?pinning|pinning.?bypass", &["MASTG-KNOW-0015", "MASTG-KNOW-0014"]),
            mk(r"(?i)dangerous.?permission|uses.?permission|runtime.?permission|app.?permission|permission.?protect|detect-dangerous-android-permissions", &["MASTG-KNOW-0017"]),
            mk(r"(?i)webview|javascript.?interface|allowfileaccess|setAllowFileAccess|safebrowsing|cookiemanager|http.?cookie|session.?cookie|webview.?cookie|custom.?tabs", &["MASTG-KNOW-0018"]),
            mk(r"(?i)deeplink|deep.?link|autoverify|custom.?scheme|intent.?filter", &["MASTG-KNOW-0019"]),
            mk(r"(?i)content.?provider|provider.?exported|fileprovider", &["MASTG-KNOW-0117", "MASTG-KNOW-0020"]),
            mk(r"(?i)serializ|object.?input|parcelable", &["MASTG-KNOW-0021"]),
            mk(r"(?i)overlay.?attack|system.?alert.?window|draw.?over|tapjack|hideoverlay|filtertouches", &["MASTG-KNOW-0022"]),
            mk(r"(?i)sdk.?version|target.?sdk|min.?sdk|enforced.?updat", &["MASTG-KNOW-0023"]),
            mk(r"(?i)pending.?intent", &["MASTG-KNOW-0024"]),
            mk(r"(?i)implicit.?intent|intent.?leak|intent.?spoof|intent.?inject", &["MASTG-KNOW-0025", "MASTG-KNOW-0020"]),
            mk(r"(?i)broadcast.?receiver|BroadcastReceiver|registerreceiver|sendbroadcast|sticky.?broadcast|ordered.?broadcast", &["MASTG-KNOW-0134", "MASTG-KNOW-0020"]),
            mk(r"(?i)\bipc\b|ipc_|exported.?activit|exported.?service|component.?exposure|uri.?grant", &["MASTG-KNOW-0020", "MASTG-KNOW-0132"]),
            mk(r"(?i)root.?detect|jailbreak", &["MASTG-KNOW-0027"]),
            mk(r"(?i)emulator|virtual.?device", &["MASTG-KNOW-0031", "MASTG-KNOW-0135"]),
            mk(r"(?i)obfuscat", &["MASTG-KNOW-0033"]),
            mk(r"(?i)play.?integrity|safety.?net|device.?binding", &["MASTG-KNOW-0035", "MASTG-KNOW-0034"]),
            mk(r"(?i)shared.?prefer|local.?storage|datastore", &["MASTG-KNOW-0036", "MASTG-KNOW-0142"]),
            mk(r"(?i)sql.?inject|sqlite|room.?sql", &["MASTG-KNOW-0037"]),
            mk(r"(?i)sqlcipher", &["MASTG-KNOW-0038"]),
            mk(r"(?i)firebase", &["MASTG-KNOW-0039"]),
            mk(r"(?i)io\\.realm|\brealm\\.|realm.?database|RealmConfiguration", &["MASTG-KNOW-0040"]),
            mk(r"(?i)external.?storage|shared.?storage|MediaStore\.|getExternal(?:Files|Storage|Cache)|world.?readable|world.?writable", &["MASTG-KNOW-0042"]),
            mk(r"(?i)internal.?storage|getfilesdir|openfileoutput", &["MASTG-KNOW-0041"]),
            mk(r"(?i)logcat|insecure.?logging|android\.util\.log|\blogging\b", &["MASTG-KNOW-0049"]),
            mk(r"(?i)backup|allowbackup|fullbackup|dataextraction", &["MASTG-KNOW-0050"]),
            mk(r"(?i)screenshot|flag_secure|secure.?flag", &["MASTG-KNOW-0053"]),
            mk(r"(?i)NotificationManager|NotificationCompat|notification_sensitive|sensitive.?data.?in.?notification|post.?notification|android\.permission\.POST_NOTIFICATIONS", &["MASTG-KNOW-0054"]),
            mk(r"(?i)keyboard.?cache|input.?type|textpassword|textnonsuggestion", &["MASTG-KNOW-0055"]),
            mk(r"(?i)input.?field|edittext|autofill", &["MASTG-KNOW-0052"]),
            mk(r"(?i)zip.?slip|path.?traversal|zipentry", &["MASTG-KNOW-0042", "MASTG-KNOW-0041"]),
            mk(r"(?i)encryption|cipher|\baes\b|\bdes\b|\brc4\b|broken.?encrypt|weak.?crypto", &["MASTG-KNOW-0012", "MASTG-KNOW-0011"]),
            mk(r"(?i)tracker|analytics|third.?party.?service", &["MASTG-KNOW-0026"]),
            mk(r"(?i)rce_dynamic|dexclassloader|pathclassloader|runtime\.exec|reflection_rce|dynamic.?load|dynamic.?code", &["MASTG-KNOW-0005", "MASTG-KNOW-0004"]),
            mk(r"(?i)nsc|cleartext.?traffic", &["MASTG-KNOW-0014"]),
            mk(r"(?i)\bjni\b|native.?lib|\belf\b", &["MASTG-KNOW-0005", "MASTG-KNOW-0006"]),
        ]
        .into_iter()
        .flatten()
        .collect()
    })
}


fn lookup_maswe(id: &str) -> Option<MasLink> {
    maswe_catalog().iter().find(|(i, _, _)| *i == id).map(|(i, t, f)| MasLink {
        id: (*i).into(),
        title: (*t).into(),
        family: (*f).into(),
        url: maswe_url(i, f),
    })
}

fn lookup_know(id: &str) -> Option<MasLink> {
    know_catalog().iter().find(|(i, _, _)| *i == id).map(|(i, t, f)| MasLink {
        id: (*i).into(),
        title: (*t).into(),
        family: (*f).into(),
        url: mastg_url(i),
    })
}

fn lookup_best(id: &str) -> Option<MasLink> {
    best_catalog().iter().find(|(i, _)| *i == id).map(|(i, t)| MasLink {
        id: (*i).into(),
        title: (*t).into(),
        family: String::new(),
        url: mastg_url(i),
    })
}

fn lookup_masvs(id: &str) -> MasLink {
    let fam = id
        .rsplit_once('-')
        .and_then(|(a, n)| {
            if n.chars().all(|c| c.is_ascii_digit()) {
                Some(a.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| id.to_string());
    let label = id.strip_prefix("MASVS-").unwrap_or(id);
    MasLink {
        id: id.into(),
        title: label.into(),
        family: fam,
        url: masvs_control_url(id),
    }
}

fn push_unique(out: &mut Vec<String>, id: &str) {
    if !out.iter().any(|x| x == id) {
        out.push(id.to_string());
    }
}

/// Strip class/method locations and sink site noise so free-text MAS hints do not
/// fire on identifiers like `mastgTestMediaStore` / `Foo#onReceiveBroadcast`.
fn strip_location_noise(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            `[^`]*\#[^`]*`                                   # `Class#method`
            | \b(?:[a-z_][\w$]*\.)+[A-Za-z_][\w$]*\#[\w$<>]+ # fqcn#method
            | \b[A-Z][\w$]*\#[\w$<>]+                        # Simple#method
            | \bSink:\s*`[^`]+`(?:\s+in\s+`[^`]+`)?
            | \s@\s*0x[0-9a-fA-F]+
            ",
        )
        .expect("location noise regex")
    });
    re.replace_all(s, " ").into_owned()
}

/// Corpus used for heuristic MASWE/KNOW hints (locations removed).
fn hint_blob(
    category: &str,
    title: &str,
    message: &str,
    problem: &str,
    recommendation: &str,
    cwe: &str,
    rule_id: &str,
) -> String {
    [
        category,
        title,
        strip_location_noise(message).as_str(),
        strip_location_noise(problem).as_str(),
        recommendation,
        cwe,
        rule_id,
    ]
    .join(" ")
}

/// Resolve MASWE / MASVS / MASTG links for a detector or Semgrep finding.
pub fn enrich_mas(
    category: &str,
    title: &str,
    message: &str,
    problem: &str,
    recommendation: &str,
    cwe: Option<&str>,
    rule_id: Option<&str>,
) -> MasEnrichment {
    let cwe_s = cwe.unwrap_or("");
    let rule_s = rule_id.unwrap_or("");
    let blob = [category, title, message, problem, recommendation, cwe_s, rule_s].join(" ");
    let hints_blob = hint_blob(
        category,
        title,
        message,
        problem,
        recommendation,
        cwe_s,
        rule_s,
    );

    let mut maswe_ids: Vec<String> = Vec::new();
    let from_category = category_maswe(category);
    for id in from_category {
        push_unique(&mut maswe_ids, id);
    }
    // Heuristic text hints only when the category has no catalog mapping.
    // Locations are stripped so class/method names cannot pollute the match.
    if from_category.is_empty() {
        for (re, ids) in maswe_hints().iter() {
            if re.is_match(&hints_blob) {
                for id in *ids {
                    push_unique(&mut maswe_ids, id);
                }
            }
        }
    }
    // Cap MASWE
    maswe_ids.truncate(3);

    let mut masvs_ids: Vec<String> = Vec::new();
    for m in Regex::new(r"MASVS-[A-Z]+-\d+").unwrap().find_iter(&blob) {
        push_unique(&mut masvs_ids, &m.as_str().to_uppercase());
    }
    for mid in &maswe_ids {
        for c in maswe_to_masvs(mid) {
            push_unique(&mut masvs_ids, c);
        }
    }

    let mut know_ids: Vec<String> = Vec::new();
    for mid in &maswe_ids {
        for k in maswe_to_know(mid) {
            push_unique(&mut know_ids, k);
        }
    }
    if from_category.is_empty() {
        for (re, ids) in know_hints().iter() {
            if re.is_match(&hints_blob) {
                for id in *ids {
                    push_unique(&mut know_ids, id);
                }
            }
        }
    }
    know_ids.truncate(4);

    let mut best_ids: Vec<String> = Vec::new();
    for mid in &maswe_ids {
        for b in maswe_to_best(mid) {
            push_unique(&mut best_ids, b);
        }
    }
    best_ids.truncate(4);
    masvs_ids.truncate(6);

    MasEnrichment {
        maswe: maswe_ids.iter().filter_map(|id| lookup_maswe(id)).collect(),
        masvs: masvs_ids.iter().map(|id| lookup_masvs(id)).collect(),
        mastg_know: know_ids.iter().filter_map(|id| lookup_know(id)).collect(),
        mastg_best: best_ids.iter().filter_map(|id| lookup_best(id)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_category_maps_to_maswe_and_masvs() {
        let e = enrich_mas(
            "intent_redirect_nested",
            "Nested Intent redirection",
            "nested Intent launched",
            "problem",
            "fix",
            Some("CWE-926"),
            None,
        );
        assert!(e.maswe.iter().any(|l| l.id == "MASWE-0032"), "{:?}", e.maswe);
        assert!(e.masvs.iter().any(|l| l.id == "MASVS-PLATFORM-1"), "{:?}", e.masvs);
        assert!(e.masvs[0].url.contains("/MASVS/controls/"));
        assert!(!e.mastg_know.is_empty());
    }

    #[test]
    fn logging_maps_to_storage_control() {
        let e = enrich_mas(
            "logging_pii",
            "PII logged",
            "password reaches Log",
            "",
            "",
            None,
            None,
        );
        assert!(e.maswe.iter().any(|l| l.id == "MASWE-0005"));
        assert!(e.masvs.iter().any(|l| l.id == "MASVS-STORAGE-2"));
        assert_eq!(e.maswe.len(), 1, "expected only MASWE-0005, got {:?}", e.maswe);
    }

    #[test]
    fn logging_ignores_mediastore_method_name_hint() {
        // MASTG-DEMO-0001: logging_pii in mastgTestMediaStore — "MediaStore" must not
        // pull MASWE-0002 / KNOW-0042 via free-text hints.
        let e = enrich_mas(
            "logging_pii",
            "PII / credentials logged",
            "Passwords, emails, tokens, or other PII markers reach Log.* Sink: `Log.e` in `org.owasp.mastestapp.MastgTest#mastgTestMediaStore`.",
            "org.owasp.mastestapp.MastgTest#mastgTestMediaStore @ 0x146",
            "Strip or redact PII/secrets before Log.*",
            Some("CWE-532"),
            None,
        );
        assert_eq!(
            e.maswe.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["MASWE-0005"],
            "{:?}",
            e.maswe
        );
        assert!(
            e.masvs.iter().all(|l| l.id == "MASVS-STORAGE-2"),
            "{:?}",
            e.masvs
        );
        assert!(
            e.mastg_know.iter().any(|l| l.id == "MASTG-KNOW-0049"),
            "{:?}",
            e.mastg_know
        );
        assert!(
            !e.mastg_know.iter().any(|l| l.id == "MASTG-KNOW-0042"),
            "MediaStore method name must not add External Storage KNOW: {:?}",
            e.mastg_know
        );
        assert!(
            !e.maswe.iter().any(|l| l.id == "MASWE-0002"),
            "{:?}",
            e.maswe
        );
    }

    #[test]
    fn semgrep_logging_ignores_method_location_media_store() {
        // Unmapped Semgrep rule id → hints run, but Class#mastgTestMediaStore must not
        // add MASWE-0002 / KNOW-0042.
        let e = enrich_mas(
            "semgrep:mastg-android-logging-apis",
            "Semgrep: mastg-android-logging-apis",
            "References to logging APIs. Sink: `Log.e` in `org.owasp.mastestapp.MastgTest#mastgTestMediaStore`.",
            "org.owasp.mastestapp.MastgTest#mastgTestMediaStore @ 0x146",
            "",
            None,
            Some("mastg-android-logging-apis"),
        );
        assert!(
            e.maswe.iter().any(|l| l.id == "MASWE-0005"),
            "{:?}",
            e.maswe
        );
        assert!(
            !e.maswe.iter().any(|l| l.id == "MASWE-0002"),
            "{:?}",
            e.maswe
        );
        assert!(
            !e.mastg_know.iter().any(|l| l.id == "MASTG-KNOW-0042"),
            "{:?}",
            e.mastg_know
        );
    }

    #[test]
    fn webviewclient_inventory_does_not_attach_file_access_or_rce_know() {
        // INFO inventory rule: setWebViewClient — MASWE-0035 ok; not 0034 (local files),
        // and "intercept" must not match bare "rce" → KNOW-0004/0005.
        let e = enrich_mas(
            "semgrep:mastg-android-webviewclient-url-handlers",
            "Semgrep: mastg-android-webviewclient-url-handlers",
            "[MASVS-CODE-4] Detected WebViewClient URL interception method.",
            "pattern match: mastg-android-webviewclient-url-handlers",
            "",
            None,
            Some("mastg-android-webviewclient-url-handlers"),
        );
        assert!(
            e.maswe.iter().any(|l| l.id == "MASWE-0035"),
            "{:?}",
            e.maswe
        );
        assert!(
            !e.maswe.iter().any(|l| l.id == "MASWE-0034"),
            "setWebViewClient inventory must not attach local-file MASWE-0034: {:?}",
            e.maswe
        );
        assert!(
            !e.mastg_know
                .iter()
                .any(|l| l.id == "MASTG-KNOW-0004" || l.id == "MASTG-KNOW-0005"),
            "\"intercept\" must not map to RCE KNOW: {:?}",
            e.mastg_know
        );
        assert!(
            e.mastg_know.iter().any(|l| l.id == "MASTG-KNOW-0018"),
            "{:?}",
            e.mastg_know
        );
    }

    #[test]
    fn newly_mapped_uri_grant_flow_skips_webview_name_pollution() {
        let e = enrich_mas(
            "uri_permission_grant_flow",
            "Intent data reaches URI grant / setResult API",
            "Untrusted Intent data flows into setResult. in Foo#testWebViewLogin",
            "com.example.Foo#testWebViewLogin",
            "",
            None,
            None,
        );
        assert!(
            e.maswe.iter().all(|l| l.id == "MASWE-0032"),
            "expected only MASWE-0032, got {:?}",
            e.maswe
        );
        assert!(
            !e.maswe.iter().any(|l| l.id == "MASWE-0035" || l.id == "MASWE-0034"),
            "{:?}",
            e.maswe
        );
    }
}
