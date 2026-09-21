#!/usr/bin/env python3
"""Generate original, dependency-free DEX/APK fixtures; no downloaded app code."""
import hashlib
from pathlib import Path
import struct
import zipfile
import zlib


def uleb(value):
    out = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        out.append(byte | (128 if value else 0))
        if not value:
            return out


def generate():
    strings = ["I", "Ljava/lang/Object;", "Lsample/Hello;", "answer"]
    string_ids = 112
    type_ids = string_ids + 4 * len(strings)
    proto_ids = type_ids + 4 * 3
    method_ids = proto_ids + 12
    class_defs = method_ids + 8
    data_off = class_defs + 32
    data = bytearray(data_off)
    offsets = []
    for value in strings:
        offsets.append(len(data))
        data.extend(uleb(len(value)) + value.encode() + b"\0")
    while len(data) % 4:
        data.append(0)
    code_off = len(data)
    # registers=1; no parameters; const/16 v0, 42; return v0.
    data.extend(struct.pack("<HHHHII3H", 1, 0, 0, 0, 0, 3, 0x0013, 42, 0x000f))
    class_data_off = len(data)
    data.extend(bytes([0, 0, 1, 0, 0, 9]) + uleb(code_off))
    while len(data) % 4:
        data.append(0)
    map_off = len(data)
    entries = [(0, 1, 0), (1, 4, string_ids), (2, 3, type_ids),
               (3, 1, proto_ids), (5, 1, method_ids), (6, 1, class_defs),
               (0x2002, 4, offsets[0]), (0x2001, 1, code_off),
               (0x2000, 1, class_data_off), (0x1000, 1, map_off)]
    data.extend(struct.pack("<I", len(entries)))
    for kind, count, offset in entries:
        data.extend(struct.pack("<HHII", kind, 0, count, offset))
    struct.pack_into("<4I", data, string_ids, *offsets)
    struct.pack_into("<3I", data, type_ids, 0, 1, 2)
    struct.pack_into("<3I", data, proto_ids, 0, 0, 0)
    struct.pack_into("<HHI", data, method_ids, 2, 0, 3)
    struct.pack_into("<8I", data, class_defs, 2, 1, 1, 0, 0xffffffff, 0, class_data_off, 0)
    data[:8] = b"dex\n035\0"
    struct.pack_into("<20I", data, 32, len(data), 112, 0x12345678, 0, 0,
                     map_off, 4, string_ids, 3, type_ids, 1, proto_ids,
                     0, 0, 1, method_ids, 1, class_defs, len(data)-data_off, data_off)
    data[12:32] = hashlib.sha1(data[32:]).digest()
    struct.pack_into("<I", data, 8, zlib.adler32(data[12:]) & 0xffffffff)
    destination = Path(__file__).resolve().parent.parent / "tests/fixtures"
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "hello.dex").write_bytes(data)
    # ZIP-based APK input fixture, deliberately not an installable application.
    with zipfile.ZipFile(destination / "hello.apk", "w") as archive:
        info = zipfile.ZipInfo("classes.dex", date_time=(2020, 1, 1, 0, 0, 0))
        info.external_attr = 0o644 << 16
        archive.writestr(info, data)
    print(f"Generated {len(data)}-byte DEX and APK archive in {destination}")


if __name__ == "__main__":
    generate()
