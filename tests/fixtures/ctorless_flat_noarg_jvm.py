#!/usr/bin/env python3
"""Modeled JVM differential for flat ctorless no-arg reconstruction."""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile


SOURCE = r"""
package sample;

@SuppressWarnings("serial")
final class Marker extends RuntimeException {
    Marker(String message) { super(message); }
}

final class Hooks {
    static int mode;
    static final StringBuilder events = new StringBuilder();
    static void hit(String value) {
        if (events.length() != 0) events.append(',');
        events.append(value);
    }
    static Marker fail(String value) { return new Marker(value); }
    static void parent(Object value) {
        hit("parent-ctor:" + value.getClass().getName());
        if (mode == 1) throw fail("PARENT");
    }
    static void parentInit() {
        hit("parent-clinit");
        if (mode == 2) throw fail("CLINIT");
    }
}

abstract class Parent {
    static { Hooks.parentInit(); }
    public Parent() { Hooks.parent(this); }
}

class Raw$4 extends Parent {
    Raw$4() { super(); }
}

class Source$4 extends Parent {
    // Reconstructed forwarding constructor removed by DEX optimization.
    Source$4() { super(); }
}

class Raw$CurrentWrite {
    public char[] a;
    public String b;
    Raw$CurrentWrite() { super(); }
}

class Source$CurrentWrite {
    public char[] a;
    public String b;
    // Reconstructed forwarding constructor removed by DEX optimization.
    Source$CurrentWrite() { super(); }
}

public final class Oracle {
    private static String fault(Throwable error) {
        String value = "fault:" + error.getClass().getName() + ":" + error.getMessage();
        if (error.getCause() != null) {
            value += ":" + error.getCause().getClass().getName()
                    + ":" + error.getCause().getMessage();
        }
        return value;
    }

    public static void main(String[] args) throws Exception {
        String kind = args[0];
        Hooks.mode = Integer.parseInt(args[1]);
        String result;
        try {
            Hooks.hit("before");
            Object value = kind.equals("raw") ? new Raw$4() : new Source$4();
            Hooks.hit("after");
            Object second = kind.equals("raw") ? new Raw$4() : new Source$4();
            Object current = kind.equals("raw")
                    ? new Raw$CurrentWrite() : new Source$CurrentWrite();
            Class<?> currentClass = current.getClass();
            Object a = currentClass.getField("a").get(current);
            Object b = currentClass.getField("b").get(current);
            int access = value.getClass().getDeclaredConstructor().getModifiers();
            result = "ok:" + (a == null) + ":" + (b == null) + ":" + access
                    + ":" + (value != second)
                    + ":" + (value.getClass().getEnclosingClass() == null);
        } catch (Throwable error) {
            result = fault(error);
        }
        System.out.println(Hooks.events + "|" + result);
    }
}
"""


def run(command: list[str], cwd: pathlib.Path) -> str:
    env = dict(os.environ)
    env.pop("JAVA_TOOL_OPTIONS", None)
    result = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode or result.stderr:
        raise SystemExit(
            f"command failed {command!r}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result.stdout.strip()


def normalized(value: str, kind: str) -> str:
    return value.replace(f"sample.{kind.title()}$4", "sample.Candidate$4")


def main() -> None:
    javac = os.environ.get("RDX_JAVAC")
    java = os.environ.get("RDX_JAVA")
    if not javac or not java:
        raise SystemExit("set RDX_JAVAC and RDX_JAVA to the pinned JDK executables")
    with tempfile.TemporaryDirectory(prefix="rdx-flat-noarg-") as directory:
        root = pathlib.Path(directory)
        package = root / "sample"
        package.mkdir()
        (package / "Oracle.java").write_text(SOURCE, encoding="utf-8")
        run(
            [javac, "-J-XX:ActiveProcessorCount=2", "-Xlint:all", "-Werror", "sample/Oracle.java"],
            root,
        )
        checks = 0
        for mode in range(3):
            raw = run([java, "-XX:ActiveProcessorCount=2", "sample.Oracle", "raw", str(mode)], root)
            source = run(
                [java, "-XX:ActiveProcessorCount=2", "sample.Oracle", "source", str(mode)],
                root,
            )
            if normalized(raw, "raw") != normalized(source, "source"):
                raise SystemExit(f"mode {mode} differs\nraw={raw}\nsource={source}")
            checks += 1
        print(f"PASS {checks} flat ctorless JVM differential scenarios")


if __name__ == "__main__":
    main()
