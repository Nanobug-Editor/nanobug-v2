//! Read the static sizing metadata accepted on Markdown preview images.

use scraper::ElementRef;

use super::layout::style_value;

/// Width requested by an HTML image, before fitting it into the preview column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::grid::windows::editor::markdown) enum ImageWidth {
    /// Fixed width in logical CSS pixels.
    Pixels(f32),
    /// Percentage of the available preview column width.
    Percent(f32),
}

/// Optional size overrides carried by an HTML image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::grid::windows::editor::markdown) struct ImageSize {
    /// Width from the HTML attribute or inline style.
    pub width: Option<ImageWidth>,
    /// Height in logical CSS pixels.
    pub height: Option<f32>,
}

impl ImageSize {
    /// Read supported width and height declarations from `element`, preferring valid inline styles.
    pub(super) fn parse(element: ElementRef<'_>) -> Self {
        let attribute_width = element.attr("width").and_then(|value| width(value, false));
        let attribute_height = element.attr("height").and_then(attribute_pixels);
        Self {
            width: style_value(element, "width", |value| width(value, true)).or(attribute_width),
            height: style_value(element, "height", css_pixels).or(attribute_height),
        }
    }
}

/// Read a percentage width or a pixel `value`, using CSS units when `css` is true.
fn width(value: &str, css: bool) -> Option<ImageWidth> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        number(percent).map(ImageWidth::Percent)
    } else {
        let pixels = if css {
            css_pixels(value)
        } else {
            attribute_pixels(value)
        };
        pixels.map(ImageWidth::Pixels)
    }
}

/// Read a unitless HTML pixel length or an explicit pixel suffix from `value`.
fn attribute_pixels(value: &str) -> Option<f32> {
    let value = value.trim();
    number(value).or_else(|| css_pixels(value))
}

/// Read a CSS pixel length from `value`, also accepting unitless zero.
fn css_pixels(value: &str) -> Option<f32> {
    if value == "0" {
        return Some(0.);
    }
    let (number_text, unit) = value.split_at_checked(value.len().checked_sub(2)?)?;
    unit.eq_ignore_ascii_case("px")
        .then(|| number(number_text))?
}

/// Parse a finite non-negative size `text` in the range of an HTML pixel dimension.
fn number(text: &str) -> Option<f32> {
    let value: f32 = text.parse().ok()?;
    (0. ..=u32::MAX as f32).contains(&value).then_some(value)
}
