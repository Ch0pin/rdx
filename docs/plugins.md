# Local plugin protocol

Protocol 1 supports `source.analyze`: a plugin receives the selected class's native source representation (Java or explicitly labelled DEX) and returns JSON. This branch also previews protocol 2 `project.analyze`, with bounded host queries against the already-loaded project. Plugins cannot replace an engine or inject decompilation passes. Project results use the host's generic evidence-group viewer.

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

Unknown fields are rejected. `args` and `executable_base` may be omitted; the other fields are required. The manifest is limited to 64 KiB. Accepted pairs are protocol `1` / `source.analyze` and protocol `2` / `project.analyze`.

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

## Project analysis preview (protocol 2)

Use a manifest with `protocol: 2` and `capability: "project.analyze"`. The GUI must explicitly enable it before GUI or MCP execution. **Run analysis** starts a background job; the optional JSON arguments field configures the analyzer. Protocol 1 plugins continue to use **Run on active class**.

The host sends `{"protocol":2,"id":1,"method":"project.analyze","project":{"apk_sha256":"...","class_count":10},"arguments":{}}`. To query the project, the plugin writes `{"protocol":2,"id":1,"method":"host.query","query_id":1,"tool":"get_method_source","arguments":{"method_id":"example.C.m()V"}}` and reads the matching response. The host returns `result` or `error`, echoing the analysis and query IDs. Query IDs must be unique positive or zero integers within an invocation.

The allowlist is `get_call_graph`, `get_method_source`, `get_methods_of_class`, `get_class_hierarchy`, `find_implementations`, and `find_instantiations`. Call graphs are forced to direct, paged `call_sites`; method source is forced to original DEX. Normal MCP selector validation applies. Queries share the existing parsed project and indexes; the plugin receives no local service credentials. This query allowlist is a protocol constraint, not an OS sandbox for the trusted executable.

Finish with `{"protocol":2,"id":1,"result":{"title":"Analysis","summary":"...","items":[{"group_id":"stable-id","label":"Group","method_id":"example.C.m()V"}],"coverage":{"status":"partial"}}}`. The Plugins manager and analysis results use independent native desktop windows. The results window renders optional `human` cards (title, explanation, observations and labelled source locations), while exact evidence stays under Technical evidence and in MCP JSON. Closing either window retains completed analyses. It shares results with MCP through `list_analysis_plugins`, `run_plugin_analysis`, `get_plugin_analysis`, and `cancel_plugin_analysis`.

One project analysis runs per session. The last four results are retained, each at most 4 MiB, with rows at most 512 KiB. Result pages contain at most 100 rows and 512 KiB of row data. Host responses are bounded to 2 MiB and analyses to 512 queries and 300 seconds. Cancellation stops the direct child process and signals cancellable host queries. A fresh process is created per run; no process persistence is assumed. Disabling a plugin or changing projects cancels its running work. Project identity is checked before and after analysis.

Job `status: complete` means execution finished; it does not assert complete analysis. Plugins must describe skipped scopes and unsupported flows in their result coverage. The GUI result selector includes jobs started through MCP.
