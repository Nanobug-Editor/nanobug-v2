//! Export the actual cursor model for a deterministic comparison animation, without a native window.
use nanobug::cursor_motion::{CursorMotion, Effect, Geometry, Vec2};
use std::{
    fs::File,
    io::{BufWriter, Write},
};

/// Write to CSV sink `writer`: visual `kind` at `frame` for `effect`, normalized `value` and cursor `points`.
fn row(
    writer: &mut impl Write,
    effect: usize,
    frame: usize,
    kind: u8,
    value: f32,
    points: [Vec2; 4],
) -> std::io::Result<()> {
    write!(writer, "{effect},{frame},{kind},{value:.6}")?;
    for point in points {
        write!(writer, ",{:.4},{:.4}", point.x, point.y)?;
    }
    writeln!(writer)
}

/// Sample all four production motion profiles at 50 Hz for a shareable model preview.
fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test-output/cursor-effects.csv".into());
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "effect,frame,kind,value,x0,y0,x1,y1,x2,y2,x3,y3")?;
    let events = [
        (0., Geometry::new(45., 62., 10., 20.)),
        (0.26, Geometry::new(330., 62., 10., 20.)),
        (0.62, Geometry::new(130., 133., 10., 20.)),
        (0.98, Geometry::new(130., 133., 2., 20.)),
        (1.12, Geometry::new(240., 133., 2., 20.)),
        (1.24, Geometry::new(250., 133., 2., 20.)),
        (1.36, Geometry::new(260., 133., 2., 20.)),
        (1.54, Geometry::new(45., 62., 10., 20.)),
    ];
    for (index, effect) in [
        Effect::Fluid,
        Effect::Elastic,
        Effect::Afterglow,
        Effect::Ripple,
    ]
    .into_iter()
    .enumerate()
    {
        let mut motion = CursorMotion::default();
        motion.set_effect(effect);
        let mut event = 0;
        for frame in 0..=125 {
            let time = frame as f64 / 50.;
            while event < events.len() && events[event].0 <= time {
                motion.retarget(events[event].1, events[event].0, 10.);
                event += 1;
            }
            motion.advance(time);
            row(
                &mut writer,
                index,
                frame,
                3,
                1.,
                motion.destination().corners(),
            )?;
            for (points, opacity) in motion.ghosts() {
                row(&mut writer, index, frame, 1, opacity, points)?;
            }
            for (center, age) in motion.pulses() {
                row(&mut writer, index, frame, 2, age, [center; 4])?;
            }
            let points = if motion.curved() {
                motion.corners()
            } else {
                motion.outline()
            };
            row(&mut writer, index, frame, 0, 1., points)?;
        }
    }
    writer.flush()
}
