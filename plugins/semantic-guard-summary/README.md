# Semantic guard summary

This optional protocol-2 plugin presents RDX's native DEX proof for one small
String comparison helper or one exact static call site. It requires Python 3.9+
and uses the standard library. Enable it explicitly in **Tools → Plugins**.

Provide one of these selector shapes in the project's analysis arguments.

```json
{"method_id":"example.Guard.equal(Ljava/lang/String;Ljava/lang/String;Z)Z"}
```

```json
{"caller_method_id":"example.Caller.run()V","call_site_offset":38}
```

The offset is an original DEX code-unit offset. The human card shows the
supported comparisons, proved boolean arguments and locations. The technical
record keeps the native proof, including operand directions, per-case null
behavior and method hashes. Proven caller literals specialize cases. The card
shows packaged resource strings separately from those literals and retains
conditional cases for the runtime resource input.

Unsupported effects, dispatch, configurations and control flow remain
unresolved. The result describes comparison semantics.
