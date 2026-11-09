//! GPU cursor layer: a unified cursor silhouette, optional overlays and demand-driven frames.
use super::{CursorCell, Panel};
use crate::ui::theme::CURSOR_FILL;
use gpui::{
    Bounds, ContentMask, Context, Entity, IntoElement, PathBuilder, Pixels, Render, Styled, Task,
    Window, canvas, point, px, rgba, size,
};
use nanobug::animation::cursor::{
    CubicSegment, CursorConfig, CursorMotion, Effect, Geometry, Target, Vec2,
};
use std::time::{Duration, Instant};

/// One global visual cursor; input and IME continue to use the shell's logical caret.
pub struct Cursor {
    /// Deterministic spring/effect state shared with regression checks.
    motion: CursorMotion,
    /// Origin of monotonic animation timestamps.
    epoch: Instant,
    /// Whether a next-frame callback is already queued.
    scheduled: bool,
    /// Whether the logical caret is inside a visible region.
    visible: bool,
    /// Pending delayed wake and its absolute deadline.
    timer: Option<(f64, Task<()>)>,
    /// Body that owns nearby text effects.
    body: Option<Entity<Panel>>,
    /// Viewport clip for scroll and visibility transitions.
    clip: Option<Bounds<Pixels>>,
    /// Inner bounds for the primary cursor during wheel-following motion.
    head_clip: Option<Bounds<Pixels>>,
    /// Document character to recolor inside the animated block head.
    text_cell: Option<CursorCell>,
}
impl Cursor {
    /// Construct a hidden cursor with the initial `effect` and the project's `config`.
    pub fn new(effect: Effect, config: CursorConfig) -> Self {
        let mut motion = CursorMotion::default();
        motion.set_effect(effect);
        motion.set_config(config);
        Self {
            motion,
            epoch: Instant::now(),
            scheduled: false,
            visible: false,
            timer: None,
            body: None,
            clip: None,
            head_clip: None,
            text_cell: None,
        }
    }
    /// Select the adjacent behavior; `reverse` cycles backwards without changing document state.
    pub fn cycle(&mut self, reverse: bool, window: &Window, cx: &mut Context<Self>) -> Effect {
        self.motion.advance(self.epoch.elapsed().as_secs_f64());
        let effect = self.motion.effect().next(reverse);
        self.motion.set_effect(effect);
        self.motion
            .set_visible(self.visible, self.epoch.elapsed().as_secs_f64());
        self.refresh(window, cx);
        effect
    }
    /// Set the visible rectangle `target`, its `visible` state and `block` style, using
    /// `cell_width` for motion pacing. Schedule drawing in `window` through `cx`.
    pub fn set(
        &mut self,
        target: Geometry,
        visible: bool,
        cell_width: f32,
        block: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.motion.retarget_caret(
            target,
            self.epoch.elapsed().as_secs_f64(),
            cell_width,
            block,
        );
        let visibility_changed = self.visible != visible;
        self.visible = visible;
        if visibility_changed {
            self.motion
                .set_visible(visible, self.epoch.elapsed().as_secs_f64());
        }
        if changed || visibility_changed {
            self.refresh(window, cx);
        } else {
            self.schedule(window, cx);
        }
    }

    /// Associate effects with `body` and its `clip`, and constrain the head to `head_clip` while wheeling.
    /// `text_cell` identifies a document glyph that takes the theme's cursor text color.
    /// `changed` clears stale viewport effects through `cx` when the owning region changes.
    pub fn set_viewport(
        &mut self,
        body: Option<Entity<Panel>>,
        clip: Option<Bounds<Pixels>>,
        head_clip: Option<Bounds<Pixels>>,
        text_cell: Option<CursorCell>,
        changed: bool,
        cx: &mut Context<Self>,
    ) {
        if changed {
            if let Some(old) = &self.body {
                old.update(cx, |panel, cx| panel.set_waves(Vec::new(), cx));
            }
            self.motion.change_viewport();
            self.body = body;
        }
        if self.clip != clip
            || self.head_clip != head_clip
            || self.text_cell != text_cell
            || changed
        {
            self.clip = clip;
            self.head_clip = head_clip;
            self.text_cell = text_cell;
            cx.notify();
        }
    }

    /// Follow the viewport's pixel `delta` and `velocity` through `cx` while it scrolls or settles.
    pub fn follow_scroll(
        &mut self,
        delta: Vec2,
        velocity: Vec2,
        active: bool,
        settling: bool,
        cx: &mut Context<Self>,
    ) {
        if delta == Vec2::default() && self.motion.scroll_stage().is_none() {
            return;
        }
        self.motion.follow_scroll(
            delta,
            velocity,
            active,
            settling,
            self.epoch.elapsed().as_secs_f64(),
        );
        self.publish_waves(cx);
        cx.notify();
    }

    /// Reset the idle clock for an input handled in `window`.
    pub fn activity(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.motion.activity(self.epoch.elapsed().as_secs_f64());
        self.refresh(window, cx);
    }

    /// Apply validated `config` to the live cursor.
    pub fn configure(&mut self, config: CursorConfig, window: &Window, cx: &mut Context<Self>) {
        self.motion.advance(self.epoch.elapsed().as_secs_f64());
        self.motion.set_config(config);
        self.motion
            .set_visible(self.visible, self.epoch.elapsed().as_secs_f64());
        self.refresh(window, cx);
    }

    /// Send the current waves to the owned body without reshaping its cached rows.
    fn publish_waves(&self, cx: &mut Context<Self>) {
        if let Some(body) = &self.body {
            let waves = self
                .motion
                .waves()
                .filter(|wave| self.visible && wave.target != Target::Cursor)
                .collect();
            body.update(cx, |panel, cx| panel.set_waves(waves, cx));
        }
    }

    /// Update the painters and request the next necessary wake.
    fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.publish_waves(cx);
        cx.notify();
        self.schedule(window, cx);
    }

    /// Queue frames for active effects and a timer for an idle delay or a pulse gap.
    fn schedule(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(delay) = (self.visible || self.motion.hiding())
            .then(|| self.motion.wake())
            .flatten()
        else {
            self.timer = None;
            return;
        };
        if self.scheduled {
            return;
        }
        if delay > 0.001 {
            let deadline = self.epoch.elapsed().as_secs_f64() + delay;
            if self
                .timer
                .as_ref()
                .is_some_and(|(old, _)| (old - deadline).abs() < 0.004)
            {
                return;
            }
            let timer = cx
                .background_executor()
                .timer(Duration::from_secs_f64(delay));
            self.timer = Some((
                deadline,
                cx.spawn_in(window, async move |weak, cx| {
                    timer.await;
                    let _ = cx.update(|window, cx| {
                        let _ = weak.update(cx, |cursor, cx| {
                            cursor.timer = None;
                            cursor.motion.advance(cursor.epoch.elapsed().as_secs_f64());
                            cursor.refresh(window, cx);
                        });
                    });
                }),
            ));
            return;
        }
        self.timer = None;
        self.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |cursor, cx| {
                cursor.scheduled = false;
                cursor.motion.advance(cursor.epoch.elapsed().as_secs_f64());
                cursor.refresh(window, cx);
            });
        });
    }
}

/// Paint an ordered polygon `outline` with RGB `color` and byte-range `alpha` in `window`.
fn paint_polygon(outline: &[Vec2], color: u32, alpha: u32, window: &mut Window) {
    if outline.len() < 3 {
        return;
    }
    let mut path = PathBuilder::fill();
    path.move_to(point(px(outline[0].x), px(outline[0].y)));
    for vertex in &outline[1..] {
        path.line_to(point(px(vertex.x), px(vertex.y)));
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, rgba((color << 8) | alpha.min(255)));
    }
}

/// Fill the closed fluid `outline` with RGB `color` and byte `alpha` in `window`.
fn paint_fluid(outline: &[CubicSegment], color: u32, alpha: u32, window: &mut Window) {
    let to_point = |p: Vec2| point(px(p.x), px(p.y));
    let mut path = PathBuilder::fill();
    path.move_to(to_point(outline[0].start));
    for segment in outline {
        path.cubic_bezier_to(
            to_point(segment.end),
            to_point(segment.control_a),
            to_point(segment.control_b),
        );
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, rgba((color << 8) | alpha.min(255)));
    }
}

impl Render for Cursor {
    /// Paint effects and the cursor silhouette, then recolor the glyph inside its head.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (opacity, scale) = self.motion.appearance();
        let alpha = (184. * opacity).clamp(0., 255.) as u32;
        let head = self.motion.head();
        let center = head.center();
        let clip = self.clip.filter(|_| {
            self.motion.scroll_stage().is_some() || self.head_clip.is_some() || !self.visible
        });
        // A fast viewport rebase can briefly carry the visual spring past the logical caret's bounds.
        let shift = self
            .head_clip
            .or(clip)
            .filter(|_| self.visible)
            .map_or_else(Vec2::default, |bounds| {
                let fit = |position: f32, extent: f32, origin: Pixels, span: Pixels| {
                    let half = (extent * scale).min(f32::from(span)) * 0.5;
                    let start = f32::from(origin) + half;
                    let end = f32::from(origin + span) - half;
                    position.clamp(start, end.max(start)) - position
                };
                Vec2::new(
                    fit(center.x, head.size.x, bounds.origin.x, bounds.size.width),
                    fit(center.y, head.size.y, bounds.origin.y, bounds.size.height),
                )
            });
        let scale_point = |p: Vec2| {
            let p = center.lerp(p, scale);
            Vec2::new(p.x + shift.x, p.y + shift.y)
        };
        let fluid = self.motion.curved().then(|| {
            self.motion.fluid_outline().map(|curve| CubicSegment {
                start: scale_point(curve.start),
                control_a: scale_point(curve.control_a),
                control_b: scale_point(curve.control_b),
                end: scale_point(curve.end),
            })
        });
        let polygon = fluid
            .is_none()
            .then(|| self.motion.outline().map(scale_point));
        let ghosts: Vec<_> = self.motion.ghosts().collect();
        let pulses: Vec<_> = self
            .motion
            .waves()
            .filter(|wave| wave.target != Target::NearbyText)
            .collect();
        let visible = self.visible || self.motion.hiding();
        let text = self
            .text_cell
            .filter(|_| self.visible && alpha > 0)
            .zip(self.body.as_ref())
            .and_then(|(cell, body)| {
                let origin = scale_point(head.origin);
                let bounds = Bounds::new(
                    point(px(origin.x), px(origin.y)),
                    size(px(head.size.x * scale), px(head.size.y * scale)),
                )
                .intersect(&self.clip?);
                let text = body.read(cx).cursor_text(cell, window)?;
                Some((text, bounds))
            });
        let text_alpha = (255. * opacity).clamp(0., 255.) as u32;
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                if !visible {
                    return;
                }
                window.with_content_mask(clip.map(|bounds| ContentMask { bounds }), |window| {
                    for (ghost, opacity) in &ghosts {
                        paint_polygon(ghost, CURSOR_FILL, (65. * opacity * opacity) as u32, window);
                    }
                    for wave in &pulses {
                        let radius = wave.front();
                        let mut ring = PathBuilder::stroke(px(1.2));
                        for index in 0..=40 {
                            let angle = index as f32 / 40. * std::f32::consts::TAU;
                            let p = point(
                                px(wave.center.x + angle.cos() * radius),
                                px(wave.center.y + angle.sin() * radius),
                            );
                            if index == 0 {
                                ring.move_to(p);
                            } else {
                                ring.line_to(p);
                            }
                        }
                        ring.close();
                        if let Ok(path) = ring.build() {
                            window.paint_path(
                                path,
                                rgba(
                                    (CURSOR_FILL << 8)
                                        | ((1. - wave.progress).powi(2) * 170. * wave.strength)
                                            .min(255.)
                                            as u32,
                                ),
                            );
                        }
                    }
                    if let Some(outline) = &fluid {
                        paint_fluid(outline, CURSOR_FILL, alpha, window);
                    } else if let Some(outline) = &polygon {
                        paint_polygon(outline, CURSOR_FILL, alpha, window);
                    }
                    if let Some((text, bounds)) = &text {
                        window.with_content_mask(Some(ContentMask { bounds: *bounds }), |window| {
                            text.paint(text_alpha, window);
                        });
                    }
                });
            },
        )
        .size_full()
    }
}
