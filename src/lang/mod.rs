//! The words the app shows, in the user's language. Each language is a
//! TOML file here (`en.toml`, `lt.toml`…), built into the program: sections
//! by where the words are used, `{name}` where a value goes in. English is
//! complete; a key another language lacks is said in English.
//!
//! `t!("editor.find")` gives the words; `t!("status.saved", name = n)` puts
//! values in. The language is chosen at startup (`language` in
//! `config.toml`, else the system's `LANG`) and in the settings; tests keep
//! English.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The languages there are: code, its own name, its words.
pub const LANGUAGES: &[(&str, &str, &str)] =
    &[("en", "English", include_str!("en.toml")), ("lt", "Lietuvių", include_str!("lt.toml"))];

type Catalog = HashMap<&'static str, &'static str>;

/// Each language's words over English's, made the first time it's chosen
/// and kept (what `t!` gave stays valid when the language changes).
static LOADED: [OnceLock<Catalog>; LANGUAGES.len()] = [const { OnceLock::new() }; LANGUAGES.len()];

/// The chosen language: an index into [`LANGUAGES`] (English before one is
/// chosen, as in tests).
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The chosen language's words.
fn words() -> &'static Catalog {
    loaded(CURRENT.load(Ordering::Relaxed))
}

/// Language `i`'s words over English's, made the first time they're asked for.
fn loaded(i: usize) -> &'static Catalog {
    LOADED[i].get_or_init(|| {
        let mut words = english();
        if i > 0 {
            words.extend(catalog(LANGUAGES[i].2));
        }
        words
    })
}

/// The chosen language's code (`en`, `lt`).
pub fn code() -> &'static str {
    LANGUAGES[CURRENT.load(Ordering::Relaxed)].0
}

/// A language's words, its keys flattened (`section.key`).
fn catalog(source: &'static str) -> Catalog {
    let table: toml::Table = toml::from_str(source).expect("a language file that parses");
    let mut out = Catalog::new();
    flatten("", &table, &mut out);
    out
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut Catalog) {
    for (key, value) in table {
        let key = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match value {
            toml::Value::Table(inner) => flatten(&key, inner, out),
            toml::Value::String(text) => {
                // Read once and kept for the program's life.
                out.insert(Box::leak(key.into_boxed_str()), Box::leak(text.clone().into_boxed_str()));
            }
            other => panic!("{key}: words are strings, not {other}"),
        }
    }
}

fn english() -> Catalog {
    catalog(LANGUAGES[0].2)
}

/// The language `setting` names (`auto`, or a code like `lt`), `auto`
/// following `LANG` (`lt_LT.UTF-8` is Lithuanian); English for any other.
fn pick(setting: &str, lang_env: Option<&str>) -> &'static (&'static str, &'static str, &'static str) {
    let wanted = if setting.eq_ignore_ascii_case("auto") {
        lang_env.and_then(|l| l.split(['_', '.', '-']).next()).unwrap_or("en").to_ascii_lowercase()
    } else {
        setting.to_ascii_lowercase()
    };
    LANGUAGES.iter().find(|(code, _, _)| *code == wanted).unwrap_or(&LANGUAGES[0])
}

/// The system's language, as `auto` follows it: the first of `LC_ALL`,
/// `LC_MESSAGES` and `LANG` that's set.
pub fn system() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty()))
}

/// Chooses the language (see [`pick`]), at startup and when the settings
/// change it; its code.
pub fn choose(setting: &str, lang_env: Option<&str>) -> &'static str {
    let (code, _, _) = pick(setting, lang_env);
    let i = LANGUAGES.iter().position(|(c, _, _)| c == code).unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
    code
}

/// Which form of a counted word `n` takes in language `code`: `one`, `few`
/// or `other` (Lithuanian has all three: 1 eilutė, 2 eilutės, 10 eilučių).
fn plural(code: &str, n: u64) -> &'static str {
    match code {
        "lt" if n % 10 == 1 && n % 100 != 11 => "one",
        "lt" if (2..=9).contains(&(n % 10)) && !(11..=19).contains(&(n % 100)) => "few",
        "lt" => "other",
        _ if n == 1 => "one",
        _ => "other",
    }
}

/// The form of `key` for `n` (`key.one`, `key.few`, `key.other`; `other`
/// when the language has no such form), with `{n}` and `values` put in.
pub fn counted(key: &str, n: u64, values: &[(&str, String)]) -> String {
    let code = code();
    let words = words();
    let form = format!("{key}.{}", plural(code, n));
    let text = words.get(form.as_str()).or_else(|| words.get(format!("{key}.other").as_str())).copied();
    debug_assert!(text.is_some(), "no `{key}.other` in lang/en.toml");
    let mut values = values.to_vec();
    values.push(("n", n.to_string()));
    put_in(text.unwrap_or(key), &values)
}

/// `template` with each `{name}` replaced by its value, in one pass: what
/// a value says (a connection named `{same}`) is put in as it is.
fn put_in(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after.find('}').map(|close| (&after[..close], &after[close + 1..]));
        if let Some((value, tail)) =
            named.and_then(|(name, tail)| values.iter().find(|(n, _)| *n == name).map(|(_, v)| (v, tail)))
        {
            out.push_str(value);
            rest = tail;
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `key`'s words in every language there is, for recognising what an
/// earlier run (or the language before a switch) wrote, as a default tab
/// name.
pub fn in_every_language(key: &str) -> Vec<&'static str> {
    (0..LANGUAGES.len()).filter_map(|i| loaded(i).get(key).copied()).collect()
}

/// The words for `key`, in the chosen language (English before one is
/// chosen, as in tests).
pub fn text(key: &str) -> &'static str {
    if let Some(text) = words().get(key) {
        return text;
    }
    debug_assert!(false, "no words for `{key}` in lang/en.toml");
    // The key itself, made `'static` once per key (it may be drawn every
    // frame).
    static MISSING: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut missing = MISSING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = missing.iter().find(|k| **k == key) {
        return known;
    }
    let leaked: &'static str = Box::leak(key.to_string().into_boxed_str());
    missing.push(leaked);
    leaked
}

/// The words for `key` with each `{name}` replaced by its value.
pub fn fill(key: &str, values: &[(&str, String)]) -> String {
    put_in(text(key), values)
}

/// `t!("section.key")`: the words, `&'static str`. `t!("section.key", name =
/// value, …)`: with the values put in, a `String`.
macro_rules! t {
    ($key:literal) => {
        $crate::lang::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::lang::fill($key, &[$((stringify!($name), $value.to_string())),+])
    };
}

/// `tn!("section.key", n)`: the form of a counted word for `n` (as a
/// number, `{n}`), a `String`; `tn!("section.key", n, name = value, …)`
/// with more values.
macro_rules! tn {
    ($key:literal, $n:expr) => {
        $crate::lang::counted($key, u64::try_from($n).unwrap_or(u64::MAX), &[])
    };
    ($key:literal, $n:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::lang::counted($key, u64::try_from($n).unwrap_or(u64::MAX), &[$((stringify!($name), $value.to_string())),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{name}` placeholders in a text.
    fn placeholders(text: &str) -> Vec<&str> {
        let mut names: Vec<&str> = text
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .collect();
        names.sort_unstable();
        names
    }

    /// Every key the code uses, from its `t!("…")`s.
    fn used_keys() -> Vec<String> {
        let mut keys = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Not the words' own module: its comments show examples.
                    if !path.ends_with("lang") {
                        stack.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let code = std::fs::read_to_string(&path).unwrap();
                    // `t!("…` and `tn!("…`, not the end of `format!("…`.
                    for macro_call in ["t!(", "tn!("] {
                        for (at, _) in code.match_indices(macro_call) {
                            let before = code[..at].chars().next_back();
                            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                                continue;
                            }
                            // The key, maybe on the next line (as rustfmt puts it).
                            let Some(rest) = code[at + macro_call.len()..].trim_start().strip_prefix('"') else {
                                continue;
                            };
                            if let Some((key, _)) = rest.split_once('"') {
                                // A counted word's forms: `key.one`, `key.other`…
                                let forms = if macro_call.starts_with("tn") { ".other" } else { "" };
                                keys.push(format!("{key}{forms}"));
                            }
                        }
                    }
                }
            }
        }
        // Keys are `section.key`; the examples in comments aren't.
        keys.retain(|k| k.contains('.') && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_'));
        keys
    }

    #[test]
    fn every_key_the_code_uses_is_in_english() {
        let en = english();
        let missing: Vec<String> = used_keys().into_iter().filter(|k| !en.contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "not in lang/en.toml: {missing:?}");
    }

    #[test]
    fn english_has_no_words_the_code_doesnt_use() {
        let used = used_keys();
        let counted =
            |k: &str| k.rsplit_once('.').is_some_and(|(stem, _)| used.iter().any(|u| *u == format!("{stem}.other")));
        let mut unused: Vec<&str> =
            english().into_keys().filter(|k| !used.iter().any(|u| u == k) && !counted(k)).collect();
        unused.sort_unstable();
        assert!(unused.is_empty(), "in lang/en.toml but not used: {unused:?}");
    }

    #[test]
    fn translations_match_english() {
        let en = english();
        for (code, _, source) in &LANGUAGES[1..] {
            for (key, text) in catalog(source) {
                // A counted word's forms are as English's `other`.
                let stem_other = key.rsplit_once('.').map(|(stem, _)| format!("{stem}.other")).unwrap_or_default();
                let english = en.get(key).or_else(|| en.get(stem_other.as_str()));
                let Some(english) = english else { panic!("{code}: `{key}` isn't in English") };
                assert_eq!(placeholders(text), placeholders(english), "{code}: `{key}`'s {{values}}");
            }
        }
    }

    #[test]
    fn the_language_follows_the_setting_then_lang() {
        let code = |setting, env| pick(setting, env).0;
        assert_eq!(code("lt", None), "lt");
        assert_eq!(code("LT", None), "lt");
        assert_eq!(code("auto", Some("lt_LT.UTF-8")), "lt");
        assert_eq!(code("auto", Some("de_DE.UTF-8")), "en");
        assert_eq!(code("auto", None), "en");
        assert_eq!(code("xx", None), "en");
    }

    #[test]
    fn counted_words_take_the_languages_forms() {
        let forms = |code| [0, 1, 2, 5, 10, 11, 12, 21, 22, 111].map(|n| plural(code, n));
        assert_eq!(
            forms("en"),
            ["other", "one", "other", "other", "other", "other", "other", "other", "other", "other"]
        );
        assert_eq!(forms("lt"), ["other", "one", "few", "few", "other", "other", "other", "one", "few", "other"]);
    }

    #[test]
    fn values_go_in_once_as_they_are() {
        let values = [("name", "{same}".to_string()), ("same", "3".to_string())];
        assert_eq!(put_in("{name}: {same} the same, {other} {", &values), "{same}: 3 the same, {other} {");
    }

    #[test]
    fn values_go_in() {
        assert_eq!(placeholders("{b} and {a}, not {} or {:>5}"), ["a", "b"]);
    }
}
