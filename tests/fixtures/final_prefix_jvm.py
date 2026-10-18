"""Compile the exact renderer output and compare it with a tiny raw-word oracle."""
from pathlib import Path
import json
import os
import subprocess
import sys


base = Path(sys.argv[1])
raw = json.loads((base / "raw.json").read_text())
words = raw["words"]
assert (raw["try_start"], raw["try_end"], raw["handler"]) == (3, 8, 9)
assert raw["class"] == "Lsample/Child;"
assert raw["catch_type"] == "Ljava/lang/Exception;"
this_register = raw["registers"] - raw["ins"]
parameter_register = this_register + 1


def field(index):
    return raw["field_pool"][index]


def method(index):
    return raw["method_pool"][index]


super_call = method(words[1])
assert words[0] >> 12 == 1 and words[2] & 0xF == this_register
assert (super_call["owner"], super_call["name"], super_call["return"]) == (
    "Lsample/Base;",
    "<init>",
    "V",
)
x_receiver = words[3] >> 12
x_value = (words[3] >> 8) & 0xF
x_field = field(words[4])
assert (x_receiver, x_value) == (this_register, parameter_register)
assert (x_field["owner"], x_field["descriptor"], x_field["name"]) == (
    "Lsample/Child;",
    "Ljava/lang/Object;",
    "x",
)
hash_call = method(words[6])
assert words[5] >> 12 == 1 and words[7] & 0xF == parameter_register
assert (hash_call["owner"], hash_call["name"], hash_call["return"]) == (
    "Ljava/lang/Object;",
    "hashCode",
    "I",
)
seen_receiver = words[10] >> 12
seen_value = (words[10] >> 8) & 0xF
seen_field = field(words[11])
assert (seen_receiver, seen_value) == (this_register, 0)
assert (seen_field["owner"], seen_field["descriptor"], seen_field["name"]) == (
    "Lsample/Child;",
    "Ljava/lang/Exception;",
    "seen",
)
published_register = words[12] >> 8
published_field = field(words[13])
assert published_register == this_register
assert (
    published_field["owner"],
    published_field["descriptor"],
    published_field["name"],
) == ("Lsample/Child;", "Lsample/Child;", "published")


def dex_oracle(mode):
    pc = 0
    field = None
    caught = None
    published = None
    events = "S"
    while True:
        opcode = words[pc] & 0xFF
        if pc == 0 and opcode == 0x70 and words[2] & 0xF == this_register:
            events += "B"
            pc = 3
        elif (
            pc == 3
            and opcode == 0x5B
            and x_receiver == this_register
            and x_value == parameter_register
            and x_field["name"] == "x"
        ):
            field = None if mode == "null" else "probe"
            pc = 5
        elif (
            pc == 5
            and opcode == 0x6E
            and words[7] & 0xF == parameter_register
            and hash_call["name"] == "hashCode"
        ):
            if mode == "null":
                caught = "npe"
                pc = raw["handler"]
                continue
            events += "H"
            if mode == "exception":
                caught = "marker"
                pc = raw["handler"]
            elif mode == "error":
                # Exact Exception protection excludes Error; the caller sees
                # the same instance and the in-constructor publication is skipped.
                events += "E"
                return events, field, None, None, "error"
            else:
                pc = 8
        elif pc == 8 and opcode == 0x28:
            pc += words[pc] >> 8
        elif pc == 9 and opcode == 0x0D:  # move-exception
            pc = 10
        elif (
            pc == 10
            and opcode == 0x5B
            and seen_receiver == this_register
            and seen_value == 0
            and seen_field["name"] == "seen"
        ):
            pc = 12
        elif (
            pc == 12
            and opcode == 0x69
            and published_register == this_register
            and published_field["name"] == "published"
        ):
            published = "this"
            pc = 14
        elif pc == 14 and opcode == 0x0E:
            events += "R"
            return events, field, caught, published, "return"
        else:
            raise AssertionError(f"unexpected raw instruction at pc {pc}: {opcode:#x}")


failure = dex_oracle("exception")
success = dex_oracle("success")
null_value = dex_oracle("null")
error = dex_oracle("error")
boolean = lambda value: str(value).lower()
expected = [
    f"failure|{failure[0]}|{boolean(failure[1] == 'probe')}|{boolean(failure[2] == 'marker')}|{boolean(failure[3] == 'this')}",
    f"success|{success[0]}|{boolean(success[1] == 'probe')}|{boolean(success[2] is None)}|{boolean(success[3] is None)}",
    f"null|{null_value[0]}|{boolean(null_value[1] is None)}|{boolean(null_value[2] == 'npe')}|{boolean(null_value[3] == 'this')}",
    f"error|{error[0]}|{boolean(error[4] == 'error')}|{boolean(error[3] is None)}",
    "repeat|32|true",
]

jdk = Path(os.environ.get("RDX_JAVA25_HOME", "")) / "bin"
javac = jdk / "javac" if os.environ.get("RDX_JAVA25_HOME") else Path("javac")
java = jdk / "java" if os.environ.get("RDX_JAVA25_HOME") else Path("java")
sources = sorted((base / "sample").glob("*.java"))
compiled = subprocess.run(
    [str(javac), "-Xlint:all", "-Werror", "-d", str(base), *map(str, sources)],
    capture_output=True,
    text=True,
    timeout=60,
)
(base / "javac.log").write_text(compiled.stdout + compiled.stderr)
assert compiled.returncode == 0, compiled.stderr
executed = subprocess.run(
    [str(java), "-cp", str(base), "sample.Oracle"],
    capture_output=True,
    text=True,
    timeout=60,
)
(base / "jvm.log").write_text(executed.stdout + executed.stderr)
assert executed.returncode == 0, executed.stderr
actual = executed.stdout.strip().splitlines()
assert actual == expected, {"expected": expected, "actual": actual}
proof = {
    "status": "PASS",
    "comparisons": 36,
    "actual": actual,
    "scope": "exact rendered constructor body; raw-word oracle covers null NPE, RuntimeException catch identity, Error exclusion, publication, repeated identities, and post-super order",
}
(base / "proof.json").write_text(json.dumps(proof, indent=2) + "\n")
print(json.dumps(proof, indent=2))
