//! What loading an edited file back would change: facts to add, confirm,
//! forget or move, matched by what they say (see [`super::file`]).

use std::collections::{HashMap, HashSet};

use super::file::Wanted;
use super::{Ontology, SubjectKey};
use crate::memory::Fact;

/// What loading the file would change.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Facts the file has that the memory doesn't: added, the user's.
    pub add: Vec<Wanted>,
    /// A model's guesses the file confirms.
    pub confirm: Vec<Fact>,
    /// Facts no longer in the file.
    pub forget: Vec<Fact>,
    /// Facts moved between this database and every database: the fact,
    /// and whether it's about every database now.
    pub moved: Vec<(Fact, bool)>,
    /// Facts the file doesn't have, kept: said after it was exported, or
    /// all of them when the file doesn't say when that was.
    pub kept: usize,
    /// The file doesn't say when it was exported (`exported` taken out,
    /// or a file written by hand): what it lacks isn't forgotten, as it
    /// can't be told from what was added since.
    pub undated: bool,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.confirm.is_empty() && self.forget.is_empty() && self.moved.is_empty()
    }
}

/// What a fact says, as facts are matched: what it's about (however it's
/// written: `customer` is `customers`), the relation (ignoring case) and
/// the value.
type Said = (SubjectKey, String, String);

fn said(ontology: &Ontology, subject: &str, relation: &str, value: &str) -> Said {
    (ontology.fact_key(subject, relation), relation.trim().to_lowercase(), value.trim().to_string())
}

/// When (RFC 3339).
fn time(text: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(text.trim()).ok()
}

/// The changes from `current` (the memory's facts now) to what the file
/// says. Each fact in the memory answers at most one line of the file,
/// the one that says it exactly first (its scope, and whether it's
/// confirmed), so a fact said both for this database and for every
/// database stays two. A fact the file doesn't
/// have is forgotten, unless it was said after the file was `exported`
/// (or the file doesn't say when that was, or the fact when it was said).
/// A confirmed fact the file only lists as a suggestion stays confirmed:
/// a suggestion can be accepted or rejected, not taken back.
pub fn plan(ontology: &Ontology, current: &[Fact], wanted: &[Wanted], exported: Option<&str>) -> Plan {
    let mut plan = Plan::default();
    let mut used = vec![false; current.len()];
    // The memory's facts by what they say: each line of the file finds
    // its own at once, not by reading them all.
    let mut by_said: HashMap<Said, Vec<usize>> = HashMap::new();
    for (i, f) in current.iter().enumerate() {
        by_said.entry(said(ontology, &f.subject, &f.relation, &f.value)).or_default().push(i);
    }
    let mut added: HashSet<(Said, Option<bool>)> = HashSet::new();
    let same_scope = |f: &Fact, w: &Wanted| w.everywhere.is_none_or(|every| every == f.database.is_empty());
    for w in wanted {
        let key = said(ontology, &w.subject, &w.relation, &w.value);
        let candidates = by_said.get(&key).map_or(&[][..], Vec::as_slice);
        let free = || candidates.iter().copied().filter(|&i| !used[i]);
        // The one it says exactly first (scope and whether it's confirmed),
        // then one in its scope, then any: an unchanged file then finds
        // each fact its own line, even a fact both suggested and confirmed.
        let same_confirmed = |i: usize| current[i].confirmed == w.confirmed;
        let found = free()
            .find(|&i| same_scope(&current[i], w) && same_confirmed(i))
            .or_else(|| free().find(|&i| same_scope(&current[i], w)))
            .or_else(|| free().find(|&i| same_confirmed(i)))
            .or_else(|| free().next());
        let Some(i) = found else {
            // Two the same in the file: the second adds nothing more.
            if added.insert((key, w.everywhere)) {
                plan.add.push(Wanted { confirmed: true, ..w.clone() });
            }
            continue;
        };
        used[i] = true;
        let f = &current[i];
        if !same_scope(f, w) {
            plan.moved.push((f.clone(), w.everywhere == Some(true)));
        }
        if w.confirmed && !f.confirmed {
            plan.confirm.push(f.clone());
        }
    }
    let exported = exported.and_then(time);
    for (i, f) in current.iter().enumerate() {
        if used[i] {
            continue;
        }
        let older = match (exported, time(&f.at)) {
            (Some(exported), Some(at)) => at <= exported,
            (None, _) => {
                plan.undated = true;
                false
            }
            // A fact that doesn't say when: not known to be in the file.
            (Some(_), None) => false,
        };
        if older {
            plan.forget.push(f.clone());
        } else {
            plan.kept += 1;
        }
    }
    plan
}
