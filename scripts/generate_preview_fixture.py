#!/usr/bin/env python3
"""Generate an original, deterministic GUI preview APK with Python standard library.

Uses existing hello.dex without modifying it. This ZIP fixture is not installable.
Run from any directory: python3 scripts/generate_preview_fixture.py
"""
from pathlib import Path
import struct
import zipfile
import zlib


def png():
    def chunk(kind, payload):
        return (struct.pack('>I', len(payload)) + kind + payload
                + struct.pack('>I', zlib.crc32(kind + payload) & 0xffffffff))
    width, height = 32, 24
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend((x * 8, y * 10, 180, 255))
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b''))


def generate():
    root = Path(__file__).resolve().parent.parent
    entries = {
        'classes.dex': (root / 'tests/fixtures/hello.dex').read_bytes(),
        'AndroidManifest.xml': '''<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="sample.preview"><application android:label="RDX preview fixture" /></manifest>
''',
        'assets/config.json': '{\n  "name": "RDX preview",\n  "enabled": true,\n  "greeting": "Hello Ελληνικά 🦀"\n}\n',
        'assets/web/app.js': 'const title = "RDX preview";\nfunction greet(name) {\n  return `Hello ${name}`;\n}\n',
        'assets/web/index.html': '<!doctype html>\n<html><head><title>RDX preview</title></head>\n<body><h1>Readable HTML source</h1></body></html>\n',
        'assets/web/style.css': 'body {\n  color: #123456;\n  background: #fafafa;\n}\n',
        'assets/text/readme.txt': 'RDX asset previews\nUTF-8: Ελληνικά 日本語 🦀\n',
        'assets/text/utf16-le.txt': b'\xff\xfe' + 'UTF-16 LE: Ελληνικά 🦀\n'.encode('utf-16-le'),
        'assets/text/utf16-be.txt': b'\xfe\xff' + 'UTF-16 BE: 日本語 🦀\n'.encode('utf-16-be'),
        'assets/text/utf8-bom.txt': b'\xef\xbb\xbf' + b'UTF-8 BOM preview\n',
        'assets/settings.yaml': 'name: RDX preview\nenabled: true\ntags:\n  - android\n  - assets\n',
        'assets/settings.properties': 'app.name=RDX preview\napp.enabled=true\n',
        'assets/settings.ini': '[preview]\nenabled=true\n',
        'assets/table.csv': 'name,value\nalpha,1\nbeta,2\n',
        'assets/images/gradient.png': png(),
        'assets/images/icon.svg': '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32">\n  <circle cx="16" cy="16" r="12" fill="teal"/>\n</svg>\n',
        'assets/data/sample.bin': bytes(range(256)),
        'assets/nested/folder/notes.md': '# Nested asset\n\nThis path preserves the original APK hierarchy.\n',
        'res/layout/main.xml': '<LinearLayout xmlns:android="http://schemas.android.com/apk/res/android"\n    android:layout_width="match_parent" android:layout_height="match_parent">\n    <TextView android:layout_width="wrap_content" android:layout_height="wrap_content"\n        android:text="RDX preview" />\n</LinearLayout>\n',
        'META-INF/MANIFEST.MF': 'Manifest-Version: 1.0\nCreated-By: RDX fixture generator\n',
        'NOTICE.txt': 'Original RDX GUI preview fixture; not an installable Android application.\n',
    }
    destination = root / 'tests/fixtures/preview.apk'
    with zipfile.ZipFile(destination, 'w') as archive:
        for name, data in entries.items():
            info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, data.encode('utf-8') if isinstance(data, str) else data)
    print(f'Generated {destination} ({len(entries)} entries, {destination.stat().st_size} bytes)')


if __name__ == '__main__':
    generate()
