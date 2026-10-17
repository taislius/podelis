//! Agents' writes waiting for the user's yes or no (connections shared as
//! `AgentAccess::Approve`): a file per request, and one for its answer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where agents' writes wait for the user's answer (see [`Request`]).
pub fn pending_dir(config_dir: &Path, project_id: &str) -> PathBuf {
    config_dir.join("agents").join(format!("{project_id}.pending"))
}

/// An agent's write waiting for the user's yes or no, on a connection
/// shared as [`crate::db::AgentAccess::Approve`]. `podelis mcp` writes it to
/// `<id>.json` in the [`pending_dir`] and waits for `<id>.answer`
/// (`approve`, or anything else for no), which Podėlis writes.
///
/// This guards against an agent's mistakes, not against an agent set on
/// harm: one with a shell as the same user could write the answer itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
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
    let path = dir.join(format!("{}.json", request.id));
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
        .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The user's answer to request `id`; `false` if the agent stopped
/// waiting for it (it may have, just as the answer came: its log says).
pub fn answer(dir: &Path, id: &str, approve: bool) -> bool {
    let asked = || dir.join(format!("{id}.json")).exists();
    if !asked() {
        return false;
    }
    let written =
        crate::config::store::write_atomic(&dir.join(format!("{id}.answer")), if approve { "approve" } else { "deny" })
            .is_ok();
    written && asked()
}

/// Takes request `id` back: no answer is waited for any more.
pub fn withdraw(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
}

/// The answer to request `id`, once there is one.
pub fn read_answer(dir: &Path, id: &str) -> Option<bool> {
    std::fs::read_to_string(dir.join(format!("{id}.answer"))).ok().map(|a| a.trim() == "approve")
}

/// Clears request `id` and its answer away.
pub fn forget(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    let _ = std::fs::remove_file(dir.join(format!("{id}.answer")));
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
            ..Request::default()
        };
        ask(dir.path(), &request("2")).unwrap();
        ask(dir.path(), &request("1")).unwrap();
        assert_eq!(pending(dir.path()).iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(read_answer(dir.path(), "1"), None);
        assert!(answer(dir.path(), "1", true));
        assert_eq!(read_answer(dir.path(), "1"), Some(true));
        assert_eq!(pending(dir.path()).len(), 1, "answered: no longer asked about");
        assert!(answer(dir.path(), "2", false));
        assert_eq!(read_answer(dir.path(), "2"), Some(false));
        forget(dir.path(), "1");
        // The agent stopped waiting: nothing to answer.
        assert!(!answer(dir.path(), "1", true));
        assert_eq!(read_answer(dir.path(), "1"), None);
        // Taken back at the deadline: an answer given before still counts.
        ask(dir.path(), &request("3")).unwrap();
        assert!(answer(dir.path(), "3", true));
        withdraw(dir.path(), "3");
        assert_eq!(read_answer(dir.path(), "3"), Some(true));
        assert!(!answer(dir.path(), "3", false), "too late now");
    }
}
