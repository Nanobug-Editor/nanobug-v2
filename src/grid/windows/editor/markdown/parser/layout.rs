//! Preserve static HTML layout hints understood by the native preview.

use scraper::ElementRef;

/// Horizontal alignment within a native text column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::grid::windows::editor::markdown) enum Alignment {
    /// Place content at the left edge of the column.
    #[default]
    Left,
    /// Center content in the column.
    Center,
    /// Place content at the right edge of the column.
    Right,
}

/// Read the last inline declaration of `name` on `element` accepted by `parse`.
pub(super) fn style_value<'a, T>(
    element: ElementRef<'a>,
    name: &str,
    parse: impl Fn(&'a str) -> Option<T>,
) -> Option<T> {
    declarations(element)
        .rev()
        .find_map(|(property, value)| property.eq_ignore_ascii_case(name).then(|| parse(value))?)
}

/// Resolve alignment on `element`, independent of any legacy element-specific `align` attribute.
pub(super) fn text_alignment(element: ElementRef<'_>) -> Option<Alignment> {
    style_value(element, "text-align", alignment)
}

/// Read the `element`'s text alignment or inherit `parent` when no supported hint is present.
pub(super) fn block_alignment(element: ElementRef<'_>, parent: Alignment) -> Alignment {
    text_alignment(element)
        .or_else(|| element.attr("align").and_then(alignment))
        .unwrap_or_else(|| {
            if element.value().name() == "center" {
                Alignment::Center
            } else {
                parent
            }
        })
}

/// Resolve an image's own horizontal position from its `element`'s auto margins or alignment.
pub(super) fn image_alignment(element: ElementRef<'_>) -> Option<Alignment> {
    let mut left = false;
    let mut right = false;
    for (property, value) in declarations(element) {
        if property.eq_ignore_ascii_case("margin") {
            let mut sides = value.split_ascii_whitespace();
            let Some(top) = sides.next() else { continue };
            let horizontal = sides.next().unwrap_or(top);
            let _bottom = sides.next();
            let trailing = sides.next().unwrap_or(horizontal);
            right = horizontal.eq_ignore_ascii_case("auto");
            left = trailing.eq_ignore_ascii_case("auto");
        } else if property.eq_ignore_ascii_case("margin-inline") {
            let mut sides = value.split_ascii_whitespace();
            let Some(start) = sides.next() else { continue };
            left = start.eq_ignore_ascii_case("auto");
            right = sides.next().unwrap_or(start).eq_ignore_ascii_case("auto");
        } else if property.eq_ignore_ascii_case("margin-left")
            || property.eq_ignore_ascii_case("margin-inline-start")
        {
            left = value.eq_ignore_ascii_case("auto");
        } else if property.eq_ignore_ascii_case("margin-right")
            || property.eq_ignore_ascii_case("margin-inline-end")
        {
            right = value.eq_ignore_ascii_case("auto");
        }
    }
    match (left, right) {
        (true, true) => Some(Alignment::Center),
        (true, false) => Some(Alignment::Right),
        (false, true) => Some(Alignment::Left),
        (false, false) => element.attr("align").and_then(alignment),
    }
}

/// Interpret a horizontal alignment `value` in the preview's left-to-right columns.
fn alignment(value: &str) -> Option<Alignment> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("center") || value.eq_ignore_ascii_case("middle") {
        Some(Alignment::Center)
    } else if value.eq_ignore_ascii_case("left") || value.eq_ignore_ascii_case("start") {
        Some(Alignment::Left)
    } else if value.eq_ignore_ascii_case("right") || value.eq_ignore_ascii_case("end") {
        Some(Alignment::Right)
    } else {
        None
    }
}

/// Iterate trimmed static declarations on `element` in source order.
fn declarations(element: ElementRef<'_>) -> impl DoubleEndedIterator<Item = (&str, &str)> {
    element
        .attr("style")
        .unwrap_or_default()
        .split(';')
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            Some((name.trim(), value.trim()))
        })
}
