//! Which of Podėlis's modules may use which: the layers in
//! `docs/architecture.md`, checked in the source so a new import that
//! points the wrong way fails here instead of being found later. Between
//! crates Cargo checks it (`podelis-core`: `db`, `sql`, `lang` can't name
//! the app); inside the app's crate one module can reach any other, so
//! this does. `crate::db` and `crate::sql` are the core's, re-exported.
//!
//! Only code that ships is checked: comments, files of tests and the
//! items marked `#[cfg(test)]` (a `mod tests;` line, a `mod tests { … }`
//! to its closing brace) are skipped, since tests may reach across to
//! reuse helpers.

#![expect(clippy::unwrap_used, reason = "tests: a panic is the failed assertion")]

use std::path::{Path, PathBuf};

/// Each module, and the modules its code must not use. From the bottom up:
/// the text editor (only the core below it), the databases' small helpers
/// `related` and `chart` (above `podelis-core`, beside `podelis-ai`: neither
/// uses it), what's on disk (the first to use `podelis-ai`, for
/// `[[models]]`), the machine, data in and out, agents and the gateway,
/// then the app, its drawing and the command line.
const RULES: &[(&str, &[&str])] = &[
    ("editor", ALL),
    ("related", &["ai", "config", "system", "transfer", "agents", "gateway", "app", "ui", "cli", "chart"]),
    ("chart", &["ai", "config", "system", "transfer", "agents", "gateway", "app", "ui", "cli", "related"]),
    ("config", &["system", "transfer", "agents", "gateway", "app", "ui", "cli"]),
    ("system", &["transfer", "agents", "gateway", "app", "ui", "cli"]),
    ("transfer", &["agents", "gateway", "app", "ui", "cli"]),
    ("agents", &["app", "ui", "cli"]),
    ("gateway", &["app", "ui", "cli"]),
    ("app", &["ui", "cli"]),
    ("ui", &["cli"]),
];

/// Every module of the app's crate and the AI crate (`ai`), for `editor`,
/// which uses only the core.
const ALL: &[&str] =
    &["ai", "related", "chart", "config", "system", "transfer", "agents", "gateway", "app", "ui", "cli"];

/// Inside `podelis-core` (Cargo sees only its edge to the app): the words
/// are below SQL and the databases, which may use each other.
const CORE_RULES: &[(&str, &[&str])] = &[("lang", &["sql", "db"])];

#[test]
fn modules_use_only_the_layers_below_them() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut wrong = Vec::new();
    for (src, rules) in [("src", RULES), ("crates/podelis-core/src", CORE_RULES)] {
        check(&root.join(src), src, rules, &mut wrong);
    }
    assert!(wrong.is_empty(), "imports against the layers (docs/architecture.md):\n{}", wrong.join("\n"));
}

/// The lines of `src`'s modules that use what their rule denies, said into
/// `wrong` (paths shown from `shown`).
fn check(src: &Path, shown: &str, rules: &[(&str, &[&str])], wrong: &mut Vec<String>) {
    for &(module, denied) in rules {
        for file in files(src, module) {
            let text = std::fs::read_to_string(&file).unwrap();
            let depth = depth_of(file.strip_prefix(src).unwrap());
            for (n, line) in shipped_lines(&text) {
                for used in modules_named(&line, depth) {
                    if denied.contains(&used.as_str()) {
                        let path = file.strip_prefix(src).unwrap().display();
                        wrong.push(format!("{shown}/{path}:{n}: {module} uses crate::{used}: {}", line.trim()));
                    }
                }
            }
        }
    }
}

/// The module's files: `src/<module>.rs` or everything under `src/<module>/`.
fn files(src: &Path, module: &str) -> Vec<PathBuf> {
    let single = src.join(format!("{module}.rs"));
    if single.is_file() {
        return vec![single];
    }
    let mut out = Vec::new();
    let mut dirs = vec![src.join(module)];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tests" && name != "snapshots" {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") && !is_test_file(&name) {
                out.push(path);
            }
        }
    }
    assert!(!out.is_empty(), "no files for module `{module}`: the rules name a module that's gone");
    out
}

/// How many modules down a file is: `lang/mod.rs` and `lang.rs` 1 (its
/// `super` is the crate's root), `app/tests/mod.rs` 2.
fn depth_of(path: &Path) -> usize {
    let parts = path.components().count();
    if path.file_name().is_some_and(|n| n == "mod.rs") { parts - 1 } else { parts }
}

fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The file's statements that ship, numbered by their first line: test
/// items left out, comments, strings and character literals blanked (so
/// neither their braces nor their words count), and a `use crate::{…}`
/// that spans lines joined into one.
fn shipped_lines(text: &str) -> Vec<(usize, String)> {
    let code = blank(text);
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut test_next = false;
    // Inside a test item: the braces still open (None until its first).
    let mut skipping: Option<Option<i32>> = None;
    let mut joining: Option<(usize, String)> = None;
    for (i, line) in code.lines().enumerate() {
        let code = line.trim();
        if let Some(depth) = &mut skipping {
            let opened = braces(code);
            let now = depth.unwrap_or(0) + opened;
            if (depth.is_none() && opened == 0 && code.ends_with(';')) || (depth.is_some() || opened > 0) && now <= 0 {
                skipping = None;
            } else if depth.is_some() || opened > 0 {
                *depth = Some(now);
            }
            continue;
        }
        if code.is_empty() {
            continue;
        }
        if code == "#[cfg(test)]" {
            test_next = true;
            continue;
        }
        if test_next && !code.starts_with("#[") {
            test_next = false;
            let opened = braces(code);
            if opened > 0 {
                skipping = Some(Some(opened));
            } else if !code.ends_with(';') && !code.ends_with('}') {
                skipping = Some(None);
            }
            continue;
        }
        if let Some((_, statement)) = &mut joining {
            statement.push(' ');
            statement.push_str(code);
            if code.ends_with(';') {
                out.extend(joining.take());
            }
            continue;
        }
        if code.contains("crate::{") && !code.ends_with(';') {
            joining = Some((i + 1, code.to_string()));
            continue;
        }
        out.push((i + 1, code.to_string()));
    }
    out
}

/// The braces a line of blanked code opens, less those it closes.
fn braces(code: &str) -> i32 {
    code.matches('{').count() as i32 - code.matches('}').count() as i32
}

/// `text` with its comments (`//`, nested `/* */`), strings (`"…"`, raw
/// `r#"…"#`, byte ones) and character literals (`'{'`, `'\''`) made
/// spaces, line breaks kept: the code alone, at its lines. A lifetime
/// (`'a`) stays.
fn blank(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let gone = |ch: char| if ch == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < c.len() {
        let (ch, next) = (c[i], c.get(i + 1).copied());
        let after_word = i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_');
        if ch == '/' && next == Some('/') {
            while i < c.len() && c[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if ch == '/' && next == Some('*') {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                } else {
                    out.push(gone(c[i]));
                    i += 1;
                    continue;
                }
                out.push_str("  ");
                i += 2;
                if depth == 0 {
                    break;
                }
            }
            continue;
        }
        // A raw string: `r"…"`, `r#"…"#`, `br"…"`.
        if !after_word && (ch == 'r' || (ch == 'b' && next == Some('r'))) {
            let mut j = i + if ch == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if c.get(j) == Some(&'"') {
                let closes = |at: usize| c[at] == '"' && (1..=hashes).all(|k| c.get(at + k) == Some(&'#'));
                out.extend(std::iter::repeat_n(' ', j + 1 - i));
                i = j + 1;
                while i < c.len() && !closes(i) {
                    out.push(gone(c[i]));
                    i += 1;
                }
                let end = (i + 1 + hashes).min(c.len());
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
                continue;
            }
        }
        if ch == '"' {
            out.push(' ');
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < c.len() {
                    out.push(gone(c[i]));
                    i += 1;
                }
            }
            if i < c.len() {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        // A character literal; not a lifetime (`'a`), whose second quote
        // never comes.
        if ch == '\'' {
            let end = if next == Some('\\') {
                (i + 3..c.len().min(i + 12)).find(|&j| c[j] == '\'')
            } else {
                (c.get(i + 2) == Some(&'\'')).then_some(i + 2)
            };
            if let Some(end) = end {
                out.extend(std::iter::repeat_n(' ', end + 1 - i));
                i = end + 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// The modules `code` names after `crate::` (or the other crates' names,
/// or as many `super::`s as reach the crate's root from a file `depth`
/// modules down), a `crate::{…}` group's too.
fn modules_named(code: &str, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    // The core's modules by their own names; the AI crate as `ai`.
    let mut code = code.replace("podelis_core::", "crate::").replace("podelis_ai::", "crate::ai::");
    if depth > 0 {
        code = code.replace(&"super::".repeat(depth), "crate::");
    }
    for (i, _) in code.match_indices("crate::") {
        let rest = &code[i + "crate::".len()..];
        if let Some(group) = rest.strip_prefix('{') {
            // The names at the group's top level: `a::b, c, d::{e, f}`.
            let mut depth = 0;
            let mut start = true;
            for (j, c) in group.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' if depth == 0 => break,
                    '}' => depth -= 1,
                    ',' if depth == 0 => start = true,
                    c if start && (c.is_alphanumeric() || c == '_') => {
                        start = false;
                        out.push(ident(&group[j..]));
                    }
                    _ => {}
                }
            }
        } else {
            out.push(ident(rest));
        }
    }
    out
}

fn ident(text: &str) -> String {
    text.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
}

#[test]
fn the_check_sees_what_it_should() {
    let named = |code: &str| modules_named(code, 0);
    assert_eq!(named("use crate::app::ConnectForm;"), ["app"]);
    assert_eq!(named("    let x = crate::cli::classify(a);"), ["cli"]);
    assert_eq!(named("use crate::application::X;"), ["application"]);
    assert_eq!(named("use podelis_core::db::Engine;"), ["db"]);
    assert_eq!(named("let m = podelis_ai::models::on_this_machine(c);"), ["ai"]);
    assert_eq!(named("use crate::{app::ConnectForm, config::{store, draft}, db};"), ["app", "config", "db"]);
    // `super` from a top module's file is the crate's root; deeper, it isn't.
    assert_eq!(modules_named("use super::db::Engine;", 1), ["db"]);
    assert!(modules_named("use super::grid::Grid;", 2).is_empty());
    assert_eq!(modules_named("use super::super::ui::X;", 2), ["ui"]);
    assert_eq!(depth_of(Path::new("lang/mod.rs")), 1);
    assert_eq!(depth_of(Path::new("app/tests/results.rs")), 3);
    // Strings, raw strings, character literals and comments hold no code.
    assert_eq!(blank("f('{', \"{x}\") // crate::app {").trim_end(), "f(   ,      )");
    // (`r"C:\dir\"` is 10 characters: no escapes in a raw string.)
    assert_eq!(blank("let p = r\"C:\\dir\\\"; {").trim_end(), format!("let p = {}; {{", " ".repeat(10)));
    assert_eq!(blank("r#\"say \"{\" here\"#; }").trim_end(), format!("{}; }}", " ".repeat(17)));
    assert_eq!(blank("fn f<'a>(x: &'a str) { '\\'' }"), "fn f<'a>(x: &'a str) {      }");
    assert_eq!(blank("/* a /* b */ { */ x").trim_start(), "x");
    let text = "\
use crate::db::Engine;
// crate::app in a comment
#[cfg(test)]
mod tests;
use crate::{
    config::store,
    gateway::client,
};
#[cfg(test)]
pub(crate) mod helpers {
    use crate::app::X;
    const BODY: &str = r#\"{
        \"a\": 1
    }\"#;
    fn f() { if true { } }
}
#[cfg(test)]
use crate::ui::Y;
fn g() {}
";
    let got: Vec<(usize, Vec<String>)> = shipped_lines(text).iter().map(|(n, l)| (*n, named(l))).collect();
    let expected = [(1, vec!["db"]), (5, vec!["config", "gateway"]), (19, vec![])];
    assert_eq!(got, expected.map(|(n, m)| (n, m.into_iter().map(String::from).collect())));
}
