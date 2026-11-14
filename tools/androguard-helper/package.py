"""Package an already-built release helper and its audited third-party notices."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[2]
BACKEND = "androguard-f7fa4a4853d957ebc3c54355d128a3835ef5ae22-rdx2"


def verify_helper(executable, root=ROOT):
    """Validate backend identity, audited lockfile and exact vendored sources."""
    executable = Path(executable)
    version = subprocess.run([str(executable), "--version"], check=True, capture_output=True, text=True, timeout=5).stdout.strip()
    if version != BACKEND:
        raise SystemExit("release helper backend/version does not match the audited source")
    notices = root / "third_party/androguard"
    audit = json.loads((notices / "dependency-audit.json").read_text())
    if audit.get("missing_license_text"):
        raise SystemExit("complete dependency license texts before packaging")
    lock_digest = hashlib.sha256((root / "tools/androguard-helper/Cargo.lock").read_bytes()).hexdigest()
    if audit.get("cargo_lock_sha256") != lock_digest:
        raise SystemExit("helper Cargo.lock changed since the dependency license audit")
    for component in json.loads((notices / "vendored-source-audit.json").read_text()):
        for entry in component["files"]:
            source = root / "tools/androguard-helper/vendor" / component["component"] / entry["path"]
            if hashlib.sha256(source.read_bytes()).hexdigest() != entry["sha256"]:
                raise SystemExit(f"vendored source changed since audit: {source}")
    return version


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: package.py ABSOLUTE_OUTPUT_ZIP")
    output = Path(sys.argv[1])
    if not output.is_absolute():
        raise SystemExit("output ZIP must be an absolute path")
    executable = ROOT / "tools/androguard-helper/target/release/rdx-androguard-helper"
    version = verify_helper(executable)
    notices = ROOT / "third_party/androguard"
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists():
        raise SystemExit(f"output already exists: {output}")
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, strict_timestamps=False) as archive:
        archive.write(executable, executable.name)
        archive.write(ROOT / "tools/androguard-helper/README.md", "README.md")
        archive.write(ROOT / "tools/androguard-helper/Cargo.lock", "Cargo.lock")
        for path in sorted(notices.rglob("*")):
            if path.is_file():
                archive.write(path, Path("third_party/androguard") / path.relative_to(notices))
    print(json.dumps({"zip": str(output), "binary_bytes": executable.stat().st_size,
                      "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                      "backend": BACKEND, "zip_bytes": output.stat().st_size}, indent=2))


if __name__ == "__main__":
    main()
