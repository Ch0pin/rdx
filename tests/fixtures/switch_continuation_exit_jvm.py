#!/usr/bin/env python3
"""Literal DEX oracle for an Integer loop frame and downstream String exit frame."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys

from switch_continuation_tail_jvm import i32, signed


def oracle(raw: dict, count: int, mode: int, throw_at: int, reference_hook=None) -> tuple[str, list]:
    words = raw["words"]
    regs = [None] * raw["registers"]
    assert raw["ins"] == 2
    regs[-2:] = [count, mode]
    pending = None
    events = []
    pc = 0
    for _ in range(20_000):
        word = words[pc]
        op = word & 255
        if op == 0:
            assert word == 0
            pc += 1
        elif op == 0x12:
            regs[(word >> 8) & 15] = signed(word >> 12, 4)
            pc += 1
        elif op == 0x13:
            regs[word >> 8] = signed(words[pc + 1], 16)
            pc += 2
        elif op in (0x01, 0x07):  # move / move-object in actual sequential order
            regs[(word >> 8) & 15] = regs[word >> 12]
            pc += 1
        elif op == 0x1A:
            regs[word >> 8] = raw["strings"][words[pc + 1]]
            pc += 2
        elif op in (0x6E, 0x71):
            method = raw["method_pool"][words[pc + 1]]
            count_args = word >> 12
            packed = words[pc + 2]
            inputs = [(packed >> (i * 4)) & 15 for i in range(min(count_args, 4))]
            if count_args == 5:
                inputs.append((word >> 8) & 15)
            assert count_args == len(inputs) == 1
            argument = regs[inputs[0]]
            owner, name = method["owner"], method["name"]
            result_type = method.get("return", method.get("return_type"))
            if op == 0x71 and owner == "Ljava/lang/Integer;" and name == "valueOf":
                assert method["parameters"] == ["I"] and result_type == owner
                assert type(argument) is int
                pending = ("Integer", argument)
            elif op == 0x6E and owner == "Ljava/lang/Integer;" and name == "intValue":
                assert method["parameters"] == [] and result_type == "I"
                assert isinstance(argument, tuple) and argument[0] == "Integer"
                pending = argument[1]
            elif op == 0x71 and owner == raw["class"] and name == "step":
                assert method["parameters"] == ["I"] and result_type == "I"
                assert type(argument) is int
                ordinal = len(events)
                events.append(argument)
                if ordinal == throw_at:
                    return "marker", events
                pending = argument
            elif reference_hook is not None:
                event, pending = reference_hook(raw, op, method, argument)
                ordinal = len(events)
                events.append(event)
                if ordinal == throw_at:
                    return "marker", events
            else:
                raise AssertionError(f"unsupported raw invocation {method} at {pc}")
            pc += 3
        elif op in (0x0A, 0x0C):
            assert pending is not None
            regs[word >> 8] = pending
            pending = None
            pc += 1
        elif op == 0x35:
            left, right = regs[(word >> 8) & 15], regs[word >> 12]
            assert type(left) is int and type(right) is int
            pc += signed(words[pc + 1], 16) if left >= right else 2
        elif op == 0x2B:
            payload = pc + signed(words[pc + 1] | (words[pc + 2] << 16), 32)
            assert words[payload] == 0x0100
            size = words[payload + 1]
            first = signed(words[payload + 2] | (words[payload + 3] << 16), 32)
            key = regs[word >> 8]
            assert type(key) is int
            index = key - first
            if 0 <= index < size:
                target = payload + 4 + index * 2
                pc += signed(words[target] | (words[target + 1] << 16), 32)
            else:
                pc += 3
        elif op == 0x28:
            pc += signed(word >> 8, 8)
        elif op == 0x29:
            pc += signed(words[pc + 1], 16)
        elif op == 0x90:
            regs[word >> 8] = i32(regs[words[pc + 1] & 255] + regs[words[pc + 1] >> 8])
            pc += 2
        elif op == 0xB0:
            destination, source = (word >> 8) & 15, word >> 12
            regs[destination] = i32(regs[destination] + regs[source])
            pc += 1
        elif op == 0xD8:
            regs[word >> 8] = i32(regs[words[pc + 1] & 255] + signed(words[pc + 1] >> 8, 8))
            pc += 2
        elif op == 0x11:
            result = regs[word >> 8]
            assert type(result) is str
            return result, events
        else:
            raise AssertionError(f"unsupported raw opcode {op:#x} at pc {pc}")
    raise AssertionError("raw exit-frame oracle exceeded bounded instruction budget")


def main(oracle_fn=oracle, hook_source="", scope=None) -> None:
    base = Path(sys.argv[1]).resolve()
    raw = json.loads((base / "raw.json").read_text())
    words = raw["words"]
    assert len(words) < 512 and all(0 <= word <= 65535 for word in words)
    descriptor = raw["class"]
    assert descriptor.startswith("Lsample/") and descriptor.endswith(";")
    name = descriptor[len("Lsample/") : -1]
    assert name.isidentifier()
    exit_pc, return_pc = raw["exit"], raw["deferred_return"]
    assert words[return_pc] & 255 == 0x11 and return_pc > exit_pc
    if words[exit_pc] & 255 == 0x28:
        target = exit_pc + signed(words[exit_pc] >> 8, 8)
    else:
        assert words[exit_pc] & 255 == 0x29
        target = exit_pc + signed(words[exit_pc + 1], 16)
    assert target == return_pc
    assert all(isinstance(value, str) and "|" not in value and "\n" not in value for value in raw["strings"])
    scenarios, expected = [], []
    throws = 0
    for count in [-1, 0, 1, 2, 5, 9]:
        for mode in [-2, 0, 1, 2]:
            _, normal_events = oracle_fn(raw, count, mode, -1)
            for throw_at in [-1, *range(len(normal_events)), len(normal_events)]:
                result, events = oracle_fn(raw, count, mode, throw_at)
                throws += result == "marker"
                scenarios.append(f"{count},{mode},{throw_at}")
                expected.append(f"{count}|{mode}|{throw_at}|{result}|{','.join(map(str, events))}")
    (base / "scenarios.txt").write_text("\n".join(scenarios) + "\n")
    method = (base / "method.java").read_text()
    assert ".method " not in method
    source = """package sample;
public class CLASS {
    static final RuntimeException MARKER = new RuntimeException("marker");
    static final StringBuilder events = new StringBuilder();
    static int throwAt, ordinal;
    public static int step(int value) {
        if (events.length() != 0) events.append(',');
        events.append(value);
        if (ordinal++ == throwAt) throw MARKER;
        return value;
    }
HOOKS
METHOD
    public static void main(String[] args) throws Exception {
        for (String line : java.nio.file.Files.readAllLines(java.nio.file.Path.of(args[0]))) {
            String[] fields = line.split(",");
            int count = Integer.parseInt(fields[0]), mode = Integer.parseInt(fields[1]);
            throwAt = Integer.parseInt(fields[2]); ordinal = 0; events.setLength(0);
            String result;
            try { result = run(count, mode); }
            catch (RuntimeException error) {
                if (error != MARKER) throw new AssertionError("exception identity", error);
                result = "marker";
            }
            System.out.println(count+"|"+mode+"|"+throwAt+"|"+result+"|"+events);
        }
    }
}
""".replace("CLASS", name).replace("HOOKS", hook_source).replace("METHOD", method)
    package = base / "sample"
    package.mkdir(exist_ok=True)
    java_file = package / f"{name}.java"
    java_file.write_text(source)
    home = os.environ.get("RDX_JAVA25_HOME")
    javac = str(Path(home) / "bin/javac") if home else "javac"
    java = str(Path(home) / "bin/java") if home else "java"
    compiled = subprocess.run([javac, "-Xlint:all", "-Werror", "-d", str(base), str(java_file)], capture_output=True, text=True, timeout=60)
    (base / "javac.log").write_text(compiled.stdout + compiled.stderr)
    assert compiled.returncode == 0, compiled.stderr
    executed = subprocess.run([java, "-cp", str(base), f"sample.{name}", str(base / "scenarios.txt")], capture_output=True, text=True, timeout=60)
    (base / "jvm.log").write_text(executed.stdout + executed.stderr)
    assert executed.returncode == 0, executed.stderr
    actual = executed.stdout.splitlines()
    assert actual == expected, {"expected": expected, "actual": actual}
    proof = {"status": "PASS", "comparisons": len(expected), "throwing_comparisons": throws, "scope": scope or "unchanged emitted method; literal raw DEX Integer carry and deferred String exit; negative/zero/one/many iterations, mixed cases, ordered effects, marker identity", "actual": actual}
    (base / "proof.json").write_text(json.dumps(proof, indent=2) + "\n")
    print(json.dumps({key: value for key, value in proof.items() if key != "actual"}, indent=2))


if __name__ == "__main__":
    main()
