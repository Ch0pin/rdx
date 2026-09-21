# Embedded code palettes

RDX includes native Syntect adaptations of four MIT-licensed editor palettes.
They need no extension host, Java runtime, network requests, or theme downloads.
These are palette adaptations, not complete VS Code/Atom UI themes. RDX maps
common Syntect syntax scopes for Java, XML, JSON and other supported viewers.

| RDX palette | Pinned upstream | Retained reference and license |
| --- | --- | --- |
| Atom One Light | [atom/one-light-syntax @ d845790](https://github.com/atom/one-light-syntax/tree/d84579027410c576086dfca14d934c4bd74b0438) | `atom-one-light/colors.less`, `atom-one-light/LICENSE.md` |
| Quiet Light | [microsoft/vscode @ b80ed11](https://github.com/microsoft/vscode/tree/b80ed11daaaef730fe8a136210b4e3b7e4eb884b/extensions/theme-quietlight) | `quiet-light/quietlight-color-theme.json`, `quiet-light/LICENSE.txt` |
| One Dark | [atom/one-dark-syntax @ 9c96f44](https://github.com/atom/one-dark-syntax/tree/9c96f4454362267ac45322063e193ccf9d2debb1) | `one-dark/colors.less`, `one-dark/LICENSE.md` |
| Dracula | [dracula/visual-studio-code @ a08a206](https://github.com/dracula/visual-studio-code/tree/a08a206f2c8420ba3c05f0e8d01d43b2f933fdf8) | `dracula/dracula.yml`, `dracula/LICENSE` |

Atom One Light and Quiet Light were selected from the user's suggested
[Best Light Themes Pack](https://github.com/geoffstevens8/best-light-themes-pack).
The pack is an extension list; the actual palettes and licenses come from the
upstream projects above.

Implementation: `src/code_view/palettes.rs`. HSL Atom colors are converted to
RGB. RDX darkens Atom One Light comments, strings, function names and variables,
darkens Quiet Light comments and strings, and brightens One Dark variables and comments and Dracula
comments for readability. Tests require at least 4.5:1 contrast between all
embedded token colors and each new palette's background. Scope mappings are
RDX-specific; font weights, semantic highlighting and editor chrome from the
upstream themes are not replicated. Existing Syntect palettes remain unchanged.
