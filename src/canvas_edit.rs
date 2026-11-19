use eframe::egui::{CursorIcon, Pos2, Rect, Vec2, pos2};
use rustlabel::Point;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Move,
    MoveCircle,
    Resize(i8, i8),
    Vertex(usize),
}

pub(crate) struct Drag {
    pub index: usize,
    pub edit: Edit,
    pub origin: Point,
    pub points: Vec<Point>,
}

pub(crate) const HANDLES: [(i8, i8); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
];

pub(crate) fn handle(rect: Rect, x: i8, y: i8) -> Pos2 {
    let axis = |direction, low: f32, high| match direction {
        -1 => low,
        1 => high,
        _ => (low + high) * 0.5,
    };
    pos2(
        axis(x, rect.min.x, rect.max.x),
        axis(y, rect.min.y, rect.max.y),
    )
}

pub(crate) fn handles_at(rect: Rect, pointer: Pos2) -> Option<Edit> {
    HANDLES
        .into_iter()
        .map(|(x, y)| (pointer.distance_sq(handle(rect, x, y)), Edit::Resize(x, y)))
        .filter(|(distance, _)| *distance <= 8. * 8.)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, edit)| edit)
}

pub(crate) fn tag_bounds(
    bounds: Rect,
    canvas: Rect,
    size: Vec2,
    occupied: &[Rect],
) -> Option<Rect> {
    if size.x > canvas.width() || size.y > canvas.height() {
        return None;
    }
    let bounds = bounds.intersect(canvas);
    let x = bounds.left().clamp(canvas.left(), canvas.right() - size.x);
    let y = bounds.top().clamp(canvas.top(), canvas.bottom() - size.y);
    let bottom = (bounds.bottom() - size.y).clamp(canvas.top(), canvas.bottom() - size.y);
    [
        pos2(x, bounds.top() - size.y),
        pos2(bounds.right(), y),
        pos2(bounds.left() - size.x, y),
        pos2(bounds.right(), bottom),
        pos2(bounds.left() - size.x, bottom),
        pos2(x, bounds.bottom()),
    ]
    .into_iter()
    .map(|origin| Rect::from_min_size(origin, size))
    .find(|tag| {
        canvas.contains_rect(*tag)
            && !occupied
                .iter()
                .any(|other| tag.intersect(*other).is_positive())
    })
}

impl Edit {
    pub fn cursor(self, dragging: bool) -> CursorIcon {
        match self {
            Self::Move | Self::MoveCircle if dragging => CursorIcon::Grabbing,
            Self::Move | Self::MoveCircle => CursorIcon::Grab,
            Self::Vertex(_) => CursorIcon::Move,
            Self::Resize(0, _) => CursorIcon::ResizeVertical,
            Self::Resize(_, 0) => CursorIcon::ResizeHorizontal,
            Self::Resize(x, y) if x == y => CursorIcon::ResizeNwSe,
            Self::Resize(_, _) => CursorIcon::ResizeNeSw,
        }
    }
}

impl Drag {
    pub fn positions(&self, pointer: Point, size: [u32; 2]) -> Vec<Point> {
        let mut points = self.points.clone();
        let delta = [pointer[0] - self.origin[0], pointer[1] - self.origin[1]];
        for axis in 0..2 {
            let limit = f64::from(size[axis]);
            match self.edit {
                Edit::Move | Edit::MoveCircle => {
                    let (low, high) = if let (Edit::MoveCircle, [center, edge]) =
                        (self.edit, self.points.as_slice())
                    {
                        let radius = (center[0] - edge[0]).hypot(center[1] - edge[1]);
                        (center[axis] - radius, center[axis] + radius)
                    } else {
                        (
                            points.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min),
                            points
                                .iter()
                                .map(|p| p[axis])
                                .fold(f64::NEG_INFINITY, f64::max),
                        )
                    };
                    if -low <= limit - high {
                        let offset = delta[axis].clamp(-low, limit - high);
                        points.iter_mut().for_each(|p| p[axis] += offset);
                    }
                }
                Edit::Resize(x, y) => {
                    let direction = [x, y][axis];
                    let low = self.points[0][axis].min(self.points[1][axis]);
                    let high = self.points[0][axis].max(self.points[1][axis]);
                    let gap = (high - low).clamp(0.001, 1.);
                    points[0][axis] = if direction < 0 {
                        (low + delta[axis]).clamp(0., (high - gap).max(0.))
                    } else {
                        low
                    };
                    points[1][axis] = if direction > 0 {
                        (high + delta[axis]).clamp((low + gap).min(limit), limit)
                    } else {
                        high
                    };
                }
                Edit::Vertex(index) => {
                    points[index][axis] = (points[index][axis] + delta[axis]).clamp(0., limit);
                }
            }
        }
        points
    }
}

pub(crate) fn read_points(item: &Value) -> Vec<Point> {
    item["points"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|point| {
            let point = [point.get(0)?.as_f64()?, point.get(1)?.as_f64()?];
            point
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1e9)
                .then_some(point)
        })
        .collect()
}

pub(crate) fn hit_order(
    items: &[Value],
    pointer: Pos2,
    screen: impl Fn(Point) -> Pos2,
) -> Vec<usize> {
    let mut hits: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let points: Vec<_> = read_points(item).into_iter().map(&screen).collect();
            let first = *points.first()?;
            let bounds = Rect::from_points(&points);
            if !bounds
                .expand(if item["shape_type"] == "circle" {
                    f32::INFINITY
                } else {
                    5.
                })
                .contains(pointer)
            {
                return None;
            }
            let area = match (item["shape_type"].as_str(), points.as_slice()) {
                (Some("circle"), [center, edge]) => {
                    let radius = center.distance(*edge);
                    if pointer.distance(*center) > radius + 5. {
                        return None;
                    }
                    std::f32::consts::PI * radius * radius
                }
                (Some("polygon"), points) if points.len() >= 3 => {
                    let mut inside = false;
                    let mut area = 0.;
                    for (a, b) in points.iter().zip(points.iter().cycle().skip(1)) {
                        area +=
                            (a.x - first.x) * (b.y - first.y) - (b.x - first.x) * (a.y - first.y);
                        if (a.y > pointer.y) != (b.y > pointer.y)
                            && pointer.x < (b.x - a.x) * (pointer.y - a.y) / (b.y - a.y) + a.x
                        {
                            inside = !inside;
                        }
                    }
                    if !inside {
                        return None;
                    }
                    area.abs() * 0.5
                }
                (Some("line" | "linestrip"), points) => {
                    let near = points.windows(2).any(|segment| {
                        let [a, b] = [segment[0], segment[1]];
                        let ab = b - a;
                        let t = ((pointer - a).dot(ab) / ab.length_sq().max(f32::EPSILON))
                            .clamp(0., 1.);
                        pointer.distance_sq(a + ab * t) <= 5. * 5.
                    });
                    if !near {
                        return None;
                    }
                    0.
                }
                (_, [_]) => {
                    if first.distance(pointer) > 7. {
                        return None;
                    }
                    0.
                }
                _ => bounds.area(),
            };
            Some((index, area))
        })
        .collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits.into_iter().map(|(index, _)| index).collect()
}

#[derive(Default)]
pub(crate) struct ClickCycle(Option<(Pos2, Vec<usize>, usize)>);

impl ClickCycle {
    pub fn choose(&mut self, position: Pos2, hits: Vec<usize>) -> Option<usize> {
        if hits.is_empty() {
            self.0 = None;
            return None;
        }
        let next = match &self.0 {
            Some((anchor, previous, index))
                if *previous == hits && position.distance(*anchor) <= 6. =>
            {
                (*anchor, (index + 1) % hits.len())
            }
            _ => (position, 0),
        };
        let selected = hits[next.1];
        self.0 = Some((next.0, hits, next.1));
        Some(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;
    use serde_json::json;

    #[test]
    fn line_hit_requires_proximity_to_the_actual_segment() {
        let shapes = [json!({"shape_type":"linestrip","points":[[10,10],[40,40],[70,15]]})];
        let screen = |[x, y]: Point| pos2(x as f32, y as f32);
        assert_eq!(hit_order(&shapes, pos2(20., 20.), screen), [0]);
        assert!(hit_order(&shapes, pos2(15., 37.), screen).is_empty());
    }

    #[test]
    fn circle_move_keeps_its_entire_radius_inside_the_image() {
        let drag = Drag {
            index: 0,
            edit: Edit::MoveCircle,
            origin: [25., 25.],
            points: vec![[25., 25.], [35., 25.]],
        };
        assert_eq!(
            drag.positions([110., -12.], [100, 100]),
            vec![[90., 10.], [100., 10.]]
        );
    }

    #[test]
    fn tags_start_above_the_top_left_corner_and_stay_outside() {
        let canvas = Rect::from_min_max(pos2(0., 0.), pos2(300., 200.));
        let size = vec2(55., 16.);
        let middle = Rect::from_min_max(pos2(120., 50.), pos2(200., 110.));
        let top_left = tag_bounds(middle, canvas, size, &[]).unwrap();
        assert_eq!(top_left.left_bottom(), middle.left_top());
        assert!(!top_left.intersect(middle).is_positive());
        let right = tag_bounds(middle, canvas, size, &[top_left]).unwrap();
        assert_eq!(right.left_top(), middle.right_top());
        let left = tag_bounds(middle, canvas, size, &[top_left, right]).unwrap();
        assert_eq!(left.right_top(), middle.left_top());
        let full_width = Rect::from_min_max(pos2(0., 50.), pos2(300., 110.));
        let first = tag_bounds(full_width, canvas, size, &[]).unwrap();
        let below = tag_bounds(full_width, canvas, size, &[first]).unwrap();
        assert_eq!(below.top(), full_width.bottom());
        let against_top = middle.translate(vec2(0., -middle.top()));
        let fallback = tag_bounds(against_top, canvas, size, &[]).unwrap();
        assert_eq!(fallback.left_top(), against_top.right_top());
        assert!(tag_bounds(canvas, canvas, size, &[]).is_none());
    }

    #[test]
    fn overlapping_clicks_cycle_big_to_small_and_reset() {
        let items = [
            json!({"shape_type":"rectangle","points":[[20,20],[40,40]]}),
            json!({"shape_type":"rectangle","points":[[0,0],[100,100]]}),
            json!({"shape_type":"rectangle","points":[[10,10],[65,65]]}),
        ];
        let p = pos2(25., 25.);
        let hits = hit_order(&items, p, |[x, y]| pos2(x as f32, y as f32));
        assert_eq!(hits, vec![1, 2, 0]);
        let mut cycle = ClickCycle::default();
        for expected in [1, 2, 0, 1] {
            assert_eq!(cycle.choose(p, hits.clone()), Some(expected));
        }
        assert_eq!(cycle.choose(pos2(35., 35.), hits), Some(1));
        assert_eq!(cycle.choose(pos2(200., 200.), vec![]), None);
    }

    #[test]
    fn all_eight_handles_move_only_the_requested_edges() {
        let bounds = Rect::from_min_max(pos2(20., 30.), pos2(60., 70.));
        for (x, y) in HANDLES {
            let point = handle(bounds, x, y);
            let edit = handles_at(bounds, point).unwrap();
            assert_eq!(edit, Edit::Resize(x, y));
            let drag = Drag {
                index: 0,
                edit,
                origin: [point.x as f64, point.y as f64],
                points: vec![[60., 70.], [20., 30.]],
            };
            let out = drag.positions([drag.origin[0] + 5., drag.origin[1] + 7.], [100, 100]);
            assert_eq!(
                out,
                vec![
                    [if x < 0 { 25. } else { 20. }, if y < 0 { 37. } else { 30. }],
                    [if x > 0 { 65. } else { 60. }, if y > 0 { 77. } else { 70. }],
                ]
            );
        }
    }

    #[test]
    fn movement_keeps_geometry_intact_at_image_boundaries() {
        let drag = Drag {
            index: 0,
            edit: Edit::Move,
            origin: [30., 40.],
            points: vec![[20., 30.], [60., 70.]],
        };
        assert_eq!(
            drag.positions([130., -60.], [100, 100]),
            vec![[60., 0.], [100., 40.]]
        );
        let drag = Drag {
            index: 0,
            edit: Edit::Vertex(1),
            origin: [40., 30.],
            points: vec![[20., 20.], [40., 30.], [45., 80.]],
        };
        assert_eq!(
            drag.positions([48., 36.], [100, 100]),
            vec![[20., 20.], [48., 36.], [45., 80.]]
        );
    }
}
