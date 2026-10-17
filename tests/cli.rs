//! `podelis` without the UI, run as a script would run it: the built
//! program, a scratch config folder and a SQLite file. What it prints, on
//! which stream, and its exit codes (see `podelis --help`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch config folder and a database file with a table `pets`.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let scratch = Scratch { dir: tempfile::tempdir().unwrap() };
        let made = scratch.run(&[
            "--create",
            "-e",
            "create table pets (id integer primary key, name text, weight real);
             insert into pets values (1, 'Rex', 12.5), (2, 'Žąsis, the goose', null), (3, 'Kit', 3)",
        ]);
        assert!(made.status.success(), "{}", text(&made.stderr));
        scratch
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("pets.db")
    }

    /// `podelis <the file> <args>`, with nothing on stdin.
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).stdin(Stdio::null()).output().unwrap()
    }

    /// `podelis <the file> <args>`, with `input` on stdin.
    fn run_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child =
            self.command(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = podelis(self.dir.path());
        command.arg(self.db()).args(args);
        command
    }
}

/// The built program, its files in `config` (never the user's own).
fn podelis(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_podelis"));
    command.env("PODELIS_CONFIG_DIR", config.join("config")).env_remove("DATABASE_URL");
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn each_format_prints_the_rows() {
    let s = Scratch::new();
    let sql = "select id, name, weight from pets order by id";
    let csv = s.run(&["-e", sql, "-f", "csv"]);
    assert!(csv.status.success());
    // RFC 4180: lines end in CRLF, a field with a comma is quoted.
    assert_eq!(text(&csv.stdout), "id,name,weight\r\n1,Rex,12.5\r\n2,\"Žąsis, the goose\",\r\n3,Kit,3\r\n");
    let tsv = s.run(&["-e", sql, "-f", "tsv"]);
    assert_eq!(text(&tsv.stdout).lines().nth(2), Some("2\tŽąsis, the goose\t"));
    let json: serde_json::Value = serde_json::from_slice(&s.run(&["-e", sql, "-f", "json"]).stdout).unwrap();
    assert_eq!(json[1]["name"], "Žąsis, the goose");
    assert_eq!(json[1]["weight"], serde_json::Value::Null);
    let table = text(&s.run(&["-e", sql, "-f", "table"]).stdout);
    assert!(table.lines().any(|l| l.contains("Rex") && l.contains("12.5")), "{table}");
}

#[test]
fn a_scripts_result_sets() {
    let s = Scratch::new();
    let script = "select 1 as a; select 2 as b";
    // JSON: an array per result set.
    let json = text(&s.run(&["-e", script, "-f", "json"]).stdout);
    let arrays: Vec<serde_json::Value> =
        serde_json::Deserializer::from_str(&json).into_iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(arrays.len(), 2, "{json}");
    // CSV: the last one, and stderr says what was left out.
    let csv = s.run(&["-e", script, "-f", "csv"]);
    assert_eq!(text(&csv.stdout), "b\r\n2\r\n");
    assert!(text(&csv.stderr).contains("1 earlier one(s)"), "{}", text(&csv.stderr));
}

#[test]
fn sql_from_stdin_and_a_row_limit() {
    let s = Scratch::new();
    let read = s.run_with_input(&["-e", "-", "-f", "csv"], "select count(*) as n from pets");
    assert_eq!(text(&read.stdout), "n\r\n3\r\n");
    let limited = s.run(&["-e", "select id from pets order by id", "--limit", "2", "-f", "csv"]);
    assert_eq!(text(&limited.stdout), "id\r\n1\r\n2\r\n");
    assert!(text(&limited.stderr).contains("first 2 of 3 rows"), "{}", text(&limited.stderr));
}

#[test]
fn exit_codes_tell_what_happened() {
    let s = Scratch::new();
    let code = |out: Output| (out.status.code(), text(&out.stderr));
    // 1: the SQL failed.
    let (status, said) = code(s.run(&["-e", "select * from nope"]));
    assert_eq!(status, Some(1), "{said}");
    assert!(said.contains("no such table"), "{said}");
    // 2: what was asked for can't be used (a file that isn't there).
    let missing = podelis(s.dir.path()).arg(s.dir.path().join("none.db")).args(["-e", "select 1"]).output().unwrap();
    let (status, said) = code(missing);
    assert_eq!(status, Some(2), "{said}");
    assert!(said.contains("--create"), "{said}");
    assert!(!s.dir.path().join("none.db").exists(), "not created");
    // 3: destructive without --yes; nothing of the script ran.
    let (status, said) = code(s.run(&["-e", "insert into pets values (4, 'Tom', 1); delete from pets"]));
    assert_eq!(status, Some(3), "{said}");
    assert!(said.contains("--yes"), "{said}");
    assert_eq!(text(&s.run(&["-e", "select count(*) from pets", "-f", "csv"]).stdout), "count(*)\r\n3\r\n");
    // With --yes it runs.
    assert_eq!(s.run(&["-e", "delete from pets where id = 3", "--yes"]).status.code(), Some(0));
    // 4: nothing answers at that address.
    let unreachable = podelis(s.dir.path()).args(["postgres://u:p@127.0.0.1:1/x", "-e", "select 1"]).output().unwrap();
    let (status, said) = code(unreachable);
    assert_eq!(status, Some(4), "{said}");
    assert!(said.contains("hint:"), "{said}");
}

#[test]
fn help_lists_the_commands() {
    let out = podelis(Path::new("unused")).arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = text(&out.stdout);
    for command in ["podelis doctor", "podelis mcp", "podelis memory", "podelis gateway", "-e, --execute"] {
        assert!(help.contains(command), "{command}: {help}");
    }
}
