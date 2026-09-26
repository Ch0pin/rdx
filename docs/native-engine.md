# Native decompilation engine

RDX reads APK and DEX files and reconstructs Java in native Rust. The GUI, CLI,
and MCP server use the same engine; no Java runtime is required to run RDX.

## Reconstruction

The engine supports typed instructions, method calls and constructors, fields,
arrays, arithmetic, branches, switches, and supported loop and exception patterns.
Shared instruction decoding and signature binding feed reconstruction for eligible
methods. Control-flow and dominance analysis support bounded compositions of
branches and loops, including nested pretest loops and simple posttest loops.

Reconstruction is incremental. Unsupported shapes retain native DEX disassembly,
so the viewer can display reconstructed Java alongside remaining DEX methods.
Source navigation retains original class, method, field, and instruction identities.

## Resources and navigation

RDX decodes Android binary XML and resource tables, resolves supported resource
references and attribute values, and provides navigation between code and resources.
Code review tools include usages, callers and callees, direct subclasses,
implementations, and a call graph with highlighted paths to Android components.
Static call graphs do not capture every reflective or runtime-dispatched call.

## Limits

The engine is under active development. Reconstruction does not establish semantic
equivalence, compilability of an entire application, or support for every Android
app. Complex control flow, exceptional state, missing types, and unsupported
metadata can still require DEX output.

Readable constructor staging can change allocation timing: argument preparation
may run before an allocation failure that would have occurred earlier in DEX.
This is a known limit, not exact behavioral equivalence.

See [architecture](architecture.md), [coverage measurement](native-coverage.md),
[validation](validation.md), and [upstream attribution](../third_party/jadx/README.md).
