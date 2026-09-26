# G01-C-start validation

Date: 2026-09-26. Gaps: M01, M07, M06. Complete for one eligible forward conditional. G01 remains open.

## Production change

`native_cfg::ControlFlowGraph::from_decoded` builds contiguous blocks and normal
successors from the shared decoded instructions. The bounded adapter follows the
split/connect and target-identity approach in pinned JADX
[BlockSplitter](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockSplitter.java).
It validates boundaries, forward target identity, block edges and reachability.

For one forward conditional (0x32..0x3d), with an optional forward goto
(0x28..0x2a), production consumes the canonical taken/fallthrough successors,
join and decoded condition/goto operands. A reverse walk over the decoded linear
join tail supplies live registers for value merging, including wide pairs.
Supported shapes include a shared tail, no-goto bypass and terminal arms whose
common exit is method end. The existing register-value expression lowering stays
in use. Shared validation failures cannot retry raw operands.

## Verification

- Six private tests cover 36 condition/goto poisoning combinations, reference/null
  conditions, bypass/terminal joins, nonempty call navigation links, excluded
  routing, 24 malformed condition/CFG mutations and 24 goto metadata mutations.
  Poisoning replaces raw branch words and legacy targets after decoding; output
  and nonempty navigation coordinates remain unchanged.
- Public fixtures cover 28 positive methods and 11 rejections. All 12 condition
  encodings and three goto widths are represented, with signed extrema,
  references, nulls, booleans, wide/reference liveouts and dead incompatible values.
- The optional JVM test compiles generated methods and checks 336 outcomes,
  including ten effect/exception trace scenarios. It covers both arms, exact
  reference/exception identity, terminal return/throw branches and earlier failures
  preventing later effects. The frozen public/JVM target passed all three tests.
- Sol implemented code and public tests; Astra independently reviewed production,
  private fixtures and the independent JVM oracle. Parent inspected integration,
  requested nonempty navigation proof and ran the full gates.

## Completed gates

Full default suite: **813 passed, zero failed, 85 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the release build passed.

The pinned Play Store report is byte-for-byte identical: 260,225 / 268,289 concrete
methods reconstructed (96.994286%), 8,064 fallbacks. This is one APK's reconstruction
metric, not semantic accuracy or universal Android coverage.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
222.492 ms before to 225.553 ms after (+1.38%, 3.061 ms). Warm median:
1.197 ms to 1.224 ms. This small local sample is not a universal performance claim.
[Machine-readable evidence and hashes](g01c-start.json).

## Boundaries and next increment

The raw eligibility scan remains. Constructor branches, multiple conditions,
loops/backedges, switches/payloads, handlers/monitors and allocation-specific
shapes retain existing routes. Full `MethodAnalysis` still uses the raw CFG
builder; the decoded builder governs this bounded production renderer path.
No general SSA/type-driven emission, synthetic blocks or exception normalization
parity is claimed. No whole visitor or delivery unit closes.

Next: **G01-C-forward-composition**. Replace the single plan with canonical
per-branch joins and decoded normal-CFG liveness for existing sequential/nested
forward conditionals, including postdominance and region-closure checks.
No release, GUI deployment, commit or push was performed.
