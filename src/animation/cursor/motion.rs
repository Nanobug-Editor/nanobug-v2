//! Frame-rate independent cursor springs and small, bounded visual effects.
use super::{
    lifecycle::{CursorConfig, Kind, Stage, Target, Timeline, Wave},
    shape::{CubicSegment, FLUID_SEGMENTS, fluid_outline},
};
use std::collections::VecDeque;

/// A logical-pixel point independent of GPUI.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    /// Horizontal logical pixels.
    pub x: f32,
    /// Vertical logical pixels.
    pub y: f32,
}
impl Vec2 {
    /// Construct logical-pixel coordinates `x`, `y`.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    /// Euclidean distance to `other`, in logical pixels.
    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
    /// Interpolate toward `other` by normalized `amount`.
    pub fn lerp(self, other: Self, amount: f32) -> Self {
        Self::new(
            self.x + (other.x - self.x) * amount,
            self.y + (other.y - self.y) * amount,
        )
    }
}

/// Logical input rectangle; visual animation never owns the editor's insertion point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    /// Logical top-left input position.
    pub origin: Vec2,
    /// Cursor width and height in logical pixels.
    pub size: Vec2,
}
impl Geometry {
    /// Construct a rectangle at `(x,y)` with `width` and `height` in logical pixels.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: Vec2::new(x, y),
            size: Vec2::new(width, height),
        }
    }
    /// Corners in clockwise order from the top left.
    pub fn corners(self) -> [Vec2; 4] {
        let Vec2 { x, y } = self.origin;
        [
            self.origin,
            Vec2::new(x + self.size.x, y),
            Vec2::new(x + self.size.x, y + self.size.y),
            Vec2::new(x, y + self.size.y),
        ]
    }
    /// Center of the logical cursor rectangle.
    pub fn center(self) -> Vec2 {
        Vec2::new(
            self.origin.x + self.size.x * 0.5,
            self.origin.y + self.size.y * 0.5,
        )
    }
}

/// Analytically integrated second-order spring; retargeting preserves both position and velocity.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    /// Current scalar position, in the caller's coordinate units.
    pub value: f64,
    /// Current velocity, in coordinate units per second.
    pub velocity: f64,
}
impl Spring {
    /// Integrate `seconds` toward `target`; `duration` is the approximate settling time and `damping` the damping ratio.
    pub fn advance(&mut self, target: f64, seconds: f64, duration: f64, damping: f64) {
        if seconds <= 0. {
            return;
        }
        let omega = 6.64 / duration.max(0.012);
        let offset = self.value - target;
        if damping >= 1. {
            let b = self.velocity + omega * offset;
            let decay = (-omega * seconds).exp();
            self.value = target + (offset + b * seconds) * decay;
            self.velocity = (self.velocity - omega * b * seconds) * decay;
        } else {
            let rate = damping * omega;
            let frequency = omega * (1. - damping * damping).sqrt();
            let (sin, cos) = (frequency * seconds).sin_cos();
            let decay = (-rate * seconds).exp();
            self.value =
                target + decay * (offset * cos + (self.velocity + rate * offset) / frequency * sin);
            self.velocity = decay
                * (self.velocity * cos
                    - (rate * self.velocity + omega * omega * offset) / frequency * sin);
        }
    }
    /// Snap exactly to `target` and clear residual velocity.
    pub fn snap(&mut self, target: f64) {
        self.value = target;
        self.velocity = 0.;
    }
}

/// Independently selectable cursor behaviors, without changing editing or input rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Inflates into a droplet before travel and shrinks to a rounded rectangle on arrival.
    #[default]
    Fluid,
    /// Directional stretch with a small elastic rebound.
    Elastic,
    /// Directional stretch with short-lived, fading afterimages.
    Afterglow,
    /// Directional stretch with a ring at the destination of larger jumps.
    Ripple,
}
impl Effect {
    /// Human-readable label used in the editor's status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fluid => "流体",
            Self::Elastic => "弹性",
            Self::Afterglow => "余晖",
            Self::Ripple => "涟漪",
        }
    }
    /// Select the adjacent effect, going backwards when `reverse` is true.
    pub fn next(self, reverse: bool) -> Self {
        let all = [Self::Fluid, Self::Elastic, Self::Afterglow, Self::Ripple];
        let i = all.iter().position(|item| *item == self).unwrap();
        all[(i + if reverse { 3 } else { 1 }) % 4]
    }
}

/// One fading afterimage; its lifetime is measured by the same monotonic animation clock.
#[derive(Clone)]
struct Ghost {
    /// Visual cursor corners at the sampled instant.
    corners: [Vec2; 4],
    /// Sample birth time, in monotonic seconds.
    born: f64,
    /// Seconds before the snapshot fades completely.
    duration: f64,
    /// Initial opacity multiplier.
    strength: f32,
}
/// Independent springs for a pixel position or rectangle size.
#[derive(Clone, Copy, Debug, Default)]
struct PointSpring {
    /// Horizontal motion state.
    x: Spring,
    /// Vertical motion state.
    y: Spring,
}
impl PointSpring {
    /// Integrate toward `target` over `elapsed` seconds with the given `duration` and `damping`.
    /// Return whether position or velocity still needs another frame.
    fn advance(&mut self, target: Vec2, elapsed: f64, duration: f64, damping: f64) -> bool {
        self.x.advance(target.x.into(), elapsed, duration, damping);
        self.y.advance(target.y.into(), elapsed, duration, damping);
        (self.x.value - f64::from(target.x))
            .abs()
            .max((self.y.value - f64::from(target.y)).abs())
            > 0.035
            || self.x.velocity.abs().max(self.y.velocity.abs()) > 0.7
    }

    /// Set both axes to `target` and clear their velocities.
    fn snap(&mut self, target: Vec2) {
        self.x.snap(target.x.into());
        self.y.snap(target.y.into());
    }

    /// Current values in logical pixels.
    fn position(self) -> Vec2 {
        Vec2::new(self.x.value as f32, self.y.value as f32)
    }

    /// Whether the head is within a quarter pixel of `target` and moving slowly enough to settle.
    fn visually_arrived(self, target: Vec2) -> bool {
        self.position().distance(target) <= 0.25 && self.x.velocity.hypot(self.y.velocity) <= 20.
    }
}

/// Brief fluid prelude that holds the visible position while the outline inflates.
#[derive(Clone, Copy)]
struct Inflation {
    /// Timestamp of the first input in this movement.
    start: f64,
    /// Timestamp at which the visible position begins to move.
    launch: f64,
}

/// Continuous animation of a rectangular head and its attached trail.
pub struct CursorMotion {
    /// Four spring samples used to derive the direction and length of the trail.
    states: [PointSpring; 4],
    /// Leading rectangle position, with both edges carried by the same translation.
    head_origin: PointSpring,
    /// Animated width and height for smooth block-to-bar transitions.
    head_size: PointSpring,
    /// Settling time of the leading rectangle in seconds.
    head_duration: f64,
    /// Immediate logical destination, separate from visible corners.
    target: Geometry,
    /// Settling time assigned to each corner for the latest movement.
    durations: [f64; 4],
    /// Shared phase and effect runtime.
    timeline: Timeline,
    /// Deadline of the optional post-move dwell.
    post_until: f64,
    /// Signed viewport speed in pixels per second.
    scroll_velocity: Vec2,
    /// Whether the viewport still has fractional motion.
    scrolling: bool,
    /// Smoothed drag from scrolling, independent of logical position.
    scroll_tail: PointSpring,
    /// Most recent integrated timestamp, never reset without integrating elapsed time.
    last_time: f64,
    /// Whether a first logical rectangle has been supplied.
    initialized: bool,
    /// Whether the latest caret is a navigation block, even when its ink is narrow.
    block: bool,
    /// Whether the spring geometry itself is still moving.
    moving: bool,
    /// Whether the current motion includes translation of the caret.
    translating: bool,
    /// Fluid shape blend from rounded rectangle (0) to droplet (1).
    fluidity: Spring,
    /// Fixed launch interval for a fluid move started from rest.
    inflation: Option<Inflation>,
    /// Direction of the droplet's short tail before and after the position moves.
    fluid_direction: PointSpring,
    /// Latest unit vector pointing back from the destination.
    fluid_aim: Vec2,
    /// Small, bounded history used only by the afterglow mode.
    ghosts: VecDeque<Ghost>,
    /// Next afterimage sampling timestamp.
    next_sample: f64,
}
impl Default for CursorMotion {
    /// Build the default fluid animation, initially without a visible logical destination.
    fn default() -> Self {
        Self {
            states: [PointSpring::default(); 4],
            head_origin: PointSpring::default(),
            head_size: PointSpring::default(),
            head_duration: 0.045,
            target: Geometry::default(),
            durations: [0.15; 4],
            timeline: Timeline::default(),
            post_until: 0.,
            scroll_velocity: Vec2::default(),
            scrolling: false,
            scroll_tail: PointSpring::default(),
            last_time: 0.,
            initialized: false,
            block: false,
            moving: false,
            translating: false,
            fluidity: Spring::default(),
            inflation: None,
            fluid_direction: PointSpring::default(),
            fluid_aim: Vec2::default(),
            ghosts: VecDeque::new(),
            next_sample: 0.,
        }
    }
}
impl CursorMotion {
    /// Chase `target` from monotonic time `now`, using `cell_width` and the target height to size short moves.
    pub fn retarget(&mut self, target: Geometry, now: f64, cell_width: f32) -> bool {
        self.retarget_caret(target, now, cell_width, target.size.x >= cell_width.max(1.))
    }

    /// Chase `target` at monotonic `now`. `cell_width` sets the step size and `block`
    /// selects navigation timing independently of the rectangle's measured ink width.
    pub fn retarget_caret(
        &mut self,
        target: Geometry,
        now: f64,
        cell_width: f32,
        block: bool,
    ) -> bool {
        let was_block = self.block;
        self.block = block;
        if self.initialized
            && self.target.size == target.size
            && self.target.origin.distance(target.origin) < 0.01
        {
            // Absolute cell projection and incremental viewport translation can differ by rounding.
            self.translate(Vec2::new(
                target.origin.x - self.target.origin.x,
                target.origin.y - self.target.origin.y,
            ));
            self.target.origin = target.origin;
            return false;
        }
        self.advance(now);
        if !self.initialized {
            self.target = target;
            self.initialized = true;
            self.snap_to_target();
            self.last_time = now;
            self.timeline
                .fire(Stage::Place, now, target.center(), 0., true);
            self.timeline.enter(Stage::Idle, now, target.center());
            return true;
        }
        let origin = if self.inflation.is_some() {
            self.head_origin.position()
        } else {
            self.target.origin
        };
        let dx = target.origin.x - origin.x;
        let dy = target.origin.y - origin.y;
        let distance = dx.hypot(dy);
        let cell_width = cell_width.max(1.);
        self.timeline.move_cells = distance / cell_width;
        if distance >= 0.01 {
            self.fluid_aim = Vec2::new(-dx / distance, -dy / distance);
            if !self.moving && self.fluidity.value == 0. {
                let duration = self.timeline.duration(Stage::Prepare, was_block && block);
                self.timeline
                    .enter(Stage::Prepare, now, self.head().center());
                self.fluid_direction.snap(self.fluid_aim);
                if duration > 0. {
                    self.inflation = Some(Inflation {
                        start: now,
                        launch: now + duration,
                    });
                }
            }
        } else if self.inflation.take().is_some() {
            // Returning to the held position cancels travel and lets the shape shrink.
            self.translating = false;
        }
        if self.inflation.is_none() {
            self.timeline
                .enter(Stage::Travel, now, self.head().center());
        }
        let columns = dx.abs() / cell_width;
        let rows = dy.abs() / target.size.y.max(1.);
        let short = columns <= 2.01 && rows <= 1.01;
        let (leading, trailing) = if short {
            let (front, compact_rear, block_rear) = match self.effect() {
                Effect::Fluid => (0.022, 0.042, 0.263),
                Effect::Elastic => (0.028, 0.049, 0.270),
                Effect::Afterglow => (0.026, 0.049, 0.241),
                Effect::Ripple => (0.024, 0.046, 0.252),
            };
            // Navigation blocks retain the long tail even on glyphs with narrow ink.
            let block_move = distance >= 0.01 && was_block && block;
            (front, if block_move { block_rear } else { compact_rear })
        } else {
            match self.effect() {
                Effect::Fluid => (0.045, 0.282),
                Effect::Elastic => (0.085, 0.294),
                Effect::Afterglow => (0.045, 0.258),
                Effect::Ripple => (0.068, 0.270),
            }
        };
        let signs = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)];
        for (i, (sx, sy)) in signs.into_iter().enumerate() {
            let alignment = if distance < 0.01 {
                0.5
            } else {
                0.5 + 0.5 * ((sx * dx + sy * dy) / distance).clamp(-1., 1.)
            };
            self.durations[i] = trailing - (trailing - leading) * f64::from(alignment);
        }
        self.head_duration = if distance < 0.01 {
            (leading + trailing) * 0.5
        } else {
            leading
        };
        if distance >= 0.01 {
            self.timeline.fire(
                Stage::Target,
                now,
                target.center(),
                distance / cell_width,
                true,
            );
        }
        self.target = target;
        self.moving = true;
        self.translating |= distance >= 0.01;
        self.next_sample = self.next_sample.max(now);
        true
    }

    /// Finish all visual geometry at the logical target and zero its velocities.
    fn snap_to_target(&mut self) {
        self.head_origin.snap(self.target.origin);
        self.head_size.snap(self.target.size);
        for (state, destination) in self.states.iter_mut().zip(self.target.corners()) {
            state.snap(destination);
        }
    }

    /// Find the instant at which the moving head crossed the arrival threshold.
    fn arrival_time(&self, before: PointSpring, seconds: f64, response: f64, damping: f64) -> f64 {
        let target = self.target.origin;
        if before.visually_arrived(target) {
            0.
        } else {
            // Resolve an arrival inside the frame so recovery starts at the same time across rates.
            let (mut low, mut high) = (0., seconds);
            for _ in 0..12 {
                let time = (low + high) * 0.5;
                let mut head = before;
                head.advance(target, time, response, damping);
                if head.visually_arrived(target) {
                    high = time;
                } else {
                    low = time;
                }
            }
            high
        }
    }

    /// Ease the curved silhouette toward the droplet amount selected by active bindings.
    fn advance_shape(&mut self, goal: f64, seconds: f64) {
        let default = if goal > self.fluidity.value {
            0.036
        } else {
            0.145
        };
        let duration = self
            .timeline
            .main
            .and_then(|stage| self.timeline.response(stage))
            .unwrap_or(default);
        let recovering = self.fluidity.value > 0.;
        self.fluidity.advance(goal, seconds, duration, 1.);
        if (self.fluidity.value - goal).abs() < 0.001 && self.fluidity.velocity.abs() < 0.04 {
            self.fluidity.snap(goal);
            if recovering
                && goal == 0.
                && self.translating
                && self.timeline.main == Some(Stage::Settle)
            {
                // At zero blend the outline has no visible tail. Finish its residual spring now.
                self.snap_to_target();
                self.moving = false;
                self.translating = false;
            }
        }
    }

    /// Integrate to monotonic `now`, returning whether geometry or effects still require frames.
    pub fn advance(&mut self, now: f64) -> bool {
        if now < self.last_time {
            return self.is_active();
        }
        let elapsed = now - self.last_time;
        let previous = self.corners();
        let previous_head = self.head();
        let previous_pull = self.combined_pull();
        let head_before = self.head_origin;
        let translating_before = self.translating;
        let previous_time = self.last_time;
        self.last_time = now;
        self.timeline.advance(now);
        // A frame can straddle launch. Give the position springs only the time after it.
        let travel = if let Some(inflation) = self.inflation {
            let duration = inflation.launch - inflation.start;
            let progress = ((now - inflation.start) / duration).clamp(0., 1.);
            let amount = f64::from(self.timeline.strength(Kind::Droplet).unwrap_or(0.).min(1.));
            self.fluidity.value = amount * progress * progress * (3. - 2. * progress);
            self.fluidity.velocity = amount * 6. * progress * (1. - progress) / duration;
            if now >= inflation.launch {
                self.inflation = None;
                self.timeline
                    .enter(Stage::Travel, inflation.launch, self.head().center());
            }
            (now - previous_time.max(inflation.launch)).max(0.)
        } else {
            elapsed
        };
        let head_damping =
            1. - 0.32 * f64::from(self.timeline.strength(Kind::Rebound).unwrap_or(0.).min(2.));
        let head_response = self
            .timeline
            .main
            .and_then(|stage| self.timeline.response(stage))
            .unwrap_or(self.head_duration);
        let response_scale = head_response / self.head_duration;
        if self.initialized && self.moving && travel > 0. {
            let mut pending = false;
            // The elastic head can rebound while the trailing corners close without overshoot.
            for ((state, destination), duration) in self
                .states
                .iter_mut()
                .zip(self.target.corners())
                .zip(self.durations)
            {
                pending |= state.advance(destination, travel, duration * response_scale, 1.);
            }
            pending |=
                self.head_origin
                    .advance(self.target.origin, travel, head_response, head_damping);
            pending |= self
                .head_size
                .advance(self.target.size, travel, head_response, 1.);
            self.moving = pending;
            if !pending {
                self.snap_to_target();
                self.translating = false;
            }
            let current = self.corners();
            if let Some((duration, strength)) = self.timeline.afterimage()
                && elapsed <= 0.25
                && previous[0].distance(current[0]) > 0.05
            {
                let head = self.head();
                let pull = self.combined_pull();
                self.next_sample = self.next_sample.max(now - duration.min(0.18));
                while self.next_sample <= now {
                    let amount =
                        ((self.next_sample - previous_time) / elapsed).clamp(0., 1.) as f32;
                    // Sample the head and pull before choosing the tip, so a turn cannot stretch two corners.
                    self.ghosts.push_back(Ghost {
                        corners: single_corner_outline(
                            Geometry {
                                origin: previous_head.origin.lerp(head.origin, amount),
                                size: previous_head.size.lerp(head.size, amount),
                            },
                            previous_pull.lerp(pull, amount),
                        ),
                        born: self.next_sample,
                        duration,
                        strength,
                    });
                    self.next_sample += 1. / 120.;
                }
            }
        }
        self.fluid_direction
            .advance(self.fluid_aim, elapsed, 0.050, 1.);
        if self.initialized && self.inflation.is_none() && travel > 0. {
            let mut shape_time = travel;
            if matches!(self.timeline.main, Some(Stage::Travel | Stage::Approach)) {
                if !self.moving
                    || (translating_before && self.head_origin.visually_arrived(self.target.origin))
                {
                    let flowing = if translating_before {
                        self.arrival_time(head_before, travel, head_response, head_damping)
                    } else {
                        travel
                    };
                    self.advance_shape(self.shape_goal(), flowing);
                    shape_time -= flowing;
                    let at = now - shape_time;
                    if translating_before {
                        self.timeline.fire(
                            Stage::Arrive,
                            at,
                            self.target.center(),
                            self.timeline.move_cells,
                            true,
                        );
                    }
                    self.timeline.enter(Stage::Settle, at, self.target.center());
                } else if self.head_origin.position().distance(self.target.origin)
                    <= self.target.size.x.max(self.target.size.y) * 1.5
                {
                    self.timeline
                        .enter(Stage::Approach, now, self.target.center());
                }
            }
            self.advance_shape(self.shape_goal(), shape_time);
            if self.timeline.main == Some(Stage::Settle) && !self.moving && !self.shape_pending() {
                let dwell = self.timeline.duration(Stage::PostMove, self.block);
                self.post_until = now + dwell;
                if dwell == 0. {
                    self.timeline.fire(
                        Stage::PostMove,
                        now,
                        self.target.center(),
                        self.timeline.move_cells,
                        true,
                    );
                    self.timeline.enter(Stage::Idle, now, self.target.center());
                } else {
                    self.timeline
                        .enter(Stage::PostMove, now, self.target.center());
                }
            }
            if self.timeline.main == Some(Stage::PostMove) && now >= self.post_until {
                self.timeline
                    .enter(Stage::Idle, self.post_until, self.target.center());
            }
        }
        self.ghosts
            .retain(|ghost| now - ghost.born < ghost.duration);
        self.ghosts.drain(..self.ghosts.len().saturating_sub(24));
        self.advance_scroll_tail(elapsed, now);
        self.is_active()
    }

    /// Current trailing corners, with no mutation of logical input state.
    pub fn corners(&self) -> [Vec2; 4] {
        self.states.map(PointSpring::position)
    }
    /// Leading rectangle with a positive width and height throughout shape changes.
    pub fn head(&self) -> Geometry {
        let origin = self.head_origin.position();
        let size = self.head_size.position();
        Geometry::new(origin.x, origin.y, size.x.max(1.), size.y.max(1.))
    }
    /// Clockwise quadrilateral with one corner pulled back and three head corners intact.
    pub fn outline(&self) -> [Vec2; 4] {
        single_corner_outline(self.head(), self.combined_pull())
    }

    /// Movement trail points after the active stretch modifiers.
    fn trail_corners(&self) -> [Vec2; 4] {
        let strength = self.timeline.strength(Kind::Stretch).unwrap_or(0.);
        let corners = self.corners();
        let head = self.head().corners();
        std::array::from_fn(|i| head[i].lerp(corners[i], strength))
    }
    /// Bias the trail vector toward the most delayed springs while blending direction continuously.
    fn movement_pull(&self) -> Vec2 {
        let mut drag = Vec2::default();
        let mut total_weight = 0.;
        for (corner, anchor) in self.trail_corners().into_iter().zip(self.head().corners()) {
            let lag = Vec2::new(corner.x - anchor.x, corner.y - anchor.y);
            let weight = (lag.x * lag.x + lag.y * lag.y).powi(2);
            drag.x += lag.x * weight;
            drag.y += lag.y * weight;
            total_weight += weight;
        }
        if total_weight > f32::EPSILON {
            drag.x /= total_weight;
            drag.y /= total_weight;
        }
        drag
    }

    /// Apply viewport drag and movement lag to the same trailing vertex.
    fn combined_pull(&self) -> Vec2 {
        let drag = self.movement_pull();
        let scroll = self.scroll_tail.position();
        Vec2::new(drag.x + scroll.x, drag.y + scroll.y)
    }

    /// Sample the fluid path, combining the short droplet tail and the lag of the moving corners.
    pub fn fluid_outline(&self) -> [CubicSegment; FLUID_SEGMENTS] {
        let head = self.head();
        let mut drag = self.movement_pull();
        let direction = self.fluid_direction.position();
        let seed = head.size.x.min(head.size.y) * 0.65;
        drag.x += direction.x * seed;
        drag.y += direction.y * seed;
        let amount = self.fluidity.value.clamp(0., 1.) as f32;
        let scroll = self.scroll_tail.position();
        let pull = Vec2::new(drag.x * amount + scroll.x, drag.y * amount + scroll.y);
        fluid_outline(head, pull, amount)
    }

    /// Whether the current appearance needs a curved silhouette.
    pub fn curved(&self) -> bool {
        self.effect() == Effect::Fluid || self.fluidity.value > 0.
    }

    /// Current main phase, for inspection and deterministic sampling.
    pub fn stage(&self) -> Option<Stage> {
        self.timeline.main
    }

    /// Current scroll phase, independent of the main movement.
    pub fn scroll_stage(&self) -> Option<Stage> {
        self.timeline.scroll
    }

    /// Opacity and scale of the main silhouette.
    pub fn appearance(&self) -> (f32, f32) {
        self.timeline.appearance()
    }

    /// Timed waves for the cursor and the owned text region.
    pub fn waves(&self) -> impl Iterator<Item = Wave> + '_ {
        self.timeline.waves().take(16)
    }

    /// Drop the old idle effect and begin its delay again on user activity at `now`.
    pub fn activity(&mut self, now: f64) {
        self.advance(now);
        self.timeline.restart_idle(now, self.target.center());
    }

    /// Trigger the visibility transition for `visible` at `now`.
    pub fn set_visible(&mut self, visible: bool, now: f64) {
        self.advance(now);
        let stage = if visible { Stage::Show } else { Stage::Hide };
        self.timeline
            .enter_display(stage, now, self.head().center());
    }

    /// Whether an outgoing visibility effect needs frames while the target is hidden.
    pub fn hiding(&self) -> bool {
        self.timeline.display == Some(Stage::Hide) && self.timeline.hiding()
    }

    /// Rebase by the viewport's pixel `delta` and track its `velocity` in pixels per second.
    /// `active` and `settling` describe travel toward the whole-cell target at monotonic `now`.
    pub fn follow_scroll(
        &mut self,
        delta: Vec2,
        velocity: Vec2,
        active: bool,
        settling: bool,
        now: f64,
    ) {
        self.advance(now);
        self.translate(delta);
        if delta == Vec2::default() && self.timeline.scroll.is_none() {
            return;
        }
        let center = self.head().center();
        if self.timeline.scroll.is_none() {
            self.timeline
                .fire(Stage::ScrollStart, now, center, 0., true);
        }
        self.scrolling = active;
        self.scroll_velocity = if active { velocity } else { Vec2::default() };
        self.timeline.enter_scroll(
            Some(if settling || !active {
                Stage::ScrollSettle
            } else {
                Stage::ScrollFollow
            }),
            now,
            center,
        );
    }

    /// Drop effects owned by the old viewport when focus or buffer changes.
    pub fn change_viewport(&mut self) {
        self.timeline
            .enter_scroll(None, self.last_time, self.head().center());
        self.timeline.clear_spatial();
        self.timeline
            .restart_idle(self.last_time, self.head().center());
        self.ghosts.clear();
        self.scrolling = false;
        self.scroll_velocity = Vec2::default();
        self.scroll_tail.snap(Vec2::default());
    }

    /// Ease the rear offset over `elapsed` and emit scroll completion once aligned and settled.
    fn advance_scroll_tail(&mut self, elapsed: f64, now: f64) {
        let Some(stage) = self.timeline.scroll else {
            return;
        };
        let (strength, max) = self.timeline.scroll_drag();
        let drag = 0.045 * strength;
        let goal = Vec2::new(
            -self.scroll_velocity.x * drag,
            -self.scroll_velocity.y * drag,
        );
        let extent = goal.distance(Vec2::default());
        let goal = if extent > max {
            Vec2::default().lerp(goal, max / extent)
        } else {
            goal
        };
        let moving = self.scroll_tail.advance(
            goal,
            elapsed,
            self.timeline.response(stage).unwrap_or(0.095),
            1.,
        );
        if !moving {
            self.scroll_tail.snap(goal);
            if !self.scrolling {
                let center = self.head().center();
                self.timeline.enter_scroll(None, now, center);
                self.timeline.fire(Stage::ScrollEnd, now, center, 0., true);
            }
        }
    }

    /// The desired amount of the curved contour for the active bindings.
    fn shape_goal(&self) -> f64 {
        f64::from(self.timeline.strength(Kind::Droplet).unwrap_or(0.).min(1.))
    }

    /// Whether the outline's easing needs another frame.
    fn shape_pending(&self) -> bool {
        self.fluidity.value != self.shape_goal() || self.fluidity.velocity != 0.
    }
    /// Immediate destination, useful for validating the input/animation separation.
    pub fn destination(&self) -> Geometry {
        self.target
    }
    /// Whether more frames are needed; an idle cursor requests no timers or redraws.
    pub fn is_active(&self) -> bool {
        self.wake().is_some()
    }

    /// Seconds until a redraw is needed, with zero requesting the next frame.
    pub fn wake(&self) -> Option<f64> {
        if self.moving
            || self.shape_pending()
            || !self.ghosts.is_empty()
            || self.timeline.scroll.is_some()
        {
            Some(0.)
        } else {
            let dwell = (self.timeline.main == Some(Stage::PostMove))
                .then_some((self.post_until - self.last_time).max(0.));
            self.timeline
                .wake()
                .into_iter()
                .chain(dwell)
                .reduce(f64::min)
        }
    }
    /// Current effect selection.
    pub fn effect(&self) -> Effect {
        self.timeline.preset
    }
    /// Switch to `effect` while retaining current geometry and velocity.
    pub fn set_effect(&mut self, effect: Effect) {
        if self.effect() != effect {
            self.fluidity.snap(0.);
            self.timeline.preset = effect;
            self.reload_bindings();
        }
    }

    /// Install validated project `config` while keeping the position springs.
    pub fn set_config(&mut self, config: CursorConfig) {
        self.timeline.config = config;
        self.reload_bindings();
    }

    /// Refresh active bindings after a preset switch or config reload.
    fn reload_bindings(&mut self) {
        let stage = if self.inflation.take().is_some() {
            Stage::Travel
        } else {
            self.timeline.main.unwrap_or(Stage::Idle)
        };
        self.timeline.reset();
        if self.initialized {
            self.timeline
                .enter(stage, self.last_time, self.head().center());
        }
        self.ghosts.clear();
        self.scroll_velocity = Vec2::default();
        self.scrolling = false;
        self.scroll_tail.snap(Vec2::default());
    }
    /// Move the visual coordinate system by `delta`, e.g. when its document viewport scrolls.
    pub fn translate(&mut self, delta: Vec2) {
        self.head_origin.x.value += f64::from(delta.x);
        self.head_origin.y.value += f64::from(delta.y);
        for state in &mut self.states {
            state.x.value += delta.x as f64;
            state.y.value += delta.y as f64;
        }
        self.target.origin.x += delta.x;
        self.target.origin.y += delta.y;
        for ghost in &mut self.ghosts {
            for corner in &mut ghost.corners {
                corner.x += delta.x;
                corner.y += delta.y;
            }
        }
        self.timeline.translate(delta);
    }
    /// Afterimage geometry and remaining normalized opacity.
    pub fn ghosts(&self) -> impl Iterator<Item = ([Vec2; 4], f32)> + '_ {
        self.ghosts.iter().map(|ghost| {
            (
                ghost.corners,
                (1. - (self.last_time - ghost.born) / ghost.duration).clamp(0., 1.) as f32
                    * ghost.strength,
            )
        })
    }
    /// Ring centers and normalized ages for the native painter.
    pub fn pulses(&self) -> impl Iterator<Item = (Vec2, f32)> + '_ {
        self.waves()
            .filter(|wave| wave.target != Target::NearbyText)
            .take(6)
            .map(|wave| (wave.center, wave.progress))
    }
}

/// Extend the corner in the pull quadrant so only its two incident edges deform.
fn single_corner_outline(head: Geometry, mut pull: Vec2) -> [Vec2; 4] {
    // Use the top or left corner for axial motion and ignore subpixel cross-axis noise.
    if pull.x.abs() < 0.1 {
        pull.x = 0.;
    }
    if pull.y.abs() < 0.1 {
        pull.y = 0.;
    }
    let mut corners = head.corners();
    let tip = match (pull.x > 0., pull.y > 0.) {
        (false, false) => &mut corners[0],
        (true, false) => &mut corners[1],
        (true, true) => &mut corners[2],
        (false, true) => &mut corners[3],
    };
    tip.x += pull.x;
    tip.y += pull.y;
    corners
}
