# Native method coverage

```sh
cargo run --release -- --native-coverage app.apk
cargo run --release -- --native-coverage app.apk com.example.
```

The command prints a JSON summary of native Java reconstruction, using Rust only.
`CLASS_PREFIX` is a literal, case-sensitive class-name prefix; append a period to
limit a package without also matching similarly named packages. No match returns
zero selected classes and methods while retaining the input class count.

`concrete_methods` excludes abstract and native declarations.
`reconstructed_methods` counts concrete methods accepted by the current Java
renderer. `fallback_methods` counts the remainder, grouped by the first reason
that stopped reconstruction. Each concrete method is attempted once; generated
text and navigation metadata are dropped immediately. Diagnostics retain at most
256 categories with 256 characters per category; excess categories are aggregated.
Opcode failure positions are removed so the same unsupported opcode groups
together. Loading still uses the native engine's existing input and metadata limits.

This measures method-renderer coverage, not whole-class Java validity, semantic
equivalence, or performance. Class-level metadata restrictions may still require
mixed Java/DEX output. One method can have several unsupported features: its first
reported reason is not an exhaustive inventory. Re-run after renderer changes to
see the remaining blockers. The command creates no source cache and exports no
decompiled files.

## Basic-block pipeline audit

```sh
cargo run --release -- --native-cfg-audit app.apk
cargo run --release -- --native-cfg-audit app.apk com.example.
```

This independently runs operand decoding, basic-block construction, then dominator
and dominance-frontier analysis on concrete methods. It reports accepted/rejected
graphs, block/edge totals, separately accepted/rejected dominator analyses,
frontier entries, blocks unreachable from actual entry and bounded failure
samples for each stage. Instruction counters include decoded instructions, register
operand groups (a wide pair is one operand), and potentially throwing instructions.
Call-binding counters separately report accepted/rejected methods, bound calls,
receivers, typed arguments and used results. They include filled-array producers.
The binder checks method references, effective prototypes, wide-word grouping and
adjacent result types/control-flow entries. Unsupported call-site metadata is an
explicit rejection. A method with no calls can pass with zero bound calls.
Decoder acceptance alone does not imply valid signatures or verifier validity;
neither does binding establish virtual dispatch or semantic equivalence.
It does not invoke the Java renderer, run SSA or prove semantic
correctness. Exceptional edges currently conservatively include every instruction
in a protected region, including instructions that cannot throw. The graph is
discarded after each method. This command is for port validation and does not
switch the GUI to the new pipeline. A graph accepted by the splitter can still
fail a later stage; graph acceptance is never counted as dominator acceptance.
Operand decoding is audited independently of graph construction: an operand failure
does not conceal graph diagnostics for the same method.

See [the pinned pass mapping and measured baseline](jadx-port-plan.md).
