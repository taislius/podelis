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
            for (n, line) in shipped_lines(&text) {
                for used in modules_named(&line) {
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

fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The file's statements that ship, numbered by their first line: test
/// items left out, comments dropped, and a `use crate::{…}` that spans
/// lines joined into one.
fn shipped_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut test_next = false;
    // Inside a test item: the braces still open (None until its first).
    let mut skipping: Option<Option<i32>> = None;
    let mut joining: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("").trim();
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

/// The braces `code` opens less those it closes, outside strings and
/// character literals (a test's `'{'` or `"{name}"` isn't a block).
fn braces(code: &str) -> i32 {
    let mut depth = 0;
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            // A character literal (`'{'`, `'\\''`), not a lifetime (`'a`).
            '\'' => {
                // After the quote: `{'`, or an escape (`\''`, `\n'`, `\u{7f}'`)
                // up to the quote after it.
                let rest: String = chars.clone().take(12).collect();
                let len = if rest.starts_with('\\') {
                    rest.get(2..).and_then(|r| r.find('\'')).map(|i| i + 3)
                } else {
                    rest.chars().nth(1).filter(|c| *c == '\'').map(|_| 2)
                };
                for _ in 0..len.unwrap_or(0) {
                    chars.next();
                }
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The modules `code` names after `crate::` (or the other crates' names), a
/// `crate::{…}` group's too.
fn modules_named(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    // The core's modules by their own names; the AI crate as `ai`.
    let code = code.replace("podelis_core::", "crate::").replace("podelis_ai::", "crate::ai::");
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
    assert_eq!(modules_named("use crate::app::ConnectForm;"), ["app"]);
    assert_eq!(modules_named("    let x = crate::cli::classify(a);"), ["cli"]);
    assert_eq!(modules_named("use crate::application::X;"), ["application"]);
    assert_eq!(modules_named("use podelis_core::db::Engine;"), ["db"]);
    assert_eq!(modules_named("let m = podelis_ai::models::on_this_machine(c);"), ["ai"]);
    assert_eq!(modules_named("use crate::{app::ConnectForm, config::{store, draft}, db};"), ["app", "config", "db"]);
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
    fn f() { if true { } }
}
#[cfg(test)]
use crate::ui::Y;
fn g() {}
";
    assert_eq!(braces("let x = '{'; f(\"{name}\") {"), 1);
    assert_eq!(braces("fn f<'a>(x: &'a str) { '\\'' }"), 0);
    assert_eq!(braces("['\\'', '{', '\\u{7b}'] {"), 1);
    let named: Vec<(usize, Vec<String>)> = shipped_lines(text).iter().map(|(n, l)| (*n, modules_named(l))).collect();
    let expected = [(1, vec!["db"]), (5, vec!["config", "gateway"]), (16, vec![])];
    assert_eq!(named, expected.map(|(n, m)| (n, m.into_iter().map(String::from).collect())));
}
