//! Material Icon Theme associations and color rasters for Explorer.

use gpui::{
    App, Bounds, Corners, DevicePixels, Global, ObjectFit, Pixels, RenderImage, SvgRenderer,
    SvgSize, Window, size,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::Path,
    sync::{Arc, LazyLock},
};

/// Original SVG bytes indexed by the ranges in the bundled theme.
const SVG_PACK: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icons/material/icons.pack"
));

/// Case-insensitive names or suffixes mapped to upstream SVG identifiers.
type Associations = HashMap<String, String>;

/// Defaults and associations from the dark variant of the published theme.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Theme {
    /// Generic file icon.
    file: String,
    /// Generic collapsed and expanded directory icons.
    folder: String,
    folder_expanded: String,
    /// Default icon for the always expanded Explorer root.
    root_folder_expanded: String,
    /// File associations by name and by dot-separated suffix.
    file_names: Associations,
    file_extensions: Associations,
    /// Directory associations for each expansion state.
    folder_names: Associations,
    folder_names_expanded: Associations,
    /// Expanded workspace root associations by directory name.
    root_folder_names_expanded: Associations,
    /// Byte index for each upstream icon identifier.
    icons: HashMap<String, SvgAsset>,
}

/// A byte range and optional non-square aspect ratio in the embedded SVG pack.
#[derive(Deserialize)]
struct SvgAsset {
    /// Start and exclusive end byte offsets.
    range: [usize; 2],
    /// Intrinsic width divided by height when the SVG is non-square.
    aspect: Option<f32>,
}

/// Parse the checked-in theme index once for both matching and rasterization.
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/icons/material/theme.json"
    )))
    .expect("parse bundled Material Icon Theme index")
});

/// Upstream icon identifier shared by both Explorer painters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntryIcon(&'static str);

impl EntryIcon {
    /// Resolve `path` by its name and, for files, its longest matching suffix.
    /// `directory` selects folder rules, and `expanded` selects their open variant.
    pub(crate) fn for_path(path: &Path, directory: bool, expanded: bool) -> Self {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if directory {
            let (names, fallback) = if expanded {
                (&THEME.folder_names_expanded, &THEME.folder_expanded)
            } else {
                (&THEME.folder_names, &THEME.folder)
            };
            return Self(named_icon(names, path, &name).unwrap_or(fallback));
        }

        let id = named_icon(&THEME.file_names, path, &name)
            .or_else(|| {
                // Testing from the first dot prefers compound suffixes such as d.ts.
                name.match_indices('.').find_map(|(index, _)| {
                    THEME
                        .file_extensions
                        .get(&name[index + 1..])
                        .map(String::as_str)
                })
            })
            .unwrap_or(&THEME.file);
        Self(id)
    }

    /// Select the expanded workspace root icon for `path`.
    pub(crate) fn root(path: &Path) -> Self {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        Self(
            THEME
                .root_folder_names_expanded
                .get(&name)
                .unwrap_or(&THEME.root_folder_expanded),
        )
    }

    /// Render the original colors with `svg`, fitting the artwork within `edge` device pixels.
    pub(crate) fn rasterize(self, svg: &SvgRenderer, edge: i32) -> Option<Arc<RenderImage>> {
        let asset = THEME.icons.get(self.0)?;
        let [start, end] = asset.range;
        let parsed = svg.parse_svg(SVG_PACK.get(start..end)?).ok()?;
        let aspect = asset.aspect.unwrap_or(1.);
        let width = (edge as f32 * aspect.min(1.)).round().max(1.) as i32;
        let height = (edge as f32 / aspect.max(1.)).round().max(1.) as i32;
        svg.render_parsed(
            &parsed,
            SvgSize::ExactSize(size(DevicePixels(width), DevicePixels(height))),
        )
        .ok()
    }
}

/// Try the `parent/name` association before the plain `name` for `path`.
fn named_icon<'a>(names: &'a Associations, path: &Path, name: &str) -> Option<&'a str> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|parent| {
            names.get(&format!(
                "{}/{name}",
                parent.to_string_lossy().to_lowercase()
            ))
        })
        .or_else(|| names.get(name))
        .map(String::as_str)
}

/// Retains color sprites across frames, shared by the root and body of the native Explorer.
#[derive(Default)]
pub(crate) struct ExplorerIconCache(RefCell<IconRasters>);

impl Global for ExplorerIconCache {}

/// One window's icon rasters at its current physical icon size.
#[derive(Default)]
struct IconRasters {
    /// Maximum width or height of the raster, in device pixels.
    edge: i32,
    /// Successful or failed rasters, filled only as icons are requested.
    images: HashMap<EntryIcon, Option<Arc<RenderImage>>>,
}

impl ExplorerIconCache {
    /// Paint `icon` within logical `bounds`, using `window`'s current DPI and `cx`'s SVG renderer.
    pub(crate) fn paint(
        &self,
        icon: EntryIcon,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &App,
    ) {
        let edge = (f32::from(bounds.size.width) * window.scale_factor())
            .round()
            .max(1.) as i32;
        let mut rasters = self.0.borrow_mut();
        if rasters.edge != edge {
            for image in rasters.images.drain().filter_map(|(_, image)| image) {
                let _ = window.drop_image(image);
            }
            rasters.edge = edge;
        }
        let image = rasters
            .images
            .entry(icon)
            .or_insert_with(|| icon.rasterize(&cx.svg_renderer(), edge));
        if let Some(image) = image {
            let image_bounds = ObjectFit::Contain.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(
                bounds,
                image_bounds,
                Corners::default(),
                image.clone(),
                0,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact names, parent-qualified names, and compound suffixes precede generic matches.
    #[test]
    fn files_follow_material_association_precedence() {
        for (path, expected) in [
            (".GITIGNORE", "git"),
            ("README.MD", "readme"),
            ("guide.md", "markdown"),
            (".config/graphqlrc.json", "graphql"),
            ("other/graphqlrc.json", "json"),
            ("types.D.TS", "typescript-def"),
            ("entry.spec.ts", "test-ts"),
            ("hello.PY", "python"),
            ("main.rs", "rust"),
            ("main.go", "go"),
            ("go.mod", "go-mod"),
            ("Worker.java", "java"),
            ("build.ps1", "powershell"),
            ("cover.PNG", "image"),
            ("cover.svg", "svg"),
            ("bundle.zip", "zip"),
            ("bundle.7z", "zip"),
            ("bundle.tar.gz", "zip"),
            ("data.nanobug-unknown", "file"),
        ] {
            assert_eq!(
                EntryIcon::for_path(Path::new(path), false, false).0,
                expected,
                "{path}"
            );
        }
    }

    /// Opening a directory preserves its category and selects the matching expanded artwork.
    #[test]
    fn folders_follow_names_and_expansion() {
        for (path, closed, open) in [
            ("Assets", "folder-resource", "folder-resource-open"),
            ("tests", "folder-test", "folder-test-open"),
            ("src", "folder-src", "folder-src-open"),
            (
                ".github/Workflows",
                "folder-gh-workflows",
                "folder-gh-workflows-open",
            ),
            ("ordinary-dir.nanobug", "folder", "folder-open"),
        ] {
            assert_eq!(
                EntryIcon::for_path(Path::new(path), true, false).0,
                closed,
                "{path}"
            );
            assert_eq!(
                EntryIcon::for_path(Path::new(path), true, true).0,
                open,
                "{path}"
            );
        }
        assert_eq!(
            EntryIcon::root(Path::new("workspace")).0,
            "folder-root-open"
        );
    }

    /// Native GPUI rendering must resolve every bundled SVG, including multi-color artwork.
    #[test]
    fn material_svgs_render_at_explorer_size() {
        let svg = SvgRenderer::new(Arc::new(()));
        for id in THEME.icons.keys() {
            let image = EntryIcon(id)
                .rasterize(&svg, 16)
                .unwrap_or_else(|| panic!("render {id}"));
            let pixels = image.as_bytes(0).unwrap().as_chunks::<4>().0;
            assert!(pixels.iter().any(|pixel| pixel[3] > 0), "empty icon {id}");
        }

        let image = EntryIcon::for_path(Path::new("main.py"), false, false)
            .rasterize(&svg, 16)
            .unwrap();
        let pixels = image.as_bytes(0).unwrap().as_chunks::<4>().0;
        assert!(
            pixels
                .iter()
                .any(|&[b, _, r, a]| a == 255 && b > 180 && r < 35)
        );
        assert!(
            pixels
                .iter()
                .any(|&[b, g, r, a]| a == 255 && b < 85 && g > 180 && r > 210)
        );
    }
}
