#!/usr/bin/env python3
"""Compare unchanged emitted Java with literal DEX execution of late switch cases."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys


def signed(value: int, width: int) -> int:
    return (value & ((1 << width) - 1)) - ((value & (1 << (width - 1))) << 1)


def i32(value: int) -> int:
    return signed(value, 32)


def oracle(raw: dict, count: int, mode: int, throw_at: int) -> tuple[str, list[int]]:
    words = raw["words"]
    registers = [None] * raw["registers"]
    assert raw["ins"] == 2
    registers[-2:] = [count, mode]
    events: list[int] = []
    pending = None
    pc = 0
    for _ in range(20_000):
        word = words[pc]
        opcode = word & 255
        if opcode == 0x00:  # nop (payloads are reached only through switch decoding)
            assert word == 0
            pc += 1
        elif opcode == 0x12:  # const/4
            registers[(word >> 8) & 15] = signed(word >> 12, 4)
            pc += 1
        elif opcode == 0x13:  # const/16
            registers[word >> 8] = signed(words[pc + 1], 16)
            pc += 2
        elif opcode == 0x01:  # move, retaining literal sequential copy order
            registers[(word >> 8) & 15] = registers[word >> 12]
            pc += 1
        elif opcode == 0x35:  # if-ge
            left = registers[(word >> 8) & 15]
            right = registers[word >> 12]
            pc += signed(words[pc + 1], 16) if left >= right else 2
        elif opcode == 0x71:  # invoke-static (35c)
            method = raw["method_pool"][words[pc + 1]]
            assert method["name"] == "step"
            assert method["owner"] == raw["class"]
            assert method["parameters"] == ["I"]
            assert method.get("return", method.get("return_type")) == "I"
            assert word >> 12 == 1
            argument = registers[words[pc + 2] & 15]
            assert argument is not None
            ordinal = len(events)
            events.append(argument)
            if ordinal == throw_at:
                return "marker", events
            pending = argument
            pc += 3
        elif opcode == 0x0A:  # move-result
            assert pending is not None
            registers[word >> 8] = pending
            pending = None
            pc += 1
        elif opcode == 0x2B:  # packed-switch; all offsets are relative to switch
            payload = pc + signed(words[pc + 1] | (words[pc + 2] << 16), 32)
            assert words[payload] == 0x0100
            size = words[payload + 1]
            first = signed(words[payload + 2] | (words[payload + 3] << 16), 32)
            index = registers[word >> 8] - first
            if 0 <= index < size:
                target = payload + 4 + index * 2
                pc += signed(words[target] | (words[target + 1] << 16), 32)
            else:
                pc += 3
        elif opcode == 0x28:  # goto
            pc += signed(word >> 8, 8)
        elif opcode == 0x29:  # goto/16
            pc += signed(words[pc + 1], 16)
        elif opcode == 0x90:  # add-int (e.g. selector mode + iteration)
            registers[word >> 8] = i32(
                registers[words[pc + 1] & 255] + registers[words[pc + 1] >> 8]
            )
            pc += 2
        elif opcode == 0xB0:  # add-int/2addr
            destination, source = (word >> 8) & 15, word >> 12
            registers[destination] = i32(registers[destination] + registers[source])
            pc += 1
        elif opcode == 0xD8:  # add-int/lit8
            destination = word >> 8
            source = words[pc + 1] & 255
            literal = signed(words[pc + 1] >> 8, 8)
            registers[destination] = i32(registers[source] + literal)
            pc += 2
        elif opcode == 0x0F:  # return
            result = registers[word >> 8]
            assert result is not None
            return str(result), events
        else:
            raise AssertionError(f"unsupported raw instruction {opcode:#x} at pc {pc}")
    raise AssertionError("raw-word oracle exceeded its bounded instruction budget")


def main() -> None:
    base = Path(sys.argv[1]).resolve()
    raw = json.loads((base / "raw.json").read_text())
    descriptor = raw["class"]
    assert descriptor.startswith("Lsample/") and descriptor.endswith(";")
    simple = descriptor[len("Lsample/") : -1]
    assert simple.isidentifier()
    words = raw["words"]
    assert len(words) < 512 and all(0 <= word <= 65535 for word in words)
    assert raw["header"] < raw["prefix_first"] < raw["latch"]
    assert raw["header"] < raw["prefix_second"] < raw["latch"]
    assert len(raw["late_cases"]) == 2
    assert all(raw["latch"] < case for case in raw["late_cases"])
    latch = raw["latch"]
    assert words[latch] & 255 == 0x28
    assert latch + signed(words[latch] >> 8, 8) == raw["header"]
    switch = raw["switch"]
    assert words[switch] & 255 == 0x2B
    payload = switch + signed(words[switch + 1] | (words[switch + 2] << 16), 32)
    assert words[payload] == 0x0100 and words[payload + 1] == 2
    assert [switch + signed(words[payload + 4 + i * 2] | (words[payload + 5 + i * 2] << 16), 32) for i in range(2)] == raw["late_cases"]

    expected = []
    scenarios = []
    throwing = 0
    for count in [-1, 0, 1, 2, 5, 9]:
        for mode in [-2, 0, 1, 2]:
            _, normal_events = oracle(raw, count, mode, -1)
            for throw_at in [-1, *range(len(normal_events)), len(normal_events)]:
                result, events = oracle(raw, count, mode, throw_at)
                throwing += result == "marker"
                key = f"{count}|{mode}|{throw_at}"
                scenarios.append(f"{count},{mode},{throw_at}")
                expected.append(f"{key}|{result}|{','.join(map(str, events))}")
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
METHOD
    public static void main(String[] args) throws Exception {
        for (String line : java.nio.file.Files.readAllLines(java.nio.file.Path.of(args[0]))) {
            String[] fields = line.split(",");
            int count = Integer.parseInt(fields[0]), mode = Integer.parseInt(fields[1]);
            throwAt = Integer.parseInt(fields[2]); ordinal = 0; events.setLength(0);
            String result;
            try { result = Integer.toString(run(count, mode)); }
            catch (RuntimeException error) {
                if (error != MARKER) throw new AssertionError("exception identity", error);
                result = "marker";
            }
            System.out.println(count+"|"+mode+"|"+throwAt+"|"+result+"|"+events);
        }
    }
}
""".replace("CLASS", simple).replace("METHOD", method)
    package = base / "sample"
    package.mkdir(exist_ok=True)
    java_file = package / f"{simple}.java"
    java_file.write_text(source)
    jdk_home = os.environ.get("RDX_JAVA25_HOME")
    javac = str(Path(jdk_home) / "bin/javac") if jdk_home else "javac"
    java = str(Path(jdk_home) / "bin/java") if jdk_home else "java"
    compiled = subprocess.run([javac, "-Xlint:all", "-Werror", "-d", str(base), str(java_file)], capture_output=True, text=True, timeout=60)
    (base / "javac.log").write_text(compiled.stdout + compiled.stderr)
    assert compiled.returncode == 0, compiled.stderr
    executed = subprocess.run([java, "-cp", str(base), f"sample.{simple}", str(base / "scenarios.txt")], capture_output=True, text=True, timeout=60)
    (base / "jvm.log").write_text(executed.stdout + executed.stderr)
    assert executed.returncode == 0, executed.stderr
    actual = executed.stdout.splitlines()
    assert actual == expected, {"expected": expected, "actual": actual}
    proof = {"status": "PASS", "comparisons": len(expected), "throwing_comparisons": throwing, "scope": "unchanged renderer method; literal DEX-word switch/copy/loop execution; zero/one/many iterations, late cases, exact ordered effects and marker exception identity", "actual": actual}
    (base / "proof.json").write_text(json.dumps(proof, indent=2) + "\n")
    print(json.dumps({key: value for key, value in proof.items() if key != "actual"}, indent=2))


if __name__ == "__main__":
    main()
