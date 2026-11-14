//! Trust-all SSL: HostnameVerifier / TrustManager / OkHttp hostnameVerifier /
//! WebView SslErrorHandler.proceed.

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, VulnFinding};

const PATTERNS: &[&str] = &[
    "setHostnameVerifier",
    "hostnameVerifier",
    "HostnameVerifier.verify",
    "checkServerTrusted",
    "X509TrustManager",
    "SSLContext.init",
    "OkHostnameVerifier",
];

fn trust_all_hints(owned: &ValueFlowAnalysisOwned) -> bool {
    owned.insn_at.values().any(|s| {
        let u = s.to_uppercase();
        u.contains("ALLOW_ALL")
            || u.contains("TRUST_ALL")
            || u.contains("TRUSTALL")
            || u.contains("DO_NOT_VERIFY")
            || u.contains("NULLHOSTNAMEVERIFIER")
            || (u.contains("RETURN") && u.contains("TRUE") && u.contains("VERIFY"))
    }) || owned
        .invoke_method_map
        .values()
        .any(|m| m.contains("NullHostnameVerifier") || m.contains("AllowAllHostnameVerifier"))
}

pub fn scan_ssl_trust_all(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let hints = trust_all_hints(owned);
    let mut out = invoke_scan(owned, class_name, method_name, "ssl_trust_all", PATTERNS);
    if hints && out.is_empty() {
        out.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "ssl_trust_all",
            &["verify", "checkServerTrusted", "setHostnameVerifier"],
        ));
    }
    // Without allow-all hints, only keep explicit verifier installs (not bare SSLContext.init).
    if !hints {
        out.retain(|f| {
            f.sink_desc.contains("setHostnameVerifier")
                || f.sink_desc.contains("hostnameVerifier")
                || f.sink_desc.contains("NullHostnameVerifier")
                || f.sink_desc.contains("AllowAllHostnameVerifier")
        });
    }
    // WebView ignores TLS errors via SslErrorHandler.proceed
    let has_ssl_error = owned.invoke_method_map.values().any(|m| {
        m.contains("onReceivedSslError") || m.contains("SslErrorHandler")
    }) || owned.insn_at.values().any(|s| {
        let u = s.to_ascii_lowercase();
        u.contains("onreceivedsslerror") || u.contains("sslerrorhandler")
    });
    if has_ssl_error {
        let proceeds = invoke_scan(
            owned,
            class_name,
            method_name,
            "ssl_trust_all",
            &["SslErrorHandler.proceed"],
        );
        if proceeds.is_empty() {
            // Fallback: proceed invoke when SslErrorHandler type is in the method
            out.extend(
                invoke_scan(
                    owned,
                    class_name,
                    method_name,
                    "ssl_trust_all",
                    &["proceed"],
                )
                .into_iter()
                .filter(|f| f.sink_desc.contains("SslErrorHandler") || f.sink_desc.ends_with("proceed")),
            );
        } else {
            out.extend(proceeds);
        }
    }
    out.sort_by_key(|f| (f.sink_offset, f.sink_desc.clone()));
    out.dedup_by_key(|f| f.sink_offset);
    out
}
