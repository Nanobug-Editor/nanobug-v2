//! Stage bindings and timed effects shared by the motion model and the native painter.
use super::motion::{Effect, Vec2};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

/// Entry points for effects on the movement, scroll and visibility tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// First valid placement.
    Place,
    /// Stable cursor position.
    Idle,
    /// A new logical target.
    Target,
    /// Visual preparation before translation.
    Prepare,
    /// Following the target.
    Travel,
    /// Braking near the target.
    Approach,
    /// First arrival for this target.
    Arrive,
    /// Restoring the normal contour.
    Settle,
    /// Optional dwell before idle.
    PostMove,
    /// First actual scroll displacement.
    ScrollStart,
    /// Following the moving viewport.
    ScrollFollow,
    /// Braking and recovering from scrolling.
    ScrollSettle,
    /// Viewport and scroll deformation have stopped.
    ScrollEnd,
    /// Entering the viewport.
    Show,
    /// Leaving the viewport.
    Hide,
}

impl Stage {
    /// Events and visibility transitions have a finite lifetime of their own.
    fn transient(self) -> bool {
        matches!(
            self,
            Self::Place
                | Self::Target
                | Self::Arrive
                | Self::ScrollStart
                | Self::ScrollEnd
                | Self::Show
                | Self::Hide
        )
    }
}

/// Effect sampled by the motion model or painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Shape the main cursor as a droplet.
    Droplet,
    /// Scale the attached movement trail.
    Stretch,
    /// Add overshoot to the head spring.
    Rebound,
    /// Emit fading snapshots of the moving outline.
    Afterimage,
    /// Expand a ring from the event position.
    Ripple,
    /// Pulse cursor opacity continuously.
    Breath,
    /// Alternate cursor opacity.
    Blink,
    /// Animate cursor size.
    Scale,
    /// Add an opacity pulse.
    Glow,
    /// Fade the main cursor in or out.
    Fade,
    /// Pull the main outline against viewport movement.
    ScrollDrag,
}

/// Which painter receives an expanding wave.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Paint a ring on the cursor layer.
    #[default]
    Cursor,
    /// Paint a wave in the nearby text.
    NearbyText,
    /// Paint both the ring and the text wave.
    Both,
}

/// Parameters for one effect binding.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Binding {
    /// Effect to sample.
    pub kind: Kind,
    /// Delay before the first sample, in milliseconds.
    pub delay_ms: u64,
    /// Duration of a pulse or history sample. Zero holds a stage modifier.
    pub duration_ms: u64,
    /// Interval between pulse starts. Zero plays once.
    pub repeat_ms: u64,
    /// Number of pulses when repeating. Zero repeats until stage exit.
    pub count: u32,
    /// Let a finite pulse finish after its stage exits.
    pub finish: bool,
    /// Effect-specific intensity.
    pub strength: f32,
    /// Ring radius or maximum scroll drag, in logical pixels.
    pub radius_px: f32,
    /// Maximum glyph displacement for a text wave.
    pub text_offset_px: f32,
    /// Minimum distance in cells for an event binding.
    pub min_cells: f32,
    /// Layer that receives a wave.
    pub target: Target,
}

impl Default for Binding {
    /// Defaults for a held stage modifier.
    fn default() -> Self {
        Self {
            kind: Kind::Stretch,
            delay_ms: 0,
            duration_ms: 0,
            repeat_ms: 0,
            count: 0,
            finish: false,
            strength: 1.,
            radius_px: 34.,
            text_offset_px: 1.,
            min_cells: 0.,
            target: Target::Cursor,
        }
    }
}

impl Binding {
    /// Pulse duration for `stage`, or zero for a held modifier.
    fn duration(&self, stage: Stage) -> u64 {
        if self.duration_ms > 0 {
            self.duration_ms
        } else if stage.transient()
            || matches!(
                self.kind,
                Kind::Ripple | Kind::Breath | Kind::Blink | Kind::Glow | Kind::Fade | Kind::Scale
            )
        {
            300
        } else {
            0
        }
    }
}

/// An optional override of one preset stage.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StageSettings {
    /// Preparation or post-move dwell length.
    duration_ms: Option<u64>,
    /// Short preparation for an insert caret.
    insert_ms: Option<u64>,
    /// Optional spring response time for movement or scroll deformation.
    response_ms: Option<u64>,
    /// Replaces the inherited effect list when supplied.
    effects: Option<Vec<Binding>>,
}

/// Project-level stage bindings shared by all presets, with per-preset overrides.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorConfig {
    /// Overrides applied to every preset.
    stages: BTreeMap<Stage, StageSettings>,
    /// Overrides applied only to the selected preset.
    presets: BTreeMap<Effect, BTreeMap<Stage, StageSettings>>,
}

impl CursorConfig {
    /// Parse and validate project `source` before installing the configuration.
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(source)?;
        for (&stage, settings) in config
            .stages
            .iter()
            .chain(config.presets.values().flatten())
        {
            anyhow::ensure!(
                settings.duration_ms.is_none_or(|n| n <= 2000)
                    && settings.insert_ms.is_none_or(|n| n <= 2000)
                    && settings
                        .response_ms
                        .is_none_or(|n| (12..=2000).contains(&n)),
                "stage {stage:?} timing must be at most 2000 ms"
            );
            for binding in settings.effects.iter().flatten() {
                anyhow::ensure!(
                    binding.delay_ms <= 60_000
                        && binding.duration_ms <= 60_000
                        && binding.repeat_ms <= 60_000
                        && binding.count <= 100
                        && binding.strength.is_finite()
                        && (0. ..=3.).contains(&binding.strength)
                        && (1. ..=512.).contains(&binding.radius_px)
                        && (0. ..=12.).contains(&binding.text_offset_px)
                        && (0. ..=1000.).contains(&binding.min_cells),
                    "invalid effect parameters for {stage:?}"
                );
                anyhow::ensure!(
                    binding.repeat_ms == 0
                        || (binding.duration(stage) > 0
                            && binding.duration(stage) <= binding.repeat_ms),
                    "repeat_ms must be at least duration_ms for {stage:?}"
                );
                anyhow::ensure!(
                    !stage.transient() || binding.repeat_ms == 0 || binding.count > 0,
                    "{stage:?} repeats require a finite count"
                );
            }
            anyhow::ensure!(
                settings
                    .effects
                    .as_ref()
                    .is_none_or(|items| items.len() <= 16),
                "at most 16 effects per stage"
            );
        }
        Ok(config)
    }

    /// Load the project override in `root`, using built-in settings when absent.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join("cursor-effects.toml");
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                Self::parse(&source).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Resolve `stage` for `preset` from built-ins, shared overrides, then preset overrides.
    fn settings(&self, preset: Effect, stage: Stage) -> StageSettings {
        let mut settings = StageSettings::default();
        let mut effects = Vec::new();
        let mut push = |kind| {
            effects.push(Binding {
                kind,
                ..Binding::default()
            })
        };
        match stage {
            Stage::Prepare if preset == Effect::Fluid => {
                settings.duration_ms = Some(34);
                settings.insert_ms = Some(8);
                push(Kind::Droplet);
            }
            Stage::Travel | Stage::Approach | Stage::Settle => {
                push(Kind::Stretch);
                if preset == Effect::Fluid && stage != Stage::Settle {
                    push(Kind::Droplet);
                } else if preset == Effect::Elastic {
                    push(Kind::Rebound);
                } else if preset == Effect::Afterglow && stage != Stage::Settle {
                    push(Kind::Afterimage);
                }
            }
            Stage::Target if preset == Effect::Ripple => {
                effects.push(Binding {
                    kind: Kind::Ripple,
                    duration_ms: 320,
                    min_cells: 1.5,
                    finish: true,
                    ..Binding::default()
                });
            }
            Stage::ScrollFollow | Stage::ScrollSettle => push(Kind::ScrollDrag),
            _ => {}
        }
        settings.effects = Some(effects);
        for custom in [
            self.stages.get(&stage),
            self.presets
                .get(&preset)
                .and_then(|stages| stages.get(&stage)),
        ]
        .into_iter()
        .flatten()
        {
            settings.duration_ms = custom.duration_ms.or(settings.duration_ms);
            settings.insert_ms = custom.insert_ms.or(settings.insert_ms);
            if let Some(effects) = &custom.effects {
                settings.effects = Some(effects.clone());
            }
        }
        settings
    }
}

/// A sampled expanding wave in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wave {
    /// Origin in logical pixels.
    pub center: Vec2,
    /// Progress through the pulse.
    pub progress: f32,
    /// Maximum radius.
    pub radius: f32,
    /// Tint intensity.
    pub strength: f32,
    /// Peak text displacement.
    pub offset: f32,
    /// Destinations for the wave.
    pub target: Target,
}

impl Wave {
    /// Radius of the active wavefront in logical pixels.
    pub fn front(self) -> f32 {
        5. + self.progress * (self.radius - 5.).max(0.)
    }

    /// Sample the tint and displacement at a glyph cluster's `position`.
    pub fn sample(self, position: Vec2) -> (f32, Vec2) {
        let dx = position.x - self.center.x;
        let dy = position.y - self.center.y;
        let distance = dx.hypot(dy);
        let weight = (1. - (distance - self.front()).abs() / 7.).clamp(0., 1.)
            * (1. - self.progress)
            * self.strength;
        let shift = self.offset * weight / distance.max(1.);
        (weight.min(1.), Vec2::new(dx * shift, dy * shift))
    }
}

/// One configured effect, timed from its stage entry.
struct Instance {
    /// Stage owning this effect.
    stage: Stage,
    /// Preset parameters at entry.
    binding: Binding,
    /// Time the first pulse is due.
    start: f64,
    /// Source position for spatial effects.
    center: Vec2,
    /// Whether this effect has left its owning phase.
    detached: bool,
}

impl Instance {
    /// Return the sample progress and next redraw delay at `now`.
    fn sample(&self, now: f64) -> (Option<f32>, Option<f64>) {
        let elapsed = (now - self.start) * 1000.;
        if elapsed < 0. {
            return (None, Some(-elapsed / 1000.));
        }
        let binding = &self.binding;
        let duration = binding.duration(self.stage) as f64;
        if duration == 0. {
            return (Some(0.), None);
        }
        let repeat = binding.repeat_ms as f64;
        if repeat > 0. {
            let cycle = (elapsed / repeat).floor();
            if binding.count > 0 && cycle >= binding.count as f64 {
                return (None, None);
            }
            let local = elapsed - cycle * repeat;
            if local >= duration {
                let next = binding.count == 0 || cycle + 1. < binding.count as f64;
                return (None, next.then_some((repeat - local) / 1000.));
            }
            self.pulse(local, duration)
        } else if elapsed < duration {
            self.pulse(elapsed, duration)
        } else {
            (None, None)
        }
    }

    /// Sample a pulse at `elapsed` milliseconds and wake at its next visible change.
    fn pulse(&self, elapsed: f64, duration: f64) -> (Option<f32>, Option<f64>) {
        let until = match self.binding.kind {
            Kind::Blink if elapsed < duration * 0.5 => duration * 0.5 - elapsed,
            Kind::Blink
            | Kind::Droplet
            | Kind::Stretch
            | Kind::Rebound
            | Kind::Afterimage
            | Kind::ScrollDrag => duration - elapsed,
            _ => 0.,
        };
        (Some((elapsed / duration) as f32), Some(until / 1000.))
    }
}

/// Active phase and overlay instances. Motion and scroll advance independently.
pub(crate) struct Timeline {
    /// Main movement stage.
    pub main: Option<Stage>,
    /// Concurrent viewport stage.
    pub scroll: Option<Stage>,
    /// Current visibility stage.
    pub display: Option<Stage>,
    /// Active profile.
    pub preset: Effect,
    /// Project overrides.
    pub config: CursorConfig,
    /// Current sample time.
    pub now: f64,
    /// Requested displacement, in cells, for movement phase filters.
    pub move_cells: f32,
    /// Bounded running effects.
    instances: Vec<Instance>,
}

impl Default for Timeline {
    /// Empty timeline awaiting its first position.
    fn default() -> Self {
        Self {
            main: None,
            scroll: None,
            display: None,
            preset: Effect::Fluid,
            config: CursorConfig::default(),
            now: 0.,
            move_cells: 0.,
            instances: Vec::new(),
        }
    }
}

impl Timeline {
    /// Optional response time in seconds for the active preset and `stage`.
    pub fn response(&self, stage: Stage) -> Option<f64> {
        self.config
            .presets
            .get(&self.preset)
            .and_then(|stages| stages.get(&stage))
            .and_then(|settings| settings.response_ms)
            .or_else(|| {
                self.config
                    .stages
                    .get(&stage)
                    .and_then(|settings| settings.response_ms)
            })
            .map(|ms| ms as f64 / 1000.)
    }

    /// Get the configured hold time of `stage` for a block or insert caret.
    pub fn duration(&self, stage: Stage, block: bool) -> f64 {
        let settings = self.config.settings(self.preset, stage);
        let ms = if block {
            settings.duration_ms
        } else {
            settings.insert_ms.or(settings.duration_ms)
        };
        ms.unwrap_or(0) as f64 / 1000.
    }

    /// Start a stage or a one-shot event at `time`, using its `center` and move length in `cells`.
    pub fn fire(&mut self, stage: Stage, time: f64, center: Vec2, cells: f32, event: bool) {
        for mut binding in self
            .config
            .settings(self.preset, stage)
            .effects
            .into_iter()
            .flatten()
        {
            if cells < binding.min_cells || binding.strength == 0. {
                continue;
            }
            if event {
                // A zero-length stage can emit a pulse but has no held modifiers.
                binding.duration_ms = binding.duration(stage).max(1);
                if binding.count == 0 {
                    binding.repeat_ms = 0;
                }
            }
            if self.instances.len() == 96 {
                if let Some(old) = self.instances.iter().position(|item| item.detached) {
                    self.instances.remove(old);
                } else {
                    break;
                }
            }
            self.instances.push(Instance {
                stage,
                start: time + binding.delay_ms as f64 / 1000.,
                binding,
                center,
                detached: event,
            });
        }
    }

    /// Leave `stage` at `time`, preserving only finite effects allowed to finish.
    fn leave(&mut self, stage: Stage, time: f64) {
        self.instances.retain_mut(|item| {
            if item.stage != stage || item.detached {
                return true;
            }
            if item.binding.finish
                && item.binding.repeat_ms == 0
                && item.binding.duration(stage) > 0
                && time >= item.start
                && item.sample(time).0.is_some()
            {
                item.detached = true;
                true
            } else {
                false
            }
        });
    }

    /// Enter the main `stage` at `time` and place its effects at `center`.
    pub fn enter(&mut self, stage: Stage, time: f64, center: Vec2) {
        if self.main != Some(stage) {
            if let Some(previous) = self.main.replace(stage) {
                self.leave(previous, time);
            }
            let cells = if stage == Stage::Idle {
                0.
            } else {
                self.move_cells
            };
            self.fire(stage, time, center, cells, false);
        }
    }

    /// Restart idle bindings after interaction, even when the caret has no new target.
    pub fn restart_idle(&mut self, time: f64, center: Vec2) {
        if self.main == Some(Stage::Idle) {
            self.instances.retain(|item| item.stage != Stage::Idle);
            self.fire(Stage::Idle, time, center, 0., false);
        }
    }

    /// Set the viewport `stage` at `time` without interrupting the main phase.
    pub fn enter_scroll(&mut self, stage: Option<Stage>, time: f64, center: Vec2) {
        if self.scroll != stage {
            if let Some(previous) = self.scroll {
                self.leave(previous, time);
            }
            self.scroll = stage;
            if let Some(stage) = stage {
                self.fire(stage, time, center, 0., false);
            }
        }
    }

    /// Update the visible `stage` at `time` and start its transition.
    pub fn enter_display(&mut self, stage: Stage, time: f64, center: Vec2) {
        if self.display != Some(stage) {
            self.display = Some(stage);
            self.instances
                .retain(|item| !matches!(item.stage, Stage::Show | Stage::Hide));
            self.fire(stage, time, center, 0., false);
        }
    }

    /// Advance to `now` and discard exhausted effects.
    pub fn advance(&mut self, now: f64) {
        self.now = now;
        self.instances.retain(|item| {
            let (sample, wake) = item.sample(now);
            sample.is_some() || wake.is_some()
        });
    }

    /// Seconds until a timed effect needs a redraw, with zero requesting the next frame.
    pub fn wake(&self) -> Option<f64> {
        self.instances
            .iter()
            .filter_map(|item| item.sample(self.now).1)
            .reduce(f64::min)
    }

    /// Combined active strength of `kind`, if configured.
    pub fn strength(&self, kind: Kind) -> Option<f32> {
        self.instances
            .iter()
            .filter(|item| item.binding.kind == kind && item.sample(self.now).0.is_some())
            .map(|item| item.binding.strength)
            .reduce(|a, b| (a + b).min(3.))
    }

    /// History lifetime and strength for the active afterimage emitter.
    pub fn afterimage(&self) -> Option<(f64, f32)> {
        self.instances
            .iter()
            .filter(|item| {
                item.binding.kind == Kind::Afterimage && item.sample(self.now).0.is_some()
            })
            .map(|item| {
                (
                    if item.binding.duration_ms == 0 {
                        0.18
                    } else {
                        item.binding.duration_ms as f64 / 1000.
                    },
                    item.binding.strength,
                )
            })
            .reduce(|(a, x), (b, y)| (a.max(b), (x + y).min(3.)))
    }

    /// Strength and length limit for active viewport drag.
    pub fn scroll_drag(&self) -> (f32, f32) {
        self.instances
            .iter()
            .filter(|item| {
                item.binding.kind == Kind::ScrollDrag && item.sample(self.now).0.is_some()
            })
            .fold((0., 0.), |(strength, radius), item| {
                (
                    (strength + item.binding.strength).min(3.),
                    radius.max(item.binding.radius_px),
                )
            })
    }

    /// Combined opacity and size for the main silhouette.
    pub fn appearance(&self) -> (f32, f32) {
        let mut opacity = 1.;
        let mut scale = 1.;
        for item in &self.instances {
            let Some(t) = item.sample(self.now).0 else {
                continue;
            };
            let strength = item.binding.strength;
            let pulse = (std::f32::consts::PI * t).sin();
            match item.binding.kind {
                Kind::Breath => opacity *= 1. - 0.55 * strength.min(1.) * pulse * pulse,
                Kind::Blink => {
                    if t >= 0.5 {
                        opacity *= 1. - strength.min(1.);
                    }
                }
                Kind::Glow => opacity *= 1. + strength * pulse,
                Kind::Scale => scale *= 1. + 0.25 * strength * pulse,
                Kind::Fade => {
                    opacity *=
                        1. - strength.min(1.) * if item.stage == Stage::Hide { t } else { 1. - t }
                }
                _ => {}
            }
        }
        (opacity, scale.clamp(0.25, 3.))
    }

    /// Sample visible waves in their entry order.
    pub fn waves(&self) -> impl Iterator<Item = Wave> + '_ {
        self.instances
            .iter()
            .rev()
            .filter(|item| item.binding.kind == Kind::Ripple)
            .filter_map(|item| {
                let progress = item.sample(self.now).0?;
                Some(Wave {
                    center: item.center,
                    progress,
                    radius: item.binding.radius_px,
                    strength: item.binding.strength,
                    offset: item.binding.text_offset_px,
                    target: item.binding.target,
                })
            })
    }

    /// Whether a hide transition still has a visible sample or pending wake.
    pub fn hiding(&self) -> bool {
        self.instances.iter().any(|item| {
            item.stage == Stage::Hide && {
                let (sample, wake) = item.sample(self.now);
                sample.is_some() || wake.is_some()
            }
        })
    }

    /// Release spatial overlays when their viewport changes.
    pub fn clear_spatial(&mut self) {
        self.instances
            .retain(|item| item.binding.kind != Kind::Ripple);
    }

    /// Rebase the anchors of active effects by viewport `delta`.
    pub fn translate(&mut self, delta: Vec2) {
        for item in &mut self.instances {
            item.center.x += delta.x;
            item.center.y += delta.y;
        }
    }

    /// Clear old effects while keeping the chosen preset and project settings.
    pub fn reset(&mut self) {
        self.instances.clear();
        self.main = None;
        self.scroll = None;
        self.display = None;
    }
}
