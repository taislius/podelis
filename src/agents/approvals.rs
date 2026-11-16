//! Agents' writes waiting for the user's yes or no (connections shared as
//! `AgentAccess::Approve`): a file per request, and one for its answer.

use crate::config::store::ProjectId;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where agents' writes wait for the user's answer (see [`Request`]).
pub fn pending_dir(config_dir: &Path, project_id: &ProjectId) -> PathBuf {
    config_dir.join("agents").join(format!("{project_id}.pending"))
}

/// A request's id: when it was asked and by which agent session. It names
/// the request's files, so one read back from a file is taken only when
/// it's letters, digits, `-`, `_` and `.`, not starting with `.` (no `/`
/// to lead out of the folder).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestId(String);

impl RequestId {
    /// A new one, for agent session `session`: ids sort by when they were asked.
    pub fn new(session: impl std::fmt::Display) -> Self {
        let session: String =
            session.to_string().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        Self(format!("{}-{session}", chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn json(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.json", self.0))
    }

    fn answer_file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.answer", self.0))
    }
}

impl TryFrom<String> for RequestId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, String> {
        let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if id.is_empty() || id.len() > 128 || id.starts_with('.') || !id.chars().all(safe) {
            return Err(format!("not a request id: {id:?}"));
        }
        Ok(Self(id))
    }
}

impl From<RequestId> for String {
    fn from(id: RequestId) -> Self {
        id.0
    }
}

/// An id made up by a test.
#[cfg(test)]
impl From<&str> for RequestId {
    fn from(id: &str) -> Self {
        Self::try_from(id.to_string()).expect("a test's id is safe")
    }
}

/// An agent's write waiting for the user's yes or no, on a connection
/// shared as [`crate::db::AgentAccess::Approve`]. `podelis mcp` writes it to
/// `<id>.json` in the [`pending_dir`] and waits for `<id>.answer`
/// (`approve`, or anything else for no), which Podėlis writes.
///
/// This guards against an agent's mistakes, not against an agent set on
/// harm: one with a shell as the same user could write the answer itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    /// Who asks: the name given, else the client's.
    pub agent: String,
    pub connection: String,
    pub production: bool,
    pub sql: String,
    /// What its dry run said (or why there was none).
    pub dry_run: String,
}

/// Leaves `request` for the user; its path.
pub fn ask(dir: &Path, request: &Request) -> anyhow::Result<PathBuf> {
    let path = request.id.json(dir);
    crate::config::store::write_atomic(&path, &serde_json::to_string(request)?)?;
    Ok(path)
}

/// A request older than this was left by an agent that's gone (one waits
/// two minutes): it's cleared away, not asked about.
const STALE: std::time::Duration = std::time::Duration::from_secs(150);

/// The requests waiting for an answer, oldest first (ids sort by time).
pub fn pending(dir: &Path) -> Vec<Request> {
    let Ok(files) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Request> = files
        .filter_map(|f| f.ok().map(|f| f.path()))
        .filter(|p| {
            // Left behind (an agent that's gone, an answer that came too
            // late): cleared away.
            let age = std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            let stale = age.is_some_and(|age| age > STALE);
            if stale {
                let _ = std::fs::remove_file(p);
            }
            !stale
        })
        .filter(|p| p.extension().is_some_and(|e| e == "json") && !p.with_extension("answer").exists())
        .filter_map(|p| {
            let request: Request = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            // Its answer is named by its id: a file named otherwise would
            // be asked about again and again.
            (p.file_stem()? == request.id.as_str()).then_some(request)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The user's answer to request `id`; `false` if the agent stopped
/// waiting for it (it may have, just as the answer came: its log says).
pub fn answer(dir: &Path, id: &RequestId, approve: bool) -> bool {
    let asked = || id.json(dir).exists();
    if !asked() {
        return false;
    }
    let written =
        crate::config::store::write_atomic(&id.answer_file(dir), if approve { "approve" } else { "deny" }).is_ok();
    written && asked()
}

/// Takes request `id` back: no answer is waited for any more.
pub fn withdraw(dir: &Path, id: &RequestId) {
    let _ = std::fs::remove_file(id.json(dir));
}

/// The answer to request `id`, once there is one.
pub fn read_answer(dir: &Path, id: &RequestId) -> Option<bool> {
    std::fs::read_to_string(id.answer_file(dir)).ok().map(|a| a.trim() == "approve")
}

/// Clears request `id` and its answer away.
pub fn forget(dir: &Path, id: &RequestId) {
    let _ = std::fs::remove_file(id.json(dir));
    let _ = std::fs::remove_file(id.answer_file(dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_wait_for_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        let request = |id: &str| Request {
            id: id.into(),
            agent: "a".into(),
            sql: "update t set x = 1 where id = 1".into(),
            connection: "c".into(),
            production: false,
            dry_run: String::new(),
        };
        ask(dir.path(), &request("2")).unwrap();
        ask(dir.path(), &request("1")).unwrap();
        assert_eq!(pending(dir.path()).iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(read_answer(dir.path(), &"1".into()), None);
        assert!(answer(dir.path(), &"1".into(), true));
        assert_eq!(read_answer(dir.path(), &"1".into()), Some(true));
        assert_eq!(pending(dir.path()).len(), 1, "answered: no longer asked about");
        assert!(answer(dir.path(), &"2".into(), false));
        assert_eq!(read_answer(dir.path(), &"2".into()), Some(false));
        forget(dir.path(), &"1".into());
        // The agent stopped waiting: nothing to answer.
        assert!(!answer(dir.path(), &"1".into(), true));
        assert_eq!(read_answer(dir.path(), &"1".into()), None);
        // Taken back at the deadline: an answer given before still counts.
        ask(dir.path(), &request("3")).unwrap();
        assert!(answer(dir.path(), &"3".into(), true));
        withdraw(dir.path(), &"3".into());
        assert_eq!(read_answer(dir.path(), &"3".into()), Some(true));
        assert!(!answer(dir.path(), &"3".into(), false), "too late now");
    }

    #[test]
    fn a_request_whose_id_would_leave_the_folder_isnt_taken() {
        let dir = tempfile::tempdir().unwrap();
        let pending = dir.path().join("p.pending");
        std::fs::create_dir(&pending).unwrap();
        for id in ["../escape", "a/b", "", "a\\b", ".hidden"] {
            let text = format!(
                r#"{{"id": "{id}", "agent": "a", "connection": "c", "production": false, "sql": "", "dry_run": ""}}"#
            );
            std::fs::write(pending.join(format!("x{}.json", id.replace(['/', '.', '\\'], "_"))), text).unwrap();
        }
        // One named otherwise than its id: its answer would never be seen.
        let text = r#"{"id": "b", "agent": "a", "connection": "c", "production": false, "sql": "", "dry_run": ""}"#;
        std::fs::write(pending.join("a.json"), text).unwrap();
        assert!(super::pending(&pending).is_empty(), "none of them is a request");
        let made = RequestId::new("sess/1 ..");
        assert!(RequestId::try_from(made.as_str().to_string()).is_ok(), "a new one is always safe: {made:?}");
    }
}
