//! The ontology of a database: what's in it, and what it means.
//!
//! A small typed graph, not RDF (see podelis-planai, ontology-model.md):
//!
//! - **Concepts:** the catalog's tables, views and columns, and the terms
//!   people use that name none of them (`revenue`). Each has a stable id
//!   (`table:public.orders`, `column:public.orders.status`, `term:revenue`)
//!   and labels: its own name (preferred, as SKOS's `prefLabel`) and the
//!   other names people use (`also called` facts: `altLabel`).
//! - **Relations** between them: a table has its columns, a foreign key
//!   references another table (by which columns), a partition is part of
//!   its partitioned table.
//! - **Assertions:** what the team has said (memory's facts), each about a
//!   concept, with where it came from and whether it's confirmed.
//!
//! It's filled by sources ([`Ontology::from_catalog`], then
//! [`Ontology::with_facts`]) and knows nothing of SQL beyond that: the
//! context builder asks it what a fact's subject names, and the
//! interchange exports ([`Ontology::to_turtle`]) read it. Never rows.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::context::Schema;
use crate::memory::{ALSO_CALLED, DEFINED_AS, EXCLUDED_BY_DEFAULT, Fact};

/// What a concept is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Table,
    View,
    Column,
    /// A word people use that names no table or column: a business term
    /// (`revenue`), known from the facts about it.
    Term,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::View => "view",
            Kind::Column => "column",
            Kind::Term => "term",
        }
    }
}

/// A concept's stable id: its kind and its qualified name, lowercased
/// (`table:public.orders`). Names are what facts and questions use;
/// a rename is a new concept, and facts about the old one go stale.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConceptId(String);

impl ConceptId {
    fn new(kind: Kind, name: &str) -> Self {
        ConceptId(format!("{}:{}", kind.prefix(), name.to_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where something in the ontology came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The database's own catalog.
    Catalog,
    /// A fact in the project's memory: its number, who said it (`user`,
    /// `model`, a migrations folder), and whether it's confirmed.
    Fact { id: i64, source: String, confirmed: bool },
}

impl Origin {
    fn of(fact: &Fact) -> Self {
        Origin::Fact { id: fact.id, source: fact.source.clone(), confirmed: fact.confirmed }
    }
}

/// Which name a label is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// Its own name (SKOS `prefLabel`).
    Preferred,
    /// Another name people use for it (`also called`; SKOS `altLabel`).
    Alternative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub kind: LabelKind,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concept {
    pub id: ConceptId,
    pub kind: Kind,
    /// Part of the database's own catalog (`pg_catalog`, `sqlite_schema`…),
    /// or one of its columns: facts may name it, but it isn't shown.
    pub system: bool,
    /// As SQL names it: `public.orders`, `public.orders.status`, `revenue`.
    pub name: String,
    pub labels: Vec<Label>,
}

/// How two concepts relate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationKind {
    /// A table (or view) has a column.
    HasColumn,
    /// A foreign key: `from`'s columns point at `to`'s, pair by pair (an
    /// empty target: `to`'s primary key, unnamed).
    References { columns: Vec<(String, String)> },
    /// A partition is part of its partitioned table (an is-a: queries
    /// read the parent).
    PartitionOf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub from: ConceptId,
    pub to: ConceptId,
    pub kind: RelationKind,
    pub origin: Origin,
}

/// What a fact says of its concept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Says {
    /// What it means or is (`means`, `is`, `are`, `equals`: the word kept).
    Means { verb: String, what: String },
    /// A business term's formula or filter.
    DefinedAs(String),
    /// Rows left out unless a question asks for them: the condition.
    ExcludedByDefault(String),
    /// What happened to it, from migrations.
    History(String),
}

impl Says {
    fn of(fact: &Fact) -> Option<Self> {
        let value = fact.value.clone();
        Some(match fact.relation.as_str() {
            ALSO_CALLED => return None,
            DEFINED_AS => Says::DefinedAs(value),
            EXCLUDED_BY_DEFAULT => Says::ExcludedByDefault(value),
            "history" => Says::History(value),
            verb => Says::Means { verb: verb.to_string(), what: value },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub about: ConceptId,
    pub says: Says,
    pub origin: Origin,
    /// When it was said (RFC 3339).
    pub at: String,
}

/// What a fact's subject names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A table, view or column of this database.
    Concept(ConceptId),
    /// A single word that names none of them: a term.
    Term(String),
    /// A table or column this database doesn't have (dropped or renamed
    /// since, or a typo): a fact about it is stale.
    Missing,
}

/// One database's ontology.
#[derive(Debug, Default, Clone)]
pub struct Ontology {
    pub concepts: BTreeMap<ConceptId, Concept>,
    pub relations: Vec<Relation>,
    pub assertions: Vec<Assertion>,
    /// Facts whose subject names a table or column that isn't here.
    pub stale: Vec<Fact>,
    /// `schema.table`, `table` and its singular, lowercased, to the tables
    /// they name: a bare name may name one in each of several schemas.
    tables: BTreeMap<String, Vec<ConceptId>>,
    /// Which of `relations` go from, and to, each concept: so a concept's
    /// relations are found without reading them all (a schema has a
    /// `HasColumn` per column).
    from: BTreeMap<ConceptId, Vec<usize>>,
    to: BTreeMap<ConceptId, Vec<usize>>,
}

impl Ontology {
    /// The concepts and relations the catalog gives: tables, views, their
    /// columns, foreign keys and partitions. System tables aren't in it.
    pub fn from_catalog(schema: &Schema) -> Self {
        let mut o = Ontology::default();
        let mut singulars = Vec::new();
        for (t, columns) in &schema.tables {
            let name = qualified(&t.schema, &t.name);
            let kind = if t.is_view { Kind::View } else { Kind::Table };
            let table = o.add(kind, &name, &t.name, t.system);
            o.index(&name, &table);
            // A bare name finds the table too (in each schema that has one),
            // and so does its singular (`customer` for `customers`), as facts
            // and questions say it, unless a table has that name.
            if !t.schema.is_empty() {
                o.index(&t.name, &table);
            }
            if let Some(one) = crate::context::singular(&t.name.to_lowercase()) {
                singulars.push((one, table.clone()));
            }
            for c in columns {
                let column = o.add(Kind::Column, &format!("{name}.{}", c.name), &c.name, t.system);
                o.relate(&table, &column, RelationKind::HasColumn);
            }
        }
        let named: std::collections::BTreeSet<String> = o.tables.keys().cloned().collect();
        for (one, table) in singulars {
            if !named.contains(&one) {
                o.index(&one, &table);
            }
        }
        for t in schema.tables.iter().map(|(t, _)| t) {
            if let Some((schema_name, parent)) = &t.partition_of {
                let (from, to) = (o.table(&t.schema, &t.name), o.table(schema_name, parent));
                if let (Some(from), Some(to)) = (from, to) {
                    o.relate(&from, &to, RelationKind::PartitionOf);
                }
            }
        }
        // One relation per key column: two keys to one table (`from_city`,
        // `to_city`) are two relations.
        for l in &schema.links {
            let (Some(from), Some(to)) = (o.table(&l.schema, &l.table), o.table(&l.target_schema, &l.target_table))
            else {
                continue;
            };
            let target = l.target_column.clone().unwrap_or_default();
            o.relate(&from, &to, RelationKind::References { columns: vec![(l.column.clone(), target)] });
        }
        o
    }

    /// The facts added: `also called` as another label of what it names,
    /// the rest as assertions about it. A fact naming a table or column
    /// that isn't here is kept aside as stale; one about a word that names
    /// neither makes that word a term.
    pub fn with_facts(mut self, facts: &[Fact]) -> Self {
        for fact in facts {
            let about = match self.resolve(&fact.subject) {
                Resolved::Concept(id) => id,
                Resolved::Term(word) => {
                    let id = ConceptId::new(Kind::Term, &word);
                    if !self.concepts.contains_key(&id) {
                        self.add(Kind::Term, &word, &word, false);
                    }
                    id
                }
                Resolved::Missing => {
                    self.stale.push(fact.clone());
                    continue;
                }
            };
            match Says::of(fact) {
                None => {
                    if let Some(concept) = self.concepts.get_mut(&about) {
                        concept.labels.push(Label {
                            text: fact.value.trim().to_string(),
                            kind: LabelKind::Alternative,
                            origin: Origin::of(fact),
                        });
                    }
                }
                Some(says) => {
                    self.assertions.push(Assertion { about, says, origin: Origin::of(fact), at: fact.at.clone() });
                }
            }
        }
        self
    }

    /// What `subject` names: `table`, `table.column`, `schema.table` or
    /// `schema.table.column`, ignoring case. A table whose columns the
    /// catalog didn't give has every column (nothing to check against).
    pub fn resolve(&self, subject: &str) -> Resolved {
        let subject = subject.trim();
        let parts: Vec<&str> = subject.split('.').collect();
        let named = |name: &str| self.tables.get(&name.to_lowercase()).map_or(&[][..], Vec::as_slice);
        // The column of the first of `tables` that has it (a bare name may
        // name a table in several schemas).
        let column_of = |tables: &[ConceptId], column: &str| {
            tables.iter().find_map(|table| {
                let id = ConceptId::new(Kind::Column, &format!("{}.{column}", self.concepts[table].name));
                let has_columns = self.relations_from(table).any(|r| r.kind == RelationKind::HasColumn);
                (self.concepts.contains_key(&id) || !has_columns).then_some(id)
            })
        };
        let found = match parts.as_slice() {
            [one] => {
                return named(one)
                    .first()
                    .cloned()
                    .map_or_else(|| Resolved::Term((*one).to_string()), Resolved::Concept);
            }
            [first, second] => match named(first) {
                [] => named(subject).first().cloned(),
                tables => column_of(tables, second),
            },
            // A schema the catalog doesn't name (`main.orders` where SQLite's
            // tables have none): the table by its own name.
            [schema, table, column] => match named(&format!("{schema}.{table}")) {
                [] => column_of(named(table), column),
                tables => column_of(tables, column),
            },
            _ => None,
        };
        found.map_or(Resolved::Missing, Resolved::Concept)
    }

    /// The table (or view) `subject` is, or whose column it is: `None` for
    /// a term, or what this database doesn't have.
    pub fn table_of(&self, subject: &str) -> Option<ConceptId> {
        let Resolved::Concept(id) = self.resolve(subject) else { return None };
        let name = id.0.split_once(':').map_or("", |(_, name)| name);
        match name.rsplit_once('.') {
            Some((table, _)) if id.0.starts_with("column:") => [Kind::Table, Kind::View]
                .into_iter()
                .map(|kind| ConceptId::new(kind, table))
                .find(|t| self.concepts.contains_key(t)),
            _ => Some(id).filter(|id| !id.0.starts_with("term:")),
        }
    }

    /// What a subject is, however it was written: `customer`, `Customers`
    /// and `public.customers` are one table. For matching facts.
    pub fn subject_key(&self, subject: &str) -> String {
        match self.resolve(subject) {
            Resolved::Concept(id) => id.0,
            Resolved::Term(word) => ConceptId::new(Kind::Term, &word).0,
            Resolved::Missing => format!("missing:{}", subject.trim().to_lowercase()),
        }
    }

    /// A table's (or view's) name as a file writes it: its own, or with
    /// its schema where another schema has one of that name. A column's:
    /// its table's, then the column.
    pub fn short_name(&self, id: &ConceptId) -> Option<(String, Option<String>)> {
        let concept = self.concepts.get(id)?;
        let table = match concept.kind {
            Kind::Column => self.relations_to(id).find(|r| r.kind == RelationKind::HasColumn)?.from.clone(),
            _ => id.clone(),
        };
        let own = &self.concepts.get(&table)?.labels.first()?.text;
        let alone = self.tables.get(&own.to_lowercase()).is_some_and(|t| t.as_slice() == [table.clone()]);
        let name = if alone { own.clone() } else { self.concepts[&table].name.clone() };
        let column = (concept.kind == Kind::Column).then(|| concept.labels.first().map(|l| l.text.clone())).flatten();
        Some((name, column))
    }

    /// The concepts a label names, ignoring case: their own names and the
    /// other names people use (`client` finds `customers`, when it's
    /// `also called` that).
    pub fn named(&self, label: &str) -> Vec<&Concept> {
        self.concepts.values().filter(|c| c.labels.iter().any(|l| l.text.eq_ignore_ascii_case(label.trim()))).collect()
    }

    /// The assertions about a concept.
    pub fn about(&self, id: &ConceptId) -> Vec<&Assertion> {
        self.assertions.iter().filter(|a| a.about == *id).collect()
    }

    /// As text: each concept with what's known of it, for `podelis
    /// gateway ontology`. Columns are shown only where something is said
    /// of them, so a big schema stays readable.
    pub fn describe(&self) -> String {
        let count = |k: Kind| self.concepts.values().filter(|c| c.kind == k && !c.system).count();
        let mut out = format!(
            "{} tables, {} views, {} columns, {} terms; {} foreign keys; {} facts, {} other names, {} stale\n",
            count(Kind::Table),
            count(Kind::View),
            count(Kind::Column),
            count(Kind::Term),
            self.relations.iter().filter(|r| matches!(r.kind, RelationKind::References { .. })).count(),
            self.assertions.len(),
            self.concepts.values().flat_map(|c| &c.labels).filter(|l| l.kind == LabelKind::Alternative).count(),
            self.stale.len(),
        );
        for c in self.concepts.values().filter(|c| !c.system) {
            let other: Vec<&str> =
                c.labels.iter().filter(|l| l.kind == LabelKind::Alternative).map(|l| l.text.as_str()).collect();
            let said = self.about(&c.id);
            let links: Vec<String> = self
                .relations_from(&c.id)
                .filter_map(|r| match &r.kind {
                    RelationKind::References { columns } => {
                        let pairs: Vec<String> = columns
                            .iter()
                            .map(|(f, t)| if t.is_empty() { format!("{f} → its key") } else { format!("{f} → {t}") })
                            .collect();
                        Some(format!("references {} ({})", self.concepts[&r.to].name, pairs.join(", ")))
                    }
                    RelationKind::PartitionOf => Some(format!("partition of {}", self.concepts[&r.to].name)),
                    RelationKind::HasColumn => None,
                })
                .collect();
            if c.kind == Kind::Column && other.is_empty() && said.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n{} {}", c.kind.prefix(), c.name);
            if !other.is_empty() {
                let _ = writeln!(out, "  also called: {}", other.join(", "));
            }
            for l in links {
                let _ = writeln!(out, "  {l}");
            }
            for a in said {
                let mark = match &a.origin {
                    Origin::Fact { confirmed: false, .. } => " (suggested, not confirmed)",
                    _ => "",
                };
                let _ = writeln!(out, "  {}{mark}", says_line(&a.says));
            }
        }
        if !self.stale.is_empty() {
            let _ = writeln!(out, "\nStale: the table or column they name isn't in this database");
            for f in &self.stale {
                let _ = writeln!(out, "  {}", f.line());
            }
        }
        out
    }

    /// As SKOS in Turtle, for tools that read it: each concept a
    /// `skos:Concept` with its labels, columns `skos:broader` their table,
    /// what's said as `skos:definition` or `skos:note`, and the SQL parts
    /// (a term's formula, a default filter) in Podėlis's own namespace.
    /// Only confirmed facts: a suggestion isn't the team's yet.
    pub fn to_turtle(&self, base: &str) -> String {
        // `table/public.order%20items`: what an IRI can't hold, encoded.
        let iri = |id: &ConceptId| {
            let (kind, name) = id.as_str().split_once(':').unwrap_or(("", id.as_str()));
            format!("<{base}{kind}/{}>", iri_part(name))
        };
        let mut out = String::from(
            "@prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n\
             @prefix podelis: <https://github.com/taislius/podelis/ns#> .\n",
        );
        for c in self.concepts.values().filter(|c| !c.system) {
            let mut lines = vec!["a skos:Concept".to_string(), format!("podelis:kind \"{}\"", c.kind.prefix())];
            for l in &c.labels {
                if let Origin::Fact { confirmed: false, .. } = l.origin {
                    continue;
                }
                let property = match l.kind {
                    LabelKind::Preferred => "skos:prefLabel",
                    LabelKind::Alternative => "skos:altLabel",
                };
                lines.push(format!("{property} {}", literal(&l.text)));
            }
            lines.push(format!("podelis:sqlName {}", literal(&c.name)));
            for r in self.relations_to(&c.id).filter(|r| r.kind == RelationKind::HasColumn) {
                lines.push(format!("skos:broader {}", iri(&r.from)));
            }
            for r in self.relations_from(&c.id) {
                match r.kind {
                    RelationKind::References { .. } => lines.push(format!("podelis:references {}", iri(&r.to))),
                    RelationKind::PartitionOf => lines.push(format!("skos:broader {}", iri(&r.to))),
                    RelationKind::HasColumn => {}
                }
            }
            for a in self.about(&c.id) {
                if let Origin::Fact { confirmed: false, .. } = a.origin {
                    continue;
                }
                lines.push(match &a.says {
                    Says::Means { verb, what } => format!("skos:definition {}", literal(&format!("{verb} {what}"))),
                    Says::DefinedAs(sql) => format!("podelis:definedAs {}", literal(sql)),
                    Says::ExcludedByDefault(sql) => format!("podelis:excludedByDefault {}", literal(sql)),
                    Says::History(what) => format!("skos:historyNote {}", literal(what)),
                });
            }
            let _ = write!(out, "\n{} {} .\n", iri(&c.id), lines.join(" ;\n    "));
        }
        out
    }

    fn add(&mut self, kind: Kind, name: &str, label: &str, system: bool) -> ConceptId {
        let id = ConceptId::new(kind, name);
        let labels = vec![Label { text: label.to_string(), kind: LabelKind::Preferred, origin: Origin::Catalog }];
        self.concepts.insert(id.clone(), Concept { id: id.clone(), kind, system, name: name.to_string(), labels });
        id
    }

    /// `name` (lowercased) finds `table`.
    fn index(&mut self, name: &str, table: &ConceptId) {
        let tables = self.tables.entry(name.to_lowercase()).or_default();
        if !tables.contains(table) {
            tables.push(table.clone());
        }
    }

    fn relate(&mut self, from: &ConceptId, to: &ConceptId, kind: RelationKind) {
        let at = self.relations.len();
        self.relations.push(Relation { from: from.clone(), to: to.clone(), kind, origin: Origin::Catalog });
        self.from.entry(from.clone()).or_default().push(at);
        self.to.entry(to.clone()).or_default().push(at);
    }

    /// The relations from `id`.
    pub fn relations_from(&self, id: &ConceptId) -> impl Iterator<Item = &Relation> {
        self.from.get(id).into_iter().flatten().map(|&i| &self.relations[i])
    }

    /// The relations to `id`.
    pub fn relations_to(&self, id: &ConceptId) -> impl Iterator<Item = &Relation> {
        self.to.get(id).into_iter().flatten().map(|&i| &self.relations[i])
    }

    fn table(&self, schema: &str, name: &str) -> Option<ConceptId> {
        self.tables.get(&qualified(schema, name).to_lowercase()).and_then(|t| t.first().cloned())
    }
}

/// `schema.table`, or `table` where there's no schema.
fn qualified(schema: &str, table: &str) -> String {
    if schema.is_empty() { table.to_string() } else { format!("{schema}.{table}") }
}

fn says_line(says: &Says) -> String {
    match says {
        Says::Means { verb, what } => format!("{verb} {what}"),
        Says::DefinedAs(sql) => format!("defined as {sql}"),
        Says::ExcludedByDefault(sql) => format!("excluded by default {sql}"),
        Says::History(what) => format!("history: {what}"),
    }
}

/// `text` as part of an IRI path: letters, digits and `-._~` as they are,
/// every other byte percent-encoded (spaces, `>`, quotes, non-ASCII).
pub fn iri_part(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A Turtle string literal.
fn literal(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    format!("\"{escaped}\"")
}

pub mod file;
pub mod outline;
pub mod plan;

#[cfg(test)]
mod tests;
