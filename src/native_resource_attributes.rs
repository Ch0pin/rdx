// Android manifest enum/flag values derived from AOSP attrs_manifest.xml and attrs.xml.
// Copyright (C) 2006 The Android Open Source Project
// Licensed under the Apache License, Version 2.0.
// https://www.apache.org/licenses/LICENSE-2.0
// Sources: https://github.com/aosp-mirror/platform_frameworks_base/tree/master/core/res/res/values
// Snapshot retrieved 2026-09-23. Unknown values stay numeric.

pub(super) fn symbolic(namespace: &str, name: &str, ty: u8, value: u32) -> Option<String> {
    if namespace != "http://schemas.android.com/apk/res/android" || !matches!(ty, 0x10 | 0x11) {
        return None;
    }
    let (flags, values): (bool, &[(u32, &str)]) = match name {
        "appCategory" => (
            false,
            &[
                (0x0, "game"),
                (0x1, "audio"),
                (0x2, "video"),
                (0x3, "image"),
                (0x4, "social"),
                (0x5, "news"),
                (0x6, "maps"),
                (0x7, "productivity"),
                (0x8, "accessibility"),
            ],
        ),
        "autoRevokePermissions" => (
            false,
            &[(0x0, "allowed"), (0x1, "discouraged"), (0x2, "disallowed")],
        ),
        "colorMode" => (
            false,
            &[(0x0, "default"), (0x1, "wideColorGamut"), (0x2, "hdr")],
        ),
        "configChanges" => (
            true,
            &[
                (0x1, "mcc"),
                (0x2, "mnc"),
                (0x4, "locale"),
                (0x8, "touchscreen"),
                (0x10, "keyboard"),
                (0x20, "keyboardHidden"),
                (0x40, "navigation"),
                (0x80, "orientation"),
                (0x100, "screenLayout"),
                (0x200, "uiMode"),
                (0x400, "screenSize"),
                (0x800, "smallestScreenSize"),
                (0x1000, "density"),
                (0x2000, "layoutDirection"),
                (0x4000, "colorMode"),
                (0x8000, "grammaticalGender"),
                (0x40000000, "fontScale"),
                (0x10000000, "fontWeightAdjustment"),
                (0x80000000, "assetsPaths"),
                (0x8000000, "resourcesUnused"),
            ],
        ),
        "documentLaunchMode" => (
            false,
            &[
                (0x0, "none"),
                (0x1, "intoExisting"),
                (0x2, "always"),
                (0x3, "never"),
            ],
        ),
        "foregroundServiceType" => (
            true,
            &[
                (0x1, "dataSync"),
                (0x2, "mediaPlayback"),
                (0x4, "phoneCall"),
                (0x8, "location"),
                (0x10, "connectedDevice"),
                (0x20, "mediaProjection"),
                (0x40, "camera"),
                (0x80, "microphone"),
                (0x100, "health"),
                (0x200, "remoteMessaging"),
                (0x400, "systemExempted"),
                (0x800, "shortService"),
                (0x2000, "mediaProcessing"),
                (0x40000000, "specialUse"),
            ],
        ),
        "gwpAsanMode" => (
            false,
            &[(0xffffffff, "default"), (0x0, "never"), (0x1, "always")],
        ),
        "installLocation" => (
            false,
            &[
                (0x0, "auto"),
                (0x1, "internalOnly"),
                (0x2, "preferExternal"),
            ],
        ),
        "intentMatchingFlags" => (
            true,
            &[
                (0x1, "none"),
                (0x2, "enforceIntentFilter"),
                (0x4, "allowNullAction"),
            ],
        ),
        "launchMode" => (
            false,
            &[
                (0x0, "standard"),
                (0x1, "singleTop"),
                (0x2, "singleTask"),
                (0x3, "singleInstance"),
                (0x4, "singleInstancePerTask"),
            ],
        ),
        "lockTaskMode" => (
            false,
            &[
                (0x0, "normal"),
                (0x1, "never"),
                (0x2, "always"),
                (0x3, "if_whitelisted"),
            ],
        ),
        "memtagMode" => (
            false,
            &[
                (0xffffffff, "default"),
                (0x0, "off"),
                (0x1, "async"),
                (0x2, "sync"),
            ],
        ),
        "pageSizeCompat" => (false, &[(0x20, "enabled"), (0x40, "disabled")]),
        "permissionFlags" => (
            true,
            &[
                (0x1, "costsMoney"),
                (0x2, "removed"),
                (0x4, "hardRestricted"),
                (0x8, "softRestricted"),
                (0x10, "immutablyRestricted"),
                (0x20, "installerExemptIgnored"),
            ],
        ),
        "permissionGroupFlags" => (true, &[(0x1, "personalInfo")]),
        "persistableMode" => (
            false,
            &[
                (0x0, "persistRootOnly"),
                (0x1, "persistNever"),
                (0x2, "persistAcrossReboots"),
            ],
        ),
        "protectionLevel" => (
            true,
            &[
                (0x0, "normal"),
                (0x1, "dangerous"),
                (0x2, "signature"),
                (0x3, "signatureOrSystem"),
                (0x4, "internal"),
                (0x10, "privileged"),
                (0x10, "system"),
                (0x20, "development"),
                (0x40, "appop"),
                (0x80, "pre23"),
                (0x100, "installer"),
                (0x200, "verifier"),
                (0x400, "preinstalled"),
                (0x800, "setup"),
                (0x1000, "instant"),
                (0x2000, "runtime"),
                (0x4000, "oem"),
                (0x8000, "vendorPrivileged"),
                (0x10000, "textClassifier"),
                (0x80000, "configurator"),
                (0x100000, "incidentReportApprover"),
                (0x200000, "appPredictor"),
                (0x400000, "module"),
                (0x800000, "companion"),
                (0x1000000, "retailDemo"),
                (0x2000000, "recents"),
                (0x4000000, "role"),
                (0x8000000, "knownSigner"),
            ],
        ),
        "recreateOnConfigChanges" => (true, &[(0x1, "mcc"), (0x2, "mnc")]),
        "reqKeyboardType" => (
            false,
            &[
                (0x0, "undefined"),
                (0x1, "nokeys"),
                (0x2, "qwerty"),
                (0x3, "twelvekey"),
            ],
        ),
        "reqNavigation" => (
            false,
            &[
                (0x0, "undefined"),
                (0x1, "nonav"),
                (0x2, "dpad"),
                (0x3, "trackball"),
                (0x4, "wheel"),
            ],
        ),
        "reqTouchScreen" => (
            false,
            &[
                (0x0, "undefined"),
                (0x1, "notouch"),
                (0x2, "stylus"),
                (0x3, "finger"),
            ],
        ),
        "requireContentUriPermissionFromCaller" => (
            false,
            &[
                (0x0, "none"),
                (0x1, "read"),
                (0x2, "write"),
                (0x3, "readOrWrite"),
                (0x4, "readAndWrite"),
            ],
        ),
        "rollbackDataPolicy" => (false, &[(0x0, "restore"), (0x1, "wipe"), (0x2, "retain")]),
        "rotationAnimation" => (
            true,
            &[
                (0x0, "rotate"),
                (0x1, "crossfade"),
                (0x2, "jumpcut"),
                (0x3, "seamless"),
            ],
        ),
        "screenDensity" => (
            false,
            &[
                (0x78, "ldpi"),
                (0xa0, "mdpi"),
                (0xf0, "hdpi"),
                (0x140, "xhdpi"),
                (0x1e0, "xxhdpi"),
                (0x280, "xxxhdpi"),
            ],
        ),
        "screenOrientation" => (
            false,
            &[
                (0xffffffff, "unspecified"),
                (0x0, "landscape"),
                (0x1, "portrait"),
                (0x2, "user"),
                (0x3, "behind"),
                (0x4, "sensor"),
                (0x5, "nosensor"),
                (0x6, "sensorLandscape"),
                (0x7, "sensorPortrait"),
                (0x8, "reverseLandscape"),
                (0x9, "reversePortrait"),
                (0xa, "fullSensor"),
                (0xb, "userLandscape"),
                (0xc, "userPortrait"),
                (0xd, "fullUser"),
                (0xe, "locked"),
            ],
        ),
        "screenSize" => (
            false,
            &[
                (0xc8, "small"),
                (0x12c, "normal"),
                (0x190, "large"),
                (0x1f4, "xlarge"),
            ],
        ),
        "uiOptions" => (true, &[(0x0, "none"), (0x1, "splitActionBarWhenNarrow")]),
        "usesPermissionFlags" => (true, &[(0x10000, "neverForLocation")]),
        "windowSoftInputMode" => (
            true,
            &[
                (0x0, "stateUnspecified"),
                (0x1, "stateUnchanged"),
                (0x2, "stateHidden"),
                (0x3, "stateAlwaysHidden"),
                (0x4, "stateVisible"),
                (0x5, "stateAlwaysVisible"),
                (0x0, "adjustUnspecified"),
                (0x10, "adjustResize"),
                (0x20, "adjustPan"),
                (0x30, "adjustNothing"),
            ],
        ),
        _ => return None,
    };
    if let Some((_, label)) = values.iter().find(|(v, _)| *v == value) {
        return Some((*label).into());
    }
    if !flags {
        return None;
    }
    let mut remaining = value;
    let mut parts = Vec::new();
    // These fields contain mutually exclusive enums inside a flag word.
    // Do not decompose signatureOrSystem (3) into dangerous|signature.
    let masks: &[u32] = match name {
        "protectionLevel" => &[0xf],
        "windowSoftInputMode" => &[0xf, 0xf0],
        _ => &[],
    };
    for mask in masks {
        let group = remaining & mask;
        if group != 0 {
            let (_, label) = values.iter().find(|(v, _)| *v == group)?;
            parts.push(*label);
        }
        remaining &= !mask;
    }
    // Prefer composite flags over their constituents; retain declaration order on ties.
    let mut candidates: Vec<_> = values
        .iter()
        .filter(|(v, _)| *v != 0 && !masks.iter().any(|mask| v & mask != 0))
        .collect();
    candidates.sort_by_key(|(v, _)| std::cmp::Reverse(v.count_ones()));
    for (bits, label) in candidates {
        if remaining & bits == *bits {
            parts.push(*label);
            remaining &= !bits;
        }
    }
    // Never discard unrecognized bits or guess enum names.
    (remaining == 0 && !parts.is_empty()).then(|| parts.join("|"))
}

#[cfg(test)]
mod tests {
    use super::symbolic;
    const ANDROID: &str = "http://schemas.android.com/apk/res/android";
    #[test]
    fn enums_and_flags_preserve_exact_values() {
        assert_eq!(
            symbolic(ANDROID, "protectionLevel", 0x11, 2).as_deref(),
            Some("signature")
        );
        assert_eq!(
            symbolic(ANDROID, "protectionLevel", 0x10, 0x12).as_deref(),
            Some("signature|privileged")
        );
        assert_eq!(
            symbolic(ANDROID, "protectionLevel", 0x11, 3).as_deref(),
            Some("signatureOrSystem")
        );
        assert_eq!(
            symbolic(ANDROID, "protectionLevel", 0x11, 0).as_deref(),
            Some("normal")
        );
        assert_eq!(
            symbolic(ANDROID, "windowSoftInputMode", 0x11, 0x12).as_deref(),
            Some("stateHidden|adjustResize")
        );
        assert_eq!(
            symbolic(ANDROID, "launchMode", 0x10, 2).as_deref(),
            Some("singleTask")
        );
        let flags = symbolic(ANDROID, "configChanges", 0x11, 0x6fa0).unwrap();
        assert!(flags.contains("orientation"));
        assert!(flags.contains("screenSize"));
    }
    #[test]
    fn unknown_bits_and_non_android_or_non_integer_values_stay_numeric() {
        assert!(symbolic(ANDROID, "protectionLevel", 0x11, 0x80000002).is_none());
        assert!(symbolic(ANDROID, "protectionLevel", 0x11, 0xf).is_none());
        assert!(symbolic(ANDROID, "windowSoftInputMode", 0x11, 0x17).is_none());
        assert!(symbolic("urn:custom", "protectionLevel", 0x11, 2).is_none());
        assert!(symbolic(ANDROID, "protectionLevel", 3, 2).is_none());
        assert!(symbolic(ANDROID, "protectionLevel", 1, 2).is_none());
    }
}
