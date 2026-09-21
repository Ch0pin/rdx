# Native Rust architecture

RDX is a native Rust application. Both GUI and CLI call `NativeEngine` in-process.
There is no Java implementation, JVM launch, worker protocol, Java dependency,
heap preference, or fallback engine. `engines.rs` is a catalog for future Rust ports.

## Engine and representations

`native_dex.rs` reads bounded DEX metadata and instruction words. Metadata strings
are interned per DEX and operand tables are shared between classes using `Arc`.
`native_engine.rs` loads APK/multidex and delegates Java reconstruction to `native_java`. Its register-value lowering
materializes effects into typed locals, preserving instruction order; unsupported
complex cyclic control flow and nested/multiple exception regions fail closed.
Wide arithmetic, supported joins and loops track register-pair ownership; wide
exception snapshots remain unsupported.
Forward branches and switches use bounded graph analysis. Single-entry while/do
loops use explicit condition/break emission to keep condition effects in order;
loop-carried assignments snapshot their inputs before updating the next iteration.
Switch keys and payload destinations are validated before traversing code;
branch-local values merge through typed locals with globally unique names.
Common continuations and their effects are emitted once. Integer, boolean and
reference comparisons retain their DEX meaning; reference comparisons use
java.lang.Object upcasts instead of introducing checked downcasts. Class and method emitters build exact Unicode source mappings as they
write, including overloaded declaration identities. `native_disassembly.rs` exposes actual instructions, operands, and
references for unsupported content. Supported methods replace their original DEX
blocks once alongside any remaining DEX members; preserved and replaced spans
are remapped. Representable declarations use Java class/interface syntax. It does not
pretend to reconstruct missing control flow, types or annotations. Encoded static
values, try regions and declared exceptions have checked native metadata readers;
supported fields and single-region handlers receive Java output. Semantic
register values remain immutable; exception handlers read separate mutable
snapshots updated only after successful writes. Deferred class resolution uses
unique capture locals, preserving aliases and allocation order.

A project-wide immutable class hierarchy is shared across DEX symbol tables.
Bounded ancestry queries and a bounded shared cache support exception typing;
missing or ambiguous definitions remain unknown. Java 25 constructor prologues
preserve supported own-field writes before superclass initialization. Generated
member aliases retain raw DEX identities in navigation mappings.

Bounded liveness tracks normal and exception successors independently. Exceptional
successors observe pre-write register state. Dead joins are pruned only with
positive analysis evidence; an unsupported instruction or exhausted budget
disables the optimization. The pass is skipped for straight-line methods.

`engine.rs` is the application interface: opening, native rendering, definition
names, resources, symbol navigation and usages. Source identities use a local
deterministic fingerprint, not a cryptographic authenticity check. Native symbol
identities include method prototypes and field types; external targets fail
explicitly. Usage scans inspect generated native reference mappings; they do not
establish reflection/runtime-only references or references in unported metadata.

`native_resources.rs` decodes Android compiled XML in Rust. Resource references
remain numeric IDs; ARSC name resolution and enum/flag names remain unported.
Assets use bounded archive/text/image readers without extraction or execution.

## GUI and search

The GUI runs engine work on background Rust threads. Search matches the native
representation directly, using cached source before rendering missing classes.
Application packages come first; user exclusions filter before source generation.
Metadata-only definition search narrows candidates. Matching, Unicode offsets,
regexes, comment classification, result delivery and indexing are Rust code.
Code search over disassembly is not equivalent to searching reconstructed Java.

The project-session index is bounded to 1 GiB disk, 96 MiB conservative index
metadata, and 64 MiB / 10,000 documents in its RAM tier. Records contain a small
metadata frame and raw UTF-8 source. Results stop at 1,000 hits / 32 MiB retained
matching content. Native searches cache navigation metadata with matched source.
Cancellation is checked between classes and resources. There are no source
handoff files or Java code caches.

Worker events explicitly wake the root and search viewports. The root drains
completion events; an unfinished search never displays a rounded 100%. A short
root repaint heartbeat while searching also keeps completion delivery active.

## Plugins and portability

The built-in sample plugin is a Rust executable. Optional user-selected external
plugins use a bounded JSON process transport; this transport is not a decompilation
worker. Third-party plugins are explicitly enabled and can be arbitrary native
executables or interpreters installed by the user. No Java plugin is bundled.

Rust/egui platform support remains macOS, Windows and Linux. This revision's GUI
was built on macOS; cross-platform compilation/interactive behavior needs CI and
platform testing. Dependency licensing and the adapted JADX algorithm notices
remain in `third_party/jadx`.
