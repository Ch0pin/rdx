# Bundled RDX plugins

Each plugin has its own directory containing a manifest, documentation, and any source, tests, or demo fixtures. Plugins are optional and require explicit enablement in **Tools → Plugins**.

| Plugin | Capability | Requirements |
| --- | --- | --- |
| [Source statistics](https://github.com/Ch0pin/rdx/tree/main/plugins/source-stats) | Analyze the active class (protocol 1) | Bundled native `rdx-source-stats` executable |
| [Logical WebView inventory](https://github.com/Ch0pin/rdx/tree/main/plugins/logical-webview-inventory) | Analyze the loaded project (protocol 2) | Python 3.9+; no third-party Python packages |

Load a plugin's `plugin.json`, enable its checkbox, then run the analysis. Adding these directories does not enable or execute plugins automatically. See the [plugin protocol](https://github.com/Ch0pin/rdx/blob/main/docs/plugins.md) for developing another plugin.

Release archives contain a `plugins` directory. Standalone macOS bundles also contain plugin files under `RDX.app/Contents/Resources/plugins`. RDX itself requires no Python; the WebView analyzer is an optional Python plugin.
