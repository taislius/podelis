//! Any text as SQL, in every engine's dialect: what handles what the user
//! types never panics, and keeps the invariants the property tests check
//! (`tests/properties.rs`), here with coverage guiding the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use podelis_core::db::Engine;
use podelis_core::sql::{self, analyze, lint, params, parse};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    for engine in Engine::ALL {
        // Tokens cover the text exactly.
        let mut pos = 0;
        for t in sql::tokenize(text, engine) {
            assert_eq!(t.range.start, pos);
            assert!(t.range.end > t.range.start && text.is_char_boundary(t.range.end));
            pos = t.range.end;
        }
        assert_eq!(pos, text.len());
        // Statements are slices of the text.
        for stmt in sql::split_statements(text, engine) {
            assert!(text.contains(stmt));
        }
        let (a, b) = sql::statement_bounds(text, text.len() / 2, engine);
        assert!(a <= b && b <= text.len());
        // Highlighting keeps every character.
        let lines = sql::highlight_lines(text, engine);
        let rebuilt: Vec<String> = lines.iter().map(|l| l.iter().map(|(s, _)| *s).collect()).collect();
        assert_eq!(rebuilt.join("\n"), text);
        // Analysis and lints point inside the text.
        let parsed = parse::parse(text, engine);
        let _ = analyze::analyze(&parsed);
        for hint in lint::lints(&parsed, "") {
            let end = hint.offset.unwrap_or(0) + hint.len.unwrap_or(0);
            assert!(end <= text.len() && text.is_char_boundary(end));
        }
        // Formatting changes nothing but spacing.
        if let Some(out) = sql::format(text, engine) {
            let words = |s: &str| {
                sql::tokenize(s, engine)
                    .into_iter()
                    .filter(|t| t.kind != sql::TokenKind::Whitespace)
                    .map(|t| s[t.range].trim_end().to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(words(text), words(&out));
        }
        // Parameters fill and map back inside the text.
        let found = params::find(text, engine);
        let values = vec!["x".to_string(); found.len()];
        let filled = params::fill(text, &found, &values, engine);
        assert_eq!(filled.to_original(filled.sql.len()), text.len());
    }
});
