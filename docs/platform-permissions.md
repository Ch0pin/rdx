# Platform permission catalog

[`src/platform_permissions_android16.json`](../src/platform_permissions_android16.json) contains 1,029 permission names and their original protection-level expressions from AOSP Android 16, API 36, tag `android-16.0.0_r1`.

Source: https://android.googlesource.com/platform/frameworks/base/+/android-16.0.0_r1/core/res/AndroidManifest.xml

Source file SHA256: `69ce991d47d0cc259c16c2f51933cd658ec5371bdb6c22dde7ca21376312fe5e`.

The source manifest is Copyright The Android Open Source Project, licensed under Apache License 2.0: https://www.apache.org/licenses/LICENSE-2.0 . The generated data preserves permission names and protection levels; absent levels use Android's `normal` default.

Rebuild with `python3 scripts/generate_platform_permissions.py` from the repository root. The script downloads the pinned AOSP mirror source, verifies its SHA256, and writes deterministic sorted JSON.

The catalog classifies declarations against this versioned snapshot. It does not identify the connected device's installed permission definitions, grant state, or runtime reachability. App declarations with conflicting platform protection levels remain unresolved; matching declarations use platform provenance. Numeric flags and symbolic aliases are normalized for that comparison.
