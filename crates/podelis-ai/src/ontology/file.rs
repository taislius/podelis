//! The ontology as a YAML file, to edit in an editor and load back:
//! `podelis ontology <connection> export`, then `… import <file>`.
//!
//! ```yaml
//! database: shop
//! tables:
//!   orders:
//!     excluded_by_default: status = 'cancelled'
//!     columns:
//!       status:
//!         means: 3 = cancelled
//!   customers:
//!     also_called: [client, buyer]
//! terms:
//!   revenue:
//!     defined_as: sum(order_items.quantity * products.price)
//! suggested:
//!   - orders.total is with VAT
//! ```
//!
//! What a table, column or term says is its facts: one value, or a list.
//! `every_database` has the same `tables` and `terms`, for facts about
//! every database in the project (a business term all of them share);
//! the rest is about this one. Moving a fact between them moves it.
//! `suggested` holds a model's guesses, not shown to models: moved into
//! `tables` or `terms` it's confirmed, deleted it's rejected. `stale` holds
//! facts about a table or column the database doesn't have, and `other`
//! confirmed facts with a relation the file has no key for; each of the
//! three a list of `about`, `relation`, `value` (and `every_database`).
//! `exported` says when the file was written: a fact added to the memory
//! since then isn't forgotten for not being in it. Facts are
//! matched by what they say, so loading the file back changes only what
//! was edited: added facts are the user's (confirmed) and about this
//! database, facts no longer in the file are forgotten.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Ontology, Resolved};
use crate::memory::{ALSO_CALLED, DEFINED_AS, EXCLUDED_BY_DEFAULT, Fact};

/// One value, or several. Written by hand, a value YAML would read as a
/// number or a yes/no (`means: 3`, `means: y`) is taken as the text it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum Values {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

/// A scalar as text, whatever YAML made of it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
struct Text(String);

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Text;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a value (text)")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Text, E> {
                Ok(Text(v.to_string()))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Text, E> {
                Ok(Text(v.to_string()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Text, E> {
                Ok(Text(v.to_string()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Text, E> {
                Ok(Text(v.to_string()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Text, E> {
                Ok(Text(v.to_string()))
            }
        }
        d.deserialize_any(Visitor)
    }
}

impl<'de> Deserialize<'de> for Values {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Many(Vec<Text>),
            One(Text),
        }
        Ok(match Option::<Raw>::deserialize(d)? {
            None => Values::None,
            Some(Raw::One(Text(one))) => Values::One(one),
            Some(Raw::Many(all)) => Values::Many(all.into_iter().map(|Text(t)| t).collect()),
        })
    }
}

impl Values {
    fn is_none(&self) -> bool {
        matches!(self, Values::None)
    }

    fn push(&mut self, value: String) {
        *self = match std::mem::take(self) {
            Values::None => Values::One(value),
            Values::One(first) => Values::Many(vec![first, value]),
            Values::Many(mut all) => {
                all.push(value);
                Values::Many(all)
            }
        };
    }

    fn all(&self) -> Vec<&str> {
        match self {
            Values::None => Vec::new(),
            Values::One(one) => vec![one.as_str()],
            Values::Many(all) => all.iter().map(String::as_str).collect(),
        }
    }
}

/// What's said of a table, a column or a term: a key per relation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    #[serde(default, skip_serializing_if = "Values::is_none")]
    means: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    is: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    are: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    equals: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    also_called: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    defined_as: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    excluded_by_default: Values,
    #[serde(default, skip_serializing_if = "Values::is_none")]
    history: Values,
    /// A table's columns (none for a column or a term).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    columns: BTreeMap<String, Said>,
}

/// The relations, as facts name them and as the file's keys do.
const RELATIONS: [(&str, &str); 8] = [
    ("means", "means"),
    ("is", "is"),
    ("are", "are"),
    ("equals", "equals"),
    (ALSO_CALLED, "also_called"),
    (DEFINED_AS, "defined_as"),
    (EXCLUDED_BY_DEFAULT, "excluded_by_default"),
    ("history", "history"),
];

impl Said {
    fn slot(&mut self, relation: &str) -> Option<&mut Values> {
        Some(match relation {
            "means" => &mut self.means,
            "is" => &mut self.is,
            "are" => &mut self.are,
            "equals" => &mut self.equals,
            ALSO_CALLED => &mut self.also_called,
            DEFINED_AS => &mut self.defined_as,
            EXCLUDED_BY_DEFAULT => &mut self.excluded_by_default,
            "history" => &mut self.history,
            _ => return None,
        })
    }

    /// (relation, value) for each fact it holds.
    fn facts(&self) -> Vec<(&'static str, &str)> {
        let slots = [
            &self.means,
            &self.is,
            &self.are,
            &self.equals,
            &self.also_called,
            &self.defined_as,
            &self.excluded_by_default,
            &self.history,
        ];
        RELATIONS
            .iter()
            .zip(slots)
            .flat_map(|((relation, _), values)| values.all().into_iter().map(|v| (*relation, v)))
            .collect()
    }
}

/// Tables (with their columns) and terms, and what's said of them.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tables: BTreeMap<String, Said>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terms: BTreeMap<String, Said>,
}

impl Part {
    fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.terms.is_empty()
    }
}

/// A fact as a list entry: `suggested`, `stale`, `other`. Kept apart,
/// not as a sentence, so it reads back exactly as written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    about: String,
    relation: String,
    value: Text,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    every_database: bool,
}

impl Entry {
    fn of(fact: &Fact) -> Self {
        Entry {
            about: fact.subject.clone(),
            relation: fact.relation.clone(),
            value: Text(fact.value.clone()),
            every_database: fact.database.is_empty(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    /// When it was written (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exported: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tables: BTreeMap<String, Said>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terms: BTreeMap<String, Said>,
    /// About every database in the project, not this one alone.
    #[serde(default, skip_serializing_if = "Part::is_empty")]
    every_database: Part,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    suggested: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stale: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    other: Vec<Entry>,
}

/// The facts of `ontology` (and its stale ones) as the file: what's
/// confirmed under `tables` and `terms`, a model's guesses under
/// `suggested`. `title` goes first, as comments.
pub fn export(ontology: &Ontology, facts: &[Fact], database: &str, title: &str) -> String {
    let exported = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let mut file = File { database: Some(database.to_string()), exported: Some(exported), ..File::default() };
    for fact in facts {
        if !fact.confirmed {
            file.suggested.push(Entry::of(fact));
            continue;
        }
        let (tables, terms) = if fact.database.is_empty() {
            (&mut file.every_database.tables, &mut file.every_database.terms)
        } else {
            (&mut file.tables, &mut file.terms)
        };
        let said = match ontology.resolve(&fact.subject) {
            Resolved::Missing => {
                file.stale.push(Entry::of(fact));
                continue;
            }
            Resolved::Term(word) => terms.entry(word).or_default(),
            // Under the table's own name, however the fact wrote it.
            Resolved::Concept(id) => match ontology.short_name(&id) {
                Some((table, Some(column))) => tables.entry(table).or_default().columns.entry(column).or_default(),
                Some((table, None)) => tables.entry(table).or_default(),
                None => tables.entry(fact.subject.trim().to_string()).or_default(),
            },
        };
        match said.slot(&fact.relation) {
            Some(values) => values.push(fact.value.clone()),
            // A relation the file has no key for: kept as it is.
            None => file.other.push(Entry::of(fact)),
        }
    }
    let body = serde_saphyr::to_string(&file).unwrap_or_default();
    let mut out = String::new();
    for line in title.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&body);
    out
}

/// A fact as the file says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub subject: String,
    pub relation: String,
    pub value: String,
    pub confirmed: bool,
    /// About every database (`every_database`), this one, or as it was
    /// (a suggestion's or a stale fact's line says nothing of it).
    pub everywhere: Option<bool>,
}

/// What a file says.
#[derive(Debug, Clone, Default)]
pub struct Read {
    /// The database it's the ontology of.
    pub database: Option<String>,
    /// When it was exported.
    pub exported: Option<String>,
    pub wanted: Vec<Wanted>,
}

/// The facts the file says, or what's wrong with it (with its line).
pub fn read(text: &str) -> Result<Read, String> {
    // YAML 1.2's booleans: only `true` and `false`, so `means: y` is `y`.
    let options = serde_saphyr::options! { strict_booleans: true };
    let file: File = serde_saphyr::from_str_with_options(text, options)
        .map_err(|e| format!("the file isn't an ontology as exported: {e}"))?;
    let mut wanted = Vec::new();
    let mut add = |subject: &str, relation: &str, value: &str, confirmed: bool, everywhere: Option<bool>| {
        let (subject, value) = (subject.trim(), value.trim());
        if !subject.is_empty() && !value.is_empty() {
            let (subject, relation, value) = (subject.into(), relation.into(), value.into());
            wanted.push(Wanted { subject, relation, value, confirmed, everywhere });
        }
    };
    let parts = [(&file.tables, &file.terms, false), (&file.every_database.tables, &file.every_database.terms, true)];
    for (tables, terms, everywhere) in parts {
        for (table, said) in tables {
            for (relation, value) in said.facts() {
                add(table, relation, value, true, Some(everywhere));
            }
            for (column, said) in &said.columns {
                if !said.columns.is_empty() {
                    return Err(format!("{table}.{column}: a column has no columns"));
                }
                for (relation, value) in said.facts() {
                    add(&format!("{table}.{column}"), relation, value, true, Some(everywhere));
                }
            }
        }
        for (term, said) in terms {
            if !said.columns.is_empty() {
                return Err(format!("{term}: a term has no columns"));
            }
            for (relation, value) in said.facts() {
                add(term, relation, value, true, Some(everywhere));
            }
        }
    }
    let entries =
        file.suggested.iter().map(|e| (e, false)).chain(file.stale.iter().chain(&file.other).map(|e| (e, true)));
    for (e, confirmed) in entries {
        let relation = if e.relation.trim().is_empty() { "means" } else { e.relation.trim() };
        add(&e.about, relation, &e.value.0, confirmed, Some(e.every_database));
    }
    Ok(Read { database: file.database, exported: file.exported, wanted })
}
