//! A small line drawn in whole numbers: how a series moved, with no axis. The page says
//! the numbers in words beside it, and its label says what it shows.

use maud::{Markup, html};

/// The drawing's width and height, in its own units.
const WIDTH: usize = 120;
const HEIGHT: i128 = 32;
/// Room left above and below the line, so its stroke isn't cut.
const MARGIN: i128 = 2;

/// `values` as a line across the box, oldest first, labelled `label`. Fewer than two
/// values draw nothing.
pub(super) fn sparkline(values: &[i64], label: &str) -> Markup {
    html! {
        @if let Some(points) = points(values) {
            svg.sparkline viewBox={ "0 0 " (WIDTH) " " (HEIGHT) } role="img" aria-label=(label) {
                polyline points=(points) {}
            }
        }
    }
}

/// The polyline's points: x spread evenly across the width, y the value scaled from the
/// smallest (bottom) to the largest (top) within the margins, a flat series across the
/// middle. Worked in `i128`, so no pair of `i64` values overflows.
fn points(values: &[i64]) -> Option<String> {
    let last = values.len().checked_sub(1).filter(|last| *last > 0)?;
    let min = i128::from(*values.iter().min()?);
    let max = i128::from(*values.iter().max()?);
    let span = max - min;
    let low = HEIGHT - MARGIN;
    let reach = HEIGHT - 2 * MARGIN;
    let mut points = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let x = index * WIDTH / last;
        let y = if span == 0 {
            HEIGHT / 2
        } else {
            low - (i128::from(*value) - min) * reach / span
        };
        points.push(format!("{x},{y}"));
    }
    Some(points.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{points, sparkline};

    /// The y of every point.
    fn ys(values: &[i64]) -> Vec<i128> {
        points(values)
            .unwrap()
            .split(' ')
            .map(|point| point.split_once(',').unwrap().1.parse().unwrap())
            .collect()
    }

    #[test]
    fn points_fit_the_box() {
        assert_eq!(points(&[0, 50, 100]).unwrap(), "0,30 60,16 120,2");
    }

    #[test]
    fn a_flat_series_is_a_flat_line() {
        assert_eq!(ys(&[7, 7, 7]), [16, 16, 16]);
    }

    #[test]
    fn one_value_draws_nothing() {
        assert_eq!(points(&[42]), None);
        assert_eq!(points(&[]), None);
        assert_eq!(sparkline(&[42], "Net worth").into_string(), "");
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        assert_eq!(ys(&[i64::MIN, i64::MAX]), [30, 2]);
        assert_eq!(ys(&[i64::MAX, 0, i64::MIN]), [2, 16, 30]);
    }

    #[test]
    fn the_drawing_says_what_it_shows() {
        let drawn = sparkline(&[1, 2], "Net worth, last 12 months").into_string();
        assert!(drawn.starts_with("<svg class=\"sparkline\" viewBox=\"0 0 120 32\" role=\"img\""));
        assert!(drawn.contains("aria-label=\"Net worth, last 12 months\""));
        assert!(drawn.contains("<polyline points=\"0,30 120,2\"></polyline>"));
    }
}
