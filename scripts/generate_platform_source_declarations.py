"""Generate complete Java source declaration facts from one pinned android.jar.

The output is proof-only metadata. It records every class and every declared
method, including declarations with an empty Exceptions attribute.
"""

import argparse
import hashlib
import json
import struct
import zipfile


MAX_ARCHIVE_BYTES = 768 * 1024 * 1024
MAX_CLASS_BYTES = 16 * 1024 * 1024
MAX_CLASSES = 100_000
MAX_METHODS = 2_000_000
MAX_ATTRIBUTES = 1_000_000


class Reader:
    def __init__(self, data):
        self.data = data
        self.at = 0

    def take(self, size):
        end = self.at + size
        if size < 0 or end > len(self.data):
            raise ValueError("truncated classfile")
        value = self.data[self.at:end]
        self.at = end
        return value

    def u1(self):
        return struct.unpack(">B", self.take(1))[0]

    def u2(self):
        return struct.unpack(">H", self.take(2))[0]

    def u4(self):
        return struct.unpack(">I", self.take(4))[0]


def ascii_utf8(raw):
    # Android SDK binary names, member names and descriptors used by this
    # certificate are ASCII. Reject another encoding rather than normalize it.
    try:
        return raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError("non-ASCII declaration identifier") from error


def descriptor_type(descriptor, at, allow_void=False):
    dimensions = 0
    while at < len(descriptor) and descriptor[at] == "[":
        dimensions += 1
        at += 1
    if dimensions > 255 or at >= len(descriptor):
        raise ValueError("invalid descriptor")
    if descriptor[at] in "ZBSCIJFD" or (allow_void and dimensions == 0 and descriptor[at] == "V"):
        return at + 1
    if descriptor[at] != "L":
        raise ValueError("invalid descriptor")
    end = descriptor.find(";", at + 1)
    if end < 0 or end == at + 1 or any(ch in ".;[" for ch in descriptor[at + 1:end]):
        raise ValueError("invalid descriptor")
    return end + 1


def validate_method_descriptor(descriptor):
    if not descriptor.startswith("("):
        raise ValueError("invalid method declaration")
    at = 1
    while at < len(descriptor) and descriptor[at] != ")":
        at = descriptor_type(descriptor, at)
    if at >= len(descriptor) or descriptor[at] != ")":
        raise ValueError("invalid method declaration")
    at = descriptor_type(descriptor, at + 1, allow_void=True)
    if at != len(descriptor):
        raise ValueError("invalid method declaration")


def parse_class(data):
    if len(data) > MAX_CLASS_BYTES:
        raise ValueError("classfile exceeds size limit")
    reader = Reader(data)
    if reader.u4() != 0xCAFEBABE:
        raise ValueError("invalid classfile magic")
    reader.take(4)
    pool_count = reader.u2()
    if pool_count < 1:
        raise ValueError("empty constant pool")
    # Retain the tag for every usable entry. A bare integer is ambiguous among
    # CONSTANT_Class, String, MethodType, Module and Package and cannot serve
    # as declaration evidence.
    pool = [None] * pool_count
    index = 1
    while index < pool_count:
        tag = reader.u1()
        if tag == 1:
            pool[index] = (tag, reader.take(reader.u2()))
        elif tag in (7, 8, 16, 19, 20):
            pool[index] = (tag, reader.u2())
        elif tag in (3, 4, 9, 10, 11, 12, 17, 18):
            pool[index] = (tag, reader.take(4))
        elif tag in (5, 6):
            pool[index] = (tag, reader.take(8))
            index += 1
            if index >= pool_count:
                raise ValueError("wide constant occupies missing pool slot")
        elif tag == 15:
            pool[index] = (tag, reader.take(3))
        else:
            raise ValueError(f"unsupported constant-pool tag {tag}")
        index += 1

    def utf8(pool_index):
        if not 0 < pool_index < len(pool):
            raise ValueError("invalid UTF8 pool reference")
        entry = pool[pool_index]
        if not isinstance(entry, tuple) or entry[0] != 1 or not isinstance(entry[1], bytes):
            raise ValueError("invalid UTF8 pool reference")
        return ascii_utf8(entry[1])

    def class_name(pool_index):
        if not 0 < pool_index < len(pool):
            raise ValueError("invalid class pool reference")
        entry = pool[pool_index]
        if not isinstance(entry, tuple) or entry[0] != 7 or not isinstance(entry[1], int):
            raise ValueError("invalid class pool reference")
        name = utf8(entry[1])
        if not name or name.startswith("[") or "." in name or ";" in name:
            raise ValueError("invalid binary class name")
        return "L" + name + ";"

    attribute_count = 0

    def attributes():
        nonlocal attribute_count
        result = {}
        count = reader.u2()
        attribute_count += count
        if attribute_count > MAX_ATTRIBUTES:
            raise ValueError("attribute count exceeds limit")
        for _ in range(count):
            name = utf8(reader.u2())
            if name in result:
                raise ValueError("duplicate classfile attribute: " + name)
            result[name] = reader.take(reader.u4())
        return result

    owner_flags = reader.u2()
    owner = class_name(reader.u2())
    parent_index = reader.u2()
    if parent_index:
        class_name(parent_index)
    for _ in range(reader.u2()):
        class_name(reader.u2())

    for _ in range(reader.u2()):
        reader.take(2)
        utf8(reader.u2())
        utf8(reader.u2())
        attributes()

    methods = {}
    source_signatures = set()
    ambiguous_source_signatures = set()
    for _ in range(reader.u2()):
        access_flags = reader.u2()
        name = utf8(reader.u2())
        descriptor = utf8(reader.u2())
        if not name:
            raise ValueError("invalid method declaration")
        validate_method_descriptor(descriptor)
        attrs = attributes()
        thrown = []
        if "Exceptions" in attrs:
            raw = attrs["Exceptions"]
            if len(raw) < 2:
                raise ValueError("truncated Exceptions attribute")
            count = struct.unpack_from(">H", raw, 0)[0]
            if len(raw) != 2 + 2 * count:
                raise ValueError("invalid Exceptions attribute length")
            thrown = [
                class_name(struct.unpack_from(">H", raw, 2 + 2 * item)[0])
                for item in range(count)
            ]
            if len(set(thrown)) != len(thrown):
                raise ValueError("duplicate declared exception")
        key = owner + "->" + name + descriptor
        if key in methods:
            raise ValueError("duplicate method signature: " + key)
        source_signature = owner + "->" + name + descriptor[: descriptor.index(")") + 1]
        if source_signature in source_signatures:
            # Covariant compiler bridges may legally have the same Java name
            # and parameters with different return descriptors. Retain every
            # exact declaration and make the whole Java selection group
            # unavailable to the proof lookup.
            ambiguous_source_signatures.add(source_signature)
        source_signatures.add(source_signature)
        methods[key] = {
            "access_flags": access_flags,
            "exceptions": thrown,
        }

    attributes()
    if reader.at != len(data):
        raise ValueError("trailing classfile bytes")
    return owner, owner_flags, methods, ambiguous_source_signatures


def generate(jar_path, output_path, expected_sha256=None):
    digest = hashlib.sha256()
    with open(jar_path, "rb") as jar_file:
        for chunk in iter(lambda: jar_file.read(1024 * 1024), b""):
            digest.update(chunk)
    jar_sha256 = digest.hexdigest()
    if expected_sha256 and jar_sha256 != expected_sha256.lower():
        raise ValueError("android.jar SHA-256 differs from the pinned value")

    classes = {}
    methods = {}
    ambiguous_source_signatures = set()
    total_bytes = 0
    with zipfile.ZipFile(jar_path) as archive:
        entries = [entry for entry in archive.infolist() if entry.filename.endswith(".class")]
        if not entries or len(entries) > MAX_CLASSES:
            raise ValueError("class entry count outside bounds")
        for entry in sorted(entries, key=lambda item: item.filename):
            if entry.file_size > MAX_CLASS_BYTES:
                raise ValueError("class entry exceeds size limit")
            total_bytes += entry.file_size
            if total_bytes > MAX_ARCHIVE_BYTES:
                raise ValueError("class bytes exceed aggregate limit")
            data = archive.read(entry)
            if len(data) != entry.file_size:
                raise ValueError("short class entry read")
            owner, owner_flags, declared, ambiguous = parse_class(data)
            if owner in classes:
                raise ValueError("duplicate class declaration: " + owner)
            classes[owner] = owner_flags
            ambiguous_source_signatures.update(ambiguous)
            for key, declaration in declared.items():
                if key in methods:
                    raise ValueError("duplicate method declaration: " + key)
                methods[key] = declaration
                if len(methods) > MAX_METHODS:
                    raise ValueError("method count exceeds limit")

    result = {
        "schema": 1,
        "complete": True,
        "source": "Android SDK API 35 android.jar",
        "android_jar_sha256": jar_sha256,
        "class_count": len(classes),
        "method_count": len(methods),
        "ambiguous_source_signature_count": len(ambiguous_source_signatures),
        "ambiguous_source_signatures": sorted(ambiguous_source_signatures),
        "classes": classes,
        "methods": methods,
    }
    with open(output_path, "w", encoding="utf-8", newline="\n") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
    return len(classes), len(methods), jar_sha256


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("android_jar")
    parser.add_argument("output")
    parser.add_argument("--expected-sha256")
    args = parser.parse_args()
    classes, methods, digest = generate(
        args.android_jar, args.output, args.expected_sha256
    )
    print(f"{classes} classes; {methods} method declarations; sha256={digest}")


if __name__ == "__main__":
    main()
