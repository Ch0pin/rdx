# Android framework attribute values

The enum and flag name/value table in `src/native_resource_attributes.rs` is
derived from Android Open Source Project framework resource declarations,
retrieved on 2026-09-23:

- https://github.com/aosp-mirror/platform_frameworks_base/blob/master/core/res/res/values/attrs_manifest.xml
- https://github.com/aosp-mirror/platform_frameworks_base/blob/master/core/res/res/values/attrs.xml (`windowSoftInputMode`)

Copyright (C) 2006 The Android Open Source Project.
Licensed under Apache License 2.0; see [LICENSE](LICENSE).

RDX extracts name/value pairs into a Rust table and uses its own formatter.
Only Android-namespace integer attributes are translated. Original string
values are preserved. Unrecognized values or residual flag bits remain numeric.
No Android SDK installation or runtime network access is required.
