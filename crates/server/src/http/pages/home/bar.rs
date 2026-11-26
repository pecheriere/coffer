//! The money bar: what cash is spoken for and what is free, drawn side by side in whole
//! numbers. It is a picture only; the legend beside it says every part in words with its
//! amount, so the drawing is hidden from assistive technology.

use maud::{Markup, html};

/// The drawing's width and height, in its own units.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 16;

/// One part of the bar: its amount in minor units and its colour class.
pub(super) struct Part {
    pub minor: i64,
    pub class: &'static str,
}

/// Whole-unit widths for `parts` in a box `width` wide, scaled to the larger of their sum and
/// `whole` (cash available, or what is spoken for when that is more). The widths add up to the
/// share of the box the parts fill, the largest remainders getting the spare units first. A
/// part of nothing, or less, gets 0.
pub(super) fn widths(parts: &[Part], whole: i64, width: u32) -> Vec<u32> {
    let amounts: Vec<i128> = parts
        .iter()
        .map(|part| i128::from(part.minor.max(0)))
        .collect();
    let total: i128 = amounts.iter().sum();
    let scale = total.max(i128::from(whole));
    if total == 0 || scale <= 0 {
        return vec![0; parts.len()];
    }
    let width = i128::from(width);
    let target = total * width / scale;
    let mut floors: Vec<i128> = amounts
        .iter()
        .map(|amount| amount * width / scale)
        .collect();
    let mut spare = target - floors.iter().sum::<i128>();
    let mut by_remainder: Vec<(usize, i128)> = amounts
        .iter()
        .enumerate()
        .map(|(index, amount)| (index, amount * width % scale))
        .collect();
    // Largest remainder first; a tie goes to the earlier part.
    by_remainder.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    for (index, remainder) in by_remainder {
        if spare == 0 {
            break;
        }
        if remainder > 0 {
            floors[index] += 1;
            spare -= 1;
        }
    }
    floors
        .into_iter()
        .map(|floor| u32::try_from(floor).unwrap_or(0))
        .collect()
}

/// The bar: one rectangle per part with a width, in order from the left, coloured by class.
pub(super) fn bar(parts: &[Part], whole: i64) -> Markup {
    let widths = widths(parts, whole, WIDTH);
    let mut left = 0;
    let mut rects = Vec::with_capacity(parts.len());
    for (part, width) in parts.iter().zip(widths) {
        if width > 0 {
            rects.push((left, width, part.class));
            left += width;
        }
    }
    html! {
        svg.money-bar viewBox={ "0 0 " (WIDTH) " " (HEIGHT) } aria-hidden="true" {
            @for (x, width, class) in rects {
                rect class=(class) x=(x) y="0" width=(width) height=(HEIGHT) {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Part, bar, widths};

    fn parts(minors: &[i64]) -> Vec<Part> {
        minors
            .iter()
            .map(|minor| Part {
                minor: *minor,
                class: "tier-must-pay",
            })
            .collect()
    }

    #[test]
    fn widths_are_whole_and_sum_to_the_box() {
        let got = widths(&parts(&[1, 10_000, 4_999]), 15_000, 320);
        assert_eq!(got.iter().sum::<u32>(), 320);
        assert_eq!(got[0], 0, "a cent is too thin to draw");
        assert_eq!(got, vec![0, 213, 107]);
    }

    #[test]
    fn a_shortfall_draws_within_the_box() {
        let got = widths(&parts(&[20_000, 10_000]), 15_000, 320);
        assert_eq!(got.iter().sum::<u32>(), 320);
    }

    #[test]
    fn parts_short_of_the_whole_leave_the_rest_empty() {
        let got = widths(&parts(&[5_000, 5_000]), 20_000, 320);
        assert_eq!(got, vec![80, 80]);
    }

    #[test]
    fn zero_and_negative_parts_get_nothing() {
        assert_eq!(widths(&parts(&[-5, 0, 100]), 100, 320), vec![0, 0, 320]);
        assert_eq!(widths(&parts(&[0, -1]), 0, 320), vec![0, 0]);
        assert_eq!(widths(&parts(&[]), 10, 320), Vec::<u32>::new());
    }

    #[test]
    fn huge_amounts_do_not_overflow() {
        let half = i64::MAX / 2;
        let got = widths(&parts(&[half, half]), i64::MAX, 320);
        assert_eq!(got.iter().sum::<u32>(), 319);
        assert!(got.iter().all(|width| *width <= 320));
    }

    #[test]
    fn bar_uses_classes_only() {
        let markup = bar(&parts(&[100, 200]), 300).into_string();
        assert!(!markup.contains("style"), "{markup}");
        assert!(!markup.contains('#'), "{markup}");
        assert!(markup.contains("class=\"tier-must-pay\""));
        assert!(markup.contains("aria-hidden=\"true\""));
        assert_eq!(markup.matches("<rect").count(), 2);
    }

    #[test]
    fn rectangles_sit_side_by_side() {
        let markup = bar(&parts(&[1, 10_000, 10_000]), 20_001).into_string();
        assert_eq!(
            markup.matches("<rect").count(),
            2,
            "the thin part is skipped"
        );
        assert!(markup.contains("x=\"0\""), "{markup}");
    }
}
