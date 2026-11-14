//! API misuse detectors for common Android security issues:
//! insecure RNG, external storage writes, SafeBrowsing disabled, sensitive
//! notifications, explicit crypto providers, debugger/root checks, deeplink
//! query params, SSLSocket without hostname verification, SDK_INT inventory,
//! HMAC prefs integrity, Compose password visibility, etc.

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, VulnFinding};

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

fn blob_has(blob: &str, needles: &[&str]) -> bool {
    let l = blob.to_ascii_lowercase();
    needles.iter().any(|n| l.contains(&n.to_ascii_lowercase()))
}

/// java.util.Random / Math.random used for security-relevant randomness.
pub fn scan_insecure_random(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "insecure_random",
        &[
            "java.util.Random.<init>",
            "java.util.Random.next",
            "Random.nextInt",
            "Random.nextDouble",
            "Random.nextBytes",
            "Math.random",
        ],
    );
    // Also catch `new Random()` when only <init> resolves oddly
    if out.is_empty() {
        let inv = invoke_blob(owned);
        if inv.contains("java.util.Random") || inv.contains("Math.random") {
            out.extend(invoke_scan(
                owned,
                class_name,
                method_name,
                "insecure_random",
                &["Random", "Math.random"],
            ));
        }
    }
    out
}

/// Date/Calendar used as entropy (non-cryptographic token sources).
pub fn scan_non_random_source(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let has_date = inv.contains("java.util.Date") || inv.contains("Calendar");
    if !has_date {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "non_random_source",
        &[
            "Date.getTime",
            "Calendar.get",
            "Calendar.getTimeInMillis",
            "System.currentTimeMillis",
        ],
    )
}

/// Writes to shared/external storage (getExternalStorageDirectory / MediaStore EXTERNAL).
pub fn scan_external_storage_write(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "external_storage_write",
        &[
            "Environment.getExternalStorageDirectory",
            "getExternalStorageDirectory",
            "getExternalFilesDir",
            "getExternalStoragePublicDirectory",
            "MediaStore.Downloads.EXTERNAL_CONTENT_URI",
            "MediaStore.Images.Media.EXTERNAL_CONTENT_URI",
            "MediaStore.Files.getContentUri",
        ],
    );
    // ContentResolver.insert toward MediaStore when EXTERNAL markers in method
    if out.is_empty() {
        let insns = insn_blob(owned);
        let inv = invoke_blob(owned);
        if (blob_has(&insns, &["EXTERNAL_CONTENT_URI", "DIRECTORY_DOWNLOADS", "MediaStore"])
            || blob_has(&inv, &["MediaStore"]))
            && inv.contains("insert")
        {
            out.extend(invoke_scan(
                owned,
                class_name,
                method_name,
                "external_storage_write",
                &["ContentResolver.insert", "insert"],
            ));
        }
    }
    out
}

/// Security.addProvider / insertProviderAt (explicit crypto provider).
pub fn scan_explicit_security_provider(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    invoke_scan(
        owned,
        class_name,
        method_name,
        "explicit_security_provider",
        &[
            "Security.addProvider",
            "Security.insertProviderAt",
            "addProvider",
            "insertProviderAt",
        ],
    )
}

/// Notification builders co-located with PII / secret-like strings.
pub fn scan_notification_sensitive(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let has_notif = inv.contains("Notification") || inv.contains("notify");
    if !has_notif {
        return Vec::new();
    }
    let insns = insn_blob(owned);
    if !blob_has(
        &insns,
        &[
            "john doe",
            "sensitive",
            "password",
            "ssn",
            "credit",
            "token",
            "secret",
            "otp",
            "pii",
            "@",
        ],
    ) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "notification_sensitive",
        &[
            "Notification.Builder",
            "NotificationCompat.Builder",
            "setContentText",
            "setContentTitle",
            "NotificationManager.notify",
            "notify",
        ],
    )
}

/// WebView settings.safeBrowsingEnabled = false.
pub fn scan_safebrowsing_disabled(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "safebrowsing_disabled",
        &[
            "setSafeBrowsingEnabled",
            "safeBrowsingEnabled",
            "WebSettings.setSafeBrowsingEnabled",
        ],
    );
    // Boolean false co-located: if invoke present keep; if only field write, still flag set*
    if out.is_empty() {
        let blob = format!("{}\n{}", insn_blob(owned), invoke_blob(owned)).to_ascii_lowercase();
        if blob.contains("safebrowsing") && (blob.contains("const/4") || blob.contains(", 0")) {
            out.extend(invoke_scan(
                owned,
                class_name,
                method_name,
                "safebrowsing_disabled",
                &["setSafeBrowsingEnabled", "WebSettings"],
            ));
        }
    }
    out
}

/// WindowManager.LayoutParams.FLAG_SECURE / addFlags inventory.
pub fn scan_flag_secure(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let insns = insn_blob(owned);
    let inv = invoke_blob(owned);
    if !(blob_has(&insns, &["FLAG_SECURE"])
        || (inv.contains("addFlags") && blob_has(&insns, &["FLAG_SECURE", "8192"])))
    {
        // 8192 == FLAG_SECURE; still require addFlags/setFlags
        if !(inv.contains("addFlags") || inv.contains("setFlags")) {
            return Vec::new();
        }
        if !has_const(&insns, 8192) && !blob_has(&insns, &["FLAG_SECURE"]) {
            return Vec::new();
        }
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "flag_secure",
        &["addFlags", "setFlags", "Window.setFlags"],
    )
}

fn has_const(blob: &str, val: u32) -> bool {
    for line in blob.lines() {
        let t = line.trim();
        if !(t.starts_with("const/") || t.starts_with("const ")) {
            continue;
        }
        if let Some(rhs) = t.rsplit(',').next() {
            if rhs.trim().parse::<u32>().ok() == Some(val) {
                return true;
            }
        }
    }
    false
}

/// Debug.isDebuggerConnected / waitingForDebugger.
pub fn scan_debugger_check(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    invoke_scan(
        owned,
        class_name,
        method_name,
        "debugger_check",
        &[
            "Debug.isDebuggerConnected",
            "Debug.waitingForDebugger",
            "isDebuggerConnected",
        ],
    )
}

/// Root manager package strings or su paths (resilience inventory).
pub fn scan_root_detection(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let insns = insn_blob(owned);
    let rootish = blob_has(
        &insns,
        &[
            "magisk",
            "supersu",
            "kernelsu",
            "com.noshufou.android.su",
            "/system/bin/su",
            "/system/xbin/su",
            "which su",
            "test-keys",
        ],
    );
    if !rootish {
        return Vec::new();
    }
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "root_detection",
        &[
            "PackageManager.getPackageInfo",
            "getPackageInfo",
            "Runtime.exec",
            "File.exists",
            "loadLibrary",
        ],
    );
    if out.is_empty() {
        // Strings alone are enough for inventory (often in <init>, checks elsewhere)
        out.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "root_detection",
            &["<init>", "getPackageInfo", "exists", "exec"],
        ));
    }
    if out.is_empty() {
        // Last resort: any invoke in the method
        if let Some((&off, method_ref)) = owned.invoke_method_map.iter().next() {
            out.push(VulnFinding::new(
                "root_detection",
                class_name,
                method_name,
                None,
                "root package/path string",
                off,
                method_ref.clone(),
            ));
        }
    }
    out
}

/// Deep link query params used without obvious validation.
pub fn scan_deeplink_query(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    if !(inv.contains("getQueryParameter") || inv.contains("getQueryParameters")) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "deeplink_query_unvalidated",
        &["getQueryParameter", "getQueryParameters", "Uri.getQueryParameter"],
    )
}

/// TracerPid / /proc/self/status anti-debug.
pub fn scan_tracerpid_check(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let insns = insn_blob(owned);
    if !blob_has(&insns, &["TracerPid", "/proc/self/status", "proc/self/status"]) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "tracerpid_check",
        &["FileInputStream", "BufferedReader", "open", "readLine", "loadLibrary"],
    )
}

/// Overlay protection APIs (setFilterTouchesWhenObscured / setHideOverlayWindows).
pub fn scan_overlay_protection_api(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    invoke_scan(
        owned,
        class_name,
        method_name,
        "overlay_protection_api",
        &[
            "setFilterTouchesWhenObscured",
            "setHideOverlayWindows",
            "filterTouchesWhenObscured",
        ],
    )
}

/// HttpURLConnection / OkHttp POST co-located with PII field names,
/// or network write APIs (PII may live in a sibling method/lambda).
pub fn scan_network_pii(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let insns = insn_blob(owned);
    let has_net = inv.contains("HttpURLConnection")
        || inv.contains("openConnection")
        || inv.contains("OkHttp")
        || inv.contains("Request.Builder")
        || inv.contains("URL.<init>");
    let has_pii = blob_has(
        &insns,
        &[
            "email",
            "phone",
            "credit_card",
            "latitude",
            "longitude",
            "ssn",
            "password",
            "precise_location",
            "john.doe",
            "email_address",
            "phone_number",
        ],
    );
    let has_postish = inv.contains("getOutputStream")
        || inv.contains("doOutput")
        || blob_has(&insns, &["application/x-www-form-urlencoded", "POST"]);
    if !((has_net && (has_pii || has_postish)) || (has_pii && inv.contains("Thread"))) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "network_pii",
        &[
            "HttpURLConnection",
            "openConnection",
            "getOutputStream",
            "URL.<init>",
            "Thread.<init>",
            "Thread.start",
            "write",
            "OkHttpClient",
        ],
    )
}

/// EditText without password variation while hints mention password/PIN (UI cache risk).
pub fn scan_ui_password_cache(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    if !(inv.contains("EditText") || inv.contains("setInputType") || inv.contains("setHint")) {
        return Vec::new();
    }
    let insns = insn_blob(owned);
    if !blob_has(&insns, &["password", "pin", "cached"]) {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "ui_password_cache",
        &["EditText.<init>", "setInputType", "setHint", "EditText"],
    )
}

/// SSLSocket createSocket/startHandshake without HostnameVerifier in the method.
pub fn scan_ssl_socket_no_hostname(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let uses_ssl_socket = inv.contains("SSLSocket") || inv.contains("SSLSocketFactory");
    if !uses_ssl_socket {
        return Vec::new();
    }
    let has_handshake = inv.contains("startHandshake") || inv.contains("createSocket");
    if !has_handshake {
        return Vec::new();
    }
    let has_hv = inv.contains("HostnameVerifier")
        || inv.contains("setHostnameVerifier")
        || inv.contains("getDefaultHostnameVerifier");
    if has_hv {
        return Vec::new();
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "ssl_socket_no_hostname",
        &[
            "SSLSocketFactory.createSocket",
            "SSLSocket.startHandshake",
            "createSocket",
            "startHandshake",
        ],
    )
}

/// Build.VERSION.SDK_INT reads (API-level branching inventory).
pub fn scan_sdk_int_check(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let blob = format!("{}\n{}", insn_blob(owned), invoke_blob(owned));
    if !(blob.contains("SDK_INT") || blob.contains("Build$VERSION") || blob.contains("VERSION.SDK_INT"))
    {
        return Vec::new();
    }
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "sdk_int_check",
        &["Build$VERSION", "SDK_INT"],
    );
    if out.is_empty() {
        // field sget may only appear in insn text
        if let Some((&off, method_ref)) = owned.invoke_method_map.iter().next() {
            out.push(VulnFinding::new(
                "sdk_int_check",
                class_name,
                method_name,
                None,
                "Build.VERSION.SDK_INT",
                off,
                method_ref.clone(),
            ));
        } else if let Some((&off, _)) = owned.insn_at.iter().next() {
            out.push(VulnFinding::new(
                "sdk_int_check",
                class_name,
                method_name,
                None,
                "Build.VERSION.SDK_INT",
                off,
                "SDK_INT",
            ));
        }
    }
    out
}

/// Mac/HMAC used with SharedPreferences (storage integrity scheme).
pub fn scan_storage_integrity_hmac(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let insns = insn_blob(owned);
    let has_mac = inv.contains("Mac.getInstance")
        || inv.contains("javax.crypto.Mac")
        || blob_has(&insns, &["HmacSHA", "HMAC"]);
    let has_prefs = inv.contains("SharedPreferences")
        || inv.contains("getSharedPreferences")
        || inv.contains("edit")
        || blob_has(&insns, &["user_role", "hmac", "shared_prefs", "app_settings"]);
    if !(has_mac && has_prefs) {
        // Also flag Mac + SecretKeySpec when role/hmac markers present
        if !(has_mac && blob_has(&insns, &["hmac", "user_role", "integrity", "tamper"])) {
            return Vec::new();
        }
    }
    invoke_scan(
        owned,
        class_name,
        method_name,
        "storage_integrity_hmac",
        &[
            "Mac.getInstance",
            "Mac.doFinal",
            "SecretKeySpec.<init>",
            "getSharedPreferences",
            "SharedPreferences",
        ],
    )
}

/// Compose SecureTextField with Visible obfuscation mode (password shown).
pub fn scan_compose_password_visible(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let inv = invoke_blob(owned);
    let insns = insn_blob(owned);
    let has_secure_tf = inv.contains("SecureTextField")
        || blob_has(&insns, &["SecureTextField"]);
    if !has_secure_tf {
        return Vec::new();
    }
    let visible = blob_has(&insns, &["Visible", "getVisible", "TextObfuscationMode"]);
    if !visible {
        return Vec::new();
    }
    let mut out = invoke_scan(
        owned,
        class_name,
        method_name,
        "compose_password_visible",
        &["SecureTextField", "TextObfuscationMode"],
    );
    if out.is_empty() {
        if let Some((&off, method_ref)) = owned.invoke_method_map.iter().next() {
            out.push(VulnFinding::new(
                "compose_password_visible",
                class_name,
                method_name,
                None,
                "SecureTextField + Visible",
                off,
                method_ref.clone(),
            ));
        }
    }
    out
}

pub fn scan_api_misuse(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut out = Vec::new();
    out.extend(scan_insecure_random(owned, class_name, method_name));
    out.extend(scan_non_random_source(owned, class_name, method_name));
    out.extend(scan_external_storage_write(owned, class_name, method_name));
    out.extend(scan_explicit_security_provider(owned, class_name, method_name));
    out.extend(scan_notification_sensitive(owned, class_name, method_name));
    out.extend(scan_safebrowsing_disabled(owned, class_name, method_name));
    out.extend(scan_flag_secure(owned, class_name, method_name));
    out.extend(scan_debugger_check(owned, class_name, method_name));
    out.extend(scan_root_detection(owned, class_name, method_name));
    out.extend(scan_deeplink_query(owned, class_name, method_name));
    out.extend(scan_tracerpid_check(owned, class_name, method_name));
    out.extend(scan_overlay_protection_api(owned, class_name, method_name));
    out.extend(scan_network_pii(owned, class_name, method_name));
    out.extend(scan_ui_password_cache(owned, class_name, method_name));
    out.extend(scan_ssl_socket_no_hostname(owned, class_name, method_name));
    out.extend(scan_sdk_int_check(owned, class_name, method_name));
    out.extend(scan_storage_integrity_hmac(owned, class_name, method_name));
    out.extend(scan_compose_password_visible(owned, class_name, method_name));
    out
}
