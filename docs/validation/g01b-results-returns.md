# G01-B-results-returns

**Completed 2026-09-25 for bounded scope; G01-B and G01 remain open.**
Tracked gaps: M01, M05, M06. [Machine-readable evidence](g01b-results-returns.json)
records hashes, commands, tests, corpus identity and timing samples.

## Production integration

`src/native_java/method.rs::decoded_result_destination` and `decoded_return_source`
now supply operands for move-result variants (0x0a..0x0c) and return variants
(0x0e..0x11) in the eligible shared straight-line route. They validate operand
arity, width, kind and metadata. Return-void requires no operands. Invalid shared
inputs fail explicitly and do not retry raw decoding.

Existing pending-value consumption, result/return type checks and constructor
initialization guards remain. Move-result also checks its decoded destination and
PC against its producer's bound result, using logarithmic lookups. Shared call
binding already enforces adjacency, result kind, and orphan rejection. The extra
check detects inconsistent shared IR after binding; normal production IR is immutable.
Legacy routes are unchanged. No claim of complete upstream visitor parity is added.

Separate Sol agents implemented and tested the change. The thread limit prevented
starting Astra; a separate Sol agent reviewed it, followed by parent inspection and
full validation. Review prompted the bound-result consistency check. A JVM fixture
mistakenly included a non-failing failure index; it was corrected before validation.

## Validation

- Full default suite: **781 passed, 0 failed, 80 ignored**. The new optional JVM test
  separately passed, exercising 12 generated methods, 16 typed input vectors,
  void return, and three normal/throwing call scenarios.
- Private tests poison raw operands after shared decoding for all seven opcodes;
  40 malformed shared cases and one changed bound destination reject.
- Four public tests cover result/return forms, overwritten inputs/results,
  adjacent-call ownership and invalid forms, plus constructor initialization guards.
  JVM checks cover identity, signed values, float/double raw bits, consecutive
  effects, and exact exception identity when either producer throws.
- Strict all-target Clippy, formatting, diff checks and release binary build passed.
- Pinned Play Store report is exactly unchanged: **260,225/268,289 reconstructed
  (96.994286%)**, 8,064 fallbacks, including identical diagnostics and samples.

## Search performance

Three alternating runs searched `getClass` across the same 300 classes. All 53
matches and source offsets matched, with no errors, skips or limits. Cold median:
**229.305 ms → 236.690 ms (+3.2%, about 7.4 ms)**. Warm median:
**1.194 ms → 1.248 ms**. The added ownership checks use logarithmic lookups, not a
method-wide scan per result. Record this small measured cost; these samples do not
prove universal performance or establish a statistically significant change.

## Remaining work and exact resume point

**Next: G01-B-arithmetic.** Migrate shared operands for numeric comparison,
unary/conversion and binary arithmetic (normal, 2addr and literal forms), preserving
existing numeric lowering, signedness, shifts, overflow and floating-point behavior.
Type/array, field and throw operands remain subsequent work; move-exception and
handler integration remain outside the straight-line route. Shared CFG and general
SSA/type-driven emission remain open. No release, commit or deployment was performed.
