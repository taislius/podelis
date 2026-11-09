//! The query screen: the navigator on the left, the editor above the
//! results on the right (each pane drawn by its file: `editor`, `results`,
//! `navigator`, as `app::query_screen` keeps their state), plus its popups
//! (`popups`).

mod editor;
mod navigator;
mod popups;
pub(in crate::ui) mod results;

use super::*;
use editor::draw_editor;
use navigator::draw_schema;
use results::draw_results;

/// The navigator's width: as resized (up to half the screen), else a
/// quarter of it within limits.
pub(crate) fn schema_width(resized: Option<u16>, total: u16) -> u16 {
    match resized {
        Some(w) => fit(w, SCHEMA_WIDTH / 2, total / 2),
        None => fit(total / 4, SCHEMA_WIDTH, 42),
    }
}

pub(in crate::ui) fn draw_query(f: &mut Frame, app: &mut App, body: Rect) {
    let show_schema = app.fit_query_screen(body.width);
    let (schema_area, main) = if show_schema {
        let [s, m] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(schema_width(app.panes.schema_width, body.width)), Constraint::Min(20)])
            .areas(body);
        (Some(s), m)
    } else {
        (None, body)
    };
    let [editor_area, results_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(app.panes.editor_percent), Constraint::Min(5)])
        .areas(main);

    // Zoomed: only the focused pane, using the whole body.
    let cursor = if app.panes.zoomed {
        // The top bar's project, drawn already, stays clickable.
        app.hits = crate::app::Hits {
            project: app.hits.project,
            navigator_too_narrow: app.hits.navigator_too_narrow,
            ..crate::app::Hits::default()
        };
        match app.focus {
            Focus::Schema => {
                app.hits.schema = Some(body);
                draw_schema(f, app, body);
                None
            }
            Focus::Results => {
                draw_results(f, app, body);
                None
            }
            Focus::Editor => draw_editor(f, app, body),
        }
    } else {
        app.hits.schema = schema_area;
        if let Some(schema_area) = schema_area {
            draw_schema(f, app, schema_area);
        }
        // The statement zoom: the editor taller, the results under it. It
        // stays while another pane has the keys (an Explain from it shows
        // its plan there); Shift+F12, or Esc in the editor, ends it.
        if app.panes.statement_zoom {
            // In place of the editor's pane, taller (two thirds at least);
            // the results under it, smaller, still whole.
            let height = editor_area.height.max(main.height.saturating_mul(2) / 3).min(main.height.saturating_sub(5));
            let popover = Rect { height, ..editor_area };
            let below = Rect { y: popover.bottom(), height: main.bottom().saturating_sub(popover.bottom()), ..main };
            draw_results(f, app, below);
            draw_editor(f, app, popover)
        } else {
            let cursor = draw_editor(f, app, editor_area);
            draw_results(f, app, results_area);
            cursor
        }
    };

    popups::draw_popups(f, app, cursor);
}
