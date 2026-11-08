# Material Icon Theme

The SVG artwork and filename, extension, and directory associations come from the published Material Icon Theme v5.40.0 VSIX. The upstream MIT notice accompanies the assets. [Upstream release](https://github.com/material-extensions/vscode-material-icon-theme/releases/tag/v5.40.0)

`icons.pack` concatenates the original SVG bytes. `theme.json` indexes each SVG by its start and end byte offsets and records the case-insensitive associations used by Explorer. The optional aspect ratio keeps non-square artwork proportional in the 16 px icon slot.

To regenerate these files, pass the official VSIX to `python tools/import_material_icons.py`. The importer checks the release checksum. Cargo uses the checked-in assets for offline builds.
