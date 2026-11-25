use std::rc::Rc;

use gpui_kit::{
    component::table::{Column, TableSelection},
    px,
};

use crate::ui::explorer::DatabaseTarget;
use crate::ui::workspace::TableTarget;
use crate::ui::workspace::table::{
    CellRange, WorkspaceTableDelegate, remap_selection, table_height,
};
use crate::{database::table::TablePreview, ui::workspace::table::delegate::append_csv_value};
use crate::{session::ConnectionTier, ui::workspace::table::PREVIEW_TABLE_SIZE};

#[test]
fn preview_scope_includes_connection_tier() {
    let generation = Rc::new(());
    let target = TableTarget {
        database: DatabaseTarget::new(
            "local".into(),
            "Local".into(),
            ConnectionTier::Local,
            "db".into(),
            Rc::downgrade(&generation),
        ),
        schema: "public".into(),
        name: "orders".into(),
    };
    assert!(target.matches_scope("local", "db", "public"));
    assert!(!target.matches_scope("other-local", "db", "public"));
    assert!(!target.matches_scope("production", "db", "public"));
    assert!(!target.matches_scope("local", "other", "public"));
    assert!(!target.matches_scope("local", "db", "other"));
    drop(generation);
    assert!(!target.database.is_live());
    // An already-open preview remains scoped to its connection after a tree refresh.
    assert!(target.matches_scope("local", "db", "public"));
}

#[test]
fn tabs_match_full_scope() {
    let generation = Rc::new(());
    let target = |tier: ConnectionTier, database: &str, schema: &str, name: &str| TableTarget {
        database: DatabaseTarget::new(
            tier.label().to_lowercase(),
            tier.label().into(),
            tier,
            database.into(),
            Rc::downgrade(&generation),
        ),
        schema: schema.into(),
        name: name.into(),
    };
    let first = target(ConnectionTier::Local, "app", "public", "orders");
    assert!(first.same_table(&target(ConnectionTier::Local, "app", "public", "orders")));
    assert!(!first.same_table(&target(ConnectionTier::Staging, "app", "public", "orders")));
    assert!(!first.same_table(&target(ConnectionTier::Local, "other", "public", "orders")));
    assert!(!first.same_table(&target(ConnectionTier::Local, "app", "other", "orders")));
    assert!(!first.same_table(&target(ConnectionTier::Local, "app", "public", "customers")));
}

#[test]
fn dock_tab_selection_move_and_last_close_preserve_panel_identity() {
    use gpui_kit::base::{
        Placement,
        dock::{InsertTarget, PaneRef, PaneTree, PanelId, RootKind},
    };

    let mut tree = PaneTree::new(RootKind::Split);
    let first = PanelId::from_u64(1);
    let second = PanelId::from_u64(2);
    let reopened = PanelId::from_u64(3);
    let root = tree.root().id();
    assert!(
        tree.insert_panel(
            first,
            InsertTarget::Split {
                node: root,
                placement: Placement::Right,
                size: None,
            }
        )
        .changed()
    );
    let group = tree.find_panel_node(first).unwrap();
    assert!(
        tree.insert_panel(
            second,
            InsertTarget::Tabs {
                node: group,
                ix: None,
                activate: true,
            }
        )
        .changed()
    );
    assert!(tree.set_active(group, 0).changed());
    let PaneRef::Tabs { panels, active_ix } = tree.find_node(group).unwrap().kind() else {
        panic!("expected a tab group");
    };
    assert_eq!(panels[active_ix], first);

    let moved = tree.move_panel(
        second,
        InsertTarget::Split {
            node: group,
            placement: Placement::Right,
            size: None,
        },
    );
    assert!(moved.changed());
    assert!(tree.find_panel_node(second).is_some());
    assert!(tree.remove_panel(first).changed());
    assert!(tree.remove_panel(second).changed());
    assert_eq!(tree.panels().count(), 0);
    let root = tree.root().id();
    assert!(
        tree.insert_panel(
            reopened,
            InsertTarget::Split {
                node: root,
                placement: Placement::Right,
                size: None,
            }
        )
        .changed()
    );
    assert!(tree.find_panel_node(reopened).is_some());
    assert!(tree.find_panel_node(first).is_none());
}

#[test]
fn cell_range_contains_rectangles_in_either_direction() {
    let range = CellRange {
        anchor: (4, 3),
        end: (1, 1),
    };
    assert!(range.contains(1, 1));
    assert!(range.contains(3, 2));
    assert!(range.contains(4, 3));
    assert!(!range.contains(0, 1));
    assert!(!range.contains(4, 4));

    let single_column = CellRange {
        anchor: (2, 2),
        end: (5, 2),
    };
    assert!(single_column.contains(4, 2));
    assert!(!single_column.contains(4, 1));
    assert!(!single_column.contains(6, 2));
}

#[test]
fn cell_selection_tracks_anchor_when_dragging_and_shift_clicking() {
    let preview = TablePreview {
        columns: vec!["a".into(), "b".into()],
        rows: vec![vec![None, None]; 4],
    };
    let columns = preview
        .columns
        .iter()
        .map(|name| Column::new(name.clone(), name.clone()))
        .collect();
    let mut delegate = WorkspaceTableDelegate::new(preview, columns, px(56.));
    delegate.begin_selection((2, 1), false);
    assert!(delegate.selecting);
    assert!(delegate.extend_selection((0, 2)));
    assert!(!delegate.extend_selection((0, 2)));
    assert_eq!(
        delegate.selection,
        Some(CellRange {
            anchor: (2, 1),
            end: (0, 2)
        })
    );
    delegate.selecting = false;
    delegate.begin_selection((3, 2), true);
    assert_eq!(
        delegate.selection,
        Some(CellRange {
            anchor: (2, 1),
            end: (3, 2)
        })
    );
    delegate.begin_selection((1, 2), false);
    assert_eq!(
        delegate.selection,
        Some(CellRange {
            anchor: (1, 2),
            end: (1, 2)
        })
    );
}

#[test]
fn copy_selection_formats_raw_values_as_csv_without_headers_or_row_numbers() {
    let preview = TablePreview {
        columns: vec!["first".into(), "second".into(), "third".into()],
        rows: vec![
            vec![Some("a,b".into()), Some("He said \"hi\"".into()), None],
            vec![
                Some("two\nlines".into()),
                Some(String::new()),
                Some("null".into()),
            ],
            vec![Some("last".into()), Some("λ".into()), Some("NULL".into())],
        ],
    };
    let columns = preview
        .columns
        .iter()
        .map(|name| Column::new(name.clone(), name.clone()))
        .collect();
    let mut delegate = WorkspaceTableDelegate::new(preview, columns, px(56.));
    delegate.begin_selection((1, 2), false);
    delegate.extend_selection((0, 1));
    assert_eq!(
        delegate
            .selection_csv(TableSelection::Cell(0, 1))
            .as_deref(),
        Some("\"a,b\",\"He said \"\"hi\"\"\"\n\"two\nlines\",\"\"")
    );
    assert_eq!(
        delegate
            .selection_csv(TableSelection::Cell(2, 1))
            .as_deref(),
        Some("last")
    );
    assert_eq!(
        delegate.selection_csv(TableSelection::Row(1)).as_deref(),
        Some("\"two\nlines\",\"\",null")
    );
    assert_eq!(
        delegate.selection_csv(TableSelection::Column(1)).as_deref(),
        Some("\"a,b\"\n\"two\nlines\"\nlast")
    );
    assert_eq!(
        delegate.selection_csv(TableSelection::Column(2)).as_deref(),
        Some("\"He said \"\"hi\"\"\"\n\"\"\nλ")
    );
    assert_eq!(
        delegate.selection_csv(TableSelection::Column(3)).as_deref(),
        Some("NULL\nnull\nNULL")
    );
    assert_eq!(delegate.selection_csv(TableSelection::None), None);
    assert_eq!(delegate.selection_csv(TableSelection::Column(0)), None);
    assert_eq!(delegate.selection_csv(TableSelection::Cell(3, 1)), None);
    assert_eq!(delegate.selection_csv(TableSelection::Cell(0, 4)), None);
}

#[test]
fn csv_escapes_carriage_returns_and_consecutive_quotes() {
    let mut csv = String::new();
    append_csv_value(&mut csv, "a\rb\"\"c");
    assert_eq!(csv, "\"a\rb\"\"\"\"c\"");
}

#[test]
fn copy_selection_ignores_empty_previews() {
    let empty_rows = TablePreview {
        columns: vec!["a".into()],
        rows: vec![],
    };
    let delegate = WorkspaceTableDelegate::new(empty_rows, vec![Column::new("a", "a")], px(56.));
    assert_eq!(delegate.selection_csv(TableSelection::Column(1)), None);
    let no_columns = TablePreview {
        columns: vec![],
        rows: vec![vec![]],
    };
    let delegate = WorkspaceTableDelegate::new(no_columns, vec![], px(56.));
    assert_eq!(delegate.selection_csv(TableSelection::Row(0)), None);
}

#[test]
fn column_moves_preserve_selected_data() {
    assert_eq!(
        remap_selection(TableSelection::Cell(2, 1), 1, 3),
        TableSelection::Cell(2, 3)
    );
    assert_eq!(
        remap_selection(TableSelection::Column(3), 1, 3),
        TableSelection::Column(2)
    );
    assert_eq!(
        remap_selection(TableSelection::Column(1), 3, 1),
        TableSelection::Column(2)
    );
    assert_eq!(
        remap_selection(TableSelection::Cell(2, 3), 3, 1),
        TableSelection::Cell(2, 1)
    );
    assert_eq!(
        remap_selection(TableSelection::Row(2), 2, 0),
        TableSelection::Row(2)
    );
    assert_eq!(
        remap_selection(TableSelection::None, 2, 0),
        TableSelection::None
    );
}

#[test]
fn short_tables_have_content_height_with_an_empty_state_minimum() {
    assert_eq!(table_height(0), table_height(3));
    assert!(table_height(1) < table_height(3));
    assert!(table_height(4) > table_height(3));
    assert!(table_height(500) > table_height(4));
    assert_eq!(
        table_height(4) - table_height(3),
        PREVIEW_TABLE_SIZE.table_row_height()
    );
}

#[test]
fn reordering_columns_keeps_cells_with_their_headers() {
    let preview = TablePreview {
        columns: vec!["first".into(), "second".into(), "third".into()],
        rows: vec![vec![Some("a".into()), None, Some("c".into())]],
    };
    let columns = preview
        .columns
        .iter()
        .map(|name| Column::new(name.clone(), name.clone()))
        .collect();
    let mut delegate = WorkspaceTableDelegate::new(preview, columns, px(56.));
    delegate.move_column_to(1, 3);
    assert_eq!(delegate.columns[0].key.as_ref(), "second");
    assert_eq!(delegate.columns[1].key.as_ref(), "third");
    assert_eq!(delegate.columns[2].key.as_ref(), "first");
    assert_eq!(delegate.rows[0], [None, Some("c".into()), Some("a".into())]);
    assert_eq!(delegate.cell_text_at(0, 0), "1");
    assert_eq!(delegate.cell_text_at(0, 1), "NULL");
    assert_eq!(delegate.cell_label_at(0, 1), "SQL NULL");
    assert_eq!(delegate.cell_text_at(0, 3), "a");
    delegate.move_column_to(0, 1);
    delegate.move_column_to(1, 0);
    delegate.move_column_to(1, 4);
    assert_eq!(delegate.rows[0], [None, Some("c".into()), Some("a".into())]);
    assert_eq!(delegate.column_at(0).name.as_ref(), "#");
}

#[test]
fn row_numbers_are_preview_positions_and_do_not_collide_with_column_keys() {
    let preview = TablePreview {
        columns: vec!["__preview_row_number".into()],
        rows: (0..500).map(|ix| vec![Some(ix.to_string())]).collect(),
    };
    let columns = preview
        .columns
        .iter()
        .map(|name| Column::new(name.clone(), name.clone()))
        .collect();
    let delegate = WorkspaceTableDelegate::new(preview, columns, px(56.));
    assert_ne!(delegate.column_at(0).key, delegate.column_at(1).key);
    assert!(!delegate.column_at(0).movable);
    assert!(!delegate.column_at(0).resizable);
    assert!(delegate.column_at(0).fixed.is_some());
    assert_eq!(delegate.cell_text_at(0, 0), "1");
    assert_eq!(delegate.cell_label_at(0, 0), "Row 1");
    assert_eq!(delegate.cell_text_at(499, 0), "500");
    assert_eq!(delegate.cell_text_at(499, 1), "499");

    let empty = WorkspaceTableDelegate::new(
        TablePreview {
            columns: vec![],
            rows: vec![],
        },
        vec![],
        px(56.),
    );
    assert!(empty.rows.is_empty());
    assert_eq!(empty.column_at(0).name.as_ref(), "#");
}
