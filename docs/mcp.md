# MCP server

Open **Tools → MCP Server…** in RDX. Open an APK/DEX, select **Start server**,
then **Copy MCP client configuration** and paste that JSON into your client's MCP
configuration. Restart the client connection after changing its configuration.
The server panel shows status, instance/project IDs, APK SHA-256, request count,
current request and errors. **Stop server** disables access without closing RDX.
**Cancel current request** cooperatively cancels source reconstruction.

The native `rdx mcp` command is the stdio gateway. No Python, JVM or separate
server install is required. Each enabled GUI instance has an authenticated
loopback service; the gateway discovers it through a private per-user registry.
The instance backend loads independently of the GUI engine, using additional
memory while enabled. Opening/reloading a GUI project stops that service.

## Agent flow

1. Call `list_instances`, or `open_apk` with an absolute local APK `path` and a
   unique `request_key`. Agent-opened APKs launch new GUI windows and automatically
   enable their service. Repeating a request key on the same gateway connection
   returns the same launch ID; different gateway connections have separate keys.
2. Poll `get_instance_info` until `state` is `Running`. Use its `instance_id` and
   `project_id` on each project tool call. `Loading` and `Failed` are explicit.
3. Get classes, then supply the exact returned `class_id` to source/metadata tools.
   Restarting the server changes project identity; stale requests are rejected.

## Currently available (19 tools)

| Area | Tools |
| --- | --- |
| Instances | `list_instances`, `get_instance_info`, `open_apk` |
| Classes | `get_all_classes`, `search_classes_by_keyword`, `find_direct_subclasses`, `find_implementations` |
| Code search | `search_code` |
| Source and metadata | `get_class_source`, `get_class_disassembly`, `get_methods_of_class`, `get_fields_of_class` |
| Resources | `get_all_resource_file_names`, `get_android_manifest`, `get_manifest_summary`, `get_resource_file`, `get_strings` |
| Call graphs | `get_call_graph` (exact `method_id`, `direction`: `callers` / `callees` / `both`, `depth`: 1–100, default 20) |
| DEX strings | `get_dex_strings` (strings from the DEX containing `class_id`) |

`search_classes_by_keyword` searches literal class-name substrings only.
`search_code` searches generated Java and mixed DEX source using a case-sensitive
literal query; it does not search resources. Resource strings include configurations; binary resources are not exposed.
Class identifiers currently follow the native engine's class-name identity.
Disassembly is `rdx-dex`, not assemblable Smali. Java source can contain explicit
DEX fallbacks. Method IDs retain full DEX signatures.

List responses contain `next_offset`; text responses use Unicode character
`offset` and `next_offset`. Continue until `next_offset` is null. Text paging
only limits tool responses: it does not truncate the GUI viewer. Lists default
to 100 items (maximum 500); text defaults to 32,000 characters (maximum 128,000).
The same filters and project identity must be retained across pages.

The broader [v1 design](mcp-v1.md) includes tools not implemented yet: method
source/search, X-Refs/callees, implementations, main-application helpers, manifest
component queries, resource resolution, GUI selection and coverage. Those tools
are deliberately absent from `tools/list`. Renaming and debugging are excluded.
The current gateway handles requests sequentially per connection; different
instances can process requests in parallel. Protocol cancellation notifications
and cross-connection open-request deduplication remain future work.

Protocol reference: [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
and [tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).

### Method call graph

`get_call_graph` requires `instance_id`, `project_id`, and an exact DEX
`method_id` from `get_methods_of_class`, such as `sample.Target.doubleValue(I)I`.
Use `direction: "callers"` to trace into a method, `"callees"` (default) to
follow outgoing calls, or `"both"`. Edges always point from caller to callee.

The response includes numbered nodes, method IDs, distance from the root,
component classifications, available declarations, call-site counts, and
component-path highlights. `truncated` reports the 5,000-node/20,000-edge cap;
`depth_boundary_reached` reports that nodes reached the requested depth.
The graph is returned as one response, without pagination. Caller queries scan
the loaded DEX call sites and may take longer than outgoing queries.

This is a static graph. It resolves inherited aliases through known superclass
metadata, but does not infer runtime virtual targets, reflection or Intent
routing. External method bodies cannot be expanded. The GUI graph continues
to follow outgoing calls; direction selection is currently an MCP feature.

`find_direct_subclasses` follows immediate superclass edges. Use `find_implementations` with `class_id` to list concrete direct and indirect implementations of an interface or class; it supports the same instance/project scope and offset/limit pagination.

## Bounded code search

`search_code` requires `instance_id`, `project_id` and `query`. Optionally set
`package_prefix` (package boundary match), `offset` (class index), `limit`
(classes to scan, default 25, maximum 500), and `source_offset` (Unicode character
offset inside the first class). Each response contains at most 100 matches.
Resume with both `next_offset` and `next_source_offset`, keeping the query and
package prefix unchanged. Continue until `exhausted` is true, and check every
page's `errors` before interpreting an empty result as absence. A failed class
is reported and does not silently count as successfully searched.

Hits include `class_id`, `source_hash`, Unicode `start`/`end`, one-based `line`,
and a bounded snippet. Source hashes identify generated text; the response
identity's APK SHA-256 identifies the input binary.

## Instance filters and manifest summary

`list_instances` accepts `apk_path` (substring), `apk_sha256` (exact), and
`package_name` (exact). Filters combine with AND and apply before pagination.
Loading instances may not have a package name yet. Filtering never replaces
explicit instance/project IDs on subsequent calls.

`get_manifest_summary` returns the same Markdown summary as the GUI, with text
pagination and the current project's identity. It covers application metadata,
exported components, permissions, and declared deeplinks. It describes manifest
declarations, not proven runtime delivery or callback reachability.

## Dispatch guidance

Call graphs expose `dispatch_targets`: matching virtual method declarations in
known superclass/interface ancestors of the root. `dispatch_targets_truncated`
indicates a bounded ancestor traversal. These are navigation/query suggestions,
not additional call edges or proven runtime targets. Query their callers
separately. The GUI offers the same declarations in a collapsed section.

Component highlights describe ancestry. An accepted service start does not
prove an app callback executed. Constructor/provider callers do not prove a
route is registered; route-table data flow and runtime intent delivery are not
inferred by this static graph.
