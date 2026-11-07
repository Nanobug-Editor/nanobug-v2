//! Replays the project configuration through the production phase and effect model.
use nanobug::{
    cursor_lifecycle::{CursorConfig, Stage, Target},
    cursor_motion::{CursorMotion, Effect, Geometry, Vec2},
    scroll_motion::GridScroll,
};

/// Idle layers share a stage, wake after a delay, and reset together on input.
#[test]
fn idle_effects_combine_and_restart() {
    let config = CursorConfig::parse(
        r#"
        [stages.idle]
        effects = [
            { kind = "breath", delay_ms = 100, duration_ms = 200, repeat_ms = 500 },
            { kind = "scale", delay_ms = 100, duration_ms = 200, repeat_ms = 500 },
            { kind = "ripple", target = "nearby_text", delay_ms = 100, duration_ms = 200, repeat_ms = 500 },
        ]
        "#,
    )
    .unwrap();
    let mut cursor = CursorMotion::default();
    cursor.set_config(config);
    cursor.retarget(Geometry::new(20., 20., 10., 20.), 0., 10.);
    assert_eq!(cursor.stage(), Some(Stage::Idle));
    assert_eq!(cursor.wake(), Some(0.1));
    cursor.advance(0.2);
    let (alpha, scale) = cursor.appearance();
    assert!(alpha < 0.7 && scale > 1.2);
    let wave = cursor.waves().next().unwrap();
    assert_eq!(wave.target, Target::NearbyText);
    let (light, shift) = wave.sample(Vec2::new(wave.center.x + wave.front(), wave.center.y));
    assert!(light > 0. && shift.x > 0.);
    assert_eq!(cursor.pulses().count(), 0);
    cursor.advance(0.35);
    assert_eq!(cursor.appearance(), (1., 1.));
    assert!(
        cursor
            .wake()
            .is_some_and(|delay| (delay - 0.25).abs() < 0.001)
    );
    cursor.activity(0.41);
    assert!(
        cursor
            .wake()
            .is_some_and(|delay| (delay - 0.1).abs() < 0.001)
    );
    assert_eq!(cursor.waves().count(), 0);
}

/// A static blink waits for its next edge instead of repainting every frame.
#[test]
fn blink_uses_timer_edges() {
    let config = CursorConfig::parse(
        "[stages.idle]\neffects = [{kind = 'blink', duration_ms = 600, repeat_ms = 600}]",
    )
    .unwrap();
    let mut cursor = CursorMotion::default();
    cursor.set_config(config);
    cursor.retarget(Geometry::new(0., 0., 10., 20.), 0., 10.);
    assert_eq!(cursor.wake(), Some(0.3));
    cursor.advance(0.3);
    assert_eq!(cursor.appearance().0, 0.);
    assert_eq!(cursor.wake(), Some(0.3));
    cursor.advance(0.6);
    assert_eq!(cursor.appearance().0, 1.);
}

/// Each preset honors shared preparation, arrival and post-move bindings.
#[test]
fn every_preset_uses_the_same_movement_phases() {
    let config = CursorConfig::parse(
        r#"
        [stages.prepare]
        duration_ms = 30
        effects = [{kind = "scale"}, {kind = "glow"}]
        [stages.arrive]
        effects = [{kind = "ripple", target = "both", duration_ms = 1200, min_cells = 2}]
        [stages.post_move]
        duration_ms = 60
        effects = [{kind = "glow"}]
        [presets.elastic.prepare]
        duration_ms = 45
        "#,
    )
    .unwrap();
    for effect in [
        Effect::Fluid,
        Effect::Elastic,
        Effect::Afterglow,
        Effect::Ripple,
    ] {
        let mut cursor = CursorMotion::default();
        cursor.set_config(config.clone());
        cursor.set_effect(effect);
        let source = Geometry::new(20., 20., 10., 20.);
        let destination = Geometry::new(120., 40., 10., 20.);
        cursor.retarget(source, 0., 10.);
        cursor.retarget(destination, 0., 10.);
        cursor.advance(0.015);
        assert_eq!(cursor.stage(), Some(Stage::Prepare));
        assert_eq!(cursor.head(), source);
        let (alpha, scale) = cursor.appearance();
        assert!(alpha > 1. && scale > 1.);
        cursor.advance(0.04);
        if effect == Effect::Elastic {
            assert_eq!(cursor.stage(), Some(Stage::Prepare));
        } else {
            assert_eq!(cursor.stage(), Some(Stage::Travel));
        }
        let mut stages = Vec::new();
        for tick in 9..=200 {
            cursor.advance(f64::from(tick) / 200.);
            stages.push(cursor.stage().unwrap());
        }
        for stage in [
            Stage::Travel,
            Stage::Approach,
            Stage::Settle,
            Stage::PostMove,
            Stage::Idle,
        ] {
            assert!(stages.contains(&stage), "{effect:?} missed {stage:?}");
        }
        assert_eq!(cursor.stage(), Some(Stage::Idle));
        assert_eq!(cursor.head(), destination);
        assert_eq!(
            cursor
                .waves()
                .filter(|wave| wave.target == Target::Both)
                .count(),
            1
        );
        cursor.advance(2.);
        assert!(!cursor.is_active());
    }
}

/// Viewport velocity deforms the outline while the head tracks the exact same row.
#[test]
fn scroll_track_combines_with_motion_and_finishes_on_a_whole_row() {
    let mut cursor = CursorMotion::default();
    cursor.set_effect(Effect::Elastic);
    let mut scroll = GridScroll::default();
    let source = Geometry::new(30., 120., 10., 20.);
    cursor.retarget(source, 0., 10.);
    scroll.request(3, 12, 0.);
    let mut dragged = false;
    let mut simultaneous = false;
    for frame in 1..=180 {
        let time = f64::from(frame) / 120.;
        let before = scroll.position();
        let active = scroll.advance(time);
        cursor.follow_scroll(
            Vec2::new(0., (before - scroll.position()) * 20.),
            Vec2::new(0., -scroll.velocity() * 20.),
            active,
            scroll.settling(),
            time,
        );
        // The editor projects a new absolute screen rectangle after every scroll update.
        let mut projected = cursor.destination();
        projected.origin.y = source.origin.y - scroll.position() * 20. + 0.0005;
        assert!(!cursor.retarget(projected, time, 10.));
        if frame < 6 {
            let expected = source.origin.y - scroll.position() * 20.;
            assert!((cursor.head().origin.y - expected).abs() < 0.001);
            dragged |= cursor.outline().iter().any(|p| p.y > expected + 21.);
        }
        if frame == 6 {
            let mut destination = cursor.destination();
            destination.origin.x += 20.;
            cursor.retarget(destination, time, 10.);
        }
        simultaneous |= cursor.scroll_stage().is_some() && cursor.stage() == Some(Stage::Travel);
    }
    assert!(dragged && simultaneous);
    assert_eq!(scroll.fraction(), 0.);
    assert_eq!(cursor.scroll_stage(), None);
    assert_eq!(cursor.stage(), Some(Stage::Idle));
    assert!((cursor.head().origin.y - 60.).abs() < 0.001);
    assert_eq!(cursor.head().origin.x, 50.);
    assert!(!cursor.is_active());
}

/// Cancelling a prepared move never emits arrival. Visibility effects release on time.
#[test]
fn cancellation_and_visibility_have_bounded_effects() {
    let mut cursor = CursorMotion::default();
    cursor.set_config(
        CursorConfig::parse(
            r#"
        [stages.arrive]
        effects = [{kind = "ripple", duration_ms = 800}]
        [stages.hide]
        effects = [{kind = "fade", duration_ms = 120}]
        "#,
        )
        .unwrap(),
    );
    let source = Geometry::new(20., 20., 10., 20.);
    cursor.retarget(source, 0., 10.);
    cursor.set_visible(true, 0.);
    cursor.retarget(Geometry::new(30., 20., 10., 20.), 0., 10.);
    cursor.retarget(source, 0.015, 10.);
    cursor.advance(0.3);
    assert_eq!(cursor.waves().count(), 0);
    cursor.set_visible(false, 0.4);
    cursor.advance(0.46);
    assert!(cursor.hiding() && cursor.appearance().0 < 0.6);
    cursor.advance(0.6);
    assert!(!cursor.hiding());
    assert!(!cursor.is_active());
}

/// Reject misspelled phases and unbounded repeat on an instantaneous event.
#[test]
fn invalid_configuration_is_reported() {
    assert!(CursorConfig::parse("[stages.arive]\neffects = []").is_err());
    assert!(
        CursorConfig::parse("[stages.arrive]\neffects = [{kind = 'ripple', repeat_ms = 500}]")
            .is_err()
    );
}
