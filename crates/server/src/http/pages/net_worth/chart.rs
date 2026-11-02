//! The net worth line, drawn on the server in integers: no script, no style attribute and
//! no colour here, only classes `base.css` paints from the theme's tokens.
//!
//! Coordinates are tenths of a pixel worked out in `i128`, so no amount can overflow them
//! and no floating point touches money. A period with no value breaks the line and is
//! never bridged or drawn as 0. The chart adds to the table beside it, which holds every
//! value: its tooltips are never the only way to read one.

use coffer_domain::money::Currency;
use jiff::civil::Date;
use maud::{Markup, html};

use crate::http::pages::amounts::{day, grouped};

/// One period, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChartPoint {
    pub(super) on: Date,
    /// In minor units; `None` when the period has no value to draw.
    pub(super) value: Option<i64>,
    /// The value in words, for the direct label and the chart's name.
    pub(super) label: String,
    /// The date and value, for the tooltip.
    pub(super) title: String,
}

/// Which of the page's two drawings: CSS shows one by width, so a phone doesn't scale
/// labels drawn for a wide chart down to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChartSize {
    Wide,
    Narrow,
}

/// A drawing's fixed measures, in pixels of its `viewBox`.
struct Frame {
    class: &'static str,
    width: i32,
    height: i32,
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
    x_labels_at: i32,
    /// The most value ticks.
    ticks: i128,
    /// The most date labels.
    x_labels: usize,
    /// The least room between two date labels' anchors.
    x_label_gap: i32,
    /// About how wide one character of a label is.
    char_width: i32,
    /// About how far a label rises above its baseline.
    text_rise: i32,
}

const WIDE: Frame = Frame {
    class: "chart chart-wide",
    width: 720,
    height: 280,
    left: 8,
    right: 712,
    top: 30,
    bottom: 252,
    x_labels_at: 272,
    ticks: 5,
    x_labels: 6,
    x_label_gap: 96,
    char_width: 7,
    text_rise: 14,
};

const NARROW: Frame = Frame {
    class: "chart chart-narrow",
    width: 360,
    height: 240,
    left: 8,
    right: 352,
    top: 30,
    bottom: 208,
    x_labels_at: 230,
    ticks: 4,
    x_labels: 4,
    x_label_gap: 84,
    char_width: 9,
    text_rise: 17,
};

/// About how far a label falls below its baseline, in tenths.
const TEXT_DROP: i32 = 30;
/// A hover target is at least this wide, in tenths.
const HIT_WIDTH: i32 = 240;
/// What a gap says of itself.
const GAP_WORDS: &str = "no total";
/// A gap this wide, in tenths, has room to say what it is.
const GAP_WORDS_WIDTH: i32 = 640;
/// Up to this many days between the ends, a date label names the day.
const DAYS_ON_LABELS: i32 = 120;
/// Up to this many, it names the month; beyond, the year.
const MONTHS_ON_LABELS: i32 = 2000;

impl ChartSize {
    fn frame(self) -> &'static Frame {
        match self {
            Self::Wide => &WIDE,
            Self::Narrow => &NARROW,
        }
    }
}

/// `tenths` as an SVG number.
fn px(tenths: i32) -> String {
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Where `value` sits, in tenths: `top + (max − value) × height / (max − min)`. Equal ends
/// draw in the middle. `None` only for a value outside `[min, max]` far past any drawing.
fn y_tenths(value: i64, min: i64, max: i64, top: i32, height: i32) -> Option<i32> {
    let (top, height) = (i128::from(top) * 10, i128::from(height) * 10);
    let span = i128::from(max).checked_sub(i128::from(min))?;
    if span == 0 {
        return i32::try_from(top.checked_add(height / 2)?).ok();
    }
    let below_max = i128::from(max).checked_sub(i128::from(value))?;
    let offset = below_max.checked_mul(height)?.checked_div(span)?;
    i32::try_from(top.checked_add(offset)?).ok()
}

/// Where the day `days` after the first sits, of `span` days in all, in tenths.
fn x_tenths(frame: &Frame, days: i32, span: i32) -> i32 {
    let (left, width) = (frame.left * 10, (frame.right - frame.left) * 10);
    if span == 0 {
        return left + width / 2;
    }
    left + i32::try_from(i64::from(days) * i64::from(width) / i64::from(span)).unwrap_or(width)
}

/// The round values to mark between `min` and `max`: multiples of the smallest step of 1,
/// 2 or 5 × 10^k giving at most `limit` of them. None for a flat line, which has no scale.
fn ticks(min: i64, max: i64, limit: i128) -> Vec<i64> {
    if min == max {
        return Vec::new();
    }
    let (low, high) = (i128::from(min), i128::from(max));
    let mut power = 1_i128;
    for _ in 0..=19 {
        for multiple in [1, 2, 5] {
            let step = multiple * power;
            let first = -((-low).div_euclid(step));
            let last = high.div_euclid(step);
            if last - first < limit {
                return (first..=last)
                    .filter_map(|tick| i64::try_from(tick * step).ok())
                    .collect();
            }
        }
        power *= 10;
    }
    Vec::new()
}

/// A drawn point's place, in tenths.
#[derive(Clone, Copy)]
struct Placed {
    x: i32,
    y: i32,
}

/// Every period's place across, and where it sits up and down if it has a value.
fn place(points: &[ChartPoint], frame: &Frame) -> Vec<(i32, Option<Placed>)> {
    let first = points.first().map(|point| point.on);
    let last = points.last().map(|point| point.on);
    let days_from_first = |on: Date| {
        first
            .and_then(|first| first.until(on).ok())
            .map_or(0, |span| span.get_days())
    };
    let span = last.map_or(0, days_from_first);
    let values = || points.iter().filter_map(|point| point.value);
    let (min, max) = (values().min().unwrap_or(0), values().max().unwrap_or(0));
    points
        .iter()
        .map(|point| {
            let x = x_tenths(frame, days_from_first(point.on), span);
            let placed = point.value.and_then(|value| {
                y_tenths(value, min, max, frame.top, frame.bottom - frame.top)
                    .map(|y| Placed { x, y })
            });
            (x, placed)
        })
        .collect()
}

/// The line's path: one `M` per unbroken run of at least two points, so a gap is never
/// crossed. A run of one has nothing to join, and draws a marker instead.
fn line_path(placed: &[(i32, Option<Placed>)]) -> String {
    let mut path = String::new();
    for run in placed.split(|(_, point)| point.is_none()) {
        if run.len() < 2 {
            continue;
        }
        for (index, point) in run
            .iter()
            .filter_map(|(_, point)| point.as_ref())
            .enumerate()
        {
            let command = if index == 0 { 'M' } else { 'L' };
            if !path.is_empty() {
                path.push(' ');
            }
            path.push(command);
            path.push_str(&px(point.x));
            path.push(' ');
            path.push_str(&px(point.y));
        }
    }
    path
}

/// The points to mark: those standing alone, with no neighbour to join a line to, and
/// the latest.
fn marks(placed: &[(i32, Option<Placed>)]) -> Vec<Placed> {
    let mut marks: Vec<Placed> = placed
        .split(|(_, point)| point.is_none())
        .filter(|run| run.len() == 1)
        .filter_map(|run| run[0].1)
        .collect();
    let latest = placed.iter().rev().find_map(|(_, point)| *point);
    if let Some(latest) = latest.filter(|latest| marks.last().is_none_or(|lone| lone.x != latest.x))
    {
        marks.push(latest);
    }
    marks
}

/// The stretches, in tenths across, holding periods with no value: from the last point
/// before each to the first after it, or to the plot's edge of the periods.
fn gaps(placed: &[(i32, Option<Placed>)]) -> Vec<(i32, i32)> {
    let mut gaps = Vec::new();
    let mut open: Option<i32> = None;
    let mut before = None;
    for (x, point) in placed {
        match (point, open) {
            (None, None) => open = Some(before.unwrap_or(*x)),
            (Some(_), Some(from)) => {
                gaps.push((from, *x));
                open = None;
            }
            _ => {}
        }
        before = Some(*x);
    }
    if let (Some(from), Some(to)) = (open, before) {
        gaps.push((from, to));
    }
    gaps
}

/// The hover target of each period, as `(x, width)` in tenths: from where its share of the
/// plot starts, never narrower than a finger, staying inside the drawing. Where they overlap
/// the next one drawn lies on top from its own start, so the band showing at any point is
/// that point's, as far as the last finger's width of the plot.
fn hit_bands(placed: &[(i32, Option<Placed>)], frame: &Frame) -> Vec<(i32, i32)> {
    let across: Vec<i32> = placed.iter().map(|(x, _)| *x).collect();
    let edge = (frame.left * 10, frame.right * 10);
    across
        .iter()
        .enumerate()
        .map(|(index, x)| {
            let from = index
                .checked_sub(1)
                .and_then(|before| across.get(before))
                .map_or(edge.0, |before| i32::midpoint(*before, *x));
            let to = across
                .get(index + 1)
                .map_or(edge.1, |after| i32::midpoint(*after, *x));
            let width = (to - from).max(HIT_WIDTH);
            (from.clamp(0, frame.width * 10 - width), width)
        })
        .collect()
}

/// A date label, as much of the date as the span needs.
fn date_label(on: Date, span_days: i32) -> String {
    let format = if span_days <= DAYS_ON_LABELS {
        "%-d %b"
    } else if span_days <= MONTHS_ON_LABELS {
        "%b %Y"
    } else {
        "%Y"
    };
    on.strftime(format).to_string()
}

/// Which periods get a date label: evenly along them, the ends included, at most
/// `frame.x_labels`, never two saying the same or crowding each other.
fn x_label_picks(
    points: &[ChartPoint],
    placed: &[(i32, Option<Placed>)],
    frame: &Frame,
) -> Vec<(usize, String)> {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return Vec::new();
    };
    let span = first.on.until(last.on).map_or(0, |span| span.get_days());
    let count = points.len();
    let wanted = frame.x_labels.min(count);
    let mut picks: Vec<(usize, String)> = Vec::new();
    for slot in 0..wanted {
        let index = if wanted == 1 {
            0
        } else {
            slot * (count - 1) / (wanted - 1)
        };
        let text = date_label(points[index].on, span);
        let close = |earlier: usize| placed[index].0 - placed[earlier].0 < frame.x_label_gap * 10;
        match picks.last() {
            Some((earlier, said)) if *said == text || close(*earlier) => {
                // The last period keeps its label, as the newest is the one to read.
                if slot + 1 == wanted && picks.len() > 1 {
                    picks.pop();
                    picks.push((index, text));
                }
            }
            _ => picks.push((index, text)),
        }
    }
    picks
}

/// How a date label is anchored, so that neither end leaves the drawing.
fn x_anchor(position: usize, picks: usize) -> &'static str {
    match (position, picks) {
        (_, 1) => "middle",
        (0, _) => "start",
        (at, count) if at + 1 == count => "end",
        _ => "middle",
    }
}

/// The space a text takes up as `((left, right), (top, bottom))`, in tenths.
type Space = ((i32, i32), (i32, i32));

/// The direct label on the latest value, and where it goes.
struct DirectLabel {
    text: String,
    x: i32,
    y: i32,
    anchor: &'static str,
}

impl DirectLabel {
    /// The space it takes up.
    fn space(&self, frame: &Frame) -> Space {
        let width = text_width(&self.text, frame);
        let left = match self.anchor {
            "end" => self.x - width,
            "middle" => self.x - width / 2,
            _ => self.x,
        };
        (
            (left, left + width),
            (self.y - frame.text_rise * 10, self.y + TEXT_DROP),
        )
    }
}

/// How wide `text` is about to be in `frame`, in tenths.
fn text_width(text: &str, frame: &Frame) -> i32 {
    i32::try_from(text.chars().count()).unwrap_or(i32::MAX / 100) * frame.char_width * 10
}

/// The lowest and highest `y` of each piece of the line between `left` and `right`.
fn line_pieces(
    placed: &[(i32, Option<Placed>)],
    (left, right): (i32, i32),
) -> impl Iterator<Item = (i32, i32)> {
    placed.windows(2).filter_map(move |pair| {
        let (from, to) = (pair[0].1?, pair[1].1?);
        if from.x > right || to.x < left {
            return None;
        }
        let y_at = |x: i32| {
            if to.x == from.x {
                return from.y;
            }
            let along = i64::from(to.y - from.y) * i64::from(x - from.x) / i64::from(to.x - from.x);
            from.y + i32::try_from(along).unwrap_or(0)
        };
        let (start, end) = (y_at(left.max(from.x)), y_at(right.min(to.x)));
        Some((start.min(end), start.max(end)))
    })
}

/// Whether the line passes through the band `[top, bottom]` between `left` and `right`.
fn line_crosses(
    placed: &[(i32, Option<Placed>)],
    across: (i32, i32),
    (top, bottom): (i32, i32),
) -> bool {
    line_pieces(placed, across).any(|(high, low)| high <= bottom && low >= top)
}

/// Where the direct label's baseline goes, in tenths: just above the latest value; if the
/// line runs through that, just below it, clear of the date labels; if there as well, just
/// above the highest the line gets under the label. It keeps off the line in all three.
fn direct_label_y(
    placed: &[(i32, Option<Placed>)],
    latest: Placed,
    across: (i32, i32),
    frame: &Frame,
) -> i32 {
    let above = (latest.y - 240, latest.y - 60);
    let below = (latest.y + 60, latest.y + 240);
    if !line_crosses(placed, across, above) {
        latest.y - 120
    } else if !line_crosses(placed, across, below) && below.1 < frame.bottom * 10 {
        latest.y + 200
    } else {
        line_pieces(placed, across)
            .map(|(high, _)| high)
            .min()
            .map_or(latest.y, |highest| highest - 100)
    }
}

/// The label of the latest value, at its side of the plot. It's dated when later periods
/// have none, since a gap too narrow for words wouldn't otherwise say it isn't the value now.
fn direct_label(
    points: &[ChartPoint],
    placed: &[(i32, Option<Placed>)],
    frame: &Frame,
) -> Option<DirectLabel> {
    let index = placed.iter().rposition(|(_, point)| point.is_some())?;
    let latest = placed[index].1?;
    let point = &points[index];
    let text = if index + 1 == points.len() {
        point.label.clone()
    } else {
        format!("{}, {}", point.label, point.on.strftime("%-d %b %Y"))
    };
    let middle = (frame.left + frame.right) * 5;
    let width = text_width(&text, frame);
    let (anchor, across) = if placed.len() == 1 {
        ("middle", (latest.x - width / 2, latest.x + width / 2))
    } else if latest.x > middle {
        ("end", (latest.x - width, latest.x))
    } else {
        ("start", (latest.x, latest.x + width))
    };
    Some(DirectLabel {
        x: latest.x,
        y: direct_label_y(placed, latest, across, frame),
        text,
        anchor,
    })
}

/// Where a value tick's label goes, above its grid line: `(x, anchor)` at the plot's left
/// if that's clear of the line and of `taken`, else at its right. With neither clear
/// it has no label: the others carry the scale, and a stub of line beside a figure reads as
/// a minus sign.
fn tick_label_place(
    tick_y: i32,
    text: &str,
    placed: &[(i32, Option<Placed>)],
    taken: &[Space],
    frame: &Frame,
) -> Option<(i32, &'static str)> {
    let width = text_width(text, frame);
    let band = (tick_y - 40 - frame.text_rise * 10, tick_y - 40 + TEXT_DROP);
    let (left, right) = (frame.left * 10, frame.right * 10);
    [
        ((left, left + width), frame.left, "start"),
        ((right - width, right), frame.right, "end"),
    ]
    .into_iter()
    .find(|(across, _, _)| {
        let over_text = taken.iter().any(|(other_across, other_band)| {
            across.0 <= other_across.1
                && other_across.0 <= across.1
                && band.0 <= other_band.1
                && other_band.0 <= band.1
        });
        !over_text && !line_crosses(placed, *across, band)
    })
    .map(|(_, x, anchor)| (x, anchor))
}

/// The chart's accessible name: its range, its first and last values, and where the rest
/// are. Empty points have no drawing and no name.
fn name(points: &[ChartPoint]) -> Option<String> {
    let (first, last) = (points.first()?, points.last()?);
    Some(format!(
        "Net worth from {} to {}: first {}, last {}. The table below holds every value.",
        day(first.on),
        day(last.on),
        first.label,
        last.label,
    ))
}

/// Whether the values have a scale that leaves zero out, so a change looks larger than it is.
pub(super) fn scale_misses_zero(points: &[ChartPoint]) -> bool {
    let values = || points.iter().filter_map(|point| point.value);
    let (Some(min), Some(max)) = (values().min(), values().max()) else {
        return false;
    };
    min != max && (min > 0 || max < 0)
}

/// One drawing of `points`, whose values are in `currency`.
pub(super) fn line_chart(points: &[ChartPoint], currency: Currency, size: ChartSize) -> Markup {
    let frame = size.frame();
    let Some(name) = name(points) else {
        return html! {};
    };
    let placed = place(points, frame);
    let values = || points.iter().filter_map(|point| point.value);
    let (min, max) = (values().min().unwrap_or(0), values().max().unwrap_or(0));
    let direct = direct_label(points, &placed, frame);
    let path = line_path(&placed);
    let plot_height = frame.bottom - frame.top;
    let picks = x_label_picks(points, &placed, frame);
    let bands = gaps(&placed);
    // Only with a line to tell it from: with none, one sentence says so instead.
    let words: Vec<i32> = bands
        .iter()
        .filter(|(from, to)| to - from >= GAP_WORDS_WIDTH && direct.is_some())
        .map(|(from, to)| i32::midpoint(*from, *to))
        .collect();
    let words_y = frame.top + plot_height / 2;
    let taken: Vec<Space> = direct
        .iter()
        .map(|label| label.space(frame))
        .chain(words.iter().map(|x| {
            let half = text_width(GAP_WORDS, frame) / 2;
            (
                (x - half, x + half),
                ((words_y - frame.text_rise) * 10, words_y * 10 + TEXT_DROP),
            )
        }))
        .collect();
    let scale: Vec<(i64, i32, String)> = ticks(min, max, frame.ticks)
        .into_iter()
        .filter_map(|tick| {
            let y = y_tenths(tick, min, max, frame.top, plot_height)?;
            Some((tick, y, grouped(tick, currency)))
        })
        .collect();
    html! {
        svg class=(frame.class) viewBox={ "0 0 " (frame.width) " " (frame.height) }
            width=(frame.width) height=(frame.height) role="img" aria-label=(name) focusable="false" {
            @for (from, to) in &bands {
                rect.chart-gap x=(px(*from)) y=(frame.top) width=(px(to - from)) height=(plot_height) {}
            }
            @for x in &words {
                text.chart-label x=(px(*x)) y=(words_y) text-anchor="middle" { (GAP_WORDS) }
            }
            @for (tick, y, _) in &scale {
                line class=(if *tick == 0 && min < 0 && max > 0 { "chart-zero" } else { "chart-grid" })
                    x1=(frame.left) x2=(frame.right) y1=(px(*y)) y2=(px(*y)) {}
            }
            @if !path.is_empty() {
                path.chart-line d=(path) {}
            }
            @for (_, y, text) in &scale {
                @if let Some((x, anchor)) = tick_label_place(*y, text, &placed, &taken, frame) {
                    text.chart-label.chart-y x=(x) y=(px(y - 40)) text-anchor=(anchor) { (text) }
                }
            }
            @for mark in marks(&placed) {
                circle.chart-mark cx=(px(mark.x)) cy=(px(mark.y)) r="5" {}
            }
            @if let Some(label) = &direct {
                text.chart-label.chart-direct x=(px(label.x)) y=(px(label.y)) text-anchor=(label.anchor) {
                    (label.text)
                }
            } @else {
                text.chart-label x=(i32::midpoint(frame.left, frame.right)) y=(frame.top + plot_height / 2)
                    text-anchor="middle" { "No total to draw: the table says why." }
            }
            @for (position, (index, text)) in picks.iter().enumerate() {
                text.chart-label.chart-x x=(px(placed[*index].0)) y=(frame.x_labels_at)
                    text-anchor=(x_anchor(position, picks.len())) { (text) }
            }
            @for ((x, width), point) in hit_bands(&placed, frame).into_iter().zip(points) {
                rect.chart-hit x=(px(x)) y=(frame.top) width=(px(width)) height=(plot_height) {
                    title { (point.title) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
