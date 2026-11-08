//! Curved cursor geometry in logical pixels, independent of the window renderer.
use super::motion::{Geometry, Vec2};

/// Number of cubic segments in the closed fluid outline.
pub const FLUID_SEGMENTS: usize = 48;

/// One cubic Bézier segment of the fluid outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicSegment {
    /// Start of this segment, matching the previous segment's end.
    pub start: Vec2,
    /// Tangent control leaving the start.
    pub control_a: Vec2,
    /// Tangent control approaching the end.
    pub control_b: Vec2,
    /// End of this segment.
    pub end: Vec2,
}

/// Intersect the `ray` with a centered rounded rectangle of extents `half` and corner `radius`.
fn rounded_edge(ray: Vec2, half: Vec2, radius: f32) -> Vec2 {
    let scale = 1. / (ray.x.abs() / half.x).max(ray.y.abs() / half.y);
    let edge = Vec2::new(ray.x * scale, ray.y * scale);
    let inner = Vec2::new(half.x - radius, half.y - radius);
    if edge.x.abs() <= inner.x || edge.y.abs() <= inner.y {
        return edge;
    }

    // The ray crosses a corner arc. Take the farther intersection with that circle.
    let a = ray.x * ray.x + ray.y * ray.y;
    let b = ray.x.abs() * inner.x + ray.y.abs() * inner.y;
    let c = inner.x * inner.x + inner.y * inner.y - radius * radius;
    let scale = (b + (b * b - a * c).max(0.).sqrt()) / a;
    Vec2::new(ray.x * scale, ray.y * scale)
}

/// Estimate a tangent from `incoming` and `outgoing` steps without exceeding their extents.
fn tangent(incoming: f32, outgoing: f32) -> f32 {
    if incoming * outgoing <= 0. {
        0.
    } else {
        ((incoming + outgoing) * 0.5)
            .abs()
            .min(1.5 * incoming.abs().min(outgoing.abs()))
            * incoming.signum()
    }
}

/// Form a single outline around `head`, pulled by the combined `drag` and inflated by `amount`.
/// At zero amount the samples follow a rounded rectangle. At one they form an expanded droplet.
pub(crate) fn fluid_outline(
    head: Geometry,
    drag: Vec2,
    amount: f32,
) -> [CubicSegment; FLUID_SEGMENTS] {
    let center = head.center();
    let half = Vec2::new(
        head.size.x * 0.5 * (1. + 0.32 * amount),
        head.size.y * 0.5 * (1. + 0.24 * amount),
    );
    let radius = half.x.min(half.y) * 0.46;
    let length = drag.x.hypot(drag.y);
    let axis = Vec2::new(drag.x / length.max(0.001), drag.y / length.max(0.001));
    let rect_extent = (half.x - radius) * axis.x.abs() + (half.y - radius) * axis.y.abs() + radius;
    let ellipse_extent = (half.x * axis.x).hypot(half.y * axis.y);
    let extent = (rect_extent + (ellipse_extent - rect_extent) * amount).max(0.001);
    let points: [Vec2; FLUID_SEGMENTS] = std::array::from_fn(|i| {
        let angle = i as f32 * std::f32::consts::TAU / FLUID_SEGMENTS as f32;
        let (sin, cos) = angle.sin_cos();
        let ellipse = Vec2::new(cos * half.x, sin * half.y);
        let base = rounded_edge(ellipse, half, radius).lerp(ellipse, amount);
        // Stretch the rear cap along the lag vector and bend its neck slightly.
        let rear = ((base.x * axis.x + base.y * axis.y) / extent).clamp(0., 1.);
        let pull = rear.powi(6);
        let bend = rear.powi(3) * (1. - rear) * length.min(radius * 4.);
        Vec2::new(
            center.x + base.x + drag.x * pull - axis.y * bend,
            center.y + base.y + drag.y * pull + axis.x * bend,
        )
    });

    let controls: [Vec2; FLUID_SEGMENTS] = std::array::from_fn(|i| {
        let previous = points[(i + FLUID_SEGMENTS - 1) % FLUID_SEGMENTS];
        let current = points[i];
        let next = points[(i + 1) % FLUID_SEGMENTS];
        Vec2::new(
            tangent(current.x - previous.x, next.x - current.x) / 3.,
            tangent(current.y - previous.y, next.y - current.y) / 3.,
        )
    });
    std::array::from_fn(|i| {
        let next = (i + 1) % FLUID_SEGMENTS;
        CubicSegment {
            start: points[i],
            control_a: Vec2::new(points[i].x + controls[i].x, points[i].y + controls[i].y),
            control_b: Vec2::new(
                points[next].x - controls[next].x,
                points[next].y - controls[next].y,
            ),
            end: points[next],
        }
    })
}
