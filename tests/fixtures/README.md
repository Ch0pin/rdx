# Original synthetic fixtures

Regenerate with `python3 scripts/generate_fixture.py`.

`hello.dex` contains `sample.Hello` with a single public static method:

```java
public static int answer() { return 42; }
```

`hello.apk` wraps that DEX as `classes.dex`. It is a ZIP-based APK input fixture,
not an installable Android application (no manifest, resources, or signature).
Both are deterministic and contain no third-party application code.

Run native engine and plugin tests with `cargo test --test integration`.
Protocol fault-injection tests use development-only Python helpers; native engine
and bundled plugin execution require no interpreter or Java SDK.

`navigation.apk`/`.dex` are checked-in synthetic fixtures originally produced
from small Java sources and Android D8. Their Java-based regeneration helper was
retired with the worker; runtime tests use the committed DEX bytes directly.

These fixtures validate basic loading/reconstruction and error recovery, not
real-world APK compatibility, decompilation correctness, or performance claims.
