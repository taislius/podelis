//! The words the code says against `lang/en.toml` (in `podelis-core`):
//! every `t!`/`tn!` key in the workspace's crates is there, and nothing there is
//! unused. Here, in the app's crate, because it reads every crate's code.

/// Every key the code uses, from its `t!("…")`s.
fn used_keys() -> Vec<String> {
    let mut keys = Vec::new();
    // Every crate of the workspace: the app's, and each of `crates/`.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        stack.push(entry.path().join("src"));
    }
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
    let en: std::collections::HashSet<&str> = crate::lang::english_keys().into_iter().collect();
    let missing: Vec<String> = used_keys().into_iter().filter(|k| !en.contains(k.as_str())).collect();
    assert!(missing.is_empty(), "not in lang/en.toml: {missing:?}");
}

#[test]
fn english_has_no_words_the_code_doesnt_use() {
    let used: std::collections::HashSet<String> = used_keys().into_iter().collect();
    let counted = |k: &str| k.rsplit_once('.').is_some_and(|(stem, _)| used.contains(&format!("{stem}.other")));
    let mut unused: Vec<&str> =
        crate::lang::english_keys().into_iter().filter(|k| !used.contains(*k) && !counted(k)).collect();
    unused.sort_unstable();
    assert!(unused.is_empty(), "in lang/en.toml but not used: {unused:?}");
}
