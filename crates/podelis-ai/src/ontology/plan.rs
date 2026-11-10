//! What loading an edited file back would change: facts to add, confirm,
//! forget or move, matched by what they say (see [`super::file`]).

use super::Ontology;
use super::file::Wanted;
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
    /// Facts the file doesn't have, kept: said after it was exported.
    pub kept: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.confirm.is_empty() && self.forget.is_empty() && self.moved.is_empty()
    }
}

/// Matched by what they say: what they're about (however it's written:
/// `customer` is `customers`, as `keys` resolved them), the relation, and
/// the value.
fn same(fact: &Fact, fact_key: &str, w: &Wanted, wanted_key: &str) -> bool {
    fact_key == wanted_key && fact.relation.trim().eq_ignore_ascii_case(&w.relation) && fact.value.trim() == w.value
}

/// Whether `fact` was said after `exported` (both RFC 3339): added to the
/// memory after the file was written, so not in it.
fn newer(fact: &Fact, exported: Option<&str>) -> bool {
    let parse = |t: &str| chrono::DateTime::parse_from_rfc3339(t.trim()).ok();
    match (parse(&fact.at), exported.and_then(parse)) {
        (Some(at), Some(exported)) => at > exported,
        _ => false,
    }
}

/// The changes from `current` (the memory's facts now) to what the file
/// says. Each fact in the memory answers at most one line of the file,
/// the one about the same scope first, so a fact said both for this
/// database and for every database stays two. A fact the file doesn't
/// have is forgotten, unless it was said after the file was `exported`.
/// A confirmed fact the file only lists as a suggestion stays confirmed:
/// a suggestion can be accepted or rejected, not taken back.
pub fn plan(ontology: &Ontology, current: &[Fact], wanted: &[Wanted], exported: Option<&str>) -> Plan {
    let mut plan = Plan::default();
    let mut used = vec![false; current.len()];
    // What each is about, resolved once (not for every pair).
    let current_keys: Vec<String> = current.iter().map(|f| ontology.subject_key(&f.subject)).collect();
    let mut added_keys: Vec<String> = Vec::new();
    let same_scope = |f: &Fact, w: &Wanted| w.everywhere.is_none_or(|every| every == f.database.is_empty());
    for w in wanted {
        let key = ontology.subject_key(&w.subject);
        let free = |i: &usize| !used[*i] && same(&current[*i], &current_keys[*i], w, &key);
        let found = (0..current.len())
            .filter(free)
            .find(|i| same_scope(&current[*i], w))
            .or_else(|| (0..current.len()).find(free));
        let Some(i) = found else {
            // Two the same in the file: the second adds nothing more.
            let again = plan.add.iter().zip(&added_keys).any(|(a, a_key)| {
                *a_key == key && a.relation == w.relation && a.value == w.value && a.everywhere == w.everywhere
            });
            if !again {
                plan.add.push(Wanted { confirmed: true, ..w.clone() });
                added_keys.push(key);
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
    for (i, f) in current.iter().enumerate() {
        if used[i] {
            continue;
        }
        if newer(f, exported) {
            plan.kept += 1;
        } else {
            plan.forget.push(f.clone());
        }
    }
    plan
}
