//! The programs installed around Podėlis.

use std::path::Path;

/// Whether `program` is on the PATH (or is a path that exists).
pub fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_programs_on_the_path() {
        assert!(super::on_path("sh"));
        assert!(!super::on_path("surely-not-a-real-program-podelis"));
    }
}
