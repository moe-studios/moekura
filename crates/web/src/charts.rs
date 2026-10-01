//! Bar charts, drawn as SVG by `bar_chart.html` (sizes go in attributes,
//! since the CSP forbids inline styles).

use minijinja::{Value, context};

/// The chart's size in SVG units; it's scaled to fit its column.
const WIDTH: i64 = 600;
const BAR_MAX: i64 = 100;
/// Room under the bars for their labels.
const LABEL_ROOM: i64 = 16;

/// One bar: its label under it (may be empty), the text shown on hover,
/// and its value.
pub struct Bar {
    pub label: String,
    pub title: String,
    pub value: i64,
}

/// A chart of `bars`, or `None` when they're all zero.
pub fn bars(bars: &[Bar]) -> Option<Value> {
    let most = bars.iter().map(|b| b.value).max().filter(|&n| n > 0)?;
    let count = i64::try_from(bars.len()).ok()?;
    let slot = WIDTH / count;
    // A quarter of each slot is the gap, unless that's under a unit.
    let gap = (slot / 4).max(i64::from(slot > 1));
    let width = slot - gap;
    let drawn: Vec<Value> = bars
        .iter()
        .zip(0..)
        .map(|(bar, i)| {
            // Anything at all shows as at least a sliver.
            let height = if bar.value == 0 {
                0
            } else {
                (bar.value * BAR_MAX / most).max(1)
            };
            let x = i * slot;
            context! {
                x => x,
                y => BAR_MAX - height,
                height => height,
                label_x => x + width / 2,
                label => bar.label,
                title => bar.title,
            }
        })
        .collect();
    Some(context! {
        bars => drawn,
        width => slot * count - gap,
        height => BAR_MAX + LABEL_ROOM,
        bar_width => width,
        label_y => BAR_MAX + LABEL_ROOM - 3,
        most => most,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(value: i64) -> Bar {
        Bar {
            label: String::new(),
            title: String::new(),
            value,
        }
    }

    #[test]
    fn scales_to_the_largest() {
        assert!(bars(&[bar(0), bar(0)]).is_none());
        let chart = bars(&[bar(0), bar(1), bar(1000)]).unwrap();
        let drawn = chart.get_attr("bars").unwrap();
        let height = |i: usize| {
            drawn
                .get_item(&Value::from(i))
                .unwrap()
                .get_attr("height")
                .unwrap()
        };
        assert_eq!(height(0), Value::from(0));
        assert_eq!(height(1), Value::from(1));
        assert_eq!(height(2), Value::from(BAR_MAX));
        assert_eq!(chart.get_attr("bar_width").unwrap(), Value::from(150));
    }
}
