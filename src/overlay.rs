use eframe::egui::{Pos2, Rect, Vec2};

use super::LiveOverlayLine;

const POSITION_SMOOTHING: f32 = 0.35;
#[cfg(test)]
const LABEL_GAP: f32 = 6.0;
#[cfg(test)]
const MAX_PREFERRED_SOURCE_GAP: f32 = 48.0;
#[cfg(test)]
const LABEL_COLLISION_PENALTY: f32 = 1_000_000.0;

pub(super) fn stabilize_lines(previous: &[LiveOverlayLine], current: &mut [LiveOverlayLine]) {
    let mut used = vec![false; previous.len()];
    for line in current {
        let mut best: Option<(usize, f32)> = None;
        for (index, candidate) in previous.iter().enumerate() {
            if used[index] || line.id != candidate.id {
                continue;
            }
            let similarity = text_similarity(&line.source, &candidate.source);
            if similarity < 0.72 {
                continue;
            }
            let current_rect = line_rect(line);
            let candidate_rect = line_rect(candidate);
            let overlap = intersection_over_union(current_rect, candidate_rect);
            let distance = current_rect.center().distance(candidate_rect.center());
            let distance_limit = current_rect
                .height()
                .max(candidate_rect.height())
                .mul_add(3.0, 32.0)
                .max(72.0);
            if overlap < 0.04 && distance > distance_limit {
                continue;
            }
            let score =
                (1.0 - similarity) * 2.0 + (1.0 - overlap) + distance / distance_limit * 0.25;
            if best.is_none_or(|(_, best_score)| score < best_score) {
                best = Some((index, score));
            }
        }

        if let Some((index, _)) = best {
            used[index] = true;
            let old = &previous[index];
            line.x = lerp(old.x, line.x, POSITION_SMOOTHING);
            line.y = lerp(old.y, line.y, POSITION_SMOOTHING);
            line.width = lerp(old.width, line.width, POSITION_SMOOTHING);
            line.height = lerp(old.height, line.height, POSITION_SMOOTHING);
        }
    }
}

#[cfg(test)]
pub(super) fn place_overlay_box(
    source: Rect,
    requested_size: Vec2,
    bounds: Rect,
    occupied: &[Rect],
    source_obstacles: &[Rect],
) -> Rect {
    let size = Vec2::new(
        requested_size.x.min(bounds.width()),
        requested_size.y.min(bounds.height()),
    );
    let min_x = bounds.min.x;
    let max_x = (bounds.max.x - size.x).max(min_x);
    let min_y = bounds.min.y;
    let max_y = (bounds.max.y - size.y).max(min_y);
    let x_candidates = [
        source.min.x,
        source.center().x - size.x * 0.5,
        source.max.x - size.x,
        source.max.x + LABEL_GAP,
        source.min.x - size.x - LABEL_GAP,
    ];
    let mut y_candidates = vec![
        source.min.y,
        source.center().y - size.y * 0.5,
        source.max.y + LABEL_GAP,
        source.min.y - size.y - LABEL_GAP,
    ];
    for obstacle in occupied.iter().chain(source_obstacles) {
        if rect_distance(*obstacle, source) <= size.y + MAX_PREFERRED_SOURCE_GAP {
            y_candidates.push(obstacle.max.y + LABEL_GAP);
            y_candidates.push(obstacle.min.y - size.y - LABEL_GAP);
        }
    }

    let mut candidates = Vec::new();
    for x in x_candidates {
        for &y in &y_candidates {
            let candidate = Rect::from_min_size(
                Pos2::new(x.clamp(min_x, max_x), y.clamp(min_y, max_y)),
                size,
            );
            if !candidates
                .iter()
                .any(|existing: &Rect| existing.min.distance(candidate.min) < 2.0)
            {
                candidates.push(candidate);
            }
        }
    }

    candidates
        .into_iter()
        .min_by(|left, right| {
            label_score(*left, source, occupied, source_obstacles).total_cmp(&label_score(
                *right,
                source,
                occupied,
                source_obstacles,
            ))
        })
        .unwrap_or_else(|| {
            Rect::from_min_size(
                Pos2::new(
                    source.min.x.clamp(min_x, max_x),
                    source.min.y.clamp(min_y, max_y),
                ),
                size,
            )
        })
}

pub(super) fn stack_overlay_boxes(
    sources: &[Rect],
    requested_sizes: &[Vec2],
    bounds: Rect,
    horizontal: HorizontalPlacement,
    gap: f32,
) -> Vec<Rect> {
    let fixed_left = match horizontal {
        HorizontalPlacement::Fixed(requested_left) => {
            let max_width = requested_sizes
                .iter()
                .map(|size| size.x.min(bounds.width()))
                .fold(0.0, f32::max);
            let max_left = (bounds.max.x - max_width).max(bounds.min.x);
            Some(requested_left.clamp(bounds.min.x, max_left))
        }
        HorizontalPlacement::Source => None,
    };
    let mut boxes = Vec::with_capacity(sources.len().min(requested_sizes.len()));
    let mut next_top = bounds.min.y;

    for (source, requested_size) in sources.iter().zip(requested_sizes) {
        let size = Vec2::new(
            requested_size.x.min(bounds.width()),
            requested_size.y.min(bounds.height()),
        );
        let max_left = (bounds.max.x - size.x).max(bounds.min.x);
        let left = fixed_left.unwrap_or_else(|| source.min.x.clamp(bounds.min.x, max_left));
        let max_top = (bounds.max.y - size.y).max(bounds.min.y);
        let desired_top = source.min.y.clamp(bounds.min.y, max_top);
        let top = desired_top.max(next_top);
        let rect = Rect::from_min_size(Pos2::new(left, top), size);
        next_top = rect.max.y + gap;
        boxes.push(rect);
    }

    if boxes.last().is_some_and(|last| last.max.y > bounds.max.y) {
        let mut next_top = bounds.max.y;
        for rect in boxes.iter_mut().rev() {
            let top = rect.min.y.min(next_top - rect.height());
            *rect = Rect::from_min_size(Pos2::new(rect.min.x, top), rect.size());
            next_top = top - gap;
        }
        if let Some(first) = boxes.first()
            && first.min.y < bounds.min.y
        {
            let shift = bounds.min.y - first.min.y;
            for rect in &mut boxes {
                *rect = rect.translate(Vec2::new(0.0, shift));
            }
        }
    }

    boxes
}

#[derive(Debug, Clone, Copy)]
pub(super) enum HorizontalPlacement {
    Fixed(f32),
    Source,
}

#[cfg(test)]
fn label_score(candidate: Rect, source: Rect, occupied: &[Rect], source_obstacles: &[Rect]) -> f32 {
    let candidate_area = candidate.area().max(1.0);
    let label_overlap_ratio = occupied
        .iter()
        .map(|other| intersection_area(candidate.expand(3.0), *other))
        .sum::<f32>()
        / candidate_area;
    let source_overlap_ratio = source_obstacles
        .iter()
        .filter(|other| !same_rect(**other, source))
        .map(|other| intersection_area(candidate, *other))
        .sum::<f32>()
        / candidate_area;
    let distance = rect_distance(candidate, source);
    let excessive_distance = (distance - MAX_PREFERRED_SOURCE_GAP).max(0.0);
    let horizontal_drift = (candidate.min.x - source.min.x).abs();

    let collision_penalty = if label_overlap_ratio > f32::EPSILON {
        LABEL_COLLISION_PENALTY + label_overlap_ratio * 10_000.0
    } else {
        0.0
    };

    collision_penalty
        + source_overlap_ratio * 2_500.0
        + distance * 18.0
        + excessive_distance * excessive_distance * 4.0
        + horizontal_drift * 0.15
}

fn line_rect(line: &LiveOverlayLine) -> Rect {
    Rect::from_min_size(
        Pos2::new(line.x, line.y),
        Vec2::new(line.width.max(1.0), line.height.max(1.0)),
    )
}

fn intersection_over_union(left: Rect, right: Rect) -> f32 {
    let intersection = intersection_area(left, right);
    if intersection <= 0.0 {
        return 0.0;
    }
    let union = left.area() + right.area() - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn intersection_area(left: Rect, right: Rect) -> f32 {
    let intersection = left.intersect(right);
    intersection.width().max(0.0) * intersection.height().max(0.0)
}

#[cfg(test)]
fn rect_distance(left: Rect, right: Rect) -> f32 {
    let horizontal = if left.max.x < right.min.x {
        right.min.x - left.max.x
    } else if right.max.x < left.min.x {
        left.min.x - right.max.x
    } else {
        0.0
    };
    let vertical = if left.max.y < right.min.y {
        right.min.y - left.max.y
    } else if right.max.y < left.min.y {
        left.min.y - right.max.y
    } else {
        0.0
    };
    horizontal.hypot(vertical)
}

#[cfg(test)]
fn same_rect(left: Rect, right: Rect) -> bool {
    left.min.distance(right.min) < 1.0 && left.max.distance(right.max) < 1.0
}

fn text_similarity(left: &str, right: &str) -> f32 {
    let left = normalized_chars(left);
    let right = normalized_chars(right);
    if left == right {
        return 1.0;
    }
    if left.len() < 2 || right.len() < 2 {
        return 0.0;
    }
    let left_pairs = left.windows(2).collect::<Vec<_>>();
    let right_pairs = right.windows(2).collect::<Vec<_>>();
    let mut used = vec![false; right_pairs.len()];
    let mut matches = 0;
    for pair in &left_pairs {
        if let Some((index, _)) = right_pairs.iter().enumerate().find(|(index, candidate)| {
            !used[*index] && candidate[0] == pair[0] && candidate[1] == pair[1]
        }) {
            used[index] = true;
            matches += 1;
        }
    }
    2.0 * matches as f32 / (left_pairs.len() + right_pairs.len()) as f32
}

fn normalized_chars(text: &str) -> Vec<char> {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    start + (end - start) * amount
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(source: &str, x: f32, y: f32) -> LiveOverlayLine {
        LiveOverlayLine {
            id: 1,
            source: source.to_owned(),
            translation: String::new(),
            x,
            y,
            width: 120.0,
            height: 20.0,
            pending: true,
        }
    }

    #[test]
    fn smooths_small_ocr_jitter_between_frames() {
        let previous = vec![line("Hello from Discord", 100.0, 80.0)];
        let mut current = vec![line("Hello from Discord", 106.0, 84.0)];
        stabilize_lines(&previous, &mut current);
        assert!((current[0].x - 102.1).abs() < 0.01);
        assert!((current[0].y - 81.4).abs() < 0.01);
    }

    #[test]
    fn does_not_smooth_a_real_large_move() {
        let previous = vec![line("Hello from Discord", 10.0, 10.0)];
        let mut current = vec![line("Hello from Discord", 500.0, 400.0)];
        stabilize_lines(&previous, &mut current);
        assert_eq!(current[0].x, 500.0);
        assert_eq!(current[0].y, 400.0);
    }

    #[test]
    fn label_candidates_avoid_other_source_lines() {
        let source = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(120.0, 18.0));
        let other = Rect::from_min_size(Pos2::new(100.0, 122.0), Vec2::new(120.0, 18.0));
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let placed = place_overlay_box(
            source,
            Vec2::new(260.0, 70.0),
            bounds,
            &[],
            &[source, other],
        );
        assert_eq!(intersection_area(placed, other), 0.0);
        assert!(bounds.contains_rect(placed));
    }

    #[test]
    fn chooses_the_nearest_collision_free_slot_when_the_screen_is_crowded() {
        let source = Rect::from_min_size(Pos2::new(220.0, 220.0), Vec2::new(150.0, 20.0));
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0));
        let occupied = [
            Rect::from_min_size(Pos2::new(190.0, 125.0), Vec2::new(430.0, 90.0)),
            Rect::from_min_size(Pos2::new(190.0, 245.0), Vec2::new(430.0, 90.0)),
        ];
        let placed =
            place_overlay_box(source, Vec2::new(410.0, 92.0), bounds, &occupied, &[source]);

        assert!(
            occupied
                .iter()
                .all(|other| !placed.expand(2.0).intersects(*other))
        );
        assert!(rect_distance(placed, source) <= 110.0);
        assert!(bounds.contains_rect(placed));
    }

    #[test]
    fn stacks_translation_blocks_in_one_vertical_column() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 500.0));
        let sources = [
            Rect::from_min_size(Pos2::new(120.0, 80.0), Vec2::new(180.0, 18.0)),
            Rect::from_min_size(Pos2::new(170.0, 105.0), Vec2::new(160.0, 18.0)),
            Rect::from_min_size(Pos2::new(95.0, 132.0), Vec2::new(210.0, 18.0)),
        ];
        let sizes = [
            Vec2::new(300.0, 54.0),
            Vec2::new(280.0, 72.0),
            Vec2::new(300.0, 46.0),
        ];
        let stacked = stack_overlay_boxes(
            &sources,
            &sizes,
            bounds,
            HorizontalPlacement::Fixed(110.0),
            LABEL_GAP,
        );

        assert_eq!(stacked.len(), 3);
        assert!(stacked.iter().all(|rect| rect.min.x == 110.0));
        assert!(stacked.windows(2).all(|pair| pair[0].max.y < pair[1].min.y));
        assert!(stacked.windows(2).all(|pair| pair[0].min.y < pair[1].min.y));
        assert!(stacked.iter().all(|rect| bounds.contains_rect(*rect)));
    }

    #[test]
    fn source_placement_keeps_each_ocr_horizontal_anchor() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 300.0));
        let sources = [
            Rect::from_min_size(Pos2::new(80.0, 40.0), Vec2::new(120.0, 18.0)),
            Rect::from_min_size(Pos2::new(210.0, 90.0), Vec2::new(100.0, 18.0)),
        ];
        let sizes = [Vec2::new(150.0, 30.0), Vec2::new(140.0, 30.0)];
        let stacked = stack_overlay_boxes(
            &sources,
            &sizes,
            bounds,
            HorizontalPlacement::Source,
            LABEL_GAP,
        );

        assert_eq!(stacked[0].min.x, 80.0);
        assert_eq!(stacked[1].min.x, 210.0);
    }
}
