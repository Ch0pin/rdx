#!/usr/bin/env python3
"""Compile an unchanged tc2.k.l renderer fragment and compare pinned DEX behavior."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import subprocess

WORDS = [4128, 13080, 57, 3, 14, 287, 13080, 4436, 33456, 26, 35656, 8305, 24273, 1, 295]
PCS = [(0, 0x20, 2), (2, 0x39, 2), (4, 0x0E, 1), (5, 0x1F, 2), (7, 0x54, 2), (9, 0x1A, 2), (11, 0x71, 3), (14, 0x27, 1)]
EVIDENCE = pathlib.Path("/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/allocation-completion-20261012/careful-resume/protected-effects-roots-private/a21-dependency-extractor-private-draft4/run-20261013T200535Z/evidence/dependency-011.json")
JAVAC_LINT_FLAGS = ["-Xlint:all,-cast", "-Werror"]


def run(command: list[str], cwd: pathlib.Path) -> str:
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)
    if result.returncode:
        raise SystemExit(json.dumps({"command": command, "stdout": result.stdout, "stderr": result.stderr}, indent=2))
    return result.stdout


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def exact_l_instructions(bytecode: str, fresh_type: str) -> list[tuple[int, str, str]]:
    declaration = (
        f"public static <{fresh_type} extends java.lang.Throwable> void "
        f"l(java.lang.Object) throws {fresh_type};"
    )
    lines = bytecode.splitlines()
    declarations = [index for index, line in enumerate(lines) if line.strip() == declaration]
    if len(declarations) != 1:
        raise SystemExit("javap lacks the exact generated generic l(Object) declaration")
    start = declarations[0] + 1
    while start < len(lines) and lines[start].strip() != "Code:":
        start += 1
    if start == len(lines):
        raise SystemExit("javap lacks l(Object) Code")
    instructions = []
    for line in lines[start + 1 :]:
        match = re.match(r"^\s*(\d+):\s+([a-z][a-z0-9_]*)\s*(.*)$", line)
        if match is None:
            if instructions:
                break
            continue
        instructions.append((int(match.group(1)), match.group(2), match.group(3).strip()))
    if not instructions:
        raise SystemExit("javap produced no l(Object) instructions")
    return instructions


def require_exact_l_bytecode(
    instructions: list[tuple[int, str, str]],
) -> dict[str, object]:
    by_opcode: dict[str, list[tuple[int, str]]] = {}
    for pc, opcode, operands in instructions:
        by_opcode.setdefault(opcode, []).append((pc, operands))

    expected_exact = {
        "instanceof": (1, "class tc2/j"),
        "checkcast": (1, "class tc2/j"),
        "getfield": (1, "Field tc2/j.a:Ljava/lang/Throwable;"),
        "invokestatic": (
            1,
            "Method kotlin/jvm/internal/Intrinsics.checkNotNullParameter:"
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        ),
        "athrow": (1, ""),
        "return": (1, ""),
    }
    for opcode, (count, comment) in expected_exact.items():
        actual = by_opcode.get(opcode, [])
        if len(actual) != count or (comment and comment not in actual[0][1]):
            raise SystemExit(f"compiled l(Object) has unexpected {opcode}: {actual}")
    invokes = [item for item in instructions if item[1].startswith("invoke")]
    if len(invokes) != 1 or invokes[0][1] != "invokestatic":
        raise SystemExit(f"compiled l(Object) has unexpected calls: {invokes}")
    field_operations = [
        item
        for item in instructions
        if item[1] in {"getfield", "putfield", "getstatic", "putstatic"}
    ]
    if len(field_operations) != 1 or field_operations[0][1] != "getfield":
        raise SystemExit(f"compiled l(Object) has unexpected field operations: {field_operations}")
    constants = [item for item in instructions if item[1] in {"ldc", "ldc_w"}]
    if len(constants) != 1 or "// String e" not in constants[0][2]:
        raise SystemExit(f"compiled l(Object) has unexpected constants: {constants}")
    allocations = [
        item
        for item in instructions
        if item[1] in {"new", "newarray", "anewarray", "multianewarray"}
    ]
    if allocations:
        raise SystemExit(f"compiled l(Object) allocates: {allocations}")
    runtime_casts = [
        item
        for item in instructions
        if item[1] == "checkcast" and "java/lang/RuntimeException" in item[2]
    ]
    if runtime_casts:
        raise SystemExit("compiled l(Object) casts to RuntimeException")

    branches = [item for item in instructions if item[1].startswith("if")]
    if len(branches) != 1 or branches[0][1] not in {"ifeq", "ifne"}:
        raise SystemExit(f"compiled l(Object) has unexpected branches: {branches}")
    if any(item[1].startswith("goto") or "switch" in item[1] for item in instructions):
        raise SystemExit("compiled l(Object) has an unexpected jump or switch")
    branch_pc, _, branch_operands = branches[0]
    target_match = re.match(r"^(\d+)$", branch_operands)
    if target_match is None:
        raise SystemExit(f"compiled l(Object) has malformed branch target: {branches[0]}")
    target = int(target_match.group(1))
    pc_to_index = {pc: index for index, (pc, _, _) in enumerate(instructions)}
    if len(pc_to_index) != len(instructions) or target not in pc_to_index:
        raise SystemExit("compiled l(Object) branch target is absent or ambiguous")
    branch_index = pc_to_index[branch_pc]
    instanceof_pc = by_opcode["instanceof"][0][0]
    if branch_index == 0 or instructions[branch_index - 1][0] != instanceof_pc:
        raise SystemExit("compiled l(Object) branch does not directly consume instanceof")
    if branch_index + 1 >= len(instructions):
        raise SystemExit("compiled l(Object) branch lacks a fallthrough instruction")
    fallthrough = instructions[branch_index + 1][0]
    if target == fallthrough:
        raise SystemExit("compiled l(Object) branch paths are identical")

    def straight_path(start_pc: int) -> list[tuple[int, str, str]]:
        path = []
        seen = set()
        index = pc_to_index[start_pc]
        while True:
            item = instructions[index]
            if item[0] in seen:
                raise SystemExit("compiled l(Object) path loops")
            seen.add(item[0])
            path.append(item)
            if item[1] in {"return", "athrow"}:
                return path
            if item[1].startswith("if") or item[1].startswith("goto") or "switch" in item[1]:
                raise SystemExit("compiled l(Object) branch arm is not straight-line")
            index += 1
            if index >= len(instructions):
                raise SystemExit("compiled l(Object) path has no terminal instruction")

    taken_path = straight_path(target)
    fallthrough_path = straight_path(fallthrough)
    if branches[0][1] == "ifne":
        wrapper_path, nonwrapper_path = taken_path, fallthrough_path
    else:
        wrapper_path, nonwrapper_path = fallthrough_path, taken_path
    if nonwrapper_path[-1][1] != "return" or any(
        item[1] not in {"nop", "return"} for item in nonwrapper_path
    ):
        raise SystemExit("compiled l(Object) nonwrapper path is not an effect-free return")
    if wrapper_path[-1][1] != "athrow" or any(item[1] == "return" for item in wrapper_path):
        raise SystemExit("compiled l(Object) wrapper path does not terminate by throwing")

    checkcast_pc = by_opcode["checkcast"][0][0]
    getfield_pc = by_opcode["getfield"][0][0]
    invoke_pc = by_opcode["invokestatic"][0][0]
    throw_pc = by_opcode["athrow"][0][0]
    marker_pc = constants[0][0]
    wrapper_pcs = [item[0] for item in wrapper_path]
    if not all(
        pc in wrapper_pcs for pc in [checkcast_pc, getfield_pc, marker_pc, invoke_pc, throw_pc]
    ):
        raise SystemExit(
            "compiled l(Object) wrapper path omits a required original operation"
        )
    if not (
        wrapper_pcs.index(checkcast_pc)
        < wrapper_pcs.index(getfield_pc)
        < wrapper_pcs.index(marker_pc)
        < wrapper_pcs.index(invoke_pc)
        < wrapper_pcs.index(throw_pc)
    ):
        raise SystemExit("compiled l(Object) wrapper operation order changed")
    return {
        "condition_opcode": branches[0][1],
        "condition_pc": branch_pc,
        "taken_target": target,
        "fallthrough_pc": fallthrough,
        "nonwrapper_path_pcs": [item[0] for item in nonwrapper_path],
        "wrapper_path_pcs": wrapper_pcs,
        "marker_pc": marker_pc,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fragment", type=pathlib.Path, required=True)
    parser.add_argument("--intrinsics-fragment", type=pathlib.Path, required=True)
    parser.add_argument("--evidence", type=pathlib.Path, default=EVIDENCE)
    parser.add_argument("--work", type=pathlib.Path, required=True)
    parser.add_argument("--javac", default="javac")
    parser.add_argument("--java", default="java")
    parser.add_argument("--javap", default="javap")
    args = parser.parse_args()

    fragment_bytes = args.fragment.read_bytes()
    fragment = fragment_bytes.decode("utf-8")
    intrinsics_fragment_bytes = args.intrinsics_fragment.read_bytes()
    intrinsics_fragment = intrinsics_fragment_bytes.decode("utf-8")
    evidence_bytes = args.evidence.read_bytes()
    evidence = json.loads(evidence_bytes)
    raw = evidence["raw"]
    instructions = evidence["observations"]["instructions"]
    if not (
        evidence["id"] == "tc2.k.l(Ljava/lang/Object;)V"
        and raw["flags"] == 9
        and (raw["registers"], raw["ins"], raw["outs"], raw["tries"]) == (2, 1, 2, 0)
        and raw["try_regions"] == []
        and raw["words"] == WORDS
        and [(item["pc"], item["opcode"], item["width"]) for item in instructions] == PCS
        and instructions[0]["reference"]["type"] == "Ltc2/j;"
        and instructions[4]["reference"] == {
            "index": 33456,
            "name": "a",
            "owner": "Ltc2/j;",
            "raw": [13080, 6519, 32623],
            "type": "Ljava/lang/Throwable;",
        }
        and instructions[5]["reference"]["string"] == "e"
        and instructions[6]["reference"]["owner"] == "Lkotlin/jvm/internal/Intrinsics;"
        and instructions[6]["reference"]["name"] == "checkNotNullParameter"
        and instructions[6]["reference"]["parameters"] == ["Ljava/lang/Object;", "Ljava/lang/String;"]
        and instructions[6]["reference"]["return"] == "V"
        and instructions[7]["reads"] == "[RegisterOperand { register: 1, kind: Reference }]"
    ):
        raise SystemExit("independent original DEX identity differs")
    required = [
        '@java.lang.SuppressWarnings("unchecked")',
        "Synthesized generic throws signature",
        "instanceof tc2.j",
        "kotlin.jvm.internal.Intrinsics.checkNotNullParameter",
    ]
    if any(token not in fragment for token in required):
        raise SystemExit("renderer fragment lacks the exact synthesized sneaky-rethrow shape")
    generic = re.search(
        r"<(RdxSneakyThrowable[0-9a-f]+(?:_[0-9]+)?) extends java\.lang\.Throwable>.*"
        r"throws \1.*throw \(\(\1\)",
        fragment,
        re.DOTALL,
    )
    if generic is None:
        raise SystemExit("renderer fragment lacks one fresh terminal generic cast/throws name")
    if re.search(r"RuntimeException\s*\)", fragment) or "new RuntimeException" in fragment:
        raise SystemExit("renderer fragment introduces a RuntimeException cast or wrapper")
    if [(pc, WORDS[pc] & 0xFF, width) for pc, _, width in PCS] != PCS:
        raise SystemExit("pinned independent DEX word identity differs")
    intrinsics_headers = re.findall(
        r"(?m)^\s*public\s+static\s+void\s+checkNotNullParameter\s*"
        r"\(java\.lang\.Object\s+\w+,\s*java\.lang\.String\s+\w+\)\s*([^\{]*)\{",
        intrinsics_fragment,
    )
    if len(intrinsics_headers) != 1 or "throws" in intrinsics_headers[0]:
        raise SystemExit("actual rendered Intrinsics header is absent, ambiguous, or checked")
    if "Synthesized generic throws signature" in intrinsics_fragment:
        raise SystemExit("actual Intrinsics fragment unexpectedly uses synthesized header authority")

    source = args.work / "src"
    classes = args.work / "classes"
    (source / "tc2").mkdir(parents=True, exist_ok=True)
    (source / "kotlin/jvm/internal").mkdir(parents=True, exist_ok=True)
    classes.mkdir(parents=True, exist_ok=True)
    (source / "tc2/k.java").write_text("package tc2;\npublic abstract class k {\n" + fragment + "}\n")
    (source / "tc2/j.java").write_text(
        "package tc2; public final class j { public final Throwable a; "
        "public j(Throwable a) { this.a = a; } }\n"
    )
    (source / "kotlin/jvm/internal/Intrinsics.java").write_text(
        "package kotlin.jvm.internal; public final class Intrinsics { "
        "public static String marker; public static Throwable injected; "
        "@java.lang.SuppressWarnings(\"unchecked\") "
        "private static <T extends Throwable> void raise(Throwable value) throws T { "
        "throw (T) value; } "
        "public static void checkNotNullParameter(Object value, String name) { "
        "marker = name; if (injected != null) { Throwable valueToThrow = injected; "
        "injected = null; Intrinsics.<RuntimeException>raise(valueToThrow); } "
        "if (value == null) throw new NullPointerException(name); } }\n"
    )
    (source / "tc2/Harness.java").write_text(
        r'''package tc2;
import java.lang.reflect.*;
public final class Harness {
  static final StackTraceElement TRACE = new StackTraceElement("raw", "marker", "Raw.java", 77);
  static final class Checked extends Exception {
    private static final long serialVersionUID = 1L;
    Checked(String s) { super(s); }
  }
  static void inference(Object value) { k.l(value); }
  static void expectReturn(Object value) {
    kotlin.jvm.internal.Intrinsics.marker = null;
    inference(value);
    if (kotlin.jvm.internal.Intrinsics.marker != null) throw new AssertionError("unexpected check");
  }
  static void expectSame(Throwable value) {
    value.setStackTrace(new StackTraceElement[] { TRACE });
    kotlin.jvm.internal.Intrinsics.marker = null;
    try { inference(new j(value)); throw new AssertionError("returned"); }
    catch (Throwable caught) {
      if (caught != value) throw new AssertionError("identity");
      if (!"e".equals(kotlin.jvm.internal.Intrinsics.marker)) throw new AssertionError("order");
      if (caught.getStackTrace().length != 1 || !TRACE.equals(caught.getStackTrace()[0]))
        throw new AssertionError("trace");
    }
  }
  static void expectInjectedFaultReplacesFieldValue() {
    Throwable fieldValue = new Throwable("field value");
    Checked injected = new Checked("injected check fault");
    injected.setStackTrace(new StackTraceElement[] { TRACE });
    kotlin.jvm.internal.Intrinsics.marker = null;
    kotlin.jvm.internal.Intrinsics.injected = injected;
    try { inference(new j(fieldValue)); throw new AssertionError("injected fault returned"); }
    catch (Throwable caught) {
      if (caught != injected || caught == fieldValue) throw new AssertionError("replacement identity");
      if (!"e".equals(kotlin.jvm.internal.Intrinsics.marker)) throw new AssertionError("replacement order");
      if (caught.getStackTrace().length != 1 || !TRACE.equals(caught.getStackTrace()[0]))
        throw new AssertionError("replacement trace");
    } finally { kotlin.jvm.internal.Intrinsics.injected = null; }
  }
  public static void main(String[] args) throws Exception {
    expectReturn(null); expectReturn(new Object());
    try { inference(new j(null)); throw new AssertionError("null field returned"); }
    catch (NullPointerException expected) {
      if (!"e".equals(expected.getMessage()) || !"e".equals(kotlin.jvm.internal.Intrinsics.marker))
        throw new AssertionError("null marker");
    }
    expectSame(new RuntimeException("runtime"));
    expectSame(new Error("error"));
    expectSame(new Checked("checked"));
    expectSame(new Throwable("throwable"));
    expectInjectedFaultReplacesFieldValue();
    Method method = k.class.getDeclaredMethod("l", Object.class);
    if (!method.getReturnType().equals(void.class) || method.getParameterTypes()[0] != Object.class)
      throw new AssertionError("descriptor");
    if (!(method.getTypeParameters().length == 1
        && method.getTypeParameters()[0].getName().startsWith("RdxSneakyThrowable")
        && method.getTypeParameters()[0].getBounds().length == 1
        && method.getTypeParameters()[0].getBounds()[0] == Throwable.class
        && method.getGenericExceptionTypes().length == 1
        && method.getGenericExceptionTypes()[0] instanceof TypeVariable
        && method.getExceptionTypes()[0] == Throwable.class))
      throw new AssertionError("synthesized reflection metadata");
    System.out.println("cases=8 identity=5 trace=5 inference=compiled reflection=synthesized");
  }
}
'''
    )
    sources = sorted(str(path) for path in source.rglob("*.java"))
    source_hashes = {
        str(path.relative_to(args.work)): sha256(path.read_bytes())
        for path in sorted(source.rglob("*.java"))
    }
    input_hashes = {
        "fragment": {"path": str(args.fragment.resolve()), "sha256": sha256(fragment_bytes)},
        "intrinsics_fragment": {
            "path": str(args.intrinsics_fragment.resolve()),
            "sha256": sha256(intrinsics_fragment_bytes),
        },
        "evidence": {"path": str(args.evidence.resolve()), "sha256": sha256(evidence_bytes)},
        "generated_sources": source_hashes,
    }
    (args.work / "input-hashes.json").write_text(json.dumps(input_hashes, indent=2) + "\n")
    # The unchanged renderer commonly emits redundant reference casts. Keep
    # every other javac warning fatal, including unchecked generic casts.
    javac_stdout = run(
        [args.javac, *JAVAC_LINT_FLAGS, "-d", str(classes), *sources], args.work
    )
    (args.work / "javac.stdout").write_text(javac_stdout)
    runtime_stdout = run([args.java, "-cp", str(classes), "tc2.Harness"], args.work)
    (args.work / "runtime.stdout").write_text(runtime_stdout)
    runtime = runtime_stdout.strip()
    expected_runtime = "cases=8 identity=5 trace=5 inference=compiled reflection=synthesized"
    if runtime != expected_runtime:
        raise SystemExit(f"unexpected JVM oracle output: {runtime!r}")
    bytecode = run([args.javap, "-classpath", str(classes), "-c", "-p", "tc2.k"], args.work)
    verbose_bytecode = run(
        [args.javap, "-classpath", str(classes), "-s", "-v", "-p", "tc2.k"], args.work
    )
    (args.work / "tc2-k.javap.txt").write_text(bytecode)
    (args.work / "tc2-k.javap.verbose.txt").write_text(verbose_bytecode)
    if "descriptor: (Ljava/lang/Object;)V" not in verbose_bytecode:
        raise SystemExit("compiled descriptor changed")
    if f"throws {generic.group(1)};" not in verbose_bytecode:
        raise SystemExit("compiled generic throws name differs from the generated source")
    instructions = exact_l_instructions(bytecode, generic.group(1))
    bytecode_paths = require_exact_l_bytecode(instructions)
    summary = {
        "evidence_sha256": sha256(evidence_bytes),
        "fragment_sha256": sha256(fragment_bytes),
        "intrinsics_fragment_sha256": sha256(intrinsics_fragment_bytes),
        "raw_words": WORDS,
        "runtime": runtime,
        "descriptor": "(Ljava/lang/Object;)V",
        "fresh_type": generic.group(1),
        "javac_lint_flags": JAVAC_LINT_FLAGS,
        "l_instructions": instructions,
        "bytecode_paths": bytecode_paths,
        "work_evidence_sha256": {
            "input-hashes.json": sha256((args.work / "input-hashes.json").read_bytes()),
            "runtime.stdout": sha256((args.work / "runtime.stdout").read_bytes()),
            "tc2-k.javap.txt": sha256((args.work / "tc2-k.javap.txt").read_bytes()),
            "tc2-k.javap.verbose.txt": sha256(
                (args.work / "tc2-k.javap.verbose.txt").read_bytes()
            ),
        },
    }
    summary_text = json.dumps(summary, indent=2) + "\n"
    (args.work / "summary.json").write_text(summary_text)
    print(summary_text, end="")


if __name__ == "__main__":
    main()
