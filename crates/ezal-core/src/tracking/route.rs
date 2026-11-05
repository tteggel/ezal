use crate::position::Pointing;
use heapless::Vec;

/// Fixed storage bound. A route that needs more refinement is rejected.
pub(super) const MAX_ROUTE_POINTS: usize = 2_048;

/// Families searched by the bounded planner, including their azimuth overlaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    /// Continuous azimuth with conventional 0–90° elevation.
    Conventional,
    /// Equivalent sky pointing throughout, with elevation 90–180°.
    Inverted,
    /// A continuous bridge near zenith, changing elevation representation.
    /// Its intentional pointing offset is included in every feasibility check.
    OverTop {
        /// Sky elevation above which the bridge is constructed.
        entry_elevation_deg: u8,
        /// Use 90–180° elevation before the bridge instead of after it.
        starts_inverted: bool,
    },
}

/// Reproducible identity within the planner's finite candidate set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteChoice {
    /// Mechanical representation and optional zenith bridge.
    pub kind: RouteKind,
    /// Whole-turn azimuth offset applied to the continuous source bearing.
    pub azimuth_turns: i8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Knot {
    pub at_ms: u32,
    pub az: f32,
    pub el: f32,
}

/// Frozen piecewise-linear mechanical trajectory. Interpolation cannot jump
/// azimuth at 0/360° or change elevation representation on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub(super) choice: RouteChoice,
    pub(super) points: Vec<Knot, MAX_ROUTE_POINTS>,
}

impl Route {
    /// Selected representation and overlap.
    pub const fn choice(&self) -> RouteChoice {
        self.choice
    }
    /// Bounded route storage used by this plan.
    pub fn point_count(&self) -> usize {
        self.points.len()
    }
    /// Duration relative to AOS, in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        self.points.last().map_or(0, |point| point.at_ms)
    }
    /// Interpolate continuous motor angles at a time within the reception window.
    /// Outside that window returns `None`, never an extrapolated command.
    pub fn pointing_at(&self, at_ms: u32) -> Option<Pointing> {
        if self.points.is_empty() || at_ms > self.duration_ms() {
            return None;
        }
        match self
            .points
            .binary_search_by_key(&at_ms, |point| point.at_ms)
        {
            Ok(index) => {
                let p = self.points[index];
                Some(Pointing::new(p.az, p.el))
            }
            Err(index) => {
                let before = *self.points.get(index.checked_sub(1)?)?;
                let after = *self.points.get(index)?;
                let fraction = (at_ms - before.at_ms) as f32 / (after.at_ms - before.at_ms) as f32;
                Some(Pointing::new(
                    before.az + (after.az - before.az) * fraction,
                    before.el + (after.el - before.el) * fraction,
                ))
            }
        }
    }
}

pub(super) fn unwrap_near(previous: f32, angle: f32) -> f32 {
    previous + wrap(angle - previous + 180.0) - 180.0
}

pub(super) fn wrap(angle: f32) -> f32 {
    angle - 360.0 * libm::floorf(angle / 360.0)
}

pub(super) const CANDIDATES: usize = 24;

pub(super) fn choice(index: usize) -> RouteChoice {
    let family = index / 3;
    RouteChoice {
        kind: match family {
            0 => RouteKind::Conventional,
            1 => RouteKind::Inverted,
            _ => RouteKind::OverTop {
                entry_elevation_deg: [80, 85, 88][(family - 2) / 2],
                starts_inverted: (family - 2) % 2 == 1,
            },
        },
        azimuth_turns: (index % 3) as i8 - 1,
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Bridge {
    pub start: Knot,
    pub end: Knot,
    pub after_shift: f32,
}

pub(super) fn bridge(points: &[Knot], choice: RouteChoice) -> Option<Bridge> {
    let RouteKind::OverTop {
        entry_elevation_deg,
        starts_inverted,
    } = choice.kind
    else {
        return None;
    };
    let first = points
        .iter()
        .position(|point| point.el >= f32::from(entry_elevation_deg))?;
    let last = points
        .iter()
        .rposition(|point| point.el >= f32::from(entry_elevation_deg))?;
    // Both ends must be inside the pass, around a genuine peak.
    if first == 0 || last + 1 == points.len() || first >= last {
        return None;
    }
    let offset = f32::from(choice.azimuth_turns) * 360.0;
    let before = points[first - 1];
    let after = points[last + 1];
    let start = Knot {
        az: before.az + offset + if starts_inverted { 180.0 } else { 0.0 },
        el: if starts_inverted {
            180.0 - before.el
        } else {
            before.el
        },
        ..before
    };
    let end_raw = after.az + offset + if starts_inverted { 0.0 } else { 180.0 };
    let end = Knot {
        az: unwrap_near(start.az, end_raw),
        el: if starts_inverted {
            after.el
        } else {
            180.0 - after.el
        },
        ..after
    };
    Some(Bridge {
        start,
        end,
        after_shift: end.az - end_raw,
    })
}

pub(super) fn transform(point: Knot, choice: RouteChoice, bridge: Option<Bridge>) -> Knot {
    let offset = f32::from(choice.azimuth_turns) * 360.0;
    match choice.kind {
        RouteKind::Conventional => Knot {
            az: point.az + offset,
            ..point
        },
        RouteKind::Inverted => Knot {
            az: point.az + offset + 180.0,
            el: 180.0 - point.el,
            ..point
        },
        RouteKind::OverTop {
            starts_inverted, ..
        } => {
            let bridge = bridge.expect("bridge candidates are checked before building");
            if point.at_ms >= bridge.start.at_ms && point.at_ms <= bridge.end.at_ms {
                let fraction = (point.at_ms - bridge.start.at_ms) as f32
                    / (bridge.end.at_ms - bridge.start.at_ms) as f32;
                Knot {
                    az: bridge.start.az + (bridge.end.az - bridge.start.az) * fraction,
                    el: bridge.start.el + (bridge.end.el - bridge.start.el) * fraction,
                    ..point
                }
            } else {
                let after = point.at_ms > bridge.end.at_ms;
                let inverted = starts_inverted != after;
                Knot {
                    az: point.az
                        + offset
                        + if inverted { 180.0 } else { 0.0 }
                        + if after { bridge.after_shift } else { 0.0 },
                    el: if inverted { 180.0 - point.el } else { point.el },
                    ..point
                }
            }
        }
    }
}
