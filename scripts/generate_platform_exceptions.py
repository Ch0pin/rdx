"""Extract API exception facts from an Android SDK android.jar (no code copied)."""
import argparse
import hashlib
import io
import json
import struct
import zipfile


def parse(data):
    stream = io.BytesIO(data)
    def u1(): return struct.unpack('>B', stream.read(1))[0]
    def u2(): return struct.unpack('>H', stream.read(2))[0]
    def u4(): return struct.unpack('>I', stream.read(4))[0]
    assert u4() == 0xCAFEBABE
    stream.read(4)
    pool = [None] * u2()
    i = 1
    while i < len(pool):
        tag = u1()
        if tag == 1:
            # Names/descriptors used here are ASCII; ignore unrelated string constants.
            pool[i] = stream.read(u2()).decode('utf-8', errors='replace')
        elif tag in (7, 8, 16, 19, 20): pool[i] = u2()
        elif tag in (3, 4, 9, 10, 11, 12, 17, 18): stream.read(4)
        elif tag in (5, 6): stream.read(8); i += 1
        elif tag == 15: stream.read(3)
        else: raise ValueError(tag)
        i += 1
    def cls(index): return 'L' + pool[pool[index]] + ';' if index else None
    flags, owner, parent = u2(), cls(u2()), cls(u2())
    interfaces = [cls(u2()) for _ in range(u2())]
    def attrs():
        result = {}
        for _ in range(u2()):
            name, size = pool[u2()], u4()
            result[name] = stream.read(size)
        return result
    for _ in range(u2()):
        stream.read(6)
        attrs()
    methods = {}
    for _ in range(u2()):
        access, name, descriptor = u2(), pool[u2()], pool[u2()]
        attributes = attrs()
        exceptions = attributes.get('Exceptions')
        if exceptions:
            count = struct.unpack_from('>H', exceptions)[0]
            thrown = [cls(struct.unpack_from('>H', exceptions, 2 + j * 2)[0]) for j in range(count)]
            if thrown:
                methods[owner + '->' + name + descriptor] = [bool(access & 8), thrown]
    return owner, parent, interfaces, methods


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('jar')
    parser.add_argument('output')
    args = parser.parse_args()
    classes, methods, types = {}, {}, {}
    with zipfile.ZipFile(args.jar) as archive:
        for name in sorted(archive.namelist()):
            if name.endswith('.class'):
                owner, parent, interfaces, declarations = parse(archive.read(name))
                classes[owner] = parent
                types[owner] = [parent, interfaces]
                methods.update(declarations)
    exceptions = {}
    for owner, parent in classes.items():
        seen, current = set(), owner
        while current and current not in seen:
            seen.add(current)
            if current == 'Ljava/lang/Throwable;':
                exceptions[owner] = parent
                break
            current = classes.get(current)
    result = {'source': 'Android SDK API 35 android.jar',
              'sha256': hashlib.sha256(open(args.jar, 'rb').read()).hexdigest(),
              'exception_parents': exceptions, 'types': types, 'methods': methods}
    with open(args.output, 'w') as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write('\n')
    print(len(exceptions), 'exception types;', len(methods), 'method declarations')


if __name__ == '__main__':
    main()
