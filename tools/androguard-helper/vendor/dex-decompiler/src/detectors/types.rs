//! Shared types and helpers for source→sink and invoke-only detectors.

use crate::decompile::value_flow::{ValueFlowAnalysis, ValueFlowAnalysisOwned};
use std::collections::BTreeSet;

/// One step on an intra-method value-flow / execution evidence path.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct VulnTraceStep {
    /// Bytecode offset within the method.
    pub offset: u32,
    /// Register holding / using the tainted value at this step, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg: Option<u32>,
    /// `source` | `propagate` | `use` | `sink` | `invoke`.
    pub kind: String,
    /// Human description (API name, insn text, etc.).
    pub description: String,
}

/// One finding: tainted value from a source reaches a dangerous sink (or a dangerous invoke is present).
#[derive(Debug, Clone, serde::Serialize)]
pub struct VulnFinding {
    pub category: String,
    /// Short human title (e.g. "Intent spoofing").
    pub title: String,
    /// `high` | `medium` | `low` | `info`.
    pub severity: String,
    /// Longer explanation of why this is flagged and what to check.
    pub message: String,
    /// One-line exact problem statement for analysts.
    pub problem: String,
    /// Concrete remediation / next-step guidance.
    pub recommendation: String,
    /// Optional CWE identifier (e.g. "CWE-926").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwe: Option<String>,
    pub class_name: String,
    pub method_name: String,
    /// Optional: offset where the tainted value is produced (move-result after source API).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_offset: Option<u32>,
    /// Register that receives the tainted value at the source, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_reg: Option<u32>,
    /// Source API / origin description (concrete method when known).
    pub source_desc: String,
    /// Offset of the sink invoke (or dangerous API).
    pub sink_offset: u32,
    /// Register used at the sink (argument carrying taint), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink_reg: Option<u32>,
    /// Sink API method reference.
    pub sink_desc: String,
    /// Ordered intra-method evidence path (source → propagations/uses → sink).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<VulnTraceStep>,
    /// All related bytecode offsets (source, copies, uses, sink), sorted unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_offsets: Vec<u32>,
    /// OWASP MASWE weaknesses linked to this finding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maswe: Vec<super::mas::MasLink>,
    /// OWASP MASVS controls (e.g. MASVS-PLATFORM-1) with docs URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masvs: Vec<super::mas::MasLink>,
    /// OWASP MASTG Knowledge articles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_know: Vec<super::mas::MasLink>,
    /// OWASP MASTG Best Practices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_best: Vec<super::mas::MasLink>,
}

/// Static metadata for a detector category.
#[derive(Debug, Clone, Copy)]
pub struct CategoryMeta {
    pub title: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub recommendation: &'static str,
    pub cwe: Option<&'static str>,
}

/// Human-readable metadata for known vuln detector categories.
pub fn category_meta(category: &str) -> CategoryMeta {
    match category {
        "intent_spoofing" => CategoryMeta {
            title: "Intent spoofing / redirect",
            severity: "high",
            message: "Untrusted Intent data (extras, URI, or getIntent) flows into an IPC launch API. An attacker-controlled Intent may redirect the app to an unexpected component or URI.",
            recommendation: "Validate action, package, component, and data URI before startActivity/sendBroadcast/startService. Prefer explicit Intents with a fixed ComponentName.",
            cwe: Some("CWE-926"),
        },
        "intent_redirect_nested" => CategoryMeta {
            title: "Nested Intent redirection",
            severity: "high",
            message: "A Parcelable/Intent extracted from an incoming Intent extra is launched (startActivity/startService/bindService/sendBroadcast). This is Play's classic intent-redirection confused-deputy: attackers reach non-exported components in your app identity.",
            recommendation: "Do not forward nested Intents. Prefer PendingIntent, validate getComponent()/getPackage() against an allow-list, strip FLAG_GRANT_* with removeFlags, or use IntentSanitizer / Android 16 launch protections.",
            cwe: Some("CWE-926"),
        },
        "intent_redirect_grant_smuggle" => CategoryMeta {
            title: "URI FLAG_GRANT smuggling on redirected Intent",
            severity: "high",
            message: "Untrusted nested Intent data reaches addFlags/setFlags/setClipData/grantUriPermission (or co-occurs with a nested Intent launch). Attackers can attach FLAG_GRANT_READ/WRITE_URI_PERMISSION and access private providers / FileProviders.",
            recommendation: "Never copy FLAG_GRANT_* from untrusted Intents. Call removeFlags(FLAG_GRANT_READ_URI_PERMISSION|FLAG_GRANT_WRITE_URI_PERMISSION|FLAG_GRANT_PERSISTABLE_URI_PERMISSION|FLAG_GRANT_PREFIX_URI_PERMISSION) before any launch, or build a fresh explicit Intent.",
            cwe: Some("CWE-926"),
        },
        "ipc_intent_validation" => CategoryMeta {
            title: "IPC Intent without validation",
            severity: "high",
            message: "Intent-derived data reaches startActivity/setResult/sendBroadcast/startService without apparent validation of the target component or URI. Validate package, action, and data before forwarding.",
            recommendation: "Check Intent.getPackage()/getComponent() against an allow-list; never forward untrusted extras into launch APIs.",
            cwe: Some("CWE-926"),
        },
        "rce_dynamic_loading" => CategoryMeta {
            title: "Dynamic code loading",
            severity: "high",
            message: "The method loads code or reflects into classes at runtime (DexClassLoader, PathClassLoader, Class.forName, Runtime.exec, etc.). Untrusted input controlling the path or class name can lead to remote code execution.",
            recommendation: "Avoid loading DEX/native code from writable or network paths. If required, verify signatures and use a fixed allow-list of class names.",
            cwe: Some("CWE-94"),
        },
        "insecure_logging" => CategoryMeta {
            title: "Sensitive data in logs",
            severity: "medium",
            message: "Data that may be sensitive (location, identifiers, credentials, clipboard, extras) reaches a logging API. Logs can be read by other apps on older Android or leak via bugreports.",
            recommendation: "Strip or redact PII/secrets before Log.*; never log tokens, passwords, or full Intent extras in production builds.",
            cwe: Some("CWE-532"),
        },
        "logging_pii" => CategoryMeta {
            title: "PII / credentials logged",
            severity: "high",
            message: "Passwords, emails, tokens, or other PII markers reach Log.* (often via StringBuilder/+). Release logcat / bugreports expose credentials to any reader with log access.",
            recommendation: "Never log passwords, emails, tokens, or auth extras in release builds; gate Log.d behind BuildConfig.DEBUG and redact PII.",
            cwe: Some("CWE-532"),
        },
        "sql_injection" => CategoryMeta {
            title: "SQL injection risk",
            severity: "high",
            message: "A value flows into a SQL/query API in a way that may allow injection. Prefer parameterized queries (SelectionArgs / ? placeholders) instead of string concatenation.",
            recommendation: "Use selectionArgs / bindArgs with `?` placeholders. Never concatenate untrusted strings into rawQuery/execSQL.",
            cwe: Some("CWE-89"),
        },
        "webview_unsafe" | "webview_unsafe_url" | "webview_javascript_interface" | "webview_file_access" => {
            CategoryMeta {
                title: "Unsafe WebView configuration",
                severity: "high",
                message: "WebView settings or load APIs enable JavaScript bridges, file access, or load untrusted content. Combined with attacker-controlled URLs this can lead to XSS or local file theft.",
                recommendation: "Disable file access and setJavaScriptEnabled unless required; never addJavascriptInterface with untrusted pages; load only HTTPS URLs you control.",
                cwe: Some("CWE-79"),
            }
        }
        "hardcoded_secrets_review" => CategoryMeta {
            title: "Possible secret / credential write",
            severity: "info",
            message: "A write/persist or HTTP body API commonly used for secrets or tokens was found. Review whether hardcoded credentials, API keys, or tokens are stored or transmitted. Standalone prefs put* is not flagged here.",
            recommendation: "Store secrets in Android Keystore / EncryptedSharedPreferences; rotate any keys found in the binary. Prefer regex/Semgrep for known cloud key formats.",
            cwe: Some("CWE-798"),
        },
        "uri_permission_result_forward" => CategoryMeta {
            title: "Activity result Intent forwarded via setResult",
            severity: "high",
            message: "onActivityResult forwards its Intent to setResult. An attacker who handled startActivityForResult can return FLAG_GRANT_* and a content URI, receiving access to providers with grantUriPermissions=true.",
            recommendation: "Do not forward untrusted result Intents. Build a new Intent with an allow-listed data URI and never copy FLAG_GRANT_* from the result.",
            cwe: Some("CWE-926"),
        },
        "uri_permission_grant_flow" => CategoryMeta {
            title: "Intent data reaches URI grant / setResult API",
            severity: "high",
            message: "Untrusted Intent data flows into setResult, addFlags/setFlags, setClipData, or grantUriPermission. Combined with FLAG_GRANT_* this can leak ContentProvider access.",
            recommendation: "Strip or avoid FLAG_GRANT_READ/WRITE_URI_PERMISSION on Intents built from untrusted input; validate URIs before granting.",
            cwe: Some("CWE-926"),
        },
        "path_traversal" => CategoryMeta {
            title: "Path traversal risk",
            severity: "high",
            message: "User- or Intent-influenced data reaches a file path API. Without sanitization (`../` rejection, canonical path checks) this can read/write outside the intended directory.",
            recommendation: "Resolve canonical paths and ensure they stay under the intended base directory; reject `..` segments from untrusted input.",
            cwe: Some("CWE-22"),
        },
        "provider_path_traversal" => CategoryMeta {
            title: "ContentProvider openFile path traversal",
            severity: "high",
            message: "Provider openFile/openAssetFile builds a File path from Uri segments without getCanonicalPath containment. Attackers with URI grants can read/write outside the intended tree (Oversecured Content Provider research).",
            recommendation: "Canonicalize paths and verify they remain under the provider root; prefer FileProvider with tightly scoped paths XML.",
            cwe: Some("CWE-22"),
        },
        "uri_permission_setresult_passthrough" => CategoryMeta {
            title: "setResult(getIntent()) URI grant passthrough",
            severity: "high",
            message: "Activity returns getIntent() via setResult without stripping FLAG_GRANT_*. Attackers startActivityForResult with a content URI + grant flags and inherit access to grantUriPermissions providers (Oversecured VulnerableActivity pattern).",
            recommendation: "Never setResult(getIntent()). Copy only needed extras; clear FLAG_GRANT_* before returning.",
            cwe: Some("CWE-926"),
        },
        "webview_resource_response_file" => CategoryMeta {
            title: "WebResourceResponse local file leak",
            severity: "high",
            message: "shouldInterceptRequest / WebResourceResponse serves local files (FileInputStream / getLastPathSegment) without path containment. Combined with XSS or arbitrary WebView URL this leaks private files (Oversecured Amazon / WebResourceResponse research).",
            recommendation: "Use WebViewAssetLoader; never map request paths to arbitrary filesystem paths; avoid Access-Control-Allow-Origin: * on local responses.",
            cwe: Some("CWE-22"),
        },
        "rce_package_context" => CategoryMeta {
            title: "createPackageContext third-party code load",
            severity: "critical",
            message: "createPackageContext + getClassLoader/loadClass without checkSignatures lets a malicious app with a matching package prefix execute code in this app's process (Oversecured + OVAA plugin ACE).",
            recommendation: "Require PackageManager.checkSignatures (or signing cert match) before loading foreign ClassLoaders; prefer in-APK modules.",
            cwe: Some("CWE-94"),
        },
        "intent_parse_uri_redirect" => CategoryMeta {
            title: "Intent.parseUri → component launch",
            severity: "high",
            message: "Intent.parseUri (intent://) flows into startActivity/startService. Attackers can reach non-exported components from WebView or deeplink-driven URLs (Oversecured deep-link ATO / protected-components research).",
            recommendation: "Disallow intent:// in WebViews; if parsing is required, strip selectors and FLAG_GRANT_*; never pass parseUri results unchecked to startActivity.",
            cwe: Some("CWE-926"),
        },
        "zip_slip" => CategoryMeta {
            title: "ZIP path traversal (ZipSlip)",
            severity: "high",
            message: "A ZIP entry name (`ZipEntry.getName`) flows into a File / FileOutputStream path without a canonical-path containment check. Crafted `../` entries can overwrite files outside the extract directory (ZipSlip / Quokka Uhale CVE-2025-58391).",
            recommendation: "Build extract paths with `getCanonicalPath()` / `toRealPath()` and verify each path stays under the destination directory before writing.",
            cwe: Some("CWE-22"),
        },
        "logcat_external_storage" => CategoryMeta {
            title: "Logcat dumped to external storage",
            severity: "medium",
            message: "The app shells out to dump logcat/dmesg/dumpsys onto external storage (`/sdcard`). Other apps with storage access can read sensitive device logs (Quokka Uhale CVE-2025-58389).",
            recommendation: "Do not redirect logcat/dumpsys to world-readable paths; keep diagnostics in app-private storage or require elevated/debug builds only.",
            cwe: Some("CWE-532"),
        },
        "weak_crypto" => CategoryMeta {
            title: "Weak cryptography",
            severity: "medium",
            message: "A cryptographic API associated with weak algorithms or modes (e.g. DES, ECB, MD5 used for security) was detected. Prefer modern algorithms (AES-GCM, SHA-256+) and platform Keystore.",
            recommendation: "Replace DES/3DES/ECB/MD5 (for security) with AES-GCM or ChaCha20-Poly1305 via AndroidKeyStore.",
            cwe: Some("CWE-327"),
        },
        "insufficient_key_length" => CategoryMeta {
            title: "Insufficient cryptographic key length",
            severity: "medium",
            message: "Key generation uses a short key size (e.g. RSA ≤1024 or AES ≤128) co-located with KeyPairGenerator/KeyGenerator/setKeySize.",
            recommendation: "Use RSA ≥2048 (prefer 3072+) or AES-256; prefer AndroidKeyStore KeyGenParameterSpec with adequate setKeySize.",
            cwe: Some("CWE-326"),
        },
        "unsafe_deserialization" => CategoryMeta {
            title: "Unsafe deserialization",
            severity: "high",
            message: "Object deserialization or similar reconstruction from untrusted input was found. Gadget chains can lead to remote code execution; prefer safe formats (JSON) with explicit types.",
            recommendation: "Avoid Java ObjectInputStream / Parcelable from untrusted sources; prefer JSON with an explicit schema.",
            cwe: Some("CWE-502"),
        },
        "pending_intent" => CategoryMeta {
            title: "Mutable / empty PendingIntent",
            severity: "high",
            message: "A PendingIntent is created in a way that may be hijacked (empty base Intent and/or mutable flags with a dangerous destination). Attackers can fill in the Intent and abuse your app's privileges.",
            recommendation: "Use FLAG_IMMUTABLE (API 31+) and always set an explicit component on the base Intent.",
            cwe: Some("CWE-927"),
        },
        "world_readable_storage" => CategoryMeta {
            title: "World-readable / world-writable storage mode",
            severity: "high",
            message: "A file or SharedPreferences API is used with a MODE_WORLD_* style mode (1/2/3). Other apps on the device may read or write the data.",
            recommendation: "Use MODE_PRIVATE (0) only. Migrate sensitive data off world-accessible files.",
            cwe: Some("CWE-732"),
        },
        "sticky_ordered_broadcast" => CategoryMeta {
            title: "Sticky / ordered broadcast",
            severity: "medium",
            message: "The method sends a sticky or ordered broadcast. Receivers can be hijacked or race for privileged data unless protected by permissions.",
            recommendation: "Prefer explicit Intents / LocalBroadcastManager alternatives; require signature permissions on sensitive actions.",
            cwe: Some("CWE-925"),
        },
        "ssl_trust_all" => CategoryMeta {
            title: "Trust-all SSL / HostnameVerifier",
            severity: "high",
            message: "The app installs a custom HostnameVerifier/TrustManager or initializes SSLContext in a way commonly used to disable certificate validation.",
            recommendation: "Use system TrustManager; never accept all hostnames; enable certificate pinning for high-value APIs.",
            cwe: Some("CWE-295"),
        },
        "pinning_bypass" => CategoryMeta {
            title: "Certificate pinning bypass surface",
            severity: "high",
            message: "Reflection or pinning APIs suggest certificate pinning may be disabled or hooked at runtime.",
            recommendation: "Avoid reflectively clearing CertificatePinner/TrustKit; fail closed on pin mismatch.",
            cwe: Some("CWE-295"),
        },
        "reflection_rce" => CategoryMeta {
            title: "Reflection invoke / dynamic class load",
            severity: "high",
            message: "User-influenced or reflective Class.forName/getMethod/Method.invoke can load attacker-controlled code paths.",
            recommendation: "Allow-list class/method names; avoid passing Intent extras into reflective APIs.",
            cwe: Some("CWE-470"),
        },
        "sqlcipher_hardcoded_passphrase" => CategoryMeta {
            title: "Hardcoded DB / SQLCipher passphrase risk",
            severity: "high",
            message: "Database open helpers or SQLCipher factories appear with passphrase/password string hints.",
            recommendation: "Derive DB keys from Android Keystore; never embed passphrases in the APK.",
            cwe: Some("CWE-321"),
        },
        "implicit_intent_launch" => CategoryMeta {
            title: "Implicit Intent launch",
            severity: "info",
            message: "An Intent is launched without setComponent/setPackage/setClass, so other apps may intercept it. Bare implicit launches are common; escalate only with sensitive extras or an export chain.",
            recommendation: "Use explicit Intents with a fixed component or package for sensitive actions.",
            cwe: Some("CWE-927"),
        },
        "implicit_intent_sensitive" => CategoryMeta {
            title: "Implicit Intent with sensitive extras",
            severity: "high",
            message: "An implicit Intent launch co-occurs with credential-/token-like extras. A malicious app can intercept the Intent and steal session data.",
            recommendation: "Use explicit Intents; never put tokens/passwords/cookies in hijackable broadcasts or activities.",
            cwe: Some("CWE-927"),
        },
        "webview_js_bridge_user_url" => CategoryMeta {
            title: "JS bridge with user-controlled WebView URL",
            severity: "high",
            message: "addJavascriptInterface is combined with loading a user-/Intent-controlled URL in the same method.",
            recommendation: "Never expose bridges to untrusted content; load only https origins you control.",
            cwe: Some("CWE-79"),
        },
        "webview_cookie_exfil" => CategoryMeta {
            title: "WebView cookie access with untrusted URL",
            severity: "high",
            message: "CookieManager get/setCookie co-occurs with Intent-/user-controlled WebView navigation. Attackers can steal session cookies once an arbitrary URL loads (TikTok CVE-2022-28799-style).",
            recommendation: "Do not call CookieManager for attacker-controlled origins; isolate auth WebViews; prefer Custom Tabs / verified App Links for login.",
            cwe: Some("CWE-201"),
        },
        "custom_tabs_intent_url" => CategoryMeta {
            title: "Custom Tabs / TWA URL from Intent",
            severity: "high",
            message: "CustomTabsIntent.launchUrl (or Trusted Web Activity) receives a URL from Intent extras / deeplink data. Attackers can open attacker-controlled pages in an app-branded browser session or steal OAuth redirects.",
            recommendation: "Allow-list hosts before launchUrl; use verified App Links for OAuth; never pass raw getStringExtra URLs into Custom Tabs.",
            cwe: Some("CWE-939"),
        },
        "deeplink_webview_js_bridge" => CategoryMeta {
            title: "Deeplink → WebView → JS bridge triad",
            severity: "critical",
            message: "Exported deeplink/App Link entry, Intent-driven WebView URL loading, and addJavascriptInterface co-locate on one component — the classic one-click account-hijack stack (Microsoft TikTok CVE-2022-28799).",
            recommendation: "Hard allow-list deeplink→WebView hosts; disable or tightly scope JS bridges; never load untrusted URLs into an authenticated WebView.",
            cwe: Some("CWE-939"),
        },
        "deeplink_webview_path_traversal" => CategoryMeta {
            title: "Deeplink URI path → WebView/Lynx JS bridge",
            severity: "critical",
            message: "Intent/deeplink URI path reaches a WebView/Lynx load that also exposes a JS interface (TikTok CVE-2024-45240-style traversal into Lynxview bridges).",
            recommendation: "Validate/canonicalize deeplink paths; never load untrusted/file/traversed URIs into Lynx/WebView with JS interfaces; allow-list hosts; drop or scope addJavascriptInterface / addJavascriptInterfaceOut.",
            cwe: Some("CWE-22"),
        },
        "exported_receiver_intent_redirect" => CategoryMeta {
            title: "Exported receiver launches nested Intent",
            severity: "high",
            message: "An exported BroadcastReceiver without a permission gate also launches a Parcelable/nested Intent (startActivity/…). Malicious apps can abuse this as a confused deputy (Oversecured TikTok NotificationBroadcastReceiver pattern).",
            recommendation: "Require signature permissions on the receiver; validate nested Intents; never forward untrusted Parcelable Intents with FLAG_GRANT_*.",
            cwe: Some("CWE-926"),
        },
        "biometric_without_crypto" => CategoryMeta {
            title: "Biometric auth without crypto-bound key",
            severity: "medium",
            message: "BiometricPrompt/Fingerprint authenticate without CryptoObject / KeyGenParameterSpec user-auth binding.",
            recommendation: "Gate secrets with BiometricPrompt.CryptoObject and setUserAuthenticationRequired(true).",
            cwe: Some("CWE-287"),
        },
        "keystore_no_user_auth" => CategoryMeta {
            title: "Keystore key without user authentication",
            severity: "medium",
            message: "KeyGenParameterSpec is built without setUserAuthenticationRequired / related auth binding.",
            recommendation: "Require user authentication for keys protecting high-value data.",
            cwe: Some("CWE-320"),
        },
        "tracker_fingerprint_api" => CategoryMeta {
            title: "Tracker / advertising fingerprint API",
            severity: "info",
            message: "Advertising or analytics SDK APIs were invoked (inventory for privacy review).",
            recommendation: "Disclose trackers; gate behind consent; avoid fingerprinting where not required.",
            cwe: Some("CWE-359"),
        },
        "rce_process_exec" => CategoryMeta {
            title: "Process execution from untrusted input",
            severity: "critical",
            message: "User-/Intent-controlled data may reach Runtime.exec or ProcessBuilder.",
            recommendation: "Never build shell commands from untrusted input; use fixed argv allow-lists.",
            cwe: Some("CWE-78"),
        },
        "credential_broadcast" => CategoryMeta {
            title: "Sensitive data sent via broadcast",
            severity: "high",
            message: "Credentials or login-related data appear to reach sendBroadcast/sendOrderedBroadcast. Exported receivers may intercept them.",
            recommendation: "Use explicit Intents with a fixed component, or signature-level permissions on the broadcast action.",
            cwe: Some("CWE-925"),
        },
        "sensitive_broadcast" => CategoryMeta {
            title: "Sensitive extras on implicit broadcast",
            severity: "high",
            message: "Telephony identifiers, auth tokens, or similarly sensitive values appear to reach sendBroadcast/sendOrderedBroadcast (Samsung implicit IPC leakage class). Any app can register a matching receiver.",
            recommendation: "Do not put IMSI/MSISDN/IMEI/tokens on implicit broadcasts. Use explicit Intents, LocalBroadcastManager (same-UID only), or signature permissions.",
            cwe: Some("CWE-925"),
        },
        "broadcast_intent_redirect" => CategoryMeta {
            title: "Broadcast-mediated intent redirection",
            severity: "high",
            message: "A BroadcastReceiver (onReceive) extracts a Parcelable/nested Intent and launches it. Attackers who can send the triggering broadcast (e.g. install_complete / PACKAGE_ADDED) get a confused-deputy redirect (Samsung Galaxy Store AppLinker pattern).",
            recommendation: "Protect the receiver with a signature permission; never forward nested Intents from broadcast extras; validate component package against an allow-list.",
            cwe: Some("CWE-926"),
        },
        "command_receiver" => CategoryMeta {
            title: "Dangerous command BroadcastReceiver",
            severity: "high",
            message: "BroadcastReceiver.onReceive invokes a dangerous side-effect API (exec, camera/recorder, component enable, file write, loadUrl, package install). If the receiver is exported without a permission, any app can trigger it (FactoryCamera-style).",
            recommendation: "Unexport debug/test receivers on production builds; require signature permissions; validate action/extras before side effects.",
            cwe: Some("CWE-925"),
        },
        "provider_sql_injection" => CategoryMeta {
            title: "ContentProvider SQL injection",
            severity: "high",
            message: "Untrusted Uri/Intent data reaches rawQuery/execSQL/query inside a ContentProvider entry. Attackers with query access can inject SQL (Samsung Billing IAPService pattern).",
            recommendation: "Use selectionArgs / bindArgs with `?` placeholders; never concatenate Uri segments or extras into SQL.",
            cwe: Some("CWE-89"),
        },
        "icc_extra_flow" => CategoryMeta {
            title: "Inter-component Intent extras flow",
            severity: "high",
            message: "The method puts const-key Intent extras and launches another component (often with setClass/setClassName/setComponent). Extras written here can be read via get*Extra in the target — classic ICC half-flow link.",
            recommendation: "Validate Intent targets and extras; do not trust getStringExtra from exported/implicit launches; prefer explicit components with signature permissions.",
            cwe: Some("CWE-926"),
        },
        "jni_taint_import_bridge" => CategoryMeta {
            title: "Taint reaches JNI with dangerous native imports",
            severity: "high",
            message: "Untrusted or sensitive values reach a native (JNI) method whose .so imports system/socket/exec-style APIs. Treat the native as a sink until ARM dataflow proves otherwise.",
            recommendation: "Avoid passing PII/user input into JNI that performs network/exec; validate and sanitize on the Java side; audit the Java_* export.",
            cwe: Some("CWE-111"),
        },
        "webview_weak_host_check" => CategoryMeta {
            title: "Weak WebView / deeplink host validation",
            severity: "high",
            message: "Host validation uses contains/endsWith/startsWith near navigation (loadUrl/startActivity). Attackers can bypass with crafted hosts (e.g. evil.com.victim.com).",
            recommendation: "Compare against an exact allow-list (equals / Uri host parse), not substring checks.",
            cwe: Some("CWE-20"),
        },
        "pick_file_theft" => CategoryMeta {
            title: "ACTION_PICK / content URI file access",
            severity: "medium",
            message: "The method combines media/content pick Intents with local copy or openInputStream — classic third-party file theft via result URI.",
            recommendation: "Validate content URIs and avoid copying attacker-controlled provider results into app-private storage without checks.",
            cwe: Some("CWE-200"),
        },
        "dynamic_register_receiver" => CategoryMeta {
            title: "Dynamic registerReceiver without permission",
            severity: "medium",
            message: "Context.registerReceiver is used without an apparent signature/custom permission gate. Dynamically registered receivers are a common IPC leak/spoof surface not visible in the manifest.",
            recommendation: "Pass a signature-level permission to registerReceiver, or use LocalBroadcastManager / explicit broadcasts only.",
            cwe: Some("CWE-925"),
        },
        "binder_intent_control" => CategoryMeta {
            title: "Intent-controlled bindService",
            severity: "high",
            message: "Untrusted Intent data reaches bindService. Attackers may bind to an unexpected service or confuse a deputy binder.",
            recommendation: "Bind only to an allow-listed ComponentName; never forward extras into bindService.",
            cwe: Some("CWE-926"),
        },
        "aidl_stub_as_interface" => CategoryMeta {
            title: "AIDL Stub.asInterface near bindService",
            severity: "info",
            message: "AIDL Stub.asInterface co-occurs with bindService — review exported binder surfaces and permission gates.",
            recommendation: "Require permissions on the service; validate calling UID/package.",
            cwe: Some("CWE-926"),
        },
        "room_sql_injection" => CategoryMeta {
            title: "Room / SupportSQLite SQL injection",
            severity: "high",
            message: "User-/Intent-controlled values reach Room/SupportSQLite raw query APIs. Prefer parameterized SimpleSQLiteQuery / bind args.",
            recommendation: "Never concatenate extras into SQL; use Room @Query with bound parameters.",
            cwe: Some("CWE-89"),
        },
        "activity_result_grant_smuggle" => CategoryMeta {
            title: "Activity-result URI grant smuggling",
            severity: "high",
            message: "onActivityResult / Activity Result callback forwards data into setResult/addFlags/grantUriPermission — classic FLAG_GRANT_* smuggle.",
            recommendation: "Do not forward result Intents; strip FLAG_GRANT_* and rebuild an allow-listed Intent.",
            cwe: Some("CWE-926"),
        },
        "activity_result_contracts" => CategoryMeta {
            title: "Activity Result Contracts surface",
            severity: "info",
            message: "registerForActivityResult / ActivityResultContracts is used — review grant handling on callbacks.",
            recommendation: "Validate URIs returned by GetContent/OpenDocument before granting or persisting.",
            cwe: Some("CWE-926"),
        },
        "intent_redirect_no_sanitizer" => CategoryMeta {
            title: "Intent redirect without IntentSanitizer",
            severity: "high",
            message: "Nested Parcelable Intent is launched without IntentSanitizer / removeFlags nearby.",
            recommendation: "Use IntentSanitizer or strip FLAG_GRANT_* and validate component/package before launch.",
            cwe: Some("CWE-926"),
        },
        "slice_provider_api" => CategoryMeta {
            title: "SliceProvider / SliceManager API",
            severity: "medium",
            message: "SliceProvider/SliceManager APIs are invoked — exported slices can leak content or actions.",
            recommendation: "Gate slices with permissions; avoid embedding sensitive PendingIntents.",
            cwe: Some("CWE-200"),
        },
        "webview_url_override" => CategoryMeta {
            title: "WebView URL override / intercept with user input",
            severity: "high",
            message: "shouldOverrideUrlLoading / shouldInterceptRequest / loadUrl handles Intent-derived URLs — redirect/bridge chains often split here.",
            recommendation: "Allow-list hosts; never startActivity or evaluateJavascript on untrusted navigation.",
            cwe: Some("CWE-939"),
        },
        "webview_postmessage" => CategoryMeta {
            title: "WebView evaluateJavascript / postMessage from Intent",
            severity: "high",
            message: "Intent-derived data reaches evaluateJavascript / postMessage / loadData.",
            recommendation: "Treat script payloads as code execution; never pass extras into JS evaluation.",
            cwe: Some("CWE-79"),
        },
        "webview_js_bridge_file_url" => CategoryMeta {
            title: "JS bridge with file/content/cache URL",
            severity: "critical",
            message: "addJavascriptInterface co-occurs with file://, content://, or cache URL hints — local file + bridge is high impact.",
            recommendation: "Disable file access and remove JS bridges from any WebView that can load local or untrusted content.",
            cwe: Some("CWE-94"),
        },
        "intent_url_network_fetch" => CategoryMeta {
            title: "Intent URL to network fetch (SSRF-ish)",
            severity: "high",
            message: "Intent/deeplink-derived URL reaches OkHttp/HttpURLConnection — attackers may hit internal endpoints.",
            recommendation: "Allow-list schemes/hosts; block link-local and private ranges.",
            cwe: Some("CWE-918"),
        },
        "ssl_bypass_webview_colocate" => CategoryMeta {
            title: "SSL bypass co-located with WebView load",
            severity: "high",
            message: "Trust-all / HostnameVerifier bypass co-occurs with WebView loadUrl in the same method.",
            recommendation: "Remove custom TrustManagers; never load untrusted URLs into a WebView that disables TLS checks.",
            cwe: Some("CWE-295"),
        },
        "device_lock_api_check" => CategoryMeta {
            title: "Device lock / biometric capability API",
            severity: "info",
            message: "KeyguardManager.isDeviceSecure or BiometricManager.canAuthenticate is used (inventory for local-auth / resilience checks).",
            recommendation: "Ensure lock-screen / biometric gates bind crypto keys (setUserAuthenticationRequired) rather than boolean checks alone.",
            cwe: Some("CWE-287"),
        },
        "strict_mode_policy" => CategoryMeta {
            title: "StrictMode policy enabled",
            severity: "info",
            message: "StrictMode.setVmPolicy / setThreadPolicy is configured (often detects leaked SQLite/cursors in debug builds).",
            recommendation: "Keep StrictMode out of production release builds; fix underlying resource leaks.",
            cwe: Some("CWE-772"),
        },
        "prefs_plaintext_secret" => CategoryMeta {
            title: "Secret-like data written to SharedPreferences",
            severity: "high",
            message: "SharedPreferences putString/putStringSet co-occurs with secret/token/key markers — likely plaintext sensitive storage in the app sandbox.",
            recommendation: "Encrypt secrets (EncryptedSharedPreferences / Keystore) before persisting; never store API tokens in plain prefs.",
            cwe: Some("CWE-312"),
        },
        "keystore_multipurpose" => CategoryMeta {
            title: "Keystore key used for multiple purposes",
            severity: "medium",
            message: "A Keystore key appears configured or used for both signature and encryption purposes (PURPOSE_SIGN/VERIFY with PURPOSE_ENCRYPT/DECRYPT, or Cipher+Signature on the same key).",
            recommendation: "Generate separate keys for signing and encryption; do not OR incompatible KeyProperties purposes.",
            cwe: Some("CWE-1240"),
        },
        "anti_frida_maps" => CategoryMeta {
            title: "Anti-Frida /proc/self/maps scan",
            severity: "info",
            message: "The method reads `/proc/self/maps` and checks for Frida/gadget markers (runtime instrumentation detection).",
            recommendation: "Treat maps-based detection as bypassable; prefer Play Integrity / attestation and defense-in-depth.",
            cwe: Some("CWE-919"),
        },
        "emulator_detection" => CategoryMeta {
            title: "Emulator detection checks",
            severity: "info",
            message: "Build.* property reads co-occur with known emulator fingerprints/package markers (Genymotion, goldfish, ranchu, etc.).",
            recommendation: "Document resilience posture; combine with integrity attestation rather than relying on Build strings alone.",
            cwe: Some("CWE-919"),
        },
        "hardcoded_crypto_secret" => CategoryMeta {
            title: "Hardcoded secret used with Cipher",
            severity: "high",
            message: "Secret/token-like string constants co-occur with Cipher.doFinal — plaintext material is present in the binary and hookable at the crypto boundary.",
            recommendation: "Do not hardcode API keys; fetch from a backend or protect with Keystore-bound secrets and anti-tamper.",
            cwe: Some("CWE-798"),
        },
        "insecure_random" => CategoryMeta {
            title: "Insecure random number generator",
            severity: "medium",
            message: "java.util.Random / Math.random is used — unsuitable for tokens, passwords, or keys.",
            recommendation: "Use java.security.SecureRandom (or platform Keystore) for security-sensitive randomness.",
            cwe: Some("CWE-330"),
        },
        "non_random_source" => CategoryMeta {
            title: "Non-random time-based token source",
            severity: "medium",
            message: "Date/Calendar/System.currentTimeMillis used as a stand-in for randomness.",
            recommendation: "Do not derive auth tokens from wall-clock time; use SecureRandom.",
            cwe: Some("CWE-330"),
        },
        "external_storage_write" => CategoryMeta {
            title: "Write to external / shared storage",
            severity: "medium",
            message: "App writes via getExternalStorageDirectory / getExternalFilesDir / MediaStore EXTERNAL URIs — data may be world-readable or backed up broadly.",
            recommendation: "Prefer app-private storage or MediaStore with appropriate scrubbing; avoid secrets on shared storage.",
            cwe: Some("CWE-922"),
        },
        "explicit_security_provider" => CategoryMeta {
            title: "Explicit cryptographic security provider",
            severity: "medium",
            message: "Security.addProvider / Cipher.getInstance with an explicit provider pins crypto to a custom or third-party provider.",
            recommendation: "Prefer the platform default provider unless a documented, maintained provider is required.",
            cwe: Some("CWE-757"),
        },
        "notification_sensitive" => CategoryMeta {
            title: "Sensitive data in notifications",
            severity: "medium",
            message: "Notification content co-occurs with PII/secret-like strings — lock-screen and other apps may expose it.",
            recommendation: "Avoid PII in notification titles/text; use publicVersion / secret extras carefully.",
            cwe: Some("CWE-200"),
        },
        "safebrowsing_disabled" => CategoryMeta {
            title: "WebView SafeBrowsing disabled",
            severity: "medium",
            message: "WebSettings.setSafeBrowsingEnabled(false) disables Safe Browsing protections.",
            recommendation: "Leave SafeBrowsing enabled (default) unless there is a strong product reason.",
            cwe: Some("CWE-693"),
        },
        "flag_secure" => CategoryMeta {
            title: "FLAG_SECURE window flag",
            severity: "info",
            message: "WindowManager.LayoutParams.FLAG_SECURE is set (screenshot / recents protection inventory).",
            recommendation: "Apply FLAG_SECURE on screens that show secrets; verify it is not stripped in dialogs.",
            cwe: Some("CWE-200"),
        },
        "debugger_check" => CategoryMeta {
            title: "JDWP debugger attachment check",
            severity: "info",
            message: "Debug.isDebuggerConnected / waitingForDebugger is used (anti-debug inventory).",
            recommendation: "Treat JDWP checks as bypassable; combine with integrity attestation.",
            cwe: Some("CWE-919"),
        },
        "root_detection" => CategoryMeta {
            title: "Root detection heuristics",
            severity: "info",
            message: "Root manager package names or su paths are checked (resilience inventory).",
            recommendation: "Root checks are bypassable; prefer Play Integrity / attestation.",
            cwe: Some("CWE-919"),
        },
        "deeplink_query_unvalidated" => CategoryMeta {
            title: "Deep link query parameter use",
            severity: "medium",
            message: "Uri.getQueryParameter is used — validate/parse types and bounds before acting on deep-link input.",
            recommendation: "Treat deep-link params as untrusted; convert types, check ranges, and require auth for sensitive actions.",
            cwe: Some("CWE-20"),
        },
        "tracerpid_check" => CategoryMeta {
            title: "TracerPid / ptrace anti-debug",
            severity: "info",
            message: "Reads /proc/self/status TracerPid (native debugger detection inventory).",
            recommendation: "TracerPid checks are bypassable; combine with stronger integrity controls.",
            cwe: Some("CWE-919"),
        },
        "overlay_protection_api" => CategoryMeta {
            title: "Overlay protection API",
            severity: "info",
            message: "setFilterTouchesWhenObscured / setHideOverlayWindows is used (overlay defense inventory).",
            recommendation: "Protect sensitive controls; require HIDE_OVERLAY_WINDOWS where appropriate (API 31+).",
            cwe: Some("CWE-1021"),
        },
        "network_pii" => CategoryMeta {
            title: "PII sent over the network",
            severity: "medium",
            message: "HTTP client APIs co-occur with PII field names (location, email, phone, payment data).",
            recommendation: "Minimize PII in requests; declare data collection; use TLS and purpose limitation.",
            cwe: Some("CWE-359"),
        },
        "ui_password_cache" => CategoryMeta {
            title: "Password/PIN field may be cached",
            severity: "low",
            message: "EditText/hint patterns suggest password/PIN UI without a clear password inputType.",
            recommendation: "Use TYPE_TEXT_VARIATION_PASSWORD / SecureTextField so IME and autofill treat fields correctly.",
            cwe: Some("CWE-522"),
        },
        "ssl_socket_no_hostname" => CategoryMeta {
            title: "SSLSocket without hostname verification",
            severity: "high",
            message: "SSLSocket/SSLSocketFactory is used without a HostnameVerifier — hostname mismatches are not rejected (unlike HttpsURLConnection).",
            recommendation: "Call HttpsURLConnection.getDefaultHostnameVerifier().verify(host, session) after handshake, or use HttpsURLConnection/OkHttp with pinning.",
            cwe: Some("CWE-295"),
        },
        "sdk_int_check" => CategoryMeta {
            title: "Build.VERSION.SDK_INT check",
            severity: "info",
            message: "Code branches on Build.VERSION.SDK_INT (API-level inventory).",
            recommendation: "Review security-relevant branches for missing modern API protections on older SDK levels.",
            cwe: None,
        },
        "storage_integrity_hmac" => CategoryMeta {
            title: "HMAC-based storage integrity",
            severity: "medium",
            message: "Mac/HMAC is used with SharedPreferences-like storage — integrity depends on key protection.",
            recommendation: "Do not hardcode HMAC keys; bind verification to Keystore-backed secrets or platform integrity APIs.",
            cwe: Some("CWE-345"),
        },
        "compose_password_visible" => CategoryMeta {
            title: "Compose SecureTextField with visible text",
            severity: "medium",
            message: "SecureTextField is configured with TextObfuscationMode.Visible — secrets may appear in cleartext on screen / screenshots.",
            recommendation: "Use TextObfuscationMode.RevealLastTyped or Hidden for password/OTP fields.",
            cwe: Some("CWE-200"),
        },
        "allow_backup" => CategoryMeta {
            title: "Application allowBackup enabled",
            severity: "medium",
            message: "android:allowBackup=true — app data may be extractable via Backup Manager / adb backup.",
            recommendation: "Set allowBackup=false or maintain fullBackupContent/dataExtractionRules that exclude secrets.",
            cwe: Some("CWE-921"),
        },
        "exported_custom_action" => CategoryMeta {
            title: "Exported component with custom intent action",
            severity: "medium",
            message: "An exported activity/receiver declares a custom (non-standard) intent action — other apps can register competing filters.",
            recommendation: "Use explicit Intents for internal IPC; avoid custom actions on exported components.",
            cwe: Some("CWE-926"),
        },
        "native_root_detection" => CategoryMeta {
            title: "Native root artifact strings",
            severity: "info",
            message: "Native libraries embed su/root path strings (root detection inventory).",
            recommendation: "Root checks in native code remain bypassable; prefer integrity attestation.",
            cwe: Some("CWE-919"),
        },
        "manifest_debuggable" => CategoryMeta {
            title: "Application debuggable",
            severity: "high",
            message: "android:debuggable=true in the manifest enables JDWP and weakens production hardening.",
            recommendation: "Ensure release builds set android:debuggable=false (default when minify/release).",
            cwe: Some("CWE-489"),
        },
        "dangerous_permission" => CategoryMeta {
            title: "Dangerous permission declared",
            severity: "info",
            message: "Manifest declares a dangerous/runtime permission (contacts, storage, location, …).",
            recommendation: "Justify each dangerous permission; request at runtime with clear UX.",
            cwe: Some("CWE-250"),
        },
        "system_alert_window" => CategoryMeta {
            title: "SYSTEM_ALERT_WINDOW permission",
            severity: "medium",
            message: "App requests SYSTEM_ALERT_WINDOW (draw over other apps) — relevant to overlay attacks.",
            recommendation: "Only request if required; victim apps should use overlay protections on sensitive UI.",
            cwe: Some("CWE-1021"),
        },
        "network_security_config_user_ca" => CategoryMeta {
            title: "Network security config trusts user CAs",
            severity: "high",
            message: "networkSecurityConfig appears to trust user-added certificate authorities.",
            recommendation: "Trust only system (or pinned) CAs in production; disable user CAs except debug builds.",
            cwe: Some("CWE-295"),
        },
        other if other.starts_with("semgrep:") => CategoryMeta {
            title: "Semgrep rule match",
            severity: "medium",
            message: "A Semgrep-style Android rule matched this method. Review the rule message and sink for context.",
            recommendation: "Open the matched rule message and confirm whether the sink is reachable with attacker-controlled data.",
            cwe: None,
        },
        _ => CategoryMeta {
            title: "Security finding",
            severity: "medium",
            message: "A security detector flagged this method. Review the source and sink for untrusted data reaching a sensitive API.",
            recommendation: "Trace the highlighted offsets in bytecode/CFG and confirm whether untrusted input reaches the sink.",
            cwe: None,
        },
    }
}

impl VulnFinding {
    /// Build a finding with category metadata filled in (no flow path yet).
    pub fn new(
        category: &str,
        class_name: &str,
        method_name: &str,
        source_offset: Option<u32>,
        source_desc: impl Into<String>,
        sink_offset: u32,
        sink_desc: impl Into<String>,
    ) -> Self {
        Self::with_flow(
            category,
            class_name,
            method_name,
            source_offset,
            None,
            source_desc,
            sink_offset,
            None,
            sink_desc,
            Vec::new(),
            Vec::new(),
        )
    }

    /// Build a finding including registers, evidence path, and analysis fields.
    pub fn with_flow(
        category: &str,
        class_name: &str,
        method_name: &str,
        source_offset: Option<u32>,
        source_reg: Option<u32>,
        source_desc: impl Into<String>,
        sink_offset: u32,
        sink_reg: Option<u32>,
        sink_desc: impl Into<String>,
        trace: Vec<VulnTraceStep>,
        evidence_offsets: Vec<u32>,
    ) -> Self {
        let meta = category_meta(category);
        let source_desc = source_desc.into();
        let sink_desc = sink_desc.into();
        let message =
            format_finding_message(meta, class_name, method_name, &source_desc, &sink_desc);
        let problem = format_problem(
            class_name,
            method_name,
            source_offset,
            source_reg,
            &source_desc,
            sink_offset,
            sink_reg,
            &sink_desc,
        );
        let cwe = meta.cwe.map(|s| s.to_string());
        let mas = super::mas::enrich_mas(
            category,
            meta.title,
            &message,
            &problem,
            meta.recommendation,
            cwe.as_deref(),
            None,
        );
        Self {
            category: category.to_string(),
            title: meta.title.to_string(),
            severity: meta.severity.to_string(),
            message,
            problem,
            recommendation: meta.recommendation.to_string(),
            cwe,
            class_name: class_name.to_string(),
            method_name: method_name.to_string(),
            source_offset,
            source_reg,
            source_desc,
            sink_offset,
            sink_reg,
            sink_desc,
            trace,
            evidence_offsets,
            maswe: mas.maswe,
            masvs: mas.masvs,
            mastg_know: mas.mastg_know,
            mastg_best: mas.mastg_best,
        }
    }

    /// Re-apply title/severity/message/cwe/recommendation/problem after changing `category`.
    pub fn refresh_category_meta(&mut self) {
        let meta = category_meta(&self.category);
        self.title = meta.title.to_string();
        self.severity = meta.severity.to_string();
        self.cwe = meta.cwe.map(|s| s.to_string());
        self.recommendation = meta.recommendation.to_string();
        self.message = format_finding_message(
            meta,
            &self.class_name,
            &self.method_name,
            &self.source_desc,
            &self.sink_desc,
        );
        self.problem = format_problem(
            &self.class_name,
            &self.method_name,
            self.source_offset,
            self.source_reg,
            &self.source_desc,
            self.sink_offset,
            self.sink_reg,
            &self.sink_desc,
        );
        self.apply_mas_enrichment(None);
    }

    /// Attach / refresh OWASP MASWE → MASVS → MASTG links from finding text.
    pub fn apply_mas_enrichment(&mut self, rule_id: Option<&str>) {
        let mas = super::mas::enrich_mas(
            &self.category,
            &self.title,
            &self.message,
            &self.problem,
            &self.recommendation,
            self.cwe.as_deref(),
            rule_id,
        );
        self.maswe = mas.maswe;
        self.masvs = mas.masvs;
        self.mastg_know = mas.mastg_know;
        self.mastg_best = mas.mastg_best;
    }
}

fn format_finding_message(
    meta: CategoryMeta,
    class_name: &str,
    method_name: &str,
    source_desc: &str,
    sink_desc: &str,
) -> String {
    let loc = format!("{class_name}#{method_name}");
    let mut msg = meta.message.to_string();
    if !source_desc.is_empty() && !sink_desc.is_empty() {
        msg.push_str(&format!(
            " Flow: `{source}` → `{sink}` in `{loc}`.",
            source = short_method_ref(source_desc),
            sink = short_method_ref(sink_desc),
            loc = loc,
        ));
    } else if !sink_desc.is_empty() {
        msg.push_str(&format!(
            " Sink: `{sink}` in `{loc}`.",
            sink = short_method_ref(sink_desc),
            loc = loc,
        ));
    } else {
        msg.push_str(&format!(" Location: `{loc}`."));
    }
    msg
}

fn format_problem(
    class_name: &str,
    method_name: &str,
    source_offset: Option<u32>,
    source_reg: Option<u32>,
    source_desc: &str,
    sink_offset: u32,
    sink_reg: Option<u32>,
    sink_desc: &str,
) -> String {
    let loc = format!("{class_name}#{method_name}");
    let sink_bit = match sink_reg {
        Some(r) => format!(
            "`{}` (v{} @ 0x{:x})",
            short_method_ref(sink_desc),
            r,
            sink_offset
        ),
        None => format!("`{}` (@ 0x{:x})", short_method_ref(sink_desc), sink_offset),
    };
    if let (Some(soff), Some(sreg)) = (source_offset, source_reg) {
        if !source_desc.is_empty() {
            return format!(
                "In `{loc}`, untrusted value from `{src}` (v{sreg} @ 0x{soff:x}) reaches sink {sink}.",
                src = short_method_ref(source_desc),
                sink = sink_bit,
            );
        }
    }
    if !source_desc.is_empty() {
        return format!(
            "In `{loc}`, data from `{src}` reaches sink {sink}.",
            src = short_method_ref(source_desc),
            sink = sink_bit,
        );
    }
    format!(
        "In `{loc}`, dangerous API {sink} is invoked.",
        sink = sink_bit
    )
}

/// Shorten `pkg.Class.method` / `Lpkg/Class;.method` style refs for display.
fn short_method_ref(method_ref: &str) -> String {
    let s = method_ref.trim();
    if s.is_empty() {
        return s.to_string();
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        s.to_string()
    }
}

pub fn method_matches_any(method_ref: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| method_ref.contains(p))
}

fn fmt_hex(off: u32) -> String {
    format!("0x{off:x}")
}

fn insn_desc(owned: &ValueFlowAnalysisOwned, offset: u32) -> String {
    let label = owned.insn_label(offset);
    if label.is_empty() {
        fmt_hex(offset)
    } else {
        format!("{} · {label}", fmt_hex(offset))
    }
}

/// Reconstruct an ordered evidence path from seed → sink using value-flow writes/reads.
pub fn build_flow_trace(
    owned: &ValueFlowAnalysisOwned,
    analysis: &ValueFlowAnalysis<'_>,
    seed_offset: u32,
    seed_reg: u32,
    sink_offset: u32,
    source_desc: &str,
    sink_desc: &str,
) -> (Vec<VulnTraceStep>, Option<u32>, Vec<u32>) {
    let flow = analysis.value_flow_from_seed(seed_offset, seed_reg);
    let sink_reg = flow
        .reads
        .iter()
        .find(|(o, _)| *o == sink_offset)
        .map(|(_, r)| *r);

    let mut evidence: BTreeSet<u32> = BTreeSet::new();
    evidence.insert(seed_offset);
    evidence.insert(sink_offset);
    for (o, _) in &flow.writes {
        if *o >= seed_offset && *o <= sink_offset {
            evidence.insert(*o);
        }
    }
    for (o, _) in &flow.reads {
        if *o >= seed_offset && *o <= sink_offset {
            evidence.insert(*o);
        }
    }

    let mut steps: Vec<VulnTraceStep> = Vec::new();
    steps.push(VulnTraceStep {
        offset: seed_offset,
        reg: Some(seed_reg),
        kind: "source".into(),
        description: format!(
            "Source `{}` defines v{seed_reg} — {}",
            short_method_ref(source_desc),
            insn_desc(owned, seed_offset)
        ),
    });

    // Intermediate writes (propagations / copies), then intermediate uses at invokes.
    let mut mids: Vec<(u32, Option<u32>, &'static str, String)> = Vec::new();
    for &(off, reg) in &flow.writes {
        if off == seed_offset || off > sink_offset || off < seed_offset {
            continue;
        }
        let kind = if owned.invoke_method_map.contains_key(&off) {
            "invoke"
        } else {
            "propagate"
        };
        let desc = if let Some(api) = owned.invoke_method_map.get(&off) {
            format!(
                "Value copied/defined in v{reg} around `{}` — {}",
                short_method_ref(api),
                insn_desc(owned, off)
            )
        } else {
            format!("Propagate into v{reg} — {}", insn_desc(owned, off))
        };
        mids.push((off, Some(reg), kind, desc));
    }
    for &(off, reg) in &flow.reads {
        if off == sink_offset || off == seed_offset || off > sink_offset || off < seed_offset {
            continue;
        }
        if let Some(api) = owned.invoke_method_map.get(&off) {
            mids.push((
                off,
                Some(reg),
                "use",
                format!(
                    "Tainted v{reg} used as argument to `{}` — {}",
                    short_method_ref(api),
                    insn_desc(owned, off)
                ),
            ));
        }
    }
    mids.sort_by_key(|(o, _, _, _)| *o);
    let mut seen_off = BTreeSet::from([seed_offset]);
    for (off, reg, kind, description) in mids {
        if !seen_off.insert(off) {
            continue;
        }
        steps.push(VulnTraceStep {
            offset: off,
            reg,
            kind: kind.into(),
            description,
        });
    }

    steps.push(VulnTraceStep {
        offset: sink_offset,
        reg: sink_reg,
        kind: "sink".into(),
        description: match sink_reg {
            Some(r) => format!(
                "Sink `{}` uses tainted v{r} — {}",
                short_method_ref(sink_desc),
                insn_desc(owned, sink_offset)
            ),
            None => format!(
                "Sink `{}` — {}",
                short_method_ref(sink_desc),
                insn_desc(owned, sink_offset)
            ),
        },
    });

    (steps, sink_reg, evidence.into_iter().collect())
}

fn invoke_only_trace(
    owned: &ValueFlowAnalysisOwned,
    sink_offset: u32,
    sink_desc: &str,
) -> (Vec<VulnTraceStep>, Vec<u32>) {
    let step = VulnTraceStep {
        offset: sink_offset,
        reg: None,
        kind: "invoke".into(),
        description: format!(
            "Dangerous API `{}` — {}",
            short_method_ref(sink_desc),
            insn_desc(owned, sink_offset)
        ),
    };
    (vec![step], vec![sink_offset])
}

/// Generic source→sink scan: seeds from api_return_sources matching source_patterns;
/// for each seed, value_flow_from_seed; if any read is an invoke matching a sink_pattern, report.
pub fn source_sink_scan(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
    category: &str,
    source_patterns: &[&str],
    sink_patterns: &[&str],
) -> Vec<VulnFinding> {
    let seeds: Vec<(u32, u32, String)> = owned
        .api_return_sources
        .iter()
        .filter(|(_, method_ref)| method_matches_any(method_ref, source_patterns))
        .map(|&((offset, reg), ref method_ref)| (offset, reg, method_ref.clone()))
        .collect();
    let mut findings = Vec::new();
    let analysis = owned.analysis();
    for (seed_offset, seed_reg, source_api) in seeds {
        let flow = analysis.value_flow_from_seed(seed_offset, seed_reg);
        for (read_offset, _reg) in &flow.reads {
            if let Some(sink_ref) = owned.invoke_method_map.get(read_offset) {
                if method_matches_any(sink_ref, sink_patterns) {
                    let (trace, sink_reg, evidence) = build_flow_trace(
                        owned,
                        &analysis,
                        seed_offset,
                        seed_reg,
                        *read_offset,
                        &source_api,
                        sink_ref,
                    );
                    findings.push(VulnFinding::with_flow(
                        category,
                        class_name,
                        method_name,
                        Some(seed_offset),
                        Some(seed_reg),
                        source_api.clone(),
                        *read_offset,
                        sink_reg,
                        sink_ref.clone(),
                        trace,
                        evidence,
                    ));
                }
            }
        }
    }
    if is_untrusted_callback(method_name) && owned.ins_size > 1 {
        let first_param = owned
            .registers_size
            .saturating_sub(owned.ins_size)
            .saturating_add(1); // skip `this`
        let source_offset = owned
            .cfg
            .blocks
            .get(owned.cfg.entry)
            .map(|block| block.start_offset)
            .unwrap_or(0);
        let mut seen = BTreeSet::new();
        for seed_reg in first_param..owned.registers_size {
            let flow = owned.value_flow_from_entry_reg(seed_reg);
            for (read_offset, read_reg) in flow.reads {
                let Some(sink_ref) = owned.invoke_method_map.get(&read_offset) else {
                    continue;
                };
                if !method_matches_any(sink_ref, sink_patterns)
                    || !seen.insert((read_offset, read_reg))
                {
                    continue;
                }
                let source_desc = format!("callback parameter {method_name}");
                let trace = vec![
                    VulnTraceStep {
                        offset: source_offset,
                        reg: Some(seed_reg),
                        kind: "source".into(),
                        description: format!(
                            "Android callback parameter v{seed_reg} is externally controlled"
                        ),
                    },
                    VulnTraceStep {
                        offset: read_offset,
                        reg: Some(read_reg),
                        kind: "sink".into(),
                        description: format!(
                            "Sink `{}` uses callback-derived v{read_reg} — {}",
                            short_method_ref(sink_ref),
                            insn_desc(owned, read_offset)
                        ),
                    },
                ];
                findings.push(VulnFinding::with_flow(
                    category,
                    class_name,
                    method_name,
                    None,
                    Some(seed_reg),
                    source_desc,
                    read_offset,
                    Some(read_reg),
                    sink_ref.clone(),
                    trace,
                    vec![source_offset, read_offset],
                ));
            }
        }
    }
    findings
}

fn is_untrusted_callback(method_name: &str) -> bool {
    matches!(
        method_name,
        "onNewIntent"
            | "onStartCommand"
            | "onReceive"
            | "onActivityResult"
            | "query"
            | "insert"
            | "update"
            | "delete"
            | "openFile"
            | "openAssetFile"
            | "shouldOverrideUrlLoading"
            | "shouldInterceptRequest"
            | "onJsPrompt"
            | "onJsAlert"
            | "onMessage"
    )
}

/// Invoke-only scan: report every invoke whose method ref matches any of the patterns.
pub fn invoke_scan(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
    category: &str,
    patterns: &[&str],
) -> Vec<VulnFinding> {
    let mut findings = Vec::new();
    for (offset, method_ref) in &owned.invoke_method_map {
        if method_matches_any(method_ref, patterns) {
            let (trace, evidence) = invoke_only_trace(owned, *offset, method_ref);
            findings.push(VulnFinding::with_flow(
                category,
                class_name,
                method_name,
                None,
                None,
                "",
                *offset,
                None,
                method_ref.clone(),
                trace,
                evidence,
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decompile::cfg::{BlockEnd, CfgBlock, MethodCfg};
    use crate::decompile::value_flow::ValueFlowAnalysisOwned;
    use std::collections::{HashMap, HashSet};

    fn make_cfg(instruction_offsets: Vec<u32>) -> MethodCfg {
        let block = CfgBlock {
            start_offset: *instruction_offsets.first().unwrap_or(&0),
            end_offset: instruction_offsets.last().copied().unwrap_or(0) + 2,
            end: BlockEnd::Exit,
            instruction_offsets: instruction_offsets.clone(),
        };
        let mut block_by_start = HashMap::new();
        block_by_start.insert(block.start_offset, 0);
        MethodCfg {
            blocks: vec![block],
            block_by_start,
            loop_headers: HashSet::new(),
            entry: 0,
            folded_const_offsets: HashSet::new(),
        }
    }

    #[test]
    fn source_sink_finding_includes_trace_and_problem() {
        let mut rw_map = HashMap::new();
        rw_map.insert(0, (vec![], vec![0]));
        rw_map.insert(2, (vec![0], vec![1]));
        rw_map.insert(4, (vec![1], vec![]));
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(4, "android.app.Activity.startActivity".to_string());
        let mut insn_at = HashMap::new();
        insn_at.insert(0, "move-result-object v0".into());
        insn_at.insert(2, "move-object v1, v0".into());
        insn_at.insert(4, "invoke-virtual {v1}, startActivity".into());
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![0, 2, 4]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![((0, 0), "android.content.Intent.getParcelableExtra".into())],
            invoke_method_map,
            insn_at,
            registers_size: 0,
            ins_size: 0,
        };
        let findings = source_sink_scan(
            &owned,
            "com.example.Main",
            "onCreate",
            "intent_spoofing",
            &["getParcelableExtra"],
            &["startActivity"],
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert!(!f.problem.is_empty(), "problem: {}", f.problem);
        assert!(!f.recommendation.is_empty());
        assert_eq!(f.source_reg, Some(0));
        assert_eq!(f.sink_reg, Some(1));
        assert!(f.trace.len() >= 2);
        assert_eq!(f.trace.first().unwrap().kind, "source");
        assert_eq!(f.trace.last().unwrap().kind, "sink");
        assert!(f.evidence_offsets.contains(&0));
        assert!(f.evidence_offsets.contains(&4));
    }
}
