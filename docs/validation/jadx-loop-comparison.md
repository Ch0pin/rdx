# JADX-guided loop reconstruction, 2026-09-25

The comparison uses the same Play Store APK as the previous coverage checkpoint:
SHA-256 `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
JADX is version 1.5.6, source commit
`28ff15e4ae69950aebea110a13e5ab895d234dfc`. Its release ZIP SHA-256 is
`545ea2be9c242511bc145755cf4bda2485ade42966e096f8b4d3da2a230e8974`.
The external JVM is used only for comparison and tests; RDX remains native Rust.

## Comparison

```sh
jadx -r -j 2 --single-class aadd --single-class-output aadd-jadx.java \
  --deobf-cfg-file-mode ignore com.android.vending.apk
jadx -r -j 2 --single-class a --single-class-output a-jadx.java \
  --deobf-cfg-file-mode ignore com.android.vending.apk
rdx --decompile com.android.vending.apk aadd
rdx --decompile com.android.vending.apk a
```

Both JADX commands saved the requested class but exited with status 3 and
reported 13 errors. The compared `aadd.c(Laada;)Ljava/lang/Object;` and
`a.t([B)Ljava/lang/String;` methods contain reconstructed Java, without a
method-error fallback. This is a method-level reference, not a clean whole-APK
JADX result or an independent semantic oracle.

`aadd.c` includes a successful search exit at `0071 -> 0012 -> 00db`.
The shared continuation physically precedes the search in one segment and
contains further loops after `00db`. RDX previously rejected this as an
unsupported loop interior edge. JADX recognizes the loop exit and common
continuation through control-flow analysis rather than address order.

`a.t` defines `v5 = 16` in the outer loop header at `0005`. The value is needed
after the loop, including the later comparison at `0032`, but it is not live
at the header's entry because the header overwrites it. RDX's carried-entry
frame did not retain this distinct exit value. JADX emits the later comparison
with the constant 16.

## Native changes and boundaries

RDX now discovers supported loop regions before checking escape paths. Its
bounded traversal follows every original normal edge, permits a separately
validated downstream loop, and rejects paths which reenter the current loop.
It rejects protected instructions in cloned continuations so calls cannot lose
their original catch ownership. Recursive rendering must establish terminal
control flow for each cloned escape region.

Ordinary guarded loops also retain a separate exit frame when header-entry
liveness omits a register needed after the loop. A header preview establishes
types without emitting or hoisting its effects; the actual header executes in
its original position, including when the body runs zero times.

This is a scoped adaptation of
[LoopInfo](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/attributes/nodes/LoopInfo.java)
and
[LoopRegionMaker](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/maker/LoopRegionMaker.java).
It is not a complete port of outblock selection, SSA-to-region integration or
JADX's loop cleanup passes. RDX can duplicate a shared continuation across
exclusive exits, making its output longer than JADX's. Existing work, nesting
and output limits still apply. Accepting a downstream loop does not prove that
it terminates at runtime.

Portable fixtures exercise return values, effect counts, exception identity,
backward conditional/unconditional exits, downstream loops and rejected reentry
or protected-interior paths. Coverage remains a count of accepted methods on
this pinned APK, not a semantic-accuracy or all-Android-apps percentage.

## Final validation

The release build reconstructs **260,225 / 268,289 concrete methods (96.994286%)**,
up from **259,509 (96.727410%)**: **716 fewer fallbacks**, with 8,064 remaining.
Both compared methods emit Java in the final release build. Full counts,
remaining reasons, samples and artifact hashes are retained in
[`coverage-jadx-loops.json`](coverage-jadx-loops.json).

`cargo test --locked --no-fail-fast` passed **756 tests**, with **76 optional
tests ignored**. Seventeen selected JVM tests passed separately. The two new
JVM tests execute 880 cases in total; the other fifteen cover existing loops,
exception exits, catch dispatch, synchronization, cleanup, switch tails and
numeric invariants. These are scoped behavioral tests, not execution of every
newly accepted APK method.

```sh
cargo test --locked --test native_backward_loop_exit \
  --test native_loop_header_liveout --test native_loop_returns \
  --test native_multiple_latches --test native_exceptions \
  --test native_exception_exits --test native_synchronized \
  --test native_standalone_cleanup --test native_shared_switch_tails \
  --test native_numeric_invariants \
  -- --ignored --skip hilt_component_manager_monitor_region
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
cargo build --release --locked --bins
python3 scripts/package_macos.py
```

All commands passed. The packaged `target/RDX.app` executable matches the
release executable used for the coverage scan.
