"""Generate exact integer-resource parameter contracts from pinned SDK metadata.

External annotation source signatures are matched to complete android.jar
declarations. Unresolved/generic/ambiguous signatures are declined, never guessed.
No SDK or XML parsing is required at application runtime.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import re
from xml.etree import ElementTree
from zipfile import ZipFile


KINDS = {
    "AnimRes": ["anim"], "AnimatorRes": ["animator"], "ArrayRes": ["array"],
    "AttrRes": ["attr"], "BoolRes": ["bool"], "ColorRes": ["color"],
    "DimenRes": ["dimen"], "DrawableRes": ["color", "drawable", "mipmap"],
    "FontRes": ["font"], "FractionRes": ["fraction"], "IdRes": ["id"],
    "IntegerRes": ["integer"], "InterpolatorRes": ["anim", "interpolator"],
    "LayoutRes": ["layout"], "MenuRes": ["menu"], "PluralsRes": ["plurals"],
    "RawRes": ["raw"], "StringRes": ["string"], "StyleRes": ["style"],
    "TransitionRes": ["transition"], "XmlRes": ["xml"],
}
PRIMITIVES = dict(zip("VZBCSIJFD", ["void", "boolean", "byte", "char", "short", "int", "long", "float", "double"]))


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def decode_type(descriptor, at):
    start = at
    while at < len(descriptor) and descriptor[at] == "[":
        at += 1
    dimensions = at - start
    if at >= len(descriptor):
        raise ValueError("truncated descriptor")
    if descriptor[at] in PRIMITIVES:
        name = PRIMITIVES[descriptor[at]]
        at += 1
    elif descriptor[at] == "L":
        end = descriptor.index(";", at)
        name = descriptor[at + 1:end].replace("/", ".").replace("$", ".")
        at = end + 1
    else:
        raise ValueError("invalid descriptor")
    return name + "[]" * dimensions, at, descriptor[start:at]


def method_parts(raw):
    owner, member = raw.split("->", 1)
    name, descriptor = member.split("(", 1)
    descriptor = "(" + descriptor
    at = 1
    parameters, parameter_types = [], []
    while descriptor[at] != ")":
        name_type, at, raw_type = decode_type(descriptor, at)
        parameters.append(name_type)
        parameter_types.append(raw_type)
    result, at, _ = decode_type(descriptor, at + 1)
    if at != len(descriptor):
        raise ValueError("trailing descriptor data")
    source_owner = owner[1:-1].replace("/", ".").replace("$", ".")
    label = owner[1:-1].replace("/", ".") + "." + member
    return f"{source_owner} {result} {name}({','.join(parameters)})", label, parameter_types


def normalize_source(signature):
    # Type arguments have no JVM identity. Type variables themselves remain
    # unresolved; this deliberately does not guess their erased upper bounds.
    depth = 0
    output = []
    for char in signature:
        if char == "<":
            depth += 1
        elif char == ">":
            depth -= 1
            if depth < 0:
                raise ValueError("unbalanced generic annotation signature")
        elif depth == 0:
            output.append(char)
    if depth:
        raise ValueError("unbalanced generic annotation signature")
    text = "".join(output).replace("...", "[]")
    return re.sub(r"\s*([(),\[\]])\s*", r"\1", " ".join(text.split()))


def generate(annotations, declarations):
    if Path(annotations).stat().st_size > 64 * 1024 * 1024 or Path(declarations).stat().st_size > 64 * 1024 * 1024:
        raise ValueError("input exceeds generator budget")
    facts = json.loads(Path(declarations).read_text())
    if facts["schema"] != 1 or not facts["complete"] or len(facts["methods"]) != facts["method_count"]:
        raise ValueError("requires complete schema-1 platform declarations")
    index = defaultdict(list)
    for raw in facts["methods"]:
        if "-><" in raw:
            continue
        source, label, parameters = method_parts(raw)
        index[normalize_source(source)].append((label, parameters))
    contracts = defaultdict(dict)
    skipped = Counter()
    admitted = Counter()
    conflicts = set()
    with ZipFile(annotations) as archive:
        entries = archive.infolist()
        if len(entries) > 20_000 or sum(info.file_size for info in entries) > 128 * 1024 * 1024:
            raise ValueError("annotation archive exceeds generator budget")
        for info in entries:
            if not info.filename.endswith("annotations.xml"):
                continue
            if info.file_size > 8 * 1024 * 1024:
                raise ValueError("annotation XML exceeds generator budget")
            for item in ElementTree.fromstring(archive.read(info)):
                kinds = set()
                for annotation in item.findall("annotation"):
                    name = annotation.attrib.get("name", "")
                    short = name.removeprefix("androidx.annotation.")
                    if short in KINDS and name == "androidx.annotation." + short:
                        kinds.add(short)
                    elif name.endswith("Res"):
                        skipped["excluded_annotation:" + name] += 1
                if not kinds:
                    continue
                match = re.fullmatch(r"(.*\)) (\d+)", item.attrib.get("name", ""))
                if not match:
                    skipped["not_parameter"] += 1
                    continue
                source, position = match.group(1), int(match.group(2))
                matches = index.get(normalize_source(source), [])
                if len(matches) != 1:
                    skipped["unresolved_or_ambiguous_declaration"] += 1
                    continue
                label, parameters = matches[0]
                if position >= len(parameters) or parameters[position] != "I":
                    skipped["not_int_parameter"] += 1
                    continue
                key = (label, position)
                if len(kinds) != 1:
                    conflicts.add(key)
                    skipped["conflicting_resource_annotations"] += 1
                    continue
                annotation = next(iter(kinds))
                value = KINDS[annotation]
                previous = contracts[label].get(position)
                if previous is not None and previous != value:
                    conflicts.add(key)
                    skipped["conflicting_resource_annotations"] += 1
                    continue
                contracts[label][position] = value
                admitted[annotation] += 1
    for label, position in conflicts:
        contracts[label].pop(position, None)
    result = {label: [[slot, kinds] for slot, kinds in sorted(slots.items())]
              for label, slots in sorted(contracts.items()) if slots}
    validate_examples(result)
    return {
        "schema": 1,
        "source": "Android SDK API 35 external parameter annotations, matched to exact android.jar declarations",
        "android_jar_sha256": facts["android_jar_sha256"],
        "annotations_zip_sha256": digest(annotations),
        "declarations_sha256": digest(declarations),
        "method_count": len(result),
        "parameter_count": sum(len(slots) for slots in result.values()),
        "admitted_annotation_counts": dict(sorted(admitted.items())),
        "skipped": dict(sorted(skipped.items())),
        "contracts": result,
    }


def validate_examples(contracts):
    examples = {
        "android.app.Activity.setContentView(I)V": [[0, ["layout"]]],
        "android.content.Context.getDrawable(I)Landroid/graphics/drawable/Drawable;": [[0, ["color", "drawable", "mipmap"]]],
        "android.app.AlertDialog$Builder.setMessage(I)Landroid/app/AlertDialog$Builder;": [[0, ["string"]]],
        "android.app.Activity.overrideActivityTransition(III)V": [[1, ["anim"]], [2, ["anim"]]],
        "android.widget.TextView.setText(I)V": [[0, ["string"]]],
    }
    for label, expected in examples.items():
        if contracts.get(label) != expected:
            raise ValueError(f"contract validation failed for {label}: {contracts.get(label)!r}")
    for excluded in [
        "android.content.res.TypedArray.getDrawable(I)Landroid/graphics/drawable/Drawable;",
        "android.app.WallpaperManager.getDrawable(I)Landroid/graphics/drawable/Drawable;",
        "android.widget.TextView.setText(Ljava/lang/CharSequence;)V",
    ]:
        if excluded in contracts:
            raise ValueError(f"index/flags/non-resource overload admitted: {excluded}")
    if "sample.Lookalike.setText(I)V" in contracts:
        raise ValueError("unrelated application method admitted")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("annotations_zip", type=Path)
    parser.add_argument("source_declarations", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    generated = generate(args.annotations_zip, args.source_declarations)
    args.output.write_text(json.dumps(generated, indent=2, sort_keys=True) + "\n")
    print(f"Validated {generated['method_count']} exact methods / {generated['parameter_count']} integer resource parameters")
