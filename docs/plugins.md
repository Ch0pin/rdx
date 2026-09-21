# Local plugin protocol

RDX currently supports one capability: `source.analyze`. A plugin receives the currently selected class's native source representation (Java or explicitly labeled DEX disassembly) and returns a JSON value for display. It cannot contribute GUI panels, replace an engine, or hook decompilation passes through this interface.

## Manifest

Create a UTF-8 JSON manifest:

```json
{
  "protocol": 1,
  "id": "example.source-analyzer",
  "name": "Example source analyzer",
  "executable": "rdx-source-stats",
  "executable_base": "application",
  "capability": "source.analyze"
}
```

Unknown fields are rejected. `args` and `executable_base` may be omitted; the other fields are required. The manifest is limited to 64 KiB. Only protocol `1` and capability `source.analyze` are accepted.

With the default `executable_base: "environment"`, a bare executable name is resolved through the process environment. An executable containing path components is resolved against the manifest directory; absolute executable paths are also accepted. The child process runs with the manifest directory as its working directory. Arguments are passed directly to the executable, without a shell.

With `executable_base: "application"`, the host loads a bare executable filename beside the RDX executable, automatically adding `.exe` on Windows. Cargo test executables also resolve the sibling binary from their parent target profile directory. No PATH fallback is used in this mode. Build the bundled native example with `cargo build --bins` (or `cargo build --release --bins`); application packaging must place `rdx-source-stats` beside `rdx`.

## Request and response

Read a newline-terminated JSON object from stdin:

```json
{"protocol":1,"id":1,"method":"source.analyze","class":"example.Main","source":"package example;\nclass Main {}"}
```

Write a matching response to stdout and flush:

```json
{"protocol":1,"id":1,"result":{"message":"Analysis complete"}}
```

Or return an error:

```json
{"protocol":1,"id":1,"error":"Could not analyze this source"}
```

Use stderr for logs. Response lines must not exceed 16 MiB. The host applies a 60-second request timeout after serialization and queuing; this covers blocked stdin writes as well as waiting for the response. Fatal transport errors terminate and reap the direct child process and close the transport. A new process is started for each analysis invocation and disposed after the result, so plugins should not rely on persistent process state. Request IDs must always be echoed rather than assumed.

## Trust and compatibility

Loading a manifest reads metadata only. Use the toolbar’s **Plugins** button, load a manifest, tick its name to enable it, then choose **Run on active class**. The GUI requires explicit enablement and invocation before execution. Loaded manifests and enablement are session-local; there is no saved plugin configuration. A plugin process inherits ordinary user privileges and can access files or the network regardless of its declared capability. The capability describes the protocol, not an enforced permission boundary. Run only plugins you trust.

This is an initial RDX-specific protocol. Existing JADX GUI plugins are not compatible, and there is no plugin marketplace, signature verification, dependency installer, automatic update mechanism, or operating-system sandbox.

See [`src/bin/rdx-source-stats.rs`](../src/bin/rdx-source-stats.rs) and [`plugins/source-stats`](../plugins/source-stats) for the bundled Rust example. It needs no interpreter and counts Unicode characters, rather than UTF-8 bytes. External plugins may still use any executable explicitly selected by the user.
