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
            if name in result:
                raise ValueError('duplicate classfile attribute: ' + name)
            result[name] = stream.read(size)
        return result
    for _ in range(u2()):
        stream.read(6)
        attrs()
    methods, constructors = {}, {}
    for _ in range(u2()):
        access, name, descriptor = u2(), pool[u2()], pool[u2()]
        attributes = attrs()
        exceptions = attributes.get('Exceptions')
        thrown = []
        if exceptions:
            count = struct.unpack_from('>H', exceptions)[0]
            thrown = [cls(struct.unpack_from('>H', exceptions, 2 + j * 2)[0]) for j in range(count)]
            if thrown:
                methods[owner + '->' + name + descriptor] = [bool(access & 8), thrown]
        # Candidate parent declarations; member staticness/access is checked
        # from the class-level InnerClasses attribute below.
        if (name == '<init>' and flags & 1 and not flags & (0x10 | 0x4000)
                and owner != 'Ljava/lang/Enum;' and access & 5):
            constructors[owner + '->' + name + descriptor] = [access, thrown]
    attributes = attrs()
    outer = None
    if '$' in owner:
        entries = attributes.get('InnerClasses', b'\0\0')
        count = struct.unpack_from('>H', entries)[0]
        matches = []
        for index in range(count):
            inner, enclosing, name, access = struct.unpack_from('>HHHH', entries, 2 + 8 * index)
            if cls(inner) == owner:
                matches.append((enclosing, name, access))
        if len(matches) == 1 and 'EnclosingMethod' not in attributes:
            enclosing, name, access = matches[0]
            if (enclosing and name and access & 7 == 1 and access & 8
                    and access & ~0x761f == 0
                    and access & 0x7611 == flags & 0x7611):
                outer = cls(enclosing)
                if owner != outer[:-1] + '$' + pool[name] + ';':
                    outer = None
        if outer is None:
            constructors.clear()
    return owner, parent, interfaces, methods, constructors, flags, outer


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('jar')
    parser.add_argument('output')
    parser.add_argument('--constructors-output')
    args = parser.parse_args()
    classes, methods, types, constructors, declarations_by_owner = {}, {}, {}, {}, {}
    with zipfile.ZipFile(args.jar) as archive:
        for name in sorted(archive.namelist()):
            if name.endswith('.class'):
                owner, parent, interfaces, declarations, declared_constructors, flags, outer = parse(archive.read(name))
                classes[owner] = parent
                types[owner] = [parent, interfaces]
                methods.update(declarations)
                declarations_by_owner[owner] = (flags, outer, declared_constructors)
    member_types = {}
    for owner, (_, outer, declared_constructors) in declarations_by_owner.items():
        # Every enclosing member must be public and static; top-level access
        # is checked too. Missing or cyclic nesting never supplies access proof.
        seen = {owner}
        accessible = True
        current, members, chain = owner, [], [owner]
        while outer is not None:
            if outer in seen or outer not in declarations_by_owner:
                accessible = False
                break
            seen.add(outer)
            flags, next_outer, _ = declarations_by_owner[outer]
            if not flags & 1 or ('$' in outer and next_outer is None):
                accessible = False
                break
            members.insert(0, current[len(outer):-1])
            chain.append(outer)
            current = outer
            outer = next_outer
        if accessible:
            constructors.update(declared_constructors)
            if members:
                member_types[owner[1:-1].replace('/', '.')] = [
                    current[1:-1].replace('/', '.'), '.'.join(members), chain]
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
    if args.constructors_output:
        with open(args.constructors_output, 'w') as output:
            json.dump({'source': result['source'], 'sha256': result['sha256'],
                       'constructors': constructors, 'member_types': member_types},
                      output, indent=2, sort_keys=True)
            output.write('\n')


if __name__ == '__main__':
    main()
