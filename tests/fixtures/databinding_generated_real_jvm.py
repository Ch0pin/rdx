#!/usr/bin/env python3
"""Compile the candidate engine's exact displayed m/b/a sources unchanged."""

from pathlib import Path
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile


TARGET = "Landroidx/databinding/m;"
JDK = Path("/private/tmp/rdx-jdk25-validation/jdk-25.0.4.1+1/Contents/Home")
PINNED_SUPPORT_METHOD = {
    "pool_relative": "evidence/pool-0.json",
    "pool_sha256": "b5b7f48a2f5789246103fd24705624ed2d35432116edd6f41378b361a0e548ec",
    "index": 7246,
    "owner": "Landroidx/databinding/d;",
    "name": "f",
    "parameters": ["Ljava/lang/Object;"],
    "return": "V",
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path: Path, source: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source.strip() + "\n")


def verify_candidate_run(run: Path) -> tuple[Path, dict]:
    manifest_path = run / "source-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    evidence = run / "evidence"
    assert Path(manifest["evidence"]) == evidence
    for relative, expected in manifest["output_sha256"].items():
        assert digest(run / relative) == expected, relative
    assert (
        manifest["output_sha256"][PINNED_SUPPORT_METHOD["pool_relative"]]
        == PINNED_SUPPORT_METHOD["pool_sha256"]
    )
    index = json.loads((evidence / "index.json").read_text())
    assert index["hierarchy_seen"] == [
        "Landroidx/databinding/a;",
        "Landroidx/databinding/b;",
        "Landroidx/databinding/m;",
    ]
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
                origin.get("initial_allocated_type") == TARGET
                and binding.get("invoked_owner") == "Ljava/lang/Object;"
                and identity.get("arguments") == []
            ):
                exact.append((observation["id"], origin["initial_new_pc"], binding["invoke_pc"]))
    assert len(exact) == 15
    return evidence, {
        "manifest_sha256": digest(manifest_path),
        "exact_sites": exact,
        "support_method": PINNED_SUPPORT_METHOD,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate-run", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    evidence, raw = verify_candidate_run(args.candidate_run)
    sources = {
        name: evidence / f"androidx.databinding.{name}.baseline.java"
        for name in ("a", "b", "m")
    }
    displayed_hashes = {name: digest(path) for name, path in sources.items()}
    m_source = sources["m"].read_text()
    assert "public m()" in m_source
    assert "public m(Object " in m_source

    with tempfile.TemporaryDirectory(prefix="rdx-real-databinding-") as temporary:
        root = Path(temporary)
        package = root / "androidx/databinding"
        package.mkdir(parents=True)
        for name, source in sources.items():
            copied = package / f"{name}.java"
            shutil.copyfile(source, copied)
            assert digest(copied) == displayed_hashes[name]
        write(root / "androidx/annotation/NonNull.java", """
            package androidx.annotation;
            public @interface NonNull {}
        """)
        write(package / "k.java", """
            package androidx.databinding;
            public interface k {}
        """)
        write(package / "j.java", """
            package androidx.databinding;
            public final class j {}
        """)
        write(package / "c.java", """
            package androidx.databinding;
            public interface c {}
        """)
        write(package / "s.java", """
            package androidx.databinding;
            public final class s implements c {}
        """)
        write(package / "d.java", """
            package androidx.databinding;
            public class d {
              public void a(Object value) {}
              public void c(Object owner, int property) {}
              public void f(Object value) {}
            }
        """)
        write(package / "t.java", """
            package androidx.databinding;
            public final class t extends d {
              public static final s i = new s();
              public t(c value) {}
              public void f(Object value) {}
            }
        """)
        write(root / "Oracle.java", """
            import androidx.databinding.m;
            import java.lang.reflect.Field;
            public final class Oracle {
              private static int checks;
              private static void check(boolean value) {
                checks++;
                if (!value) throw new AssertionError("check " + checks);
              }
              private static Object callbacks(m value) throws Exception {
                Field field = androidx.databinding.a.class.getDeclaredField("mCallbacks");
                field.setAccessible(true);
                return field.get(value);
              }
              public static void main(String[] args) throws Exception {
                m fresh = new m();
                check(fresh.a == null);
                check(callbacks(fresh) == null);
                Object marker = new Object();
                m retained = new m(marker);
                check(retained.a == marker);
                check(callbacks(retained) == null);
                check(m.class.getConstructor() != null);
                check(m.class.getConstructor(Object.class) != null);
                check(m.class.getDeclaredConstructors().length == 2);
                check(androidx.databinding.b.class.getDeclaredConstructors().length == 1);
                check(androidx.databinding.a.class.getDeclaredConstructors().length == 1);
                System.out.println("PASS checks=" + checks);
              }
            }
        """)
        classes = root / "classes"
        java = sorted(str(path) for path in root.rglob("*.java"))
        child_env = os.environ.copy()
        for inherited_option in ("JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS"):
            child_env.pop(inherited_option, None)
        compile_result = subprocess.run(
            [
                str(JDK / "bin/javac"),
                "-J-XX:ActiveProcessorCount=2",
                "-Xlint:all,-serial,-cast",
                "-Werror",
                "-d",
                str(classes),
                *java,
            ],
            text=True,
            capture_output=True,
            check=False,
            env=child_env,
        )
        (args.output / "javac.stdout").write_text(compile_result.stdout)
        (args.output / "javac.stderr").write_text(compile_result.stderr)
        if compile_result.returncode != 0:
            raise SystemExit(compile_result.returncode)
        result = subprocess.run(
            [
                str(JDK / "bin/java"),
                "-XX:ActiveProcessorCount=2",
                "-cp",
                str(classes),
                "Oracle",
            ],
            text=True,
            capture_output=True,
            check=False,
            env=child_env,
        )
        (args.output / "java.stdout").write_text(result.stdout)
        (args.output / "java.stderr").write_text(result.stderr)
        if result.returncode != 0 or result.stdout != "PASS checks=9\n" or result.stderr:
            raise SystemExit(f"unexpected JVM result: {result!r}")
    (args.output / "evidence.json").write_text(json.dumps({
        "status": "passed",
        "candidate_run": str(args.candidate_run),
        "candidate_displayed_source_sha256": displayed_hashes,
        "raw_allocation_provenance": raw,
        "unchanged_displayed_sources": True,
        "jdk": str(JDK),
        "child_java_option_environment": "sanitized; explicit two-processor flags",
        "javac_lint": "-Xlint:all,-serial,-cast",
        "lint_exclusions": {
            "serial": "unchanged displayed m is Serializable and has no serialVersionUID",
            "cast": "unchanged displayed m setter retains two baseline redundant Object casts",
        },
        "checks": 9,
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
