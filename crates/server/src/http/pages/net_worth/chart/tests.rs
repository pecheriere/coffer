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
    assert!(
        svg.contains(">3 USD, 3 Jan 2026</text>"),
        "the latest value, dated"
    );
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

/// Only a value that isn't the last period needs a date: a gap narrower than its words
/// doesn't say so.
#[test]
fn the_label_of_the_last_period_has_no_date() {
    let svg = drawn(&[Some(1), Some(3)]);
    assert!(svg.contains(">3 USD</text>"));
}

/// The gap runs from the last value before it to the first after it, or to the last period.
#[test]
fn a_gap_band_spans_its_neighbours() {
    let band = |values: &[Option<i64>]| {
        let svg = drawn(values);
        (
            attributes(&svg, "chart-gap", "x")[0].to_owned(),
            attributes(&svg, "chart-gap", "width")[0].to_owned(),
        )
    };
    assert_eq!(
        band(&[Some(1), Some(2), None, Some(3)]),
        ("242.6".to_owned(), "469.4".to_owned()),
        "in the middle"
    );
    assert_eq!(
        band(&[Some(1), Some(2), None]),
        ("360.0".to_owned(), "352.0".to_owned()),
        "at the end"
    );
    assert_eq!(
        band(&[None, Some(1), Some(2)]),
        ("8.0".to_owned(), "352.0".to_owned()),
        "at the start"
    );
}

#[test]
fn a_gap_says_what_it_is_when_there_is_room() {
    let wide = drawn(&[Some(1), Some(2), None, Some(3)]);
    assert!(wide.contains(">no total</text>"));

    let mut values = vec![Some(1); 100];
    values[50] = None;
    let narrow = drawn(&values);
    assert_eq!(attributes(&narrow, "chart-gap", "x").len(), 1);
    assert!(!narrow.contains(">no total</text>"), "too narrow for words");
}

/// Each tick's label, read as `(text, x, y, anchor)`.
fn tick_labels(svg: &str) -> Vec<(String, f64, f64, String)> {
    svg.split("<text ")
        .skip(1)
        .filter(|element| element.contains("chart-y"))
        .map(|element| {
            let attribute = |name: &str| {
                element
                    .split_once(&format!(" {name}=\""))
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(value, _)| value.to_owned())
            };
            let text = element
                .split_once('>')
                .unwrap()
                .1
                .split_once('<')
                .unwrap()
                .0;
            (
                text.to_owned(),
                attribute("x").unwrap().parse().unwrap(),
                attribute("y").unwrap().parse().unwrap(),
                attribute("text-anchor").unwrap_or_else(|| "start".to_owned()),
            )
        })
        .collect()
}

/// The labels name the values at the height they're drawn: a rising line from 0.00 to
/// 1,000.00 on a plot from 252 up to 30.
#[test]
fn ticks_say_their_value_at_their_height() {
    let svg = drawn(&[Some(0), Some(100_000)]);

    let grid: Vec<&str> = attributes(&svg, "chart-grid", "y1");
    assert_eq!(grid, ["252.0", "141.0", "30.0"]);
    assert_eq!(
        tick_labels(&svg),
        [
            ("0.00".to_owned(), 712.0, 248.0, "end".to_owned()),
            ("500.00".to_owned(), 8.0, 137.0, "start".to_owned()),
            ("1,000.00".to_owned(), 8.0, 26.0, "start".to_owned()),
        ]
    );
}

/// The y of the line at `x`, on the segment holding it.
fn line_y_at(line: &[(f64, f64)], x: f64) -> Option<f64> {
    line.windows(2).find_map(|pair| {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        (x0 <= x && x <= x1).then(|| y0 + (y1 - y0) * (x - x0) / (x1 - x0).max(0.001))
    })
}

/// No tick's label sits on the line, whatever its shape: a stub of line beside a label
/// reads as a minus sign.
#[test]
fn tick_labels_keep_off_the_line() {
    let rising: Vec<Option<i64>> = (0..40).map(|at| Some(at * 1_000)).collect();
    let falling: Vec<Option<i64>> = (0..40).rev().map(|at| Some(at * 1_000)).collect();
    let valley: Vec<Option<i64>> = (0_i64..40)
        .map(|at| Some((at - 20).abs() * 1_000))
        .collect();
    let crossing: Vec<Option<i64>> = (0..40).map(|at| Some((at - 15) * 1_000)).collect();
    for values in [&rising, &falling, &valley, &crossing] {
        for size in [ChartSize::Wide, ChartSize::Narrow] {
            let frame = size.frame();
            let svg = line_chart(&points(values), usd(), size).into_string();
            let line = path_points(&line_of(&svg));
            let labels = tick_labels(&svg);
            assert!(!labels.is_empty(), "the scale is marked");
            for (text, x, y, anchor) in labels {
                let width = f64::from(i32::try_from(text.len()).unwrap() * frame.char_width);
                let (left, right) = if anchor == "end" {
                    (x - width, x)
                } else {
                    (x, x + width)
                };
                let mut at = left;
                while at <= right {
                    if let Some(line_y) = line_y_at(&line, at) {
                        assert!(
                            line_y < y - 14.0 || line_y > y + 3.0,
                            "the line runs through a label"
                        );
                    }
                    at += 1.0;
                }
            }
        }
    }
}

/// On a dense range bands overlap, and the last drawn wins: each must win over its own
/// point, not a later one's.
#[test]
fn the_band_under_a_point_is_that_points() {
    let values = vec![Some(1); 92];
    let svg = drawn(&values);
    let lefts: Vec<f64> = attributes(&svg, "chart-hit", "x")
        .iter()
        .map(|x| x.parse().unwrap())
        .collect();
    let widths: Vec<f64> = attributes(&svg, "chart-hit", "width")
        .iter()
        .map(|x| x.parse().unwrap())
        .collect();
    let crosses: Vec<f64> = (0_i32..92)
        .map(|at| f64::from(WIDE.left) + f64::from(at) * 704.0 / 91.0)
        .collect();
    for (index, x) in crosses.iter().enumerate().filter(|(_, x)| **x < 690.0) {
        let winner = lefts
            .iter()
            .zip(&widths)
            .rposition(|(left, width)| left <= x && *x < left + width);
        assert_eq!(winner, Some(index), "the band over a point names it");
    }
}

/// How much of the date a label needs depends on how long the chart runs.
#[test]
fn date_labels_say_as_much_as_the_span_needs() {
    let labels = |last: Date| {
        let ends = [
            ChartPoint {
                on: START,
                value: Some(1),
                label: String::new(),
                title: String::new(),
            },
            ChartPoint {
                on: last,
                value: Some(2),
                label: String::new(),
                title: String::new(),
            },
        ];
        let svg = line_chart(&ends, usd(), ChartSize::Wide).into_string();
        svg.split("chart-x")
            .skip(1)
            .map(|element| {
                element
                    .split_once('>')
                    .unwrap()
                    .1
                    .split_once('<')
                    .unwrap()
                    .0
                    .to_owned()
            })
            .collect::<Vec<String>>()
    };
    assert_eq!(labels(date(2026, 3, 1)), ["1 Jan", "1 Mar"]);
    assert_eq!(labels(date(2027, 1, 1)), ["Jan 2026", "Jan 2027"]);
    assert_eq!(labels(date(2035, 1, 1)), ["2026", "2035"]);
}

/// The line holds the left of this tick's height, and a gap's words the right.
#[test]
fn a_tick_keeps_off_a_gaps_words() {
    let middle = Some(55_000);
    let svg = drawn(&[
        middle,
        middle,
        middle,
        Some(0),
        Some(100_000),
        middle,
        middle,
        middle,
        middle,
        None,
    ]);

    assert!(svg.contains(">no total</text>"));
    let labels: Vec<String> = tick_labels(&svg).into_iter().map(|label| label.0).collect();
    assert_eq!(labels, ["0.00", "1,000.00"], "no room for the middle one");
}

/// A label that would sit under the direct label goes elsewhere, so the two never collide.
#[test]
fn a_tick_gives_way_to_the_direct_label() {
    let mut values = vec![Some(0), Some(100)];
    values.resize(40, None);
    let svg = drawn(&values);
    let labels = tick_labels(&svg);
    assert!(!labels.is_empty(), "ticks stay");
    let direct_y: f64 = attributes(&svg, "chart-direct", "y")[0].parse().unwrap();
    let direct_x: f64 = attributes(&svg, "chart-direct", "x")[0].parse().unwrap();
    for (_, x, y, anchor) in labels {
        let near_left = anchor == "start" && (x - direct_x).abs() < 80.0;
        assert!(!near_left || (y - direct_y).abs() >= 16.0, "they'd collide");
    }
}
