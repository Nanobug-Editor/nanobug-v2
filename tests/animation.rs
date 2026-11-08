//! Deterministic visual-motion checks; timestamps describe real target/frame event order.
use nanobug::animation::{
    cursor::{CursorMotion, Effect, Geometry},
    scroll::{GridScroll, ScrollInput},
};

/// Inputs arriving immediately before frames must not discard the preceding elapsed time.
#[test]
fn repeated_targets_before_each_frame_do_not_starve_elapsed_time() {
    let mut motion = CursorMotion::default();
    motion.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
    for frame in 1..=30 {
        let now = frame as f64 / 60.;
        motion.retarget(
            Geometry::new(frame as f32 * 10., 0., 10., 20.),
            now - 0.001,
            10.,
        );
        motion.advance(now);
    }
    let lag = 310. - motion.corners()[1].x;
    assert!(
        lag < 40.,
        "continuous input left the leading edge {lag:.2}px behind"
    );
}

/// A long jump must visibly deform instead of translating one rigid rectangle.
#[test]
fn long_jump_has_a_faster_front_than_rear() {
    let mut motion = CursorMotion::default();
    motion.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
    motion.retarget(Geometry::new(200., 0., 10., 20.), 0., 10.);
    motion.advance(0.050);
    let corners = motion.corners();
    let front = (corners[1].x - 10.) / 200.;
    let rear = corners[0].x / 200.;
    assert!(
        front - rear > 0.20,
        "cursor remained rigid: front={front:.3}, rear={rear:.3}"
    );
}

/// All effects stretch on short and long cardinal moves and can reverse from their current shape.
#[test]
fn all_effects_pull_in_each_direction() {
    let origin = Geometry::new(100., 100., 10., 20.);
    let start = origin.corners();
    for effect in [
        Effect::Fluid,
        Effect::Elastic,
        Effect::Afterglow,
        Effect::Ripple,
    ] {
        for (dx, dy, front, rear) in [
            (10.0_f32, 0., 1, 0),
            (-10., 0., 0, 1),
            (0., 20., 3, 0),
            (0., -20., 0, 3),
        ] {
            for (steps, time) in [(1., 1. / 60.), (20., 0.050)] {
                let time = if effect == Effect::Fluid {
                    time + 0.034
                } else {
                    time
                };
                let mut motion = CursorMotion::default();
                motion.set_effect(effect);
                motion.retarget(origin, 0., 10.);
                motion.retarget(
                    Geometry::new(100. + dx * steps, 100. + dy * steps, 10., 20.),
                    0.,
                    10.,
                );
                motion.advance(time);
                let corners = motion.corners();
                let progress = |i: usize| {
                    ((corners[i].x - start[i].x) * dx + (corners[i].y - start[i].y) * dy)
                        / dx.hypot(dy)
                };
                let minimum_pull = if steps == 1. {
                    1.5
                } else {
                    dx.hypot(dy) * steps * 0.5
                };
                assert!(
                    progress(front) - progress(rear) > minimum_pull,
                    "{effect:?} lacked a visible pull over {steps} step(s) toward ({dx}, {dy})"
                );

                let head = motion.head();
                motion.retarget(origin, time, 10.);
                assert_eq!(motion.corners(), corners);
                assert_eq!(motion.head(), head);
                motion.advance(1.);
                assert_eq!(motion.corners(), start);
                assert_eq!(motion.head(), origin);
                assert!(!motion.is_active());
            }
        }
    }
}

/// A one-cell move should have a visible attached trail through five samples at 60 Hz.
#[test]
fn a_block_step_stays_stretched_for_five_frames() {
    for effect in [
        Effect::Fluid,
        Effect::Elastic,
        Effect::Afterglow,
        Effect::Ripple,
    ] {
        let mut motion = CursorMotion::default();
        motion.set_effect(effect);
        motion.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
        motion.retarget(Geometry::new(10., 0., 10., 20.), 0., 10.);
        let stretch = [1., 2., 3., 4., 5.].map(|frame| {
            motion.advance(frame / 60.);
            let outline = if effect == Effect::Fluid {
                motion
                    .fluid_outline()
                    .into_iter()
                    .map(|segment| segment.start)
                    .collect()
            } else {
                motion.outline().to_vec()
            };
            let left = outline.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
            let right = outline
                .iter()
                .map(|p| p.x)
                .fold(f32::NEG_INFINITY, f32::max);
            right - left - 10.
        });
        assert!(
            stretch[2] >= 5. && stretch.iter().all(|&extra_width| extra_width >= 3.),
            "{effect:?} had too little stretch over five frames: {stretch:.2?} px"
        );
    }
}

/// A resting fluid caret expands in place and a second input shares its original launch time.
#[test]
fn fluid_inflates_before_travel_without_restarting_on_input() {
    let origin = Geometry::new(100., 100., 10., 20.);
    let first = Geometry::new(110., 100., 10., 20.);
    let latest = Geometry::new(120., 100., 10., 20.);
    let mut motion = CursorMotion::default();
    motion.retarget(origin, 0., 10.);
    motion.retarget(first, 0., 10.);
    assert_eq!(motion.destination(), first);
    motion.advance(0.016);
    let inflating = motion.fluid_outline();
    assert_eq!(motion.head(), origin);
    assert_eq!(motion.corners(), origin.corners());
    assert!(inflating.iter().any(|segment| segment.start.y > 121.));
    assert!(inflating.iter().any(|segment| segment.start.x < 98.));

    motion.retarget(latest, 0.016, 10.);
    assert_eq!(motion.destination(), latest);
    assert_eq!(motion.fluid_outline(), inflating);
    motion.advance(0.030);
    assert_eq!(motion.head(), origin);
    motion.advance(0.044);
    assert!(motion.head().origin.x > 110.);
}

/// The rounded shape returns as soon as the head lands, while the attached trail is closing.
#[test]
fn fluid_recovers_without_waiting_for_the_tail() {
    let target = Geometry::new(240., 210., 10., 20.);
    let mut idle = CursorMotion::default();
    idle.retarget(target, 0., 10.);
    let rounded = idle.fluid_outline();
    assert!(rounded.iter().all(|segment| {
        target
            .corners()
            .iter()
            .all(|&corner| segment.start.distance(corner) > 0.5)
    }));

    let mut motion = CursorMotion::default();
    motion.retarget(Geometry::new(60., 30., 10., 20.), 0., 10.);
    motion.retarget(target, 0., 10.);
    motion.advance(0.05);
    let moving = motion.fluid_outline();
    assert!(
        moving
            .iter()
            .any(|segment| segment.start.x < motion.head().origin.x - 70.)
    );
    assert!(
        moving
            .iter()
            .zip(moving.iter().cycle().skip(1))
            .all(|(a, b)| a.end == b.start)
    );

    let arrived_at = (13..=240)
        .map(|frame| f64::from(frame) / 240.)
        .find(|&time| {
            motion.advance(time);
            motion.head().origin.distance(target.origin) < 0.05
        })
        .expect("the head should reach its destination");
    let expansion =
        |motion: &CursorMotion| motion.fluid_outline()[0].start.x - motion.head().corners()[1].x;
    let first = expansion(&motion);
    assert!(motion.is_active() && first > 0.5);
    motion.advance(arrived_at + 0.035);
    let next = expansion(&motion);
    assert!(
        next > 0.05 && next < first - 0.2,
        "head arrived at {:.1} ms, expansion {first:.2} → {next:.2} px after 35 ms",
        arrived_at * 1000.
    );
    motion.advance(arrived_at + 0.21);
    assert_eq!(motion.fluid_outline(), rounded);
    assert!(!motion.is_active());
}

/// Typing-sized movement must use the faster short-distance profile.
#[test]
fn a_single_typing_step_finishes_quickly() {
    let mut motion = CursorMotion::default();
    motion.retarget(Geometry::new(0., 0., 2., 20.), 0., 10.);
    motion.retarget(Geometry::new(10., 0., 2., 20.), 0., 10.);
    motion.advance(0.040);
    assert!(
        (motion.corners()[0].x - 10.).abs() < 0.5,
        "typing cursor still trails after 40 ms"
    );
}

/// The polygon effects keep a full character block at the front of a diagonal jump.
#[test]
fn diagonal_cursor_has_a_block_at_its_leading_end() {
    let mut motion = CursorMotion::default();
    motion.set_effect(Effect::Afterglow);
    motion.retarget(Geometry::new(50., 50., 10., 20.), 0., 10.);
    motion.retarget(Geometry::new(230., 230., 10., 20.), 0., 10.);
    motion.advance(0.05);
    let leading = motion.corners()[2];
    let head = Geometry::new(leading.x - 10., leading.y - 20., 10., 20.);
    let outline = motion.outline();
    for point in head.corners() {
        assert!(
            outline
                .iter()
                .zip(outline.iter().cycle().skip(1))
                .all(|(a, b)| {
                    (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x) >= -0.01
                }),
            "painted outline lost a corner of the leading block: {point:?} outside {outline:?}"
        );
    }
}

/// The attached trail changes one corner, keeping the other three at the head during travel.
#[test]
fn polygon_effects_stretch_one_corner_toward_the_previous_position() {
    for effect in [Effect::Elastic, Effect::Afterglow, Effect::Ripple] {
        for (x, y, tip) in [
            (220., 260., 0),
            (-220., 260., 1),
            (-220., -260., 2),
            (220., -260., 3),
            (220., 0., 0),
            (0., -260., 3),
        ] {
            let mut motion = CursorMotion::default();
            motion.set_effect(effect);
            let origin = Geometry::new(300., 300., 10., 20.);
            let target = Geometry::new(300. + x, 300. + y, 10., 20.);
            motion.retarget(origin, 0., 10.);
            motion.retarget(target, 0., 10.);
            motion.advance(0.050);
            let head = motion.head().corners();
            let outline = motion.outline();
            for (index, (corner, anchor)) in outline.into_iter().zip(head).enumerate() {
                if index == tip {
                    let pull = (anchor.x - corner.x) * x + (anchor.y - corner.y) * y;
                    assert!(
                        pull > 0.5 * (x * x + y * y),
                        "{effect:?} lost its long trail"
                    );
                } else {
                    assert_eq!(corner, anchor, "{effect:?} stretched a second corner");
                }
            }
            motion.advance(1.);
            assert_eq!(motion.outline(), target.corners());
        }
    }
}

/// Replay a fixed target for `seconds` using frames at `rate` Hz.
fn sample(rate: f64, seconds: f64) -> CursorMotion {
    let mut motion = CursorMotion::default();
    motion.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
    motion.retarget(Geometry::new(500., 240., 10., 20.), 0., 10.);
    let mut frame = 1;
    while frame as f64 / rate < seconds {
        motion.advance(frame as f64 / rate);
        frame += 1;
    }
    motion.advance(seconds);
    motion
}

/// Equal wall-clock progress keeps the same position and recovery outline across refresh rates.
#[test]
fn identical_elapsed_time_matches_at_60_144_and_240_hz() {
    for seconds in [0.085, 0.150] {
        let reference = sample(60., seconds);
        for rate in [144., 240.] {
            let motion = sample(rate, seconds);
            for (a, b) in reference.corners().into_iter().zip(motion.corners()) {
                assert!(
                    a.distance(b) < 0.005,
                    "rate-dependent position at {rate} Hz"
                );
            }
            for (a, b) in reference
                .fluid_outline()
                .into_iter()
                .zip(motion.fluid_outline())
            {
                assert!(
                    a.start.distance(b.start) < 0.02,
                    "rate-dependent fluid recovery at {rate} Hz"
                );
            }
        }
    }
}

/// A new target or cursor shape must not teleport the existing visual corners.
#[test]
fn reversing_and_changing_shape_does_not_jump_current_corners() {
    let mut motion = sample(120., 0.055);
    let before = motion.corners();
    let contour = motion.fluid_outline();
    let target = Geometry::new(-100., 100., 2., 20.);
    motion.retarget(target, 0.055, 10.);
    assert_eq!(before, motion.corners());
    assert_eq!(contour, motion.fluid_outline());
    assert_eq!(motion.destination(), target);
    motion.advance(1.);
    assert_eq!(motion.corners(), target.corners());
    assert!(!motion.is_active());
}

/// Effects must differ, remain bounded during stress, and expire after an idle interval.
#[test]
fn effects_have_distinct_motion_and_bounded_finite_lifetimes() {
    let mut samples = Vec::new();
    for effect in [
        Effect::Fluid,
        Effect::Elastic,
        Effect::Afterglow,
        Effect::Ripple,
    ] {
        let mut motion = CursorMotion::default();
        motion.set_effect(effect);
        motion.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
        motion.retarget(Geometry::new(250., 0., 10., 20.), 0., 10.);
        motion.advance(0.075);
        samples.push(motion.corners()[0].x);
        assert_eq!(motion.ghosts().count() > 0, effect == Effect::Afterglow);
        assert_eq!(motion.pulses().count() > 0, effect == Effect::Ripple);
        for frame in 10..=100 {
            let now = frame as f64 / 120.;
            motion.retarget(
                Geometry::new(
                    if frame % 2 == 0 { 0. } else { 700. },
                    frame as f32,
                    2.,
                    20.,
                ),
                now,
                10.,
            );
            motion.advance(now + 0.001);
            assert!(motion.ghosts().count() <= 24 && motion.pulses().count() <= 6);
            assert!(
                motion
                    .corners()
                    .iter()
                    .all(|p| p.x.is_finite() && p.y.is_finite())
            );
        }
        motion.advance(3.);
        assert!(!motion.is_active(), "{effect:?} never became idle");
    }
    for i in 0..samples.len() {
        for j in i + 1..samples.len() {
            assert!((samples[i] - samples[j]).abs() > 1.);
        }
    }
}

/// Scrolling text and its cursor must translate together without adding another easing step.
#[test]
fn viewport_rebase_preserves_cursor_progress_relative_to_text() {
    let mut motion = sample(120., 0.06);
    let before = motion.corners();
    let head = motion.head();
    motion.translate(nanobug::animation::cursor::Vec2::new(0., -37.5));
    for (old, new) in before.into_iter().zip(motion.corners()) {
        assert_eq!(new.x, old.x);
        assert!((new.y - old.y + 37.5).abs() < 0.0001);
    }
    let shifted = motion.head();
    assert_eq!(shifted.size, head.size);
    assert_eq!(shifted.origin.x, head.origin.x);
    assert!((shifted.origin.y - head.origin.y + 37.5).abs() < 0.0001);
}

/// A moving viewport may have a fractional offset, but its final position must be exactly integral.
#[test]
fn smooth_scroll_moves_continuously_but_stops_at_an_exact_whole_row() {
    let mut scroll = GridScroll::default();
    scroll.request(13, 100, 0.);
    scroll.advance(0.040);
    assert!(scroll.position() > 0. && scroll.position() < 13.);
    assert!(scroll.fraction() > 0.);
    for frame in 6..=120 {
        scroll.advance(frame as f64 / 120.);
    }
    assert_eq!(scroll.position(), 13.);
    assert_eq!(scroll.fraction(), 0.);
    assert!(!scroll.is_active());
}

/// Three-row wheel ticks at 12 Hz should flow together on a 60 Hz display.
#[test]
fn repeated_wheel_ticks_keep_an_even_pace() {
    let mut scroll = GridScroll::default();
    let mut smallest = f32::INFINITY;
    let mut largest = 0.0_f32;
    for frame in 0..=60 {
        let before = scroll.position();
        let input = if frame % 5 == 0 {
            ScrollInput::Wheel(3)
        } else {
            ScrollInput::Advance
        };
        scroll.track_rows(input, None, 40, 500, f64::from(frame) / 60.);
        if frame > 20 {
            let step = scroll.position() - before;
            smallest = smallest.min(step);
            largest = largest.max(step);
        }
    }
    assert!(
        smallest > 0. && largest <= smallest * 2.,
        "sustained wheel input moved {smallest:.3} to {largest:.3} rows per frame"
    );
}

/// New wheel targets and smaller viewports must preserve continuity and stay inside valid rows.
#[test]
fn scroll_retarget_and_viewport_resize_preserve_bounds_and_integer_landing() {
    let mut scroll = GridScroll::default();
    scroll.request(20, 50, 0.);
    scroll.advance(0.04);
    let before = scroll.position();
    scroll.request(40, 50, 0.04);
    assert_eq!(scroll.position(), before);
    scroll.advance(0.06);
    scroll.request(999, 3, 0.06);
    for frame in 8..=120 {
        scroll.advance(frame as f64 / 120.);
        assert!((0. ..=3.).contains(&scroll.position()));
    }
    assert_eq!(scroll.position(), 3.);
    assert_eq!(scroll.fraction(), 0.);
    scroll.request(100, 0, 1.);
    assert_eq!(scroll.position(), 0.);
    assert!(!scroll.is_active());
}

/// At the fourth row, further movement toward an edge with room scrolls the viewport in step.
#[test]
fn keyboard_scroll_keeps_the_caret_on_the_fourth_row_from_either_edge() {
    let height = 20;
    let rows = 100;
    for (top, previous, edge, next, expected) in [
        (10, 14, 13, 12, 9),
        (10, 25, 26, 27, 11),
        (3, 7, 6, 5, 3),
        (77, 92, 93, 94, 77),
    ] {
        let mut scroll = GridScroll::default();
        scroll.snap(top, 0.);
        scroll.track_rows(ScrollInput::Reveal, Some(previous), height, rows, 0.);
        let (caret, _) = scroll.track_rows(ScrollInput::Reveal, Some(edge), height, rows, 0.);
        assert_eq!(caret, Some(edge));
        assert_eq!(
            scroll.target(),
            top,
            "the caret is already on the fourth row"
        );

        let (caret, _) = scroll.track_rows(ScrollInput::Reveal, Some(next), height, rows, 0.);
        assert_eq!(caret, Some(next));
        assert_eq!(scroll.target(), expected);
        assert_eq!(scroll.position(), top as f32);
        scroll.track_rows(ScrollInput::Advance, caret, height, rows, 0.5);
        assert_eq!(scroll.position(), expected as f32);
    }
}

/// The next step past the eighth column starts an integer-target scroll in either direction.
#[test]
fn horizontal_navigation_follows_the_eighth_display_column() {
    for (width, glyph, previous, edge, next, target) in [
        (30, 1, 18, 17, 16, 9),
        (30, 1, 31, 32, 33, 11),
        (6, 2, 13, 12, 11, 9),
        (6, 2, 11, 12, 13, 11),
    ] {
        let mut scroll = GridScroll::default();
        scroll.snap(10, 0.);
        for column in [previous, edge] {
            scroll.track_columns(
                ScrollInput::Reveal,
                Some(column..column + glyph),
                width,
                Some(100),
                0.,
            );
            assert_eq!(scroll.target(), 10);
        }
        let caret = next..next + glyph;
        scroll.track_columns(
            ScrollInput::Reveal,
            Some(caret.clone()),
            width,
            Some(100),
            0.,
        );
        assert_eq!(scroll.target(), target);
        scroll.track_columns(ScrollInput::Advance, Some(caret.clone()), width, None, 0.02);
        assert!(scroll.fraction() > 0.);
        scroll.track_columns(ScrollInput::Advance, Some(caret), width, None, 0.5);
        assert_eq!(scroll.position(), target as f32);
    }
}

/// A wheel pan may leave the caret offscreen until navigation reveals its entire glyph.
#[test]
fn horizontal_wheel_pans_to_the_content_limit_and_reveal_restores_the_caret() {
    let width = 6;
    let caret = 8..10;
    let mut scroll = GridScroll::default();
    scroll.track_columns(
        ScrollInput::Reveal,
        Some(caret.clone()),
        width,
        Some(50),
        0.,
    );
    scroll.track_columns(
        ScrollInput::Wheel(100),
        Some(caret.clone()),
        width,
        Some(50),
        0.,
    );
    scroll.track_columns(ScrollInput::Advance, Some(caret.clone()), width, None, 0.5);
    assert_eq!(scroll.position(), 44.);
    scroll.track_columns(ScrollInput::Advance, Some(caret.clone()), width, None, 1.);
    assert_eq!(scroll.position(), 44.);

    let next = 10..12;
    scroll.track_columns(ScrollInput::Reveal, Some(next.clone()), width, Some(50), 1.);
    assert_eq!(scroll.position(), 10.);
    scroll.track_columns(ScrollInput::Advance, Some(next), width, None, 1.5);
    assert!((6. ..=10.).contains(&scroll.position()));

    scroll.track_columns(
        ScrollInput::Wheel(-100),
        Some(caret.clone()),
        width,
        Some(50),
        1.5,
    );
    scroll.track_columns(ScrollInput::Advance, Some(caret), width, None, 2.);
    assert_eq!(scroll.position(), 0.);
}

/// During wheel animation, only complete rows count toward the three-row inset on either side.
#[test]
fn wheel_caret_stays_inside_the_visible_row_margins_on_every_frame() {
    let height = 12;
    let rows = 200;
    let mut scroll = GridScroll::default();
    let mut caret = Some(7);
    for (begin, delta, destination) in [(0., 100, 100), (1., 200, 188), (2., -300, 0)] {
        (caret, _) = scroll.track_rows(ScrollInput::Wheel(delta), caret, height, rows, begin);
        let remaining = (scroll.position() - destination as f32).abs();
        assert!(
            remaining <= 3.,
            "a multi-screen wheel jump left {remaining:.1} rows to animate"
        );
        let mut fractional = false;
        for frame in 1..=60 {
            let time = begin + f64::from(frame) / 120.;
            (caret, _) = scroll.track_rows(ScrollInput::Advance, caret, height, rows, time);
            let first = scroll.position().ceil() as usize;
            let last = scroll.first_cell() + height - 1;
            let min = first + if first > 0 { 3 } else { 0 };
            let max = last - if last + 1 < rows { 3 } else { 0 };
            assert!((min..=max).contains(&caret.unwrap()));
            fractional |= scroll.fraction() > 0.;
        }
        assert!(fractional);
        assert_eq!(scroll.position(), destination as f32);
        assert_eq!(scroll.fraction(), 0.);
    }
}

/// A navigation jump interrupts a pending wheel destination and reveals the full caret row immediately.
#[test]
fn navigation_jump_during_wheel_scroll_keeps_the_target_row_visible() {
    let height = 20;
    let rows = 500;
    let mut scroll = GridScroll::default();
    scroll.track_rows(ScrollInput::Wheel(250), Some(8), height, rows, 0.);
    let (caret, _) = scroll.track_rows(ScrollInput::Advance, Some(8), height, rows, 0.04);
    assert!(caret.unwrap() > 8);
    let (caret, shift) = scroll.track_rows(ScrollInput::Reveal, Some(420), height, rows, 0.04);
    assert_eq!(caret, Some(420));
    assert_eq!(
        shift, 0.,
        "instant reveal should not translate the old visual cursor"
    );
    for frame in 0..=60 {
        let (caret, _) = scroll.track_rows(
            ScrollInput::Advance,
            caret,
            height,
            rows,
            0.04 + f64::from(frame) / 120.,
        );
        assert_eq!(caret, Some(420));
        assert!(scroll.position() <= 420. && scroll.position() + height as f32 >= 421.);
    }
    assert_eq!(scroll.position(), scroll.target() as f32);
    assert!(!scroll.is_active());
}
