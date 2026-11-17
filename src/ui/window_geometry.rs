//! Persist the main native window's normal size, placement and display mode.

use gpui::{App, Bounds, DisplayId, Pixels, Window, WindowBounds, point, px, size};
use nanobug::app_data::settings;
use std::path::Path;
use toml_edit::{InlineTable, Value};

/// Name of the main-window placement inside the portable UI settings.
const KEY: &str = "window_bounds";

/// Bounds are relative to the identified display when its stable ID is available.
#[derive(Clone, Debug, PartialEq)]
struct Geometry {
    /// Windowed restore rectangle and whether to open maximized or fullscreen.
    bounds: WindowBounds,
    /// Stable identity of the original display across launches.
    display: Option<String>,
}

impl Geometry {
    /// Convert a saved inline `table` into GPUI bounds, rejecting invalid coordinates and sizes.
    fn from_settings(table: &InlineTable) -> Option<Self> {
        let number = |key| {
            let value = table.get(key)?;
            let value = value
                .as_float()
                .or_else(|| value.as_integer().map(|n| n as f64))? as f32;
            value.is_finite().then_some(px(value))
        };
        let dimensions = size(number("width")?, number("height")?);
        if dimensions.width <= px(0.) || dimensions.height <= px(0.) {
            return None;
        }
        let bounds = Bounds::new(point(number("x")?, number("y")?), dimensions);
        let bounds = match table.get("mode").and_then(Value::as_str) {
            Some("maximized") => WindowBounds::Maximized(bounds),
            Some("fullscreen") => WindowBounds::Fullscreen(bounds),
            _ => WindowBounds::Windowed(bounds),
        };
        let display = table
            .get("display")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(Self { bounds, display })
    }

    /// Encode the placement as one setting so it is replaced atomically.
    fn to_settings(&self) -> Value {
        let rect = self.bounds.get_bounds();
        let mut table = InlineTable::new();
        for (key, value) in [
            ("x", rect.origin.x),
            ("y", rect.origin.y),
            ("width", rect.size.width),
            ("height", rect.size.height),
        ] {
            table.insert(key, Value::from(f64::from(value.as_f32())));
        }
        let mode = match self.bounds {
            WindowBounds::Windowed(_) => "windowed",
            WindowBounds::Maximized(_) => "maximized",
            WindowBounds::Fullscreen(_) => "fullscreen",
        };
        table.insert("mode", Value::from(mode));
        if let Some(display) = &self.display {
            table.insert("display", Value::from(display.as_str()));
        }
        Value::InlineTable(table)
    }
}

/// Place `bounds` on `area` if its old display or usable title area is unavailable.
fn visible_bounds(
    bounds: Bounds<Pixels>,
    area: Bounds<Pixels>,
    same_display: bool,
) -> Bounds<Pixels> {
    let title_center = point(bounds.center().x, bounds.top());
    if same_display && area.contains(&bounds.center()) && area.contains(&title_center) {
        return bounds;
    }
    let dimensions = size(
        bounds.size.width.min(area.size.width),
        bounds.size.height.min(area.size.height),
    );
    Bounds::new(
        area.center() - point(dimensions.width / 2., dimensions.height / 2.),
        dimensions,
    )
}

/// Choose the launch bounds and display in `cx` from the main window settings at `path`.
pub(crate) fn startup(path: Option<&Path>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let saved = path.and_then(|path| {
        settings::read_ui(path, |ui| {
            Geometry::from_settings(ui.get(KEY)?.as_value()?.as_inline_table()?)
        })
        .ok()
        .flatten()
        .flatten()
    });
    let Some(saved) = saved else {
        return (WindowBounds::centered(size(px(1320.), px(840.)), cx), None);
    };
    let mut rect = saved.bounds.get_bounds();
    let matched = cx
        .displays()
        .into_iter()
        .find(|display| match &saved.display {
            Some(id) => display.uuid().is_ok_and(|uuid| uuid.to_string() == *id),
            None => display.bounds().contains(&rect.center()),
        });
    let same_display = matched.is_some();
    let Some(display) = matched.or_else(|| cx.primary_display()) else {
        return (saved.bounds, None);
    };
    if same_display && saved.display.is_some() {
        rect.origin += display.bounds().origin;
    }
    let rect = visible_bounds(rect, display.bounds(), same_display);
    let bounds = match saved.bounds {
        WindowBounds::Windowed(_) => WindowBounds::Windowed(rect),
        WindowBounds::Maximized(_) => WindowBounds::Maximized(rect),
        WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(rect),
    };
    (bounds, Some(display.id()))
}

/// Save the native placement of `window` to `path` in `cx` before the window is destroyed.
pub(crate) fn save(path: &Path, window: &Window, cx: &App) -> anyhow::Result<()> {
    let display = window.display(cx).and_then(|display| {
        display
            .uuid()
            .ok()
            .map(|id| (id.to_string(), display.bounds().origin))
    });
    let bounds = window.inner_window_bounds();
    let local = |rect: Bounds<Pixels>| display.as_ref().map_or(rect, |(_, origin)| rect - *origin);
    let geometry = Geometry {
        bounds: match bounds {
            WindowBounds::Windowed(rect) => WindowBounds::Windowed(local(rect)),
            WindowBounds::Maximized(rect) => WindowBounds::Maximized(local(rect)),
            WindowBounds::Fullscreen(rect) => WindowBounds::Fullscreen(local(rect)),
        },
        display: display.map(|(id, _)| id),
    };
    settings::save_ui_value(path, KEY, geometry.to_settings())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep normal coordinates exact and center a reachable placement when a display is removed.
    #[test]
    fn placement_round_trips_and_fits_a_replacement_display() {
        let saved = Bounds::new(point(px(-980.), px(91.5)), size(px(900.), px(540.)));
        let geometry = Geometry {
            bounds: WindowBounds::Maximized(saved),
            display: Some("old-display".into()),
        };
        let value = geometry.to_settings();
        assert_eq!(
            Geometry::from_settings(value.as_inline_table().unwrap()),
            Some(geometry)
        );
        let old = Bounds::new(point(px(-1920.), px(0.)), size(px(1920.), px(1080.)));
        assert_eq!(visible_bounds(saved, old, true), saved);
        let replacement = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(720.)));
        assert_eq!(
            visible_bounds(saved, replacement, false),
            Bounds::new(point(px(190.), px(90.)), saved.size)
        );
    }
}
