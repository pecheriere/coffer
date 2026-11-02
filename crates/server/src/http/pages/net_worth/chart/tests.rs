use coffer_domain::money::Currency;
use jiff::ToSpan as _;
use jiff::civil::{Date, date};

use super::{ChartPoint, ChartSize, NARROW, WIDE, line_chart, ticks, y_tenths};

const START: Date = date(2026, 1, 1);

fn usd() -> Currency {
    Currency::parse("USD").unwrap()
}

/// A period per value, one day apart.
fn points(values: &[Option<i64>]) -> Vec<ChartPoint> {
    values
        .iter()
        .zip(0_i64..)
        .map(|(value, offset)| ChartPoint {
            on: START.checked_add(offset.days()).unwrap(),
            value: *value,
            label: value.map_or_else(|| "no total".to_owned(), |value| format!("{value} USD")),
            title: format!("day {offset}"),
        })
        .collect()
}

fn drawn(values: &[Option<i64>]) -> String {
    line_chart(&points(values), usd(), ChartSize::Wide).into_string()
}

/// The values of attribute `name` on each element with this class.
fn attributes<'a>(svg: &'a str, class: &str, name: &str) -> Vec<&'a str> {
    let wanted = format!("{name}=\"");
    svg.split('<')
        .filter(|element| {
            element
                .split_once("class=\"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .is_some_and(|(classes, _)| classes.split(' ').any(|one| one == class))
        })
        .filter_map(|element| {
            let (_, rest) = element.split_once(wanted.as_str())?;
            rest.split_once('"').map(|(value, _)| value)
        })
        .collect()
}

/// The `d` of the line, which must be one element; empty with no line.
fn line_of(svg: &str) -> String {
    let found = attributes(svg, "chart-line", "d");
    assert!(found.len() <= 1, "one path holds the line");
    found.first().copied().unwrap_or_default().to_owned()
}

/// Each point of a path, in pixels, read as written.
fn path_points(path: &str) -> Vec<(f64, f64)> {
    let numbers: Vec<f64> = path
        .split([' ', 'M', 'L'])
        .filter(|word| !word.is_empty())
        .map(|word| word.parse().unwrap())
        .collect();
    numbers.chunks(2).map(|pair| (pair[0], pair[1])).collect()
}

#[test]
fn coordinates_are_exact() {
    assert_eq!(y_tenths(0, 0, 100, 0, 200), Some(2000));
    assert_eq!(y_tenths(50, 0, 100, 0, 200), Some(1000));
    assert_eq!(y_tenths(100, 0, 100, 0, 200), Some(0));
    assert_eq!(y_tenths(0, 0, 100, 24, 200), Some(2240), "plus top");
    assert_eq!(y_tenths(100, 0, 100, 24, 200), Some(240));
}

#[test]
fn a_flat_series_draws_in_the_middle() {
    assert_eq!(y_tenths(7, 7, 7, 24, 200), Some(1240));
    let path = line_of(&drawn(&[Some(7), Some(7), Some(7)]));
    let middle = f64::from(WIDE.top + WIDE.bottom) / 2.0;
    let drawn = path_points(&path);
    assert_eq!(drawn.len(), 3);
    assert!(drawn.iter().all(|(_, y)| (y - middle).abs() < 0.01));
}

#[test]
fn a_gap_breaks_the_line() {
    let svg = drawn(&[Some(1), Some(2), None, Some(3), Some(4)]);
    let path = line_of(&svg);
    assert_eq!(path.matches('M').count(), 2);
    assert_eq!(path.matches('L').count(), 2, "nothing crosses the gap");
    let across: Vec<f64> = path_points(&path).iter().map(|(x, _)| *x).collect();
    assert_eq!(across.len(), 4, "the period with no value isn't a point");
    assert_eq!(
        attributes(&svg, "chart-gap", "x").len(),
        1,
        "the gap is shown"
    );
}

/// A value next to a gap has no neighbour, so a line alone wouldn't show it at all.
#[test]
fn a_value_between_two_gaps_draws_a_marker() {
    let svg = drawn(&[Some(1), Some(2), None, Some(3), None, Some(4), Some(5)]);
    assert_eq!(line_of(&svg).matches('M').count(), 2);
    assert_eq!(
        attributes(&svg, "chart-mark", "cx").len(),
        2,
        "the lone value and the latest"
    );
}

#[test]
fn a_lone_point_draws_a_marker() {
    let svg = drawn(&[Some(5)]);
    assert_eq!(line_of(&svg), "", "no line");
    let marks = attributes(&svg, "chart-mark", "r");
    assert_eq!(marks.len(), 1);
    assert!(
        marks[0].parse::<i32>().unwrap() >= 4,
        "at least 8 px across"
    );
}

#[test]
fn only_the_latest_value_is_labelled() {
    let svg = drawn(&[Some(1), Some(2), Some(3), None]);
    let labelled = svg.split("chart-direct").count() - 1;
    assert_eq!(labelled, 1);
    assert!(svg.contains(">3 USD</text>"), "the latest value");
    assert!(!svg.contains(">2 USD</text>"));
}

#[test]
fn all_null_draws_no_line_and_says_so() {
    let svg = drawn(&[None, None]);
    assert_eq!(line_of(&svg), "");
    assert_eq!(attributes(&svg, "chart-mark", "r").len(), 0, "no marker");
    assert_eq!(attributes(&svg, "chart-grid", "y1").len(), 0, "no scale");
    assert!(svg.contains("No total to draw"));
}

#[test]
fn largest_values_cannot_overflow() {
    let svg = drawn(&[Some(i64::MIN), Some(0), Some(i64::MAX)]);
    let drawn = path_points(&line_of(&svg));
    assert_eq!(drawn.len(), 3);
    assert_eq!(drawn[0].1, f64::from(WIDE.bottom));
    assert_eq!(drawn[2].1, f64::from(WIDE.top));
    assert!(
        attributes(&svg, "chart-y", "y").len() >= 2,
        "the scale is still marked"
    );
}

#[test]
fn ticks_are_round() {
    for exponent in 0..=18 {
        let span = 10_i64.pow(exponent);
        for low in [0, 3, -span / 2, 5 * span] {
            for limit in [NARROW.ticks, WIDE.ticks] {
                let found = ticks(low, low + span, limit);
                assert!(!found.is_empty(), "a span has a tick");
                assert!(
                    i128::try_from(found.len()).unwrap() <= limit,
                    "at most the limit"
                );
                let step = found.get(1).map_or(1, |second| second - found[0]);
                let mut mantissa = step;
                while mantissa % 10 == 0 {
                    mantissa /= 10;
                }
                assert!(matches!(mantissa, 1 | 2 | 5), "1, 2 or 5 x 10^k");
                assert!(found.iter().all(|tick| tick % step == 0), "a multiple");
                assert!(found.iter().all(|tick| (low..=low + span).contains(tick)));
            }
        }
    }
    const {
        assert!(WIDE.ticks <= 5);
        assert!(NARROW.ticks < WIDE.ticks, "fewer on a phone");
    }
}

#[test]
fn zero_line_only_when_crossing() {
    let zero = |values: &[Option<i64>]| attributes(&drawn(values), "chart-zero", "y1").len();
    assert_eq!(zero(&[Some(-100), Some(100)]), 1);
    assert_eq!(zero(&[Some(100), Some(200)]), 0);
    assert_eq!(zero(&[Some(-200), Some(-100)]), 0);
    assert_eq!(zero(&[Some(0), Some(100)]), 0, "touching isn't crossing");
    assert_eq!(zero(&[Some(0), Some(0)]), 0);
}

#[test]
fn negative_net_worth_stays_in_the_plot() {
    let svg = drawn(&[Some(-500), Some(-100), Some(-300)]);
    let drawn = path_points(&line_of(&svg));
    assert_eq!(drawn[0].1, f64::from(WIDE.bottom), "the lowest at the foot");
    assert_eq!(drawn[1].1, f64::from(WIDE.top), "the highest at the head");
    assert!(
        drawn
            .iter()
            .all(|(_, y)| (f64::from(WIDE.top)..=f64::from(WIDE.bottom)).contains(y))
    );
}

#[test]
fn at_most_six_x_labels() {
    for count in [1, 2, 3, 7, 53, 92, 683] {
        let values = vec![Some(1); count];
        for (size, most) in [(ChartSize::Wide, 6), (ChartSize::Narrow, 4)] {
            let svg = line_chart(&points(&values), usd(), size).into_string();
            let labels = attributes(&svg, "chart-x", "x");
            assert!((1..=most).contains(&labels.len()), "{count} points");
        }
    }
}

/// Any hover target is big enough to hit, and there's one per period.
#[test]
fn each_period_has_a_hit_band_with_its_title() {
    for count in [1, 2, 92, 683] {
        let values = vec![Some(1); count];
        let svg = drawn(&values);
        let widths = attributes(&svg, "chart-hit", "width");
        assert_eq!(widths.len(), count);
        assert!(
            widths
                .iter()
                .all(|width| width.parse::<f64>().unwrap() >= 24.0)
        );
        let lefts = attributes(&svg, "chart-hit", "x");
        for (left, width) in lefts.iter().zip(&widths) {
            let right = left.parse::<f64>().unwrap() + width.parse::<f64>().unwrap();
            assert!(right <= f64::from(WIDE.width), "stays in the drawing");
        }
        assert_eq!(svg.matches("<title>").count(), count);
    }
}

#[test]
fn titles_are_escaped() {
    let mut all = points(&[Some(1)]);
    all[0].title = "<script>&".to_owned();
    all[0].label = "<b>".to_owned();
    let svg = line_chart(&all, usd(), ChartSize::Wide).into_string();
    assert!(svg.contains("<title>&lt;script&gt;&amp;</title>"));
    assert!(svg.contains("&lt;b&gt;"));
    assert!(!svg.contains("<b>"));
}

#[test]
fn no_style_colour_or_script() {
    for size in [ChartSize::Wide, ChartSize::Narrow] {
        let values = [Some(-100), Some(100), None, Some(50), Some(70)];
        let svg = line_chart(&points(&values), usd(), size).into_string();
        assert!(!svg.contains(" style="), "no inline style");
        assert!(!svg.contains('#'), "no colour literal");
        assert!(!svg.contains("<script"), "no script");
        assert!(!svg.contains("href"), "no reference");
        assert!(!svg.contains(" fill="), "colour comes from the classes");
        assert!(!svg.contains(" stroke="), "colour comes from the classes");
    }
}

#[test]
fn each_drawing_has_a_name_with_its_range_and_ends() {
    let svg = drawn(&[Some(1), None, Some(3)]);
    assert!(svg.contains("role=\"img\""));
    let names = svg.split("aria-label=\"").nth(1).unwrap();
    assert!(
        names.starts_with(
            "Net worth from 1 January 2026 to 3 January 2026: first 1 USD, last 3 USD."
        )
    );
}

#[test]
fn the_two_drawings_are_their_own_size() {
    let wide = drawn(&[Some(1), Some(2)]);
    let narrow = line_chart(&points(&[Some(1), Some(2)]), usd(), ChartSize::Narrow).into_string();
    assert!(wide.contains("viewBox=\"0 0 720 280\""));
    assert!(wide.contains("chart-wide"));
    assert!(narrow.contains("viewBox=\"0 0 360 240\""));
    assert!(narrow.contains("chart-narrow"));
}

/// The line drops just before its end: above the last value it would run through the
/// label, so the label goes below.
#[test]
fn the_direct_label_keeps_off_the_line() {
    let mut values = vec![Some(0)];
    values.resize(17, Some(100));
    values.extend([Some(200), Some(100), Some(100)]);
    let svg = drawn(&values);
    let label: f64 = attributes(&svg, "chart-direct", "y")[0].parse().unwrap();
    let mark: f64 = attributes(&svg, "chart-mark", "cy")[0].parse().unwrap();
    assert!(label > mark, "below the latest value");

    let rising = drawn(&[Some(0), Some(50), Some(100)]);
    let label: f64 = attributes(&rising, "chart-direct", "y")[0].parse().unwrap();
    let mark: f64 = attributes(&rising, "chart-mark", "cy")[0].parse().unwrap();
    assert!(label < mark, "above it when that's clear");
}

/// With the line both above and below where the label would go, it goes over the line.
#[test]
fn the_direct_label_clears_a_line_on_both_sides() {
    let mut values = vec![Some(50); 96];
    values.extend([Some(0), Some(200), Some(100), Some(100)]);
    let svg = drawn(&values);
    let label: f64 = attributes(&svg, "chart-direct", "y")[0].parse().unwrap();
    assert!(label < f64::from(WIDE.top), "above the line's top");
    assert!(label >= 12.0, "and still in the drawing");
}

/// A label the plot's first tick would sit under gives way, so the two never collide.
#[test]
fn a_tick_gives_way_to_the_direct_label() {
    let mut values = vec![Some(0), Some(100)];
    values.resize(40, None);
    let svg = drawn(&values);
    let ys = attributes(&svg, "chart-y", "y");
    assert!(!ys.is_empty(), "other ticks stay");
    let direct = attributes(&svg, "chart-direct", "y");
    let direct_y: f64 = direct[0].parse().unwrap();
    assert!(
        ys.iter()
            .all(|y| (y.parse::<f64>().unwrap() - direct_y).abs() >= 16.0)
    );
}
