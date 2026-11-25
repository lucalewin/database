use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme as _, Size, StyleSized as _, table::Column};
use gpui_kit::{App, StyleRefinement, TextStyle, Window, px};

use crate::database::table::TablePreview;

pub(crate) const PREVIEW_TABLE_SIZE: Size = Size::XSmall;
const MIN_AUTO_WIDTH: f32 = 60.;
const MAX_AUTO_WIDTH: f32 = 360.;
const MAX_SHAPED_BYTES: usize = 1024;

/// The text used for a single-line preview header or cell.
///
/// CRLF becomes one space; other control characters and Unicode line and
/// paragraph separators become spaces. The underlying database value is not
/// changed. Use this for cell rendering as well as measurement.
pub(crate) fn single_line_display(text: &str) -> Cow<'_, str> {
    let Some(first) =
        text.find(|ch: char| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    else {
        return Cow::Borrowed(text);
    };

    let mut result = String::with_capacity(text.len());
    result.push_str(&text[..first]);
    let mut chars = text[first..].chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
        }
        if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
            result.push(' ');
        } else {
            result.push(ch);
        }
    }
    Cow::Owned(result)
}

/// Display SQL NULL as `NULL`; a non-null value such as `null` stays intact.
pub(crate) fn preview_cell_display(value: Option<&str>) -> Cow<'_, str> {
    single_line_display(cell_text(value))
}

fn cell_text(value: Option<&str>) -> &str {
    value.unwrap_or("NULL")
}

/// Builds initial widths for the compact, unsorted preview table.
///
/// Match the preview's monospace font, text size, and table cell padding. The
/// width cap applies only here, not to `Column::max_width`, so resizing remains
/// available beyond it.
pub(crate) fn columns_for_preview(
    preview: &TablePreview,
    window: &mut Window,
    cx: &mut App,
) -> Vec<Column> {
    let table_size = PREVIEW_TABLE_SIZE;
    let font_size = StyleRefinement::default()
        .table_cell_size(table_size)
        .text
        .font_size
        .expect("the preview table size sets a font size")
        .to_pixels(cx.theme().font_size);
    let text_style = TextStyle {
        font_family: "JetBrains Mono".into(),
        ..TextStyle::default()
    };
    let padding = table_size.table_cell_padding();
    let scale = window.scale_factor();

    preview
        .columns
        .iter()
        .enumerate()
        .map(|(col_ix, name)| {
            let content_width = widest_content(preview, col_ix, |text| {
                if text.is_empty() {
                    return 0.;
                }
                let run = text_style.to_run(text.len());
                let line = window.text_system().shape_line(
                    text.to_owned().into(),
                    font_size,
                    &[run],
                    None,
                );
                f32::from(line.width())
            });
            let width = auto_width(
                content_width,
                f32::from(padding.left),
                f32::from(padding.right),
                scale,
            );
            Column::new(name.clone(), single_line_display(name).into_owned()).width(px(width + 8.))
        })
        .collect()
}

// Check the input byte length before normalizing or allocating. Extremely long
// values conservatively force the auto-size cap, even if they are mostly
// combining characters or controls; they are never passed to the shaper.
fn measured_width(text: &str, measure: &mut impl FnMut(&str) -> f32) -> f32 {
    if text.len() > MAX_SHAPED_BYTES {
        return MAX_AUTO_WIDTH;
    }
    measure(&single_line_display(text))
}

fn widest_content(
    preview: &TablePreview,
    col_ix: usize,
    mut measure: impl FnMut(&str) -> f32,
) -> f32 {
    let mut widest = measured_width(&preview.columns[col_ix], &mut measure);
    for row in &preview.rows {
        if widest >= MAX_AUTO_WIDTH {
            break;
        }
        if let Some(value) = row.get(col_ix) {
            widest = widest.max(measured_width(cell_text(value.as_deref()), &mut measure));
        }
    }
    widest
}

// Match the table's fit-column rounding: round content and each inset up in
// device pixels, including the fractional-scale division correction.
fn auto_width(content: f32, left: f32, right: f32, scale: f32) -> f32 {
    let device_pixels = |length: f32| (length * scale).ceil();
    let mut content_pixels = device_pixels(content);
    if content_pixels / scale < content {
        content_pixels += 1.;
    }
    let width = (content_pixels + device_pixels(left) + device_pixels(right)) / scale;
    width.clamp(MIN_AUTO_WIDTH, MAX_AUTO_WIDTH)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use gpui_kit::{
        StyleRefinement,
        component::{Size, StyleSized as _, table::Column},
        px,
    };

    use crate::database::table::TablePreview;
    use crate::ui::workspace::table::sizing::{
        MAX_AUTO_WIDTH, MAX_SHAPED_BYTES, MIN_AUTO_WIDTH, PREVIEW_TABLE_SIZE, auto_width,
        measured_width, preview_cell_display, single_line_display, widest_content,
    };

    #[test]
    fn display_normalizes_line_breaks_and_controls_without_changing_unicode() {
        assert!(matches!(single_line_display("café 🐈"), Cow::Borrowed(_)));
        assert_eq!(
            single_line_display("a\r\nb\rc\nd\te\u{85}f\u{2028}g\u{2029}h\0"),
            "a b c d e f g h "
        );
        assert_eq!(single_line_display("\n\n"), "  ");
    }

    #[test]
    fn cell_display_keeps_sql_and_json_null_distinct() {
        assert_eq!(preview_cell_display(None), "NULL");
        assert_eq!(preview_cell_display(Some("null")), "null");
        assert_eq!(preview_cell_display(Some("a\nb")), "a b");
        assert_eq!(preview_cell_display(Some("")), "");
    }

    #[test]
    fn measures_header_and_every_present_cell_including_null() {
        let preview = TablePreview {
            columns: vec!["header\nname".into(), "other".into()],
            rows: vec![
                vec![Some("x".into())],
                vec![None],
                vec![],
                vec![Some("最後の長い値".into())],
            ],
        };
        let mut seen = Vec::new();
        let widest = widest_content(&preview, 0, |text| {
            seen.push(text.to_owned());
            match text {
                "最後の長い値" => 100.,
                _ => 10.,
            }
        });
        assert_eq!(seen, ["header name", "x", "NULL", "最後の長い値"]);
        assert_eq!(widest, 100.);
        assert_eq!(auto_width(widest, 8., 8., 1.), 116.);
    }

    #[test]
    fn very_long_values_do_not_reach_the_shaper() {
        let at_limit = "é".repeat(MAX_SHAPED_BYTES / 2);
        let over_limit = format!("{at_limit}é");
        let mut seen = Vec::new();
        let mut measure = |text: &str| {
            seen.push(text.len());
            12.
        };
        assert_eq!(measured_width(&at_limit, &mut measure), 12.);
        assert_eq!(measured_width(&over_limit, &mut measure), MAX_AUTO_WIDTH);
        assert_eq!(seen, [MAX_SHAPED_BYTES]);
        assert_eq!(auto_width(MAX_AUTO_WIDTH, 8., 8., 1.), MAX_AUTO_WIDTH);
    }

    #[test]
    fn auto_width_includes_padding_rounds_up_and_clamps() {
        assert_eq!(auto_width(0., 8., 8., 1.), MIN_AUTO_WIDTH);
        assert_eq!(auto_width(70.2, 8., 8., 1.), 87.);
        assert_eq!(auto_width(400., 8., 8., 1.), MAX_AUTO_WIDTH);
        assert_eq!(auto_width(70.1, 8., 8., 1.25), 86.4);
        assert!(auto_width(70.1, 8., 8., 1.25) >= 70.1 + 16.);
    }

    #[test]
    fn preview_uses_compact_row_geometry_and_text() {
        assert!(PREVIEW_TABLE_SIZE.table_row_height() < Size::default().table_row_height());
        let compact = StyleRefinement::default().table_cell_size(PREVIEW_TABLE_SIZE);
        let standard = StyleRefinement::default().table_cell_size(Size::default());
        assert!(
            compact.text.font_size.unwrap().to_pixels(px(16.))
                < standard.text.font_size.unwrap().to_pixels(px(16.))
        );
    }

    #[test]
    fn initial_cap_does_not_limit_manual_resizing() {
        let column = Column::new("key", "name").width(px(MAX_AUTO_WIDTH));
        assert!(column.resizable);
        assert!(column.max_width > px(MAX_AUTO_WIDTH));
    }
}
