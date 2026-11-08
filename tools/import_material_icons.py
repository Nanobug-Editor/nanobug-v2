"""Bundle the original Material Icon Theme SVGs and dark theme associations."""

import argparse
from hashlib import file_digest
import json
from pathlib import Path, PurePosixPath
from xml.etree import ElementTree
from zipfile import ZipFile


# Use the published VSIX so associations match the shipped theme.
RELEASE = "https://github.com/material-extensions/vscode-material-icon-theme/releases/tag/v5.40.0"
SHA256 = "55b565f200881abfd807baf9be52fce45fd52fef013ba71b0f8394984ce1ae06"
OUTPUT = Path(__file__).resolve().parents[1] / "assets" / "icons" / "material"

# Explorer's workspace root is always expanded. Individual entries use both folder states.
DEFAULTS = ("file", "folder", "folderExpanded", "rootFolderExpanded")
ASSOCIATIONS = (
    "fileNames",
    "fileExtensions",
    "folderNames",
    "folderNamesExpanded",
    "rootFolderNamesExpanded",
)


def import_theme(vsix: Path) -> None:
    """Build the checked-in byte pack and index from the pinned upstream archive."""
    with vsix.open("rb") as stream:
        if file_digest(stream, "sha256").hexdigest() != SHA256:
            raise ValueError(f"Expected the published VSIX from {RELEASE}")

    with ZipFile(vsix) as archive:
        theme = json.loads(archive.read("extension/dist/material-icons.json"))
        license_text = archive.read("extension/LICENSE.txt")
        pack = bytearray()
        icons = {}
        for name, definition in sorted(theme["iconDefinitions"].items()):
            path = PurePosixPath(definition["iconPath"])
            if path.parent != PurePosixPath("../icons") or path.suffix != ".svg":
                raise ValueError(f"Unexpected icon path: {path}")
            svg = archive.read(f"extension/icons/{path.name}")
            root = ElementTree.fromstring(svg)
            _, _, width, height = map(float, root.attrib["viewBox"].replace(",", " ").split())
            if width <= 0 or height <= 0:
                raise ValueError(f"Invalid viewBox for {path}")
            start = len(pack)
            pack.extend(svg)
            icons[name] = {"range": [start, len(pack)]}
            if width != height:
                icons[name]["aspect"] = width / height

    manifest = {key: theme[key] for key in DEFAULTS}
    used_icons = set(manifest.values())
    for key in ASSOCIATIONS:
        rules = theme[key]
        lower_rules = {name.lower(): icon for name, icon in rules.items()}
        if len(lower_rules) != len(rules):
            raise ValueError(f"Duplicate case-insensitive associations in {key}")
        manifest[key] = lower_rules
        used_icons.update(lower_rules.values())

    missing = used_icons - icons.keys()
    if missing:
        raise ValueError(f"Missing SVG definitions: {sorted(missing)}")
    manifest["icons"] = icons
    manifest["source"] = {"release": RELEASE, "vsixSha256": SHA256}

    OUTPUT.mkdir(parents=True, exist_ok=True)
    (OUTPUT / "icons.pack").write_bytes(pack)
    (OUTPUT / "theme.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    (OUTPUT / "Material-Icon-Theme-MIT.txt").write_bytes(license_text)
    print(f"Packed {len(icons)} upstream SVGs ({len(pack):,} bytes)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("vsix", type=Path, help="Path to the upstream v5.40.0 VSIX")
    import_theme(parser.parse_args().vsix)
