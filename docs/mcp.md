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

## Currently available (15 tools)

| Area | Tools |
| --- | --- |
| Instances | `list_instances`, `get_instance_info`, `open_apk` |
| Classes | `get_all_classes`, `search_classes_by_keyword`, `find_direct_subclasses` |
| Source and metadata | `get_class_source`, `get_class_disassembly`, `get_methods_of_class`, `get_fields_of_class` |
| Resources | `get_all_resource_file_names`, `get_android_manifest`, `get_resource_file`, `get_strings` |
| DEX strings | `get_dex_strings` (strings from the DEX containing `class_id`) |

Search is currently a literal class-name substring search, not a method-body
search. Resource strings include configurations; binary resources are not exposed.
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
