//! Smooth viewport movement that settles on whole character rows or columns.
use crate::cursor_motion::Spring;
use std::ops::Range;

/// Complete rows kept between the caret and a scrollable edge.
const ROW_CARET_MARGIN: usize = 3;
/// Complete display columns kept between the caret and a scrollable edge.
const COLUMN_CARET_MARGIN: usize = 7;
/// Approximate time in seconds for an isolated step to cover 99% of its distance.
const SCROLL_RESPONSE: f64 = 0.24;
/// Cells animated next to the destination after a jump beyond one viewport.
const FAR_SCROLL_TAIL: f64 = 2.;

/// How the next viewport update treats its logical caret.
#[derive(Clone, Copy)]
pub enum ScrollInput {
    /// Continue the current scroll without choosing a new destination.
    Advance,
    /// Reveal a caret moved by editing or navigation.
    Reveal,
    /// Place the viewport at an explicit first cell, retaining the caret.
    Position(usize),
    /// Scroll by a signed number of cells. Row scrolling can carry the caret along.
    Wheel(isize),
}

/// Smooth movement along one viewport axis, with a whole-cell logical target.
#[derive(Default)]
pub struct GridScroll {
    /// Continuous offset and velocity in cells.
    spring: Spring,
    /// Integer destination.
    target: usize,
    /// Largest permitted first cell after accounting for viewport size.
    maximum: usize,
    /// Last integrated monotonic timestamp, in seconds.
    last_time: f64,
    /// Last tracked caret row or column, retained while a prompt owns the cursor.
    caret_cell: Option<usize>,
    /// Whether the wheel controls this axis instead of caret navigation.
    wheeling: bool,
}

impl GridScroll {
    /// Update a viewport of `height` rows over `rows` content rows at monotonic `now`.
    /// `input` chooses the scroll intent and `caret` names the row to keep visible, if present.
    /// Return the new caret row and the animated offset in rows, excluding any jump needed to reveal it.
    pub fn track_rows(
        &mut self,
        input: ScrollInput,
        caret: Option<usize>,
        height: usize,
        rows: usize,
        now: f64,
    ) -> (Option<usize>, f32) {
        let height = height.max(1);
        let maximum = rows.saturating_sub(height);
        let caret = caret.filter(|_| rows > 0).map(|row| row.min(rows - 1));
        let before = self.spring.value;
        let previous_target = self.target;
        let target = match input {
            ScrollInput::Advance => self.target,
            ScrollInput::Position(top) => {
                self.wheeling = false;
                top
            }
            ScrollInput::Reveal => {
                let top = if std::mem::take(&mut self.wheeling) && caret.is_some() {
                    self.first_cell()
                } else {
                    self.target
                }
                .min(maximum);
                caret.map_or(top, |row| self.reveal_row(top, row, height, maximum))
            }
            ScrollInput::Wheel(delta) => {
                let target = self.target.saturating_add_signed(delta).min(maximum);
                self.wheeling |= target != self.target;
                target
            }
        };
        let limits = caret
            .filter(|_| !self.wheeling)
            .map(|row| (row.saturating_sub(height - 1), row.min(maximum)));
        let target = limits.map_or(target, |(first, last)| target.clamp(first, last));
        self.request(target, maximum, now);

        if height == 1 && self.wheeling {
            self.spring.snap(self.target as f64);
        }
        let delta = (self.spring.value - before) as f32;
        self.keep_caret_visible(limits);
        self.limit_far_scroll(previous_target, height);
        let caret = if self.wheeling {
            caret.map(|row| {
                let (first, last) = self.complete_rows(height, rows);
                let (above, below) = self.wheel_row_insets(height, rows);
                row.clamp(first + above, last - below)
            })
        } else {
            caret
        };
        self.caret_cell = caret.or(self.caret_cell);
        self.wheeling &= self.is_active();
        (caret, delta)
    }

    /// Apply horizontal `input` to a `width`-column viewport at monotonic `now`.
    /// `caret` is the display-cell span to reveal. `columns` bounds new scroll requests
    /// when supplied. Return the animated displacement, excluding jumps to reveal the caret.
    pub fn track_columns(
        &mut self,
        input: ScrollInput,
        caret: Option<Range<usize>>,
        width: usize,
        columns: Option<usize>,
        now: f64,
    ) -> f32 {
        let width = width.max(1);
        let maximum = columns.map_or(self.maximum, |columns| columns.saturating_sub(width));
        let before = self.spring.value;
        let previous_target = self.target;
        let target = match input {
            ScrollInput::Advance => self.target,
            ScrollInput::Position(first) => {
                self.wheeling = false;
                first.min(maximum)
            }
            ScrollInput::Reveal => {
                if let Some(caret) = &caret {
                    let first = if std::mem::take(&mut self.wheeling) {
                        self.first_cell()
                    } else {
                        self.target
                    };
                    self.reveal_column(first, caret, width, maximum)
                } else {
                    self.target
                }
            }
            ScrollInput::Wheel(delta) => {
                let target = self.target.saturating_add_signed(delta).min(maximum);
                self.wheeling |= target != self.target;
                target
            }
        };
        let limits = caret.as_ref().filter(|_| !self.wheeling).map(|caret| {
            let end = caret.end.min(caret.start + width);
            (end.saturating_sub(width), caret.start)
        });
        let target = limits.map_or(target, |(first, last)| target.clamp(first, last));
        self.request(target, maximum.max(target), now);
        let delta = (self.spring.value - before) as f32;
        self.keep_caret_visible(limits);
        self.limit_far_scroll(previous_target, width);
        self.caret_cell = caret.map(|caret| caret.start).or(self.caret_cell);
        delta
    }

    /// Start from `first` and keep `caret` on the eighth visible column from a scrollable edge.
    /// Reduce the margin for a narrow viewport and cap rightward scrolling at `maximum`.
    fn reveal_column(
        &self,
        first: usize,
        caret: &Range<usize>,
        width: usize,
        maximum: usize,
    ) -> usize {
        let cells = (caret.end - caret.start).min(width);
        let end = caret.start + cells;
        let inset = COLUMN_CARET_MARGIN.min((width - cells) / 2);
        if self
            .caret_cell
            .is_some_and(|previous| caret.start < previous)
            && caret.start < first + inset
        {
            caret.start.saturating_sub(inset)
        } else if self
            .caret_cell
            .is_some_and(|previous| caret.start > previous)
            && first < maximum
            && end > first + width - inset
        {
            end.saturating_sub(width - inset).min(maximum)
        } else {
            first
        }
    }

    /// Snap the spring if its position would put the caret outside the viewport's `limits`.
    fn keep_caret_visible(&mut self, limits: Option<(usize, usize)>) {
        if let Some((first, last)) = limits {
            let bounded = self.spring.value.clamp(first as f64, last as f64);
            if bounded != self.spring.value {
                self.spring.snap(bounded);
            }
        }
    }

    /// Rebase a new jump larger than `viewport` from `previous_target` to a short visible tail.
    /// The caller measures the animated displacement first so cursor effects exclude the jump.
    fn limit_far_scroll(&mut self, previous_target: usize, viewport: usize) {
        let target = self.target as f64;
        let remaining = self.spring.value - target;
        if self.target.abs_diff(previous_target) > viewport && remaining.abs() > viewport as f64 {
            self.spring
                .snap(target + remaining.signum() * FAR_SCROLL_TAIL.min(viewport as f64));
        }
    }

    /// Start from `top` and scroll when `row` moves past the fourth row from a scrollable edge.
    /// `height` is the viewport's row count and `maximum` is the final legal top row.
    fn reveal_row(&self, top: usize, row: usize, height: usize, maximum: usize) -> usize {
        let inset = ROW_CARET_MARGIN.min((height - 1) / 2);
        let bottom = top + height - 1;
        if self.caret_cell.is_some_and(|previous| row < previous)
            && top > ROW_CARET_MARGIN
            && row < top + inset
        {
            row.saturating_sub(inset)
        } else if self.caret_cell.is_some_and(|previous| row > previous)
            && maximum - top > ROW_CARET_MARGIN
            && row > bottom - inset
        {
            row.saturating_sub(height - 1 - inset).min(maximum)
        } else {
            top
        }
    }

    /// First and last whole content rows in a viewport of `height` over `rows` total rows.
    fn complete_rows(&self, height: usize, rows: usize) -> (usize, usize) {
        let first = self.spring.value.ceil() as usize;
        let last = (self.first_cell() + height).min(rows).saturating_sub(1);
        (first, last)
    }

    /// Top and bottom caret padding while the wheel scrolls a viewport of `height` over `rows` rows.
    pub fn wheel_row_insets(&self, height: usize, rows: usize) -> (usize, usize) {
        if !self.wheeling {
            return (0, 0);
        }
        let (first, last) = self.complete_rows(height, rows);
        let margin = ROW_CARET_MARGIN.min(last.saturating_sub(first) / 2);
        (
            if first > 0 { margin } else { 0 },
            if last + 1 < rows { margin } else { 0 },
        )
    }

    /// Integrate the old destination, then request integer `target` within `maximum` at `now`.
    pub fn request(&mut self, target: usize, maximum: usize, now: f64) {
        self.advance(now);
        self.maximum = maximum;
        self.target = target.min(maximum);
        self.spring.value = self.spring.value.clamp(0., maximum as f64);
        if maximum == 0 {
            self.spring.snap(0.);
        }
    }
    /// Advance to monotonic `now`, snapping exactly to a whole cell once the spring settles.
    pub fn advance(&mut self, now: f64) -> bool {
        let elapsed = (now - self.last_time).max(0.);
        self.last_time = self.last_time.max(now);
        self.spring
            .advance(self.target as f64, elapsed, SCROLL_RESPONSE, 1.);
        let bounded = self.spring.value.clamp(0., self.maximum as f64);
        if bounded != self.spring.value {
            self.spring.snap(bounded);
        }
        if (self.spring.value - self.target as f64).abs() < 0.01 && self.spring.velocity.abs() < 0.5
        {
            self.spring.snap(self.target as f64);
        }
        self.is_active()
    }
    /// Visual position in fractional cells, used only for drawing and pointer hit testing.
    pub fn position(&self) -> f32 {
        self.spring.value as f32
    }
    /// Current signed viewport speed in cells per second.
    pub fn velocity(&self) -> f32 {
        self.spring.velocity as f32
    }
    /// Whether the viewport is decelerating into its final half cell.
    pub fn settling(&self) -> bool {
        (self.spring.value - self.target as f64).abs() < 0.5 && self.spring.velocity.abs() < 12.
    }
    /// First cell required by the retained viewport body.
    pub fn first_cell(&self) -> usize {
        self.spring.value.floor().max(0.) as usize
    }
    /// Fractional cell displacement, exactly zero when scrolling has stopped.
    pub fn fraction(&self) -> f32 {
        (self.spring.value - self.spring.value.floor()) as f32
    }
    /// Clamped logical destination.
    pub fn target(&self) -> usize {
        self.target
    }
    /// Whether another viewport animation frame is necessary.
    pub fn is_active(&self) -> bool {
        self.spring.value != self.target as f64 || self.spring.velocity != 0.
    }
    /// Restore an inactive buffer directly to its integer `cell` at `now`.
    pub fn snap(&mut self, cell: usize, now: f64) {
        self.target = cell;
        self.maximum = self.maximum.max(cell);
        self.spring.snap(cell as f64);
        self.last_time = now;
        self.wheeling = false;
    }
}
