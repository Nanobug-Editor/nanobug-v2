//! Serialized drawing and interaction data for the experimental native Canvas.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A complete drawing in logical pixels relative to the plugin region.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Canvas {
    pub viewport: Option<u64>,
    /// Start or replace an incremental baseline with a nonzero sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    pub draw: Vec<Draw>,
    pub hits: Vec<Hit>,
    pub input: Option<Rect>,
    /// Host-owned editable text shaped in exact Canvas coordinates and painted above the drawing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_input: Option<TextInput>,
    /// Opt-in pointer, keyboard and frame callbacks for this complete scene.
    #[serde(skip_serializing_if = "Interaction::is_empty")]
    pub interaction: Interaction,
}

/// The host's current baseline for this live module and viewport.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Sync {
    pub generation: u64,
    pub viewport: Option<u64>,
    /// Host baseline epoch. Echo it in deltas so a reset cannot revive old operations.
    pub epoch: u64,
    /// Last successfully prepared sequence. Absent when a complete scene is required.
    pub sequence: Option<u64>,
}

/// Apply these changes atomically to the previous Canvas.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    pub generation: u64,
    pub viewport: Option<u64>,
    pub epoch: u64,
    pub base: u64,
    pub sequence: u64,
    pub changes: Vec<Change>,
}

/// A semantic single-line field. The host paints the confirmed draft using this layout.
/// Surrounding decorations and background can use ordinary drawing commands.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextInput {
    pub bounds: Rect,
    pub font: Option<String>,
    pub font_options: Option<FontOptions>,
    pub size: f32,
    pub color: Option<Rgba>,
}

impl TextInput {
    fn valid(&self) -> bool {
        rect(self.bounds)
            && self.bounds[2] >= 2.
            && (1. ..=128.).contains(&self.size)
            && self.font.as_ref().is_none_or(|font| font.len() <= 120)
            && self.font_options.as_ref().is_none_or(FontOptions::valid)
    }
}

/// An operation on the baseline Canvas. Coordinates and input retain the complete-scene rules.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Replace a range of top-level commands. Indices see preceding changes in this delta.
    Splice {
        start: usize,
        remove: usize,
        draw: Vec<Draw>,
    },
    Hits {
        hits: Vec<Hit>,
    },
    Interaction {
        interaction: Interaction,
    },
    Input {
        bounds: Option<Rect>,
    },
    TextInput {
        input: Option<TextInput>,
    },
}

impl Delta {
    /// Resolve a complete candidate before the host prepares or displays any changes.
    pub fn apply(self, mut canvas: Canvas) -> Option<Canvas> {
        if canvas.viewport != self.viewport
            || canvas.sequence != Some(self.base)
            || self.base.checked_add(1) != Some(self.sequence)
            || self.changes.is_empty()
            || self.changes.len() > 64
        {
            return None;
        }
        for change in self.changes {
            match change {
                Change::Splice {
                    start,
                    remove,
                    draw,
                } => {
                    let end = start.checked_add(remove)?;
                    if end > canvas.draw.len() || canvas.draw.len() - remove + draw.len() > 512 {
                        return None;
                    }
                    canvas.draw.splice(start..end, draw);
                }
                Change::Hits { hits } => canvas.hits = hits,
                Change::Interaction { interaction } => canvas.interaction = interaction,
                Change::Input { bounds } => canvas.input = bounds,
                Change::TextInput { input } => canvas.text_input = input,
            }
        }
        canvas.sequence = Some(self.sequence);
        canvas.validate().then_some(canvas)
    }
}

/// Coordinates use logical pixels. The host clips painting and hit testing to the plugin region.
pub type Point = [f32; 2];
/// Rectangle coordinates in x, y, width, height order.
pub type Rect = [f32; 4];
/// Corner radii in clockwise order starting at the top-left.
pub type Corners = [f32; 4];
/// Color in 0xRRGGBBAA order.
pub type Rgba = u32;

/// Logical region and physical scaling reported to the same module that supplies its scene.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Viewport {
    pub id: u64,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    /// Whether this host can paint a native Apple NV12 surface.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub apple_surface: bool,
    /// Whether this experimental UI host understands [`Draw::ShapedLine`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shaped_line: bool,
    /// Host colors, text size and language for this layout, when supplied by the editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
}

/// Style defaults shared with the editor's native UI.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Appearance {
    pub language: Language,
    /// Effective default font size in logical pixels, including user zoom.
    pub font_size: f32,
    /// Unscaled size selected in the editor, when the host publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_font_size: Option<f32>,
    /// Bundled family currently used by the editor's regular grid text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<EditorFont>,
    /// User interface scale, separate from the display's physical `Viewport::scale`.
    pub user_scale: f32,
    /// Named palette, separate from application-specific colors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<EditorTheme>,
    pub colors: ThemeColors,
}

/// Built-in monospace faces available in the editor.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorFont {
    RecMonoCasual,
    MapleMonoNlCn,
    #[serde(other)]
    Other,
}

impl EditorFont {
    /// Font family accepted by Canvas text and measurement APIs.
    pub fn family(self) -> Option<&'static str> {
        match self {
            Self::RecMonoCasual => Some("Rec Mono Casual"),
            Self::MapleMonoNlCn => Some("Maple Mono NL CN"),
            Self::Other => None,
        }
    }
}

/// Palette in use, including when it was chosen from the system appearance.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorTheme {
    Light,
    Dark,
    #[serde(other)]
    Other,
}

/// RGBA colors for the editor's usual surfaces, labels and selection.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ThemeColors {
    pub background: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub selection: Rgba,
    pub warning: Rgba,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<Rgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Rgba>,
}

/// The editor language used by the plugin's view. Older SDKs fall back to English.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum Language {
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[default]
    #[serde(rename = "en", other)]
    English,
}

impl Language {
    /// Choose the translation for one message, falling back to the English source.
    pub fn text<'a>(self, english: &'a str, simplified_chinese: Option<&'a str>) -> &'a str {
        match self {
            Self::English => english,
            Self::SimplifiedChinese => simplified_chinese.unwrap_or(english),
        }
    }
}

/// A host-routed action associated with the current complete scene.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Hit {
    pub bounds: Rect,
    pub action: String,
}

/// Named callbacks delivered asynchronously through `PluginEvent::Ui`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Interaction {
    pub pointer: Vec<PointerRegion>,
    pub keys: Vec<KeyBinding>,
    /// Request one callback after the visible scene is drawn, including any requested underline geometry.
    pub next_frame: Option<String>,
}

/// The kind of callback the scene registered. Motion, wheel and frame callbacks are passive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    MouseDown,
    MouseUp,
    MouseMove,
    Wheel,
    KeyDown,
    NextFrame,
}

impl EventKind {
    /// Whether delivery is triggered by an explicit key or button action.
    pub fn is_passive(self) -> bool {
        matches!(self, Self::MouseMove | Self::Wheel | Self::NextFrame)
    }
}

/// Pointer callbacks and native cursor style for a rectangular part of the scene.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PointerRegion {
    pub bounds: Rect,
    pub cursor: Option<CursorStyle>,
    pub mouse_down: Option<String>,
    pub mouse_up: Option<String>,
    pub mouse_move: Option<String>,
    pub wheel: Option<String>,
}

impl PointerRegion {
    /// Get the action registered for a particular pointer event.
    pub fn action(&self, kind: EventKind) -> Option<&str> {
        match kind {
            EventKind::MouseDown => self.mouse_down.as_deref(),
            EventKind::MouseUp => self.mouse_up.as_deref(),
            EventKind::MouseMove => self.mouse_move.as_deref(),
            EventKind::Wheel => self.wheel.as_deref(),
            _ => None,
        }
    }
}

/// Platform cursor shapes supported by the pinned GPUI host.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Arrow,
    IBeam,
    Crosshair,
    ClosedHand,
    OpenHand,
    PointingHand,
    ResizeLeft,
    ResizeRight,
    ResizeLeftRight,
    ResizeUp,
    ResizeDown,
    ResizeUpDown,
    ResizeUpLeftDownRight,
    ResizeUpRightDownLeft,
    ResizeColumn,
    ResizeRow,
    VerticalText,
    OperationNotAllowed,
    DragLink,
    DragCopy,
    ContextualMenu,
}

/// A key callback active only when the user gives the Canvas keyboard focus.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub key: String,
    #[serde(default)]
    pub modifiers: KeyModifiers,
    pub action: String,
}

/// Match all held modifiers. `platform` denotes Command, Windows or Super.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyModifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
    pub function: bool,
}

impl Interaction {
    fn is_empty(&self) -> bool {
        self.pointer.is_empty() && self.keys.is_empty() && self.next_frame.is_none()
    }

    fn validate(&self) -> bool {
        let mut keys = BTreeSet::new();
        self.pointer.len() <= 128
            && self.keys.len() <= 64
            && self.next_frame.as_deref().is_none_or(callback_name)
            && self.pointer.iter().all(|region| {
                rect(region.bounds)
                    && [
                        &region.mouse_down,
                        &region.mouse_up,
                        &region.mouse_move,
                        &region.wheel,
                    ]
                    .into_iter()
                    .all(|action| action.as_deref().is_none_or(callback_name))
            })
            && self.keys.iter().all(|binding| {
                !binding.key.is_empty()
                    && binding.key.len() <= 64
                    && !binding.key.chars().any(char::is_control)
                    && callback_name(&binding.action)
                    && keys.insert((binding.key.to_ascii_lowercase(), binding.modifiers))
            })
    }

    /// Resolve the topmost region that registered this event at `position`.
    pub fn pointer_action(&self, kind: EventKind, [x, y]: Point) -> Option<&str> {
        self.pointer.iter().rev().find_map(|region| {
            let [left, top, width, height] = region.bounds;
            ((left..left + width).contains(&x) && (top..top + height).contains(&y))
                .then(|| region.action(kind))
                .flatten()
        })
    }

    /// Resolve a key exactly, including its held modifiers.
    pub fn key_action(&self, key: &str, modifiers: KeyModifiers) -> Option<&str> {
        self.keys
            .iter()
            .find(|binding| binding.key.eq_ignore_ascii_case(key) && binding.modifiers == modifiers)
            .map(|binding| binding.action.as_str())
    }

    /// Ensure a callback's action and event kind are both registered by this scene.
    pub fn declares(&self, kind: EventKind, action: &str) -> bool {
        match kind {
            EventKind::NextFrame => self.next_frame.as_deref() == Some(action),
            EventKind::KeyDown => self.keys.iter().any(|key| key.action == action),
            _ => self
                .pointer
                .iter()
                .any(|region| region.action(kind) == Some(action)),
        }
    }
}

/// Native background fills. Gradient stops pair a color with a fraction from zero to one.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "fill", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fill {
    Solid {
        color: Rgba,
    },
    Linear {
        angle: f32,
        from: (Rgba, f32),
        to: (Rgba, f32),
        #[serde(default)]
        oklab: bool,
    },
    Slash {
        color: Rgba,
        width: f32,
        interval: f32,
    },
    Checkerboard {
        color: Rgba,
        size: f32,
    },
}

/// Native path geometry. Angles in path arcs are in degrees.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "segment", rename_all = "snake_case", deny_unknown_fields)]
pub enum Segment {
    Move {
        to: Point,
    },
    Line {
        to: Point,
    },
    Quadratic {
        to: Point,
        control: Point,
    },
    Cubic {
        to: Point,
        first: Point,
        second: Point,
    },
    Arc {
        to: Point,
        radii: Point,
        rotation: f32,
        large: bool,
        sweep: bool,
    },
    Polygon {
        points: Vec<Point>,
        closed: bool,
    },
    Close,
}

/// The rule used to fill overlapping contours in a path.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillRule {
    #[default]
    EvenOdd,
    NonZero,
}

/// How the stroke ends at the start or end of an open contour.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineCap {
    #[default]
    Butt,
    Square,
    Round,
}

/// How the stroke connects two adjacent segments at a corner.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineJoin {
    #[default]
    Miter,
    MiterClip,
    Round,
    Bevel,
}

/// Native tessellation options for one filled or stroked path.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PathStyle {
    Fill {
        #[serde(default)]
        rule: FillRule,
        /// Maximum distance from the curve to its approximation, in logical pixels.
        tolerance: Option<f32>,
    },
    Stroke {
        /// Stroke width in logical pixels.
        width: f32,
        #[serde(default)]
        start_cap: LineCap,
        #[serde(default)]
        end_cap: LineCap,
        #[serde(default)]
        join: LineJoin,
        /// Ratio of the maximum miter length to the stroke width.
        miter_limit: Option<f32>,
        /// Maximum distance from the curve to its approximation, in logical pixels.
        tolerance: Option<f32>,
    },
}

impl PathStyle {
    fn valid(self) -> bool {
        let tolerance_in_range =
            |tolerance: Option<f32>| tolerance.is_none_or(|value| (0.01..=64.).contains(&value));
        match self {
            Self::Fill { tolerance, .. } => tolerance_in_range(tolerance),
            Self::Stroke {
                width,
                miter_limit,
                tolerance,
                ..
            } => {
                positive(width)
                    && miter_limit.is_none_or(|limit| (1. ..=128.).contains(&limit))
                    && tolerance_in_range(tolerance)
            }
        }
    }
}

/// Parameters shared by an inset shadow and a drop shadow.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Shadow {
    pub color: Rgba,
    pub offset: Point,
    pub blur: f32,
    pub spread: f32,
    #[serde(default)]
    pub inset: bool,
}

/// One text range with its own colors and decoration. Length is in UTF-8 bytes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TextRun {
    pub length: usize,
    pub color: Rgba,
    pub background: Option<Rgba>,
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_options: Option<FontOptions>,
    pub underline: Option<Decoration>,
    pub strike: Option<Decoration>,
}

/// Optional native font refinements, shared by painting and measuring text.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontOptions {
    /// Weight on the native CSS-compatible scale, with regular at 400 and bold at 700.
    pub weight: Option<f32>,
    pub slant: Option<FontSlant>,
    /// Replaces the family’s default OpenType features when present.
    pub features: Option<BTreeMap<String, u32>>,
    /// Additional system font families in fallback order.
    pub fallbacks: Option<Vec<String>>,
}

impl FontOptions {
    fn valid(&self) -> bool {
        self.weight
            .is_none_or(|weight| (1. ..=1000.).contains(&weight))
            && self.features.as_ref().is_none_or(|features| {
                features.len() <= 32
                    && features.keys().all(|tag| {
                        tag.len() == 4 && tag.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    })
            })
            && self.fallbacks.as_ref().is_none_or(|fonts| {
                fonts.len() <= 16
                    && fonts
                        .iter()
                        .all(|font| !font.is_empty() && font.len() <= 120)
            })
    }
}

/// The shape of the selected native font face.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontSlant {
    Normal,
    Italic,
    Oblique,
}

/// Optional alignment and line count for a Canvas text block.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextLayout {
    pub align: TextAlignment,
    /// Alignment width in logical pixels. Defaults to the wrapping width when supplied.
    pub width: Option<f32>,
    /// Maximum number of visible wrapped lines, within the scene's text budget.
    pub max_lines: Option<u8>,
}

/// Horizontal alignment inside the native text box.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
}

/// A single line for which the host returns advances and UTF-8 caret positions.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MeasureText {
    pub text: String,
    pub font: Option<String>,
    pub size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_options: Option<FontOptions>,
    /// Shape against a font shipped by the current plugin, instead of a system family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_font: Option<PackageFont>,
}

impl MeasureText {
    pub fn valid(&self) -> bool {
        self.text.len() <= 512
            && !self.text.contains('\n')
            && (1. ..=128.).contains(&self.size)
            && self.font.as_ref().is_none_or(|font| font.len() <= 120)
            && self.font_options.as_ref().is_none_or(FontOptions::valid)
            && self.package_font.as_ref().is_none_or(|font| {
                self.font.is_none() && self.font_options.is_none() && font.valid()
            })
    }
}

/// A verified OpenType font file owned by this plugin package.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFont {
    /// Relative asset path in the installed package.
    pub asset: String,
    /// Zero-based face index in a font collection.
    #[serde(default)]
    pub face: u32,
}

impl PackageFont {
    /// Check a package font reference before reading or shaping the asset.
    pub fn valid(&self) -> bool {
        self.asset.len() <= 240 && Self::is_font_asset(&self.asset)
    }

    /// Identify supported file extensions. The host separately validates the actual font data.
    pub fn is_font_asset(asset: &str) -> bool {
        asset.rsplit_once('.').is_some_and(|(_, ext)| {
            ["ttf", "otf", "ttc"]
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
    }
}

/// An optional line style for underlining or striking text.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Decoration {
    pub thickness: f32,
    pub color: Option<Rgba>,
    #[serde(default)]
    pub wavy: bool,
}

/// A rule selected by the exact UTF-8 range delivered by the native underline painter.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnderlineOverride {
    /// Byte offsets into the original line. Adjacent matching native styles can form one range.
    pub range: [usize; 2],
    /// Skip the entire underline for this range.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip: bool,
    /// Replace its appearance. Without a color, keep the resolved native color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<Decoration>,
    /// Horizontal intervals in logical pixels measured from the native underline start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<[f32; 2]>,
}

/// Native underline segments for one tagged line, included in the scene's next-frame callback.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LineUnderlines {
    /// The tag assigned by [`Draw::ShapedLine`].
    pub tag: u32,
    pub segments: Vec<UnderlineGeometry>,
}

/// One segment reported by GPUI before applying overrides, clipping, or pixel snapping.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UnderlineGeometry {
    /// UTF-8 byte offsets into the original line.
    pub range: [usize; 2],
    /// Origin in Canvas-local logical pixels, including any parent group offsets.
    pub origin: Point,
    pub width: f32,
    pub thickness: f32,
    /// Resolved color as sRGB red, green, blue and alpha in the 0–1 range.
    pub color: [f32; 4],
    pub wavy: bool,
}

/// Native drawing primitives in paint order. Assets refer only to files declared by the package.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Draw {
    Quad {
        bounds: Rect,
        fill: Fill,
        #[serde(default)]
        corners: Corners,
        #[serde(default)]
        borders: [f32; 4],
        #[serde(default)]
        border: Rgba,
        #[serde(default)]
        dashed: bool,
    },
    Path {
        segments: Vec<Segment>,
        fill: Fill,
        /// Width for a default stroke. Use either this field or `style`.
        stroke: Option<f32>,
        /// Fill rule or complete stroke style. Omit both options for the default fill.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<PathStyle>,
        #[serde(default)]
        dash: Vec<f32>,
        transform: Option<[f32; 6]>,
    },
    Shadows {
        bounds: Rect,
        #[serde(default)]
        corners: Corners,
        values: Vec<Shadow>,
        #[serde(default)]
        inset: bool,
    },
    Text {
        origin: Point,
        text: String,
        size: f32,
        line_height: f32,
        wrap: Option<f32>,
        runs: Vec<TextRun>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<TextLayout>,
    },
    /// Paint one unwrapped native line with optional rules for its native underline segments.
    /// Use only when [`Viewport::shaped_line`] is true. [`Draw::Text`] is the fallback.
    ShapedLine {
        origin: Point,
        text: String,
        size: f32,
        line_height: f32,
        runs: Vec<TextRun>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<TextLayout>,
        /// The host leaves native segments without matching overrides unchanged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        underlines: Vec<UnderlineOverride>,
        /// Return this line's segments under `argument.underlines` in `Interaction::next_frame`.
        /// Tags are unique within the scene. An absent tag avoids collecting geometry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        underline_tag: Option<u32>,
    },
    Glyphs {
        baseline: Point,
        text: String,
        font: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_options: Option<FontOptions>,
        size: f32,
        color: Rgba,
    },
    /// One shaped line using a font from the package, painted relative to its baseline.
    PackageText {
        baseline: Point,
        text: String,
        font: PackageFont,
        size: f32,
        color: Rgba,
    },
    Underline {
        origin: Point,
        width: f32,
        style: Decoration,
        #[serde(default)]
        exclusions: Vec<[f32; 2]>,
    },
    Strike {
        origin: Point,
        width: f32,
        style: Decoration,
    },
    Svg {
        bounds: Rect,
        asset: String,
        color: Rgba,
        /// Scale along the local x and y axes before rotating around the SVG center.
        scale: Option<Point>,
        /// Clockwise radians around the SVG center.
        rotation: Option<f32>,
        /// Offset in logical pixels after scaling and rotation.
        offset: Option<Point>,
        /// Affine transform in [a, b, c, d, tx, ty] order around the SVG center.
        /// Use this or scale/rotation/offset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<[f32; 6]>,
    },
    Image {
        bounds: Rect,
        image_bounds: Option<Rect>,
        asset: String,
        #[serde(default)]
        frame: usize,
        #[serde(default)]
        corners: Corners,
        #[serde(default)]
        grayscale: bool,
    },
    /// Full-range NV12 plane data painted by the native Apple CoreVideo surface.
    AppleSurface {
        bounds: Rect,
        /// Verified package path to tightly packed Y rows followed by interleaved CbCr rows.
        asset: String,
        /// Even width and height in pixels.
        dimensions: [u16; 2],
    },
    Group {
        clip: Option<Rect>,
        /// Offset relative to the enclosing group.
        offset: Option<Point>,
        /// Offset relative to the Canvas origin, independent of the enclosing group's offset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        absolute_offset: Option<Point>,
        layer: Option<Rect>,
        opacity: Option<f32>,
        draw: Vec<Draw>,
    },
}

impl Canvas {
    /// Reject an invalid or excessive scene before it can enqueue host work.
    pub fn validate(&self) -> bool {
        let valid_hit =
            |hit: &Hit| rect(hit.bounds) && !hit.action.is_empty() && hit.action.len() <= 128;
        let mut tags = BTreeSet::new();
        self.input.is_none_or(rect)
            && self.text_input.as_ref().is_none_or(TextInput::valid)
            && (self.input.is_none() || self.text_input.is_none())
            && self.sequence != Some(0)
            && self.hits.len() <= 128
            && self.hits.iter().all(valid_hit)
            && self.interaction.validate()
            && validate_draw(&self.draw, 0, &mut 0, &mut tags)
            && (tags.is_empty() || self.interaction.next_frame.is_some())
            && self.resources().len() <= 8
    }

    /// Whether the scene declares a grid-aligned or a semantic Canvas text field.
    pub fn has_input(&self) -> bool {
        self.input.is_some() || self.text_input.is_some()
    }

    /// Check whether the requested frame callback should include native underline geometry.
    pub fn reports_underlines(&self) -> bool {
        fn tagged(draw: &[Draw]) -> bool {
            draw.iter().any(|command| match command {
                Draw::ShapedLine { underline_tag, .. } => underline_tag.is_some(),
                Draw::Group { draw, .. } => tagged(draw),
                _ => false,
            })
        }
        tagged(&self.draw)
    }

    /// Unique assets that must be prepared before this complete scene replaces the previous one.
    pub fn resources(&self) -> BTreeSet<&str> {
        fn collect<'a>(draw: &'a [Draw], names: &mut BTreeSet<&'a str>) {
            for command in draw {
                match command {
                    Draw::Image { asset, .. }
                    | Draw::Svg { asset, .. }
                    | Draw::AppleSurface { asset, .. } => {
                        names.insert(asset);
                    }
                    Draw::PackageText { font, .. } => {
                        names.insert(&font.asset);
                    }
                    Draw::Group { draw, .. } => collect(draw, names),
                    _ => {}
                }
            }
        }
        let mut names = BTreeSet::new();
        collect(&self.draw, &mut names);
        names
    }
}

fn callback_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn finite(value: f32) -> bool {
    value.is_finite() && value.abs() <= 16_384.
}
fn positive(value: f32) -> bool {
    finite(value) && value > 0.
}
fn sizes(values: [f32; 4]) -> bool {
    values.into_iter().all(|value| finite(value) && value >= 0.)
}
fn point(values: Point) -> bool {
    values.into_iter().all(finite)
}
fn rect(values: Rect) -> bool {
    point([values[0], values[1]]) && positive(values[2]) && positive(values[3])
}
fn unit(value: f32) -> bool {
    (0. ..=1.).contains(&value)
}
fn decoration(value: &Decoration) -> bool {
    positive(value.thickness) && value.thickness <= 64.
}

fn fill(value: &Fill) -> bool {
    match value {
        Fill::Solid { .. } => true,
        Fill::Linear {
            angle, from, to, ..
        } => finite(*angle) && unit(from.1) && unit(to.1),
        // GPUI packs slash widths in fixed point.
        Fill::Slash {
            width, interval, ..
        } => positive(*width) && positive(*interval) && *width <= 128. && *interval <= 128.,
        Fill::Checkerboard { size, .. } => positive(*size),
    }
}

fn segment(value: &Segment) -> bool {
    match value {
        Segment::Move { to } | Segment::Line { to } => point(*to),
        Segment::Quadratic { to, control } => point(*to) && point(*control),
        Segment::Cubic { to, first, second } => point(*to) && point(*first) && point(*second),
        Segment::Arc {
            to,
            radii,
            rotation,
            ..
        } => point(*to) && radii.iter().copied().all(positive) && finite(*rotation),
        Segment::Polygon { points, .. } => points.len() <= 256 && points.iter().copied().all(point),
        Segment::Close => true,
    }
}

/// Validate runs against the text's UTF-8 byte offsets and budget.
fn text_runs(text: &str, runs: &[TextRun], maximum: usize) -> bool {
    let mut end = 0usize;
    runs.len() <= maximum
        && runs.iter().all(|run| {
            end = end.saturating_add(run.length);
            run.length > 0
                && text.is_char_boundary(end)
                && run.font.as_ref().is_none_or(|name| name.len() <= 120)
                && run.font_options.as_ref().is_none_or(FontOptions::valid)
                && run.underline.as_ref().is_none_or(decoration)
                && run.strike.as_ref().is_none_or(decoration)
        })
        && (runs.is_empty() || end == text.len())
}

/// Validate ordered, distinct overrides that match complete native ranges.
fn underline_overrides(text: &str, overrides: &[UnderlineOverride]) -> bool {
    overrides.len() <= 16
        && overrides.iter().all(|rule| {
            let [start, end] = rule.range;
            start < end
                && text.is_char_boundary(start)
                && text.is_char_boundary(end)
                && (!rule.skip || rule.style.is_none() && rule.exclusions.is_empty())
                && rule.style.as_ref().is_none_or(decoration)
                && rule.exclusions.len() <= 16
                && rule
                    .exclusions
                    .iter()
                    .all(|&range| point(range) && 0. <= range[0] && range[0] < range[1])
        })
        && overrides
            .windows(2)
            .all(|pair| pair[0].range[1] <= pair[1].range[0])
}

fn validate_draw(draw: &[Draw], depth: usize, total: &mut usize, tags: &mut BTreeSet<u32>) -> bool {
    *total += draw.len();
    if depth > 8 || *total > 512 {
        return false;
    }
    draw.iter().all(|command| match command {
        Draw::Quad {
            bounds,
            fill: background,
            corners,
            borders,
            ..
        } => rect(*bounds) && fill(background) && sizes(*corners) && sizes(*borders),
        Draw::Path {
            segments,
            fill: background,
            stroke,
            style,
            dash,
            transform,
        } => {
            segments.len() <= 1024
                && segments.iter().all(segment)
                && fill(background)
                && stroke.is_none_or(positive)
                && (style.is_none() || stroke.is_none())
                && style.is_none_or(PathStyle::valid)
                && dash.len() <= 32
                && dash.iter().all(|&width| positive(width) && width >= 0.5)
                && (dash.is_empty()
                    || stroke.is_some()
                    || matches!(style, Some(PathStyle::Stroke { .. })))
                && transform.is_none_or(|values| values.into_iter().all(finite))
        }
        Draw::Shadows {
            bounds,
            corners,
            values,
            ..
        } => {
            rect(*bounds)
                && sizes(*corners)
                && values.len() <= 8
                && values.iter().all(|shadow| {
                    point(shadow.offset)
                        && (0. ..=256.).contains(&shadow.blur)
                        && finite(shadow.spread)
                })
        }
        Draw::Text {
            origin,
            text,
            size,
            line_height,
            wrap,
            runs,
            layout,
        } => {
            point(*origin)
                && text.len() <= 4096
                && (1. ..=128.).contains(size)
                && positive(*line_height)
                && wrap.is_none_or(|width| width >= 2. && positive(width))
                && layout.is_none_or(|layout| {
                    layout.width.is_none_or(positive)
                        && layout
                            .max_lines
                            .is_none_or(|lines| (1..=64).contains(&lines))
                })
                && text_runs(text, runs, 64)
        }
        Draw::ShapedLine {
            origin,
            text,
            size,
            line_height,
            runs,
            layout,
            underlines,
            underline_tag,
        } => {
            point(*origin)
                && text.len() <= 512
                && !text.contains('\n')
                && (1. ..=128.).contains(size)
                && positive(*line_height)
                && layout.is_none_or(|layout| {
                    layout.width.is_none_or(positive) && layout.max_lines.is_none()
                })
                && text_runs(text, runs, 16)
                && underline_overrides(text, underlines)
                && underline_tag.is_none_or(|tag| tags.len() < 16 && tags.insert(tag))
        }
        Draw::Glyphs {
            baseline,
            text,
            font,
            font_options,
            size,
            ..
        } => {
            point(*baseline)
                && text.len() <= 64
                && !text.contains('\n')
                && (1. ..=128.).contains(size)
                && font.as_ref().is_none_or(|name| name.len() <= 120)
                && font_options.as_ref().is_none_or(FontOptions::valid)
        }
        Draw::PackageText {
            baseline,
            text,
            font,
            size,
            ..
        } => {
            point(*baseline)
                && text.len() <= 512
                && !text.contains('\n')
                && (1. ..=128.).contains(size)
                && font.valid()
        }
        Draw::Underline {
            origin,
            width,
            style,
            exclusions,
        } => {
            point(*origin)
                && positive(*width)
                && decoration(style)
                && exclusions.len() <= 64
                && exclusions
                    .iter()
                    .copied()
                    .all(|range| point(range) && range[0] <= range[1])
        }
        Draw::Strike {
            origin,
            width,
            style,
        } => point(*origin) && positive(*width) && decoration(style),
        Draw::Svg {
            bounds,
            asset,
            scale,
            rotation,
            offset,
            transform,
            ..
        } => {
            rect(*bounds)
                && asset.len() <= 240
                && scale.is_none_or(point)
                && rotation.is_none_or(finite)
                && offset.is_none_or(point)
                && transform.is_none_or(|matrix| {
                    scale.is_none()
                        && rotation.is_none()
                        && offset.is_none()
                        && matrix.into_iter().all(finite)
                })
        }
        Draw::Image {
            bounds,
            image_bounds,
            asset,
            corners,
            ..
        } => {
            rect(*bounds) && asset.len() <= 240 && image_bounds.is_none_or(rect) && sizes(*corners)
        }
        Draw::AppleSurface {
            bounds,
            asset,
            dimensions,
        } => {
            rect(*bounds)
                && !asset.is_empty()
                && asset.len() <= 240
                && dimensions
                    .iter()
                    .all(|dimension| (2..=2048).contains(dimension) && dimension.is_multiple_of(2))
        }
        Draw::Group {
            clip,
            offset,
            absolute_offset,
            layer,
            opacity,
            draw,
        } => {
            clip.is_none_or(rect)
                && offset.is_none_or(point)
                && absolute_offset.is_none_or(point)
                && (offset.is_none() || absolute_offset.is_none())
                && layer.is_none_or(rect)
                && opacity.is_none_or(unit)
                && validate_draw(draw, depth + 1, total, tags)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{Canvas, Viewport};
    use serde_json::{Value, json};

    #[test]
    fn shaped_line_rejects_ambiguous_or_excessive_reports() {
        let command = json!({
            "op": "shaped_line", "origin": [0, 0], "text": "中文", "size": 16,
            "line_height": 22, "runs": [], "underline_tag": 7,
            "underlines": [{"range": [0, 3], "exclusions": [[1, 2]]}],
        });
        let scene = |draw: Vec<Value>| {
            serde_json::from_value::<Canvas>(json!({
                "draw": draw, "interaction": {"next_frame": "painted"},
            }))
            .unwrap()
        };
        let mut single = scene(vec![command.clone()]);
        assert!(single.validate() && single.reports_underlines());
        single.interaction.next_frame = None;
        assert!(!single.validate());

        let mut split_character = command.clone();
        split_character["underlines"][0]["range"][1] = json!(2);
        assert!(!scene(vec![split_character]).validate());

        let nested = json!({"op": "group", "draw": [command.clone()]});
        assert!(!scene(vec![command.clone(), nested]).validate());

        let mut commands = (0..16)
            .map(|tag| {
                let mut command = command.clone();
                command["underline_tag"] = json!(tag);
                command
            })
            .collect::<Vec<_>>();
        assert!(scene(commands.clone()).validate());
        let mut extra = command;
        extra["underline_tag"] = json!(16);
        commands.push(extra);
        assert!(!scene(commands).validate());
    }

    #[test]
    fn shaped_line_capability_defaults_off_for_older_viewports() {
        let old = json!({"id": 1, "width": 320, "height": 160, "scale": 1});
        let mut viewport = serde_json::from_value::<Viewport>(old).unwrap();
        assert!(!viewport.shaped_line);
        assert!(
            serde_json::to_value(viewport)
                .unwrap()
                .get("shaped_line")
                .is_none()
        );
        viewport.shaped_line = true;
        assert_eq!(serde_json::to_value(viewport).unwrap()["shaped_line"], true);
    }
}
