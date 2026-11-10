//! The ontology as the app's browser lists it: by table, then terms,
//! suggestions and stale facts.

use std::collections::BTreeMap;

use super::{Ontology, Resolved};
use crate::memory::Fact;

/// What a section of the outline is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Table,
    View,
    Column,
    Term,
    /// A model's guesses, not yet confirmed.
    Suggested,
    /// Facts about what this database doesn't have.
    Stale,
}

impl SectionKind {
    /// As the gateway sends it.
    pub fn as_str(self) -> &'static str {
        match self {
            SectionKind::Table => "table",
            SectionKind::View => "view",
            SectionKind::Column => "column",
            SectionKind::Term => "term",
            SectionKind::Suggested => "suggested",
            SectionKind::Stale => "stale",
        }
    }

    /// As the gateway sent it.
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Table, Self::View, Self::Column, Self::Term, Self::Suggested, Self::Stale]
            .into_iter()
            .find(|k| k.as_str() == text)
    }
}

/// A part of the ontology, as the app's browser shows it: a table, one of
/// its columns, a term, or the suggestions or stale facts, with its facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    /// As a fact about it is written (`orders`, `orders.status`, `revenue`);
    /// empty for the suggestions and the stale facts.
    pub subject: String,
    pub facts: Vec<Fact>,
}

/// The ontology for the browser: every table and view (so a fact can be
/// added to any), each followed by its columns that have facts, then the
/// terms, then the model's suggestions and the stale facts.
pub fn outline(ontology: &Ontology, facts: &[Fact]) -> Vec<Section> {
    use super::Kind;
    let mut by_subject: BTreeMap<String, Vec<Fact>> = BTreeMap::new();
    let (mut suggested, mut stale) = (Vec::new(), Vec::new());
    for f in facts {
        if !f.confirmed {
            suggested.push(f.clone());
        } else if ontology.resolve(&f.subject) == Resolved::Missing {
            stale.push(f.clone());
        } else {
            by_subject.entry(ontology.subject_key(&f.subject)).or_default().push(f.clone());
        }
    }
    let mut out = Vec::new();
    let mut tables: Vec<&super::Concept> =
        ontology.concepts.values().filter(|c| matches!(c.kind, Kind::Table | Kind::View) && !c.system).collect();
    tables.sort_by_key(|c| ontology.short_name(&c.id).map(|(n, _)| n.to_lowercase()));
    for table in tables {
        let Some((name, _)) = ontology.short_name(&table.id) else { continue };
        let kind = if table.kind == Kind::View { SectionKind::View } else { SectionKind::Table };
        let own = by_subject.remove(table.id.as_str()).unwrap_or_default();
        out.push(Section { kind, subject: name.clone(), facts: own });
        let columns = ontology.relations_from(&table.id).filter(|r| r.kind == super::RelationKind::HasColumn);
        for r in columns {
            if let Some(facts) = by_subject.remove(r.to.as_str()) {
                let column = ontology.short_name(&r.to).and_then(|(_, c)| c).unwrap_or_default();
                out.push(Section { kind: SectionKind::Column, subject: format!("{name}.{column}"), facts });
            }
        }
    }
    // What's left: terms, and tables or columns the list above doesn't
    // reach (a system table; a column of a table the catalog gave no
    // columns for), each under what it is, not as a term. Those first.
    let (terms, others): (Vec<_>, Vec<_>) = by_subject.into_iter().partition(|(key, _)| key.starts_with("term:"));
    for (key, facts) in others {
        let kind = match key.split_once(':').map(|(kind, _)| kind) {
            Some("view") => SectionKind::View,
            Some("column") => SectionKind::Column,
            _ => SectionKind::Table,
        };
        let id = super::ConceptId(key);
        let subject = match ontology.short_name(&id) {
            Some((table, Some(column))) => format!("{table}.{column}"),
            Some((table, None)) => table,
            None => facts[0].subject.clone(),
        };
        out.push(Section { kind, subject, facts });
    }
    for (key, facts) in terms {
        let subject = key.strip_prefix("term:").map_or_else(|| facts[0].subject.clone(), str::to_string);
        out.push(Section { kind: SectionKind::Term, subject, facts });
    }
    if !suggested.is_empty() {
        out.push(Section { kind: SectionKind::Suggested, subject: String::new(), facts: suggested });
    }
    if !stale.is_empty() {
        out.push(Section { kind: SectionKind::Stale, subject: String::new(), facts: stale });
    }
    out
}
