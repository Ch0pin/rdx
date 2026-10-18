"""Independent class-file fixtures for SDK enclosing-member access proofs."""
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/generate_platform_exceptions.py'
SPEC = importlib.util.spec_from_file_location('platform_generator', SCRIPT)
GEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GEN)


def classfile(owner, flags=1, inner=None, enclosing_method=False, constructor_flags=1, duplicate_attribute=False):
    pool = []
    def utf(text):
        raw = text.encode('utf8'); pool.append(b'\x01' + struct.pack('>H', len(raw)) + raw)
        return len(pool)
    def cls(text):
        name = utf(text); pool.append(b'\x07' + struct.pack('>H', name)); return len(pool)
    owner_index, parent_index = cls(owner), cls('java/lang/Object')
    init, descriptor = utf('<init>'), utf('()V')
    attributes = []
    if inner is not None:
        entries = []
        for outer, name, access in inner:
            outer_index = cls(outer) if outer else 0
            entries.append(struct.pack('>HHHH', owner_index, outer_index, utf(name) if name else 0, access))
        attributes.append((utf('InnerClasses'), struct.pack('>H', len(entries)) + b''.join(entries)))
    if enclosing_method:
        attributes.append((utf('EnclosingMethod'), struct.pack('>HH', parent_index, 0)))
    if duplicate_attribute and attributes:
        attributes.append(attributes[0])
    return (struct.pack('>IHHH', 0xCAFEBABE, 0, 61, len(pool)+1) + b''.join(pool)
            + struct.pack('>HHHHHH', flags, owner_index, parent_index, 0, 0, 1)
            + struct.pack('>HHHH', constructor_flags, init, descriptor, 0)
            + struct.pack('>H', len(attributes))
            + b''.join(struct.pack('>HI', name, len(value)) + value for name, value in attributes))


def generate(classes, section="constructors"):
    with tempfile.TemporaryDirectory(prefix='rdx-platform-generator-') as temporary:
        directory = Path(temporary); jar = directory / 'sdk.jar'
        with zipfile.ZipFile(jar, 'w') as archive:
            for owner, data in classes.items(): archive.writestr(owner + '.class', data)
        subprocess.run(['python3', str(SCRIPT), str(jar), str(directory / 'exceptions.json'),
                        '--constructors-output', str(directory / 'constructors.json')],
                       check=True, capture_output=True)
        return json.loads((directory / 'constructors.json').read_text())[section]


class ConstructorMetadataTests(unittest.TestCase):
    def test_public_static_nested_chain_and_old_top_level_constructor(self):
        classes = {'p/Outer': classfile('p/Outer'),
                   'p/Outer$Middle': classfile('p/Outer$Middle', inner=[('p/Outer', 'Middle', 9)]),
                   'p/Outer$Middle$Leaf': classfile('p/Outer$Middle$Leaf', inner=[('p/Outer$Middle', 'Leaf', 9)], constructor_flags=4)}
        facts = generate(classes)
        self.assertEqual(facts['Lp/Outer;-><init>()V'], [1, []])
        self.assertEqual(facts['Lp/Outer$Middle$Leaf;-><init>()V'], [4, []])

    def test_missing_nonstatic_private_and_invalid_member_metadata_decline(self):
        variants = [None, [('p/Outer', 'Leaf', 1)], [('p/Outer', 'Leaf', 10)],
                    [('p/Outer', 'Wrong', 9)], [(None, 'Leaf', 9)], [('p/Outer', None, 9)],
                    [('p/Outer', 'Leaf', 9), ('p/Outer', 'Leaf', 9)]]
        for metadata in variants:
            with self.subTest(metadata=metadata):
                facts = generate({'p/Outer': classfile('p/Outer'),
                                  'p/Outer$Leaf': classfile('p/Outer$Leaf', inner=metadata)})
                self.assertNotIn('Lp/Outer$Leaf;-><init>()V', facts)

    def test_missing_private_and_nonstatic_enclosing_chain_declines(self):
        leaf = classfile('p/Outer$Middle$Leaf', inner=[('p/Outer$Middle', 'Leaf', 9)])
        variants = [{}, {'p/Outer$Middle': classfile('p/Outer$Middle', inner=[('p/Outer', 'Middle', 1)])},
                    {'p/Outer$Middle': classfile('p/Outer$Middle', inner=[('p/Outer', 'Middle', 10)])},
                    {'p/Outer$Middle': classfile('p/Outer$Middle', inner=[('p/Outer', 'Middle', 9)]),
                     'p/Outer': classfile('p/Outer', flags=0)}]
        for outer in variants:
            with self.subTest(outer=list(outer)):
                self.assertNotIn('Lp/Outer$Middle$Leaf;-><init>()V', generate({**outer, 'p/Outer$Middle$Leaf': leaf}))

    def test_local_class_and_private_constructor_decline(self):
        for options in [{'enclosing_method': True}, {'constructor_flags': 2}, {'flags': 0}]:
            with self.subTest(options=options):
                facts = generate({'p/Outer': classfile('p/Outer'), 'p/Outer$Leaf': classfile('p/Outer$Leaf', inner=[('p/Outer', 'Leaf', 9)], **options)})
                self.assertNotIn('Lp/Outer$Leaf;-><init>()V', facts)

    def test_invalid_visibility_unknown_bits_and_class_flag_mismatch_decline(self):
        for member_flags, class_flags in [(11, 1), (13, 1), (0x8009, 1), (25, 1), (9, 0x401)]:
            with self.subTest(member_flags=member_flags, class_flags=class_flags):
                facts = generate({'p/Outer': classfile('p/Outer'), 'p/Outer$Leaf': classfile('p/Outer$Leaf', flags=class_flags, inner=[('p/Outer', 'Leaf', member_flags)])})
                self.assertNotIn('Lp/Outer$Leaf;-><init>()V', facts)

    def test_duplicate_innerclasses_attribute_rejects_generation(self):
        with self.assertRaises(subprocess.CalledProcessError):
            generate({'p/Outer': classfile('p/Outer'), 'p/Outer$Leaf': classfile('p/Outer$Leaf', inner=[('p/Outer', 'Leaf', 9)], duplicate_attribute=True)})

    def test_canonical_mapping_uses_exact_deep_chain_and_preserves_dollar_identifier(self):
        classes = {'p/Outer': classfile('p/Outer'),
                   'p/Outer$Middle': classfile('p/Outer$Middle', inner=[('p/Outer', 'Middle', 9)]),
                   'p/Outer$Middle$Price$Tag': classfile('p/Outer$Middle$Price$Tag', inner=[('p/Outer$Middle', 'Price$Tag', 9)])}
        mappings = generate(classes, 'member_types')
        self.assertEqual(mappings['p.Outer$Middle$Price$Tag'], ['p.Outer', 'Middle.Price$Tag', ['Lp/Outer$Middle$Price$Tag;', 'Lp/Outer$Middle;', 'Lp/Outer;']])

    def test_unproven_members_never_receive_canonical_mapping(self):
        for metadata in [None, [('p/Outer', 'Leaf', 1)], [('p/Outer', 'Leaf', 10)], [('p/Outer', 'Wrong', 9)], [('p/Outer', 'Leaf', 11)]]:
            with self.subTest(metadata=metadata):
                mappings = generate({'p/Outer': classfile('p/Outer'), 'p/Outer$Leaf': classfile('p/Outer$Leaf', inner=metadata)}, 'member_types')
                self.assertNotIn('p.Outer$Leaf', mappings)
        self.assertEqual(generate({'p/Price$Tag': classfile('p/Price$Tag')}, 'member_types'), {})
        self.assertEqual(generate({'p/Outer$Leaf': classfile('p/Outer$Leaf', inner=[('p/Outer', 'Leaf', 9)])}, 'member_types'), {})

    def test_real_sdk_nested_constructor_facts_have_exact_access_and_signature(self):
        facts = json.loads((ROOT / 'data/android-35-constructors.json').read_text())['constructors']
        for signature in ['Landroid/view/View$BaseSavedState;-><init>(Landroid/os/Parcel;)V',
                          'Landroid/view/ViewGroup$MarginLayoutParams;-><init>(II)V']:
            self.assertEqual(facts[signature], [1, []])
        mappings = json.loads((ROOT / 'data/android-35-constructors.json').read_text())['member_types']
        self.assertEqual(mappings['android.view.View$BaseSavedState'], ['android.view.View', 'BaseSavedState', ['Landroid/view/View$BaseSavedState;', 'Landroid/view/View;']])
        self.assertEqual(mappings['android.view.ViewGroup$MarginLayoutParams'], ['android.view.ViewGroup', 'MarginLayoutParams', ['Landroid/view/ViewGroup$MarginLayoutParams;', 'Landroid/view/ViewGroup;']])


if __name__ == '__main__': unittest.main()
