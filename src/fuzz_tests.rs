//! Property tests: random input for everything that handles what the user
//! types. Nothing may panic, and core invariants must hold.

use proptest::prelude::*;

use crate::complete::{self, TableRef};
use crate::db::{ColumnInfo, Engine, ErrorPos};
use crate::editor::TextEditor;
use crate::{analyze, sql};

/// SQL-ish text: keywords, quotes, comments, dollar quotes, unicode, newlines.
fn sqlish() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        Just("select ".to_string()),
        Just("from ".to_string()),
        Just("where ".to_string()),
        Just(";".to_string()),
        Just("'".to_string()),
        Just("\"".to_string()),
        Just("`".to_string()),
        Just("--".to_string()),
        Just("/*".to_string()),
        Just("*/".to_string()),
        Just("$$".to_string()),
        Just("$tag$".to_string()),
        Just("\\".to_string()),
        Just("\n".to_string()),
        Just(".".to_string()),
        Just("(".to_string()),
        Just(")".to_string()),
        Just("ž".to_string()),
        Just("日本".to_string()),
        Just(":".to_string()),
        Just("?".to_string()),
        Just("$1".to_string()),
        "[a-z_0-9]{1,6}",
        any::<char>().prop_map(|c| c.to_string()),
    ];
    prop::collection::vec(atom, 0..40).prop_map(|parts| parts.concat())
}

fn engine() -> impl Strategy<Value = Engine> {
    prop_oneof![Just(Engine::Postgres), Just(Engine::MySql), Just(Engine::Sqlite)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Formatting never panics and, when it formats, changes nothing but
    /// spacing.
    #[test]
    fn formatting_changes_only_spacing(text in sqlish(), engine in engine()) {
        if let Some(out) = sql::format(&text, engine) {
            let words = |s: &str| sql::tokenize(s, engine)
                .into_iter()
                .filter(|t| t.kind != sql::TokenKind::Whitespace)
                .map(|t| s[t.range].trim_end().to_string())
                .collect::<Vec<_>>();
            prop_assert_eq!(words(&text), words(&out));
        }
    }

    /// Real statements full of awkward strings always format, with every
    /// string as it was.
    #[test]
    fn statements_format_with_their_strings(
        values in prop::collection::vec(prop_oneof![
            "'[a-z ]{0,8}'",
            "\\$\\$[a-z' ;\n]{0,10}\\$\\$",
            "\\$q\\$[a-z' ]{0,10}\\$q\\$",
            "E'[a-z ]{0,6}'",
            "U&'[a-z ]{0,6}'",
            "\"[A-Za-z ]{1,6}\"",
            "[0-9]{1,4}",
        ], 1..5),
    ) {
        let sql = format!("select {} from t where x in ({}) order by 1", values.join(", "), values.join(", "));
        let out = sql::format(&sql, Engine::Postgres);
        prop_assert!(out.is_some(), "refused {}", sql);
        let out = out.unwrap();
        for v in &values {
            prop_assert_eq!(out.matches(v.as_str()).count(), sql.matches(v.as_str()).count(), "{} in {}", v, out);
        }
    }

    #[test]
    fn tokens_cover_the_text_exactly(text in sqlish(), engine in engine()) {
        let tokens = sql::tokenize(&text, engine);
        let mut pos = 0;
        for t in &tokens {
            prop_assert_eq!(t.range.start, pos);
            prop_assert!(t.range.end > t.range.start);
            prop_assert!(text.is_char_boundary(t.range.end));
            pos = t.range.end;
        }
        prop_assert_eq!(pos, text.len());
    }

    #[test]
    fn statements_are_slices_of_the_text(text in sqlish(), engine in engine(), at in 0usize..400) {
        for stmt in sql::split_statements(&text, engine) {
            prop_assert!(!stmt.trim().is_empty());
            prop_assert!(text.contains(stmt));
        }
        let offset = at.min(text.len());
        if text.is_char_boundary(offset)
            && let Some(stmt) = sql::statement_at(&text, offset, engine)
        {
            prop_assert!(text.contains(stmt));
        }
        let (a, b) = sql::statement_bounds(&text, offset, engine);
        prop_assert!(a <= b && b <= text.len());
    }

    #[test]
    fn highlighting_keeps_every_character(text in sqlish(), engine in engine()) {
        let lines = sql::highlight_lines(&text, engine);
        let rebuilt: Vec<String> = lines.iter().map(|l| l.iter().map(|(s, _)| *s).collect()).collect();
        prop_assert_eq!(rebuilt.join("\n"), text);
    }

    #[test]
    fn analysis_never_panics(text in sqlish(), engine in engine()) {
        let parsed = crate::parse::parse(&text, engine);
        let _ = analyze::analyze(&parsed);
        let _ = analyze::edit_source(&parsed);
        if let Some(hint) = analyze::syntax_hint(&parsed) {
            prop_assert!(hint.offset.is_none_or(|o| o <= text.len()));
        }
        for hint in crate::lint::lints(&parsed, "") {
            let end = hint.offset.unwrap_or(0) + hint.len.unwrap_or(0);
            prop_assert!(end <= text.len() && text.is_char_boundary(end));
        }
    }

    #[test]
    fn error_ranges_stay_inside_the_text(text in sqlish(), c in 0usize..200, line in 0usize..20, token in "[a-z']{0,5}") {
        for pos in [ErrorPos::Char(c), ErrorPos::Line(line), ErrorPos::Token(token.clone()), ErrorPos::Near { line, text: token.clone() }] {
            if let Some((start, len)) = analyze::error_range(&text, &pos) {
                prop_assert!(start + len <= text.len());
                prop_assert!(text.is_char_boundary(start) && text.is_char_boundary(start + len));
            }
        }
    }

    #[test]
    fn completion_never_panics(text in sqlish(), engine in engine(), at in 0usize..400) {
        let columns = vec![ColumnInfo { name: "id".into(), data_type: "int".into(), nullable: false, primary_key: true }];
        let tables = [TableRef { schema: "public", name: "users", columns: &columns }];
        let mut cursor = at.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let (prefix_len, _) = complete::complete(&text, cursor, engine, &tables);
        prop_assert!(prefix_len <= text[..cursor].chars().count());
    }

    /// Random editing keeps the cursor inside the text and never panics.
    #[test]
    fn editor_survives_random_edits(ops in prop::collection::vec((0u8..18, any::<char>(), "[a-zž\n ]{0,4}"), 0..60)) {
        let mut ed = TextEditor::new();
        for (op, ch, s) in ops {
            match op {
                0 => ed.insert_char(if ch.is_control() { 'x' } else { ch }),
                1 => ed.insert_str(&s),
                2 => ed.insert_newline(),
                3 => ed.backspace(),
                4 => ed.delete(),
                5 => { ed.prepare_move(ch.is_alphabetic()); ed.move_left(); }
                6 => { ed.prepare_move(ch.is_alphabetic()); ed.move_right(); }
                7 => { ed.prepare_move(ch.is_alphabetic()); ed.move_up(); }
                8 => { ed.prepare_move(ch.is_alphabetic()); ed.move_down(); }
                9 => { ed.prepare_move(false); ed.move_word_left(); }
                10 => { ed.prepare_move(false); ed.move_word_right(); }
                11 => { ed.undo(); }
                12 => { ed.redo(); }
                13 => ed.select_all(),
                14 => { ed.cut(); }
                15 => ed.replace_before_cursor(s.chars().count() % 3, &s),
                16 => ed.set_text(&s),
                _ => ed.scroll_to_cursor(5, 10),
            }
            prop_assert!(ed.cursor_row < ed.lines.len());
            prop_assert!(ed.cursor_col <= ed.lines[ed.cursor_row].chars().count());
            let offset = ed.cursor_offset();
            prop_assert!(offset <= ed.text().len());
            prop_assert_eq!(ed.pos_of_offset(offset), (ed.cursor_row, ed.cursor_col));
        }
    }

    #[test]
    fn library_search_never_panics(query in "\\PC{0,20}", text in sqlish()) {
        let _ = crate::library::matches(&query, &[&text]);
    }

    #[test]
    fn parameters_fill_and_map_back(text in sqlish(), engine in engine(), value in "\\PC{0,8}") {
        let params = crate::params::find(&text, engine);
        for p in &params {
            for r in &p.ranges {
                prop_assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
            }
        }
        let values = vec![value; params.len()];
        let filled = crate::params::fill(&text, &params, &values, engine);
        for offset in 0..=filled.sql.len() {
            prop_assert!(filled.to_original(offset) <= text.len());
        }
        prop_assert_eq!(filled.to_original(filled.sql.len()), text.len());
    }
}
