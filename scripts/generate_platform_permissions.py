#!/usr/bin/env python3
"""Rebuild the pinned AOSP permission snapshot; reject unexpected source bytes."""
import hashlib
import json
from pathlib import Path
from urllib.request import urlopen
import xml.etree.ElementTree as ET

TAG = "android-16.0.0_r1"
SOURCE = f"https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/{TAG}/core/res/AndroidManifest.xml"
SHA256 = "69ce991d47d0cc259c16c2f51933cd658ec5371bdb6c22dde7ca21376312fe5e"

def main():
    with urlopen(SOURCE, timeout=60) as response:
        source = response.read()
    if hashlib.sha256(source).hexdigest() != SHA256:
        raise SystemExit("Pinned manifest SHA256 mismatch; catalog left unchanged")
    root = ET.fromstring(source)
    android = "{http://schemas.android.com/apk/res/android}"
    permissions = {
        permission.attrib[android + "name"]:
            permission.get(android + "protectionLevel", "normal")
        for permission in root.findall("permission")
    }
    destination = Path(__file__).resolve().parents[1] / "src/platform_permissions_android16.json"
    destination.write_text(json.dumps(permissions, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"Wrote {len(permissions)} permission declarations to {destination}")

if __name__ == "__main__":
    main()
