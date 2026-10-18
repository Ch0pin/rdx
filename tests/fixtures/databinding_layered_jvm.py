#!/usr/bin/env python3
"""Independent raw-order and JVM oracle for the exact m/b/a constructor shape."""

from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import tempfile

SOURCES = {
    "androidx/databinding/Hooks.java": r"""
package androidx.databinding;
public final class Hooks {
    private Hooks() {}
    public static final StringBuilder events = new StringBuilder();
    public static void event(String value) {
        if (events.length() != 0) events.append(',');
        events.append(value);
        if (value.equals(System.getProperty("fail", ""))) throw new Fault(value);
    }
    public static final class Fault extends RuntimeException {
        private static final long serialVersionUID = 1L;
        Fault(String value) { super(value); }
    }
}
""",
    "androidx/databinding/a.java": r"""
package androidx.databinding;
public abstract class a {
    static { Hooks.event("a.clinit"); }
    private transient Object mCallbacks;
    public final Object callbacks() { return mCallbacks; }
}
""",
    "androidx/databinding/b.java": r"""
package androidx.databinding;
public abstract class b extends a {
    static { Hooks.event("b.clinit"); }
    public b() { super(); }
}
""",
    "androidx/databinding/m.java": r"""
package androidx.databinding;
@SuppressWarnings("serial")
public final class m extends b implements java.io.Serializable {
    private static final long serialVersionUID = 1L;
    static { Hooks.event("m.clinit"); }
    public Object a;
    public m() { super(); }
    public m(Object value) { super(); this.a = value; }
}
""",
    "Oracle.java": r"""
import androidx.databinding.Hooks;
import androidx.databinding.m;
public final class Oracle {
    public static void main(String[] args) throws Exception {
        String mode = args[0];
        try {
            Hooks.event("factory.before");
            if (mode.equals("retained")) {
                Object marker = new Object();
                m value = new m(marker);
                Hooks.event("factory.after");
                boolean identity = value.a == marker;
                boolean callbacks = value.callbacks() == null;
                boolean signatures = m.class.getConstructor() != null
                    && m.class.getConstructor(Object.class) != null
                    && m.class.getDeclaredConstructors().length == 2;
                System.out.println("OK:retained:" + Hooks.events + ":" + identity
                    + ":" + callbacks + ":" + signatures);
            } else {
                m value = new m();
                Hooks.event("factory.after");
                boolean defaults = value.a == null && value.callbacks() == null;
                System.out.println("OK:default:" + Hooks.events + ":" + defaults);
            }
        } catch (Throwable failure) {
            Throwable cause = failure.getCause() == null ? failure : failure.getCause();
            System.out.println("FAIL:" + mode + ":" + Hooks.events + ":"
                + failure.getClass().getName() + ":" + cause.getClass().getName()
                + ":" + cause.getMessage());
        }
    }
}
""",
}

def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

def raw_facts(evidence: Path) -> dict:
    manifest = json.loads((evidence.parent / "manifest.json").read_text())
    pinned = manifest["output_sha256"]
    for path in sorted(evidence.glob("observation-*.json")):
        key = "evidence/" + path.name
        assert digest(path) == pinned[key]
    constructor = json.loads((evidence / "observation-0004.json").read_text())
    assert constructor["id"] == "androidx.databinding.b.<init>()V"
    assert constructor["words"] == [0x1070, 31985, 0, 0x000E]
    calls = constructor["analysis"]["bound_calls"]["calls"]
    assert len(calls) == 1
    assert calls[0]["pc"] == 0 and calls[0]["kind"] == "Direct"
    assert calls[0]["target"]["declaring_type"] == "Ljava/lang/Object;"
    assert calls[0]["arguments"] == []
    exact = []
    for path in sorted(evidence.glob("observation-*.json")):
        observation = json.loads(path.read_text())
        bindings = observation.get("analysis", {}).get("method_analysis", {}).get(
            "constructors", {}
        ).get("bindings", [])
        for binding in bindings:
            origin = binding.get("origin", {})
            identity = binding.get("call_identity", {})
            if (
                origin.get("initial_allocated_type") == "Landroidx/databinding/m;"
                and binding.get("invoked_owner") == "Ljava/lang/Object;"
                and identity.get("arguments") == []
            ):
                assert origin["initial_new_pc"] < binding["invoke_pc"]
                exact.append({
                    "method": observation["id"],
                    "new_pc": origin["initial_new_pc"],
                    "init_pc": binding["invoke_pc"],
                })
    assert len(exact) == 15
    return {
        "retained_b_words": constructor["words"],
        "retained_b_call_order": ["Object.<init>@0", "return@3"],
        "source_manifest_sha256": digest(evidence.parent / "manifest.json"),
        "allocation_sites": exact,
    }

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--raw-evidence", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    facts = raw_facts(args.raw_evidence)
    expected = {
        "default": "OK:default:factory.before,a.clinit,b.clinit,m.clinit,factory.after:true\n",
        "retained": "OK:retained:factory.before,a.clinit,b.clinit,m.clinit,factory.after:true:true:true\n",
        "fail-a.clinit": "FAIL:fail-a.clinit:factory.before,a.clinit:java.lang.ExceptionInInitializerError:androidx.databinding.Hooks$Fault:a.clinit\n",
        "fail-b.clinit": "FAIL:fail-b.clinit:factory.before,a.clinit,b.clinit:java.lang.ExceptionInInitializerError:androidx.databinding.Hooks$Fault:b.clinit\n",
        "fail-m.clinit": "FAIL:fail-m.clinit:factory.before,a.clinit,b.clinit,m.clinit:java.lang.ExceptionInInitializerError:androidx.databinding.Hooks$Fault:m.clinit\n",
    }
    results = {}
    with tempfile.TemporaryDirectory(prefix="rdx-databinding-layered-") as temporary:
        root = Path(temporary)
        for name, source in SOURCES.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source.strip() + "\n")
        java = sorted(str(path) for path in root.rglob("*.java"))
        compile_result = subprocess.run(
            ["javac", "-Xlint:all", "-Werror", "-d", str(root / "classes"), *java],
            text=True, capture_output=True, check=False,
        )
        (args.output / "javac.stdout").write_text(compile_result.stdout)
        (args.output / "javac.stderr").write_text(compile_result.stderr)
        if compile_result.returncode != 0:
            raise SystemExit(compile_result.returncode)
        for mode, wanted in expected.items():
            fail = mode.removeprefix("fail-") if mode.startswith("fail-") else ""
            command = ["java"]
            if fail:
                command.append("-Dfail=" + fail)
            command += ["-cp", str(root / "classes"), "Oracle", mode]
            result = subprocess.run(command, text=True, capture_output=True, check=False)
            if result.returncode != 0 or result.stdout != wanted or result.stderr:
                raise SystemExit(f"unexpected {mode} result: {result!r}")
            results[mode] = result.stdout.strip()
    (args.output / "evidence.json").write_text(json.dumps({
        "status": "passed",
        "raw": facts,
        "jvm": results,
        "checks": {
            "exact_raw_sites": 15,
            "class_initialization_modes": 4,
            "retained_and_default_identity_modes": 2,
        },
    }, indent=2) + "\n")

if __name__ == "__main__":
    main()
