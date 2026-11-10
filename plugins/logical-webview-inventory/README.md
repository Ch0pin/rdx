# Logical WebView inventory — preview

The analyzer is a standalone Python 3.9+ executable plugin, with no third-party Python dependencies. It uses RDX protocol v2 host queries against the project already loaded in RDX. It does not open the APK, run Java reconstruction, attach to a device, install hooks, or collect runtime traffic.

Requires an RDX build with protocol-v2 project analysis support (current main). The released v0.8.0 predates this integration. RDX itself remains native and needs no Python; this optional plugin requires Python 3.9 or newer on PATH. If your interpreter is named `python` rather than `python3`, change the manifest's `executable` value, or supply its absolute path.

The canonical source is [RDX plugins/logical-webview-inventory](https://github.com/Ch0pin/rdx/tree/main/plugins/logical-webview-inventory). Release archives place bundled plugins in their `plugins` directory; standalone macOS bundles also include them under `RDX.app/Contents/Resources/plugins`.

## Try it in the GUI

1. Open an APK in RDX, or open the included `demo/classes.dex` fixture from this plugin's directory.
2. Open **Tools → Plugins**, choose **Add plugin…**, select this directory's `plugin.json`, and tick its enable checkbox.
3. Click **Run analysis**. A separate results window explains each WebView reference in plain language. **Show code locations** opens the relevant methods; **Technical evidence** reveals the exact receiver/register information. **Copy JSON for an agent** preserves the structured evidence exposed through MCP. Both the Plugins manager and results window can be moved outside the main RDX window.

The demo contains five sink sites and should yield four groups: Help (two sinks), Checkout, an embedded factory allocation, and a dynamic parameter. Help and Checkout have distinct client registrations. `Screen.java` is the original demo source; the DEX is static analysis input, not an installable application.

The optional JSON field under **Advanced analysis options** accepts, for example:

```json
{"package_prefix":"com.example","max_sites":100,"include_callers":true}
```

An empty object scans the first 100 matching sites across the loaded project. `max_sites` is 1–200. Exceeding a budget is reported as partial coverage; narrow the package prefix to inspect another scope. Prefixes select sink-containing classes and can exclude obfuscated top-level packages.

## Agent access

Enable **Tools → MCP Server** in RDX. Use its instance/project IDs with:

```json
{"tool":"list_analysis_plugins","arguments":{"instance_id":"INSTANCE","project_id":"PROJECT"}}
{"tool":"run_plugin_analysis","arguments":{"instance_id":"INSTANCE","project_id":"PROJECT","plugin_id":"logical-webview-inventory","arguments":{"max_sites":100}}}
{"tool":"get_plugin_analysis","arguments":{"instance_id":"INSTANCE","project_id":"PROJECT","analysis_id":"ANALYSIS","offset":0,"limit":25}}
```

Poll the returned analysis ID until `status` is `complete`, `failed`, or `cancelled`, then follow `next_offset` for all groups. `complete` is job completion; `result.coverage` states analysis coverage separately. `cancel_plugin_analysis` requests cancellation. The GUI result selector includes jobs started through MCP. Agents can invoke enabled plugin IDs; the MCP endpoints do not accept executable paths or enable plugins.

## What a group means

Each group has an APK-scoped static ID, receiver evidence, sink method IDs/DEX code-unit offsets, local client registration candidates, direct caller context, and declared Activity/Fragment ancestry where available. A returned local allocation is labelled as a factory candidate. The IDs remain stable for the same APK and receiver origin; they identify static groups, not runtime instances.

The plugin queries five platform signatures: both `loadUrl` overloads, `loadData`, `loadDataWithBaseURL`, and `evaluateJavascript`. RDX's invoke index resolves supported inherited aliases. Overridden methods, reflection, dynamically loaded code and native calls may require additional tracing.

Register tracking follows moves, allocations, fields and call results over bounded control flow. Joins retain only origins agreed by every incoming path; protected instructions contribute conservative pre-instruction state to exception handlers. Switch-containing methods remain unresolved until switch control flow is supported. Parameter mapping accounts for the receiver and two-register long/double arguments. It does not infer Compose routes, field writers in other methods, cross-method object aliases, client execution order or runtime reachability. Empty client/host lists mean no supported evidence was found, not proof that none exists.

No URL/header values are included in the inventory. The result contains code identities and static relationships. Logs use stderr; stdout carries only the JSON protocol.

## Tests

```sh
# From the RDX repository root:
python3 -m unittest discover -s plugins/logical-webview-inventory/tests -v
```

The RDX host includes transport, enablement, query allowlist, cancellation, paging and project identity tests. Its opt-in `supplied_plugin_inventory` test exercises this actual executable against a supplied DEX/APK.
