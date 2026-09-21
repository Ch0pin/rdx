# Bundled code fonts

RDX embeds unmodified, static, regular-weight TrueType fonts. No system font
installation or network connection is required. Their SIL Open Font License 1.1
notices are included beside this file. The font names remain their original names.

| Font | Official source pinned for this copy | Local license | TTF SHA-256 |
| --- | --- | --- | --- |
| JetBrains Mono | [JetBrains/JetBrainsMono, commit 19371302b95d218af43299bce79ddbddd0bc364d](https://github.com/JetBrains/JetBrainsMono/blob/19371302b95d218af43299bce79ddbddd0bc364d/fonts/ttf/JetBrainsMono-Regular.ttf) | [JetBrainsMono-OFL.txt](JetBrainsMono-OFL.txt) | `e6fd0d7e91550b3ed2b735d4312474362c4716edc4fc0577a0f61ed782d5aed1` |
| Fira Code | [Official 6.2 release archive](https://github.com/tonsky/FiraCode/releases/download/6.2/Fira_Code_v6.2.zip), `ttf/FiraCode-Regular.ttf`; tag commit `eee6db993696aba61ff4eef03698e2987d79910c` | [FiraCode-LICENSE.txt](FiraCode-LICENSE.txt) | `5992ab9640e2df491b2f609467b1de60e8bc39b2c28db184342a0592d98f6117` |
| Source Code Pro | [adobe-fonts/source-code-pro, commit 803b7e23ec97ae58b6232ea76519a76d428ba268](https://github.com/adobe-fonts/source-code-pro/blob/803b7e23ec97ae58b6232ea76519a76d428ba268/TTF/SourceCodePro-Regular.ttf) | [SourceCodePro-LICENSE.md](SourceCodePro-LICENSE.md) | `74bd80d3e42a08517cd7e1108ba3d86f2da29ac0f3065be95e0357956ab9db37` |

The embedded TTF files live in `assets/fonts/`. Named editor families retain egui's
default monospace fallback chain for characters absent from the selected font.
Programming ligature shaping is not promised by the viewer.
