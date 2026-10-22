//! What a model is shown of a database for a question: the tables that
//! matter, not the whole catalog.
//!
//! 1. **Entry points:** tables scored by the question's words against
//!    their names, their columns' names and the database's comments.
//! 2. **Their neighbours** through foreign keys, so joins can be written:
//!    tables linked to two chosen ones (`order_items` between `orders` and
//!    `products`), or to a chosen one and another neighbour (`rental`
//!    between `customer` and `inventory`), first, then those linked to one.
//!    A partition isn't a table of its own here: its keys are its
//!    parent's, and queries read the parent.
//! 3. **Rendered compactly,** within a budget: each table with its columns,
//!    keys, where its foreign keys point, and its comments.
//!
//! Never rows: they're private, and text in them could steer a model.
//! Each table chosen says why, so the user can see what the model saw.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::db::{ColumnInfo, Comment, Link, TableInfo};

/// The most tables matched by words, before their neighbours.
pub const MAX_ENTRY: usize = 8;
/// The most tables in all.
pub const MAX_TABLES: usize = 16;
/// A schema this small goes in whole when no table matches.
const SMALL_SCHEMA: usize = 12;
/// About the most text a context takes (≈ 4 characters a token).
pub const MAX_CHARS: usize = 16_000;
/// The most columns listed for one table.
const MAX_COLUMNS: usize = 40;
/// A comment is cut at this many characters.
const MAX_COMMENT: usize = 200;

/// The database as the builder sees it.
pub struct Schema {
    /// Its name, for the model ("PostgreSQL").
    pub engine: String,
    /// For parsing SQL written for it.
    pub dialect: crate::db::Engine,
    pub tables: Vec<(TableInfo, Vec<ColumnInfo>)>,
    pub links: Vec<Link>,
    pub comments: Vec<Comment>,
}

/// A table chosen, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// `schema.table` (or `table` where there's no schema).
    pub name: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub text: String,
    pub tables: Vec<Chosen>,
    /// Tables left out for the budget, though they qualified.
    pub left_out: usize,
    /// The team's facts it shows (their lines).
    pub facts: Vec<String>,
}

impl Context {
    /// Adds what the team has said about these tables (confirmed facts),
    /// as long as they fit the budget.
    pub fn with_facts(mut self, facts: &[super::memory::Fact]) -> Self {
        if facts.is_empty() || self.tables.is_empty() {
            return self;
        }
        let mut section =
            String::from("\nWhat this team has said about its data (confirmed; follow it over guesses):\n");
        for f in facts {
            let line = format!("- {}\n", f.line());
            if self.text.len() + section.len() + line.len() > MAX_CHARS + MAX_CHARS / 4 {
                break;
            }
            section.push_str(&line);
            self.facts.push(f.line());
        }
        if !self.facts.is_empty() {
            self.text.push_str(&section);
        }
        self
    }
}

/// The words of `text`, lowercase, identifiers split at `_` (not part of
/// a word) and at case changes (`orderItems` → order, items), each also
/// without a plural ending, and without `-ed`, `-ing` or `-al` (`rented`
/// and `rental` → rent).
fn words(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |w: String| {
        if w.chars().count() < 2 || STOP.contains(&w.as_str()) {
            return;
        }
        let one = singular(&w);
        if let Some(stem) = stem(one.as_deref().unwrap_or(&w)) {
            out.insert(stem);
        }
        if let Some(one) = one {
            out.insert(one);
        }
        out.insert(w);
    };
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        let mut part = String::new();
        let mut prev_lower = false;
        for c in token.chars() {
            if c.is_uppercase() && prev_lower && !part.is_empty() {
                add(std::mem::take(&mut part));
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            part.extend(c.to_lowercase());
        }
        if !part.is_empty() {
            add(part);
        }
    }
    out
}

/// Words that say nothing about tables.
const STOP: &[&str] = &[
    "the", "of", "in", "on", "at", "to", "for", "by", "with", "and", "or", "an", "is", "are", "was", "were", "be",
    "how", "many", "much", "what", "which", "who", "whose", "when", "where", "show", "list", "give", "get", "find",
    "me", "all", "each", "per", "from", "that", "this", "these", "those", "than", "over", "under", "it", "its", "my",
    "our", "id", "ids", "select", "count", "sum", "top", "last", "first", "number",
];

/// `customers` → customer, `categories` → category, `addresses` → address.
fn singular(w: &str) -> Option<String> {
    if w.len() > 4 && w.ends_with("ies") {
        return Some(format!("{}y", &w[..w.len() - 3]));
    }
    if w.len() > 4 && (w.ends_with("sses") || w.ends_with("xes") || w.ends_with("ches") || w.ends_with("shes")) {
        return Some(w[..w.len() - 2].to_string());
    }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
        return Some(w[..w.len() - 1].to_string());
    }
    None
}

/// `rented`, `rental` → rent; `returned` → return; `billing` → bill;
/// `shipped` → ship. Only stems of four letters or more: shorter ones
/// (`used` → us) would match too much.
fn stem(w: &str) -> Option<String> {
    let base = ["ing", "ed", "al"].iter().find_map(|suffix| w.strip_suffix(suffix))?;
    let mut base = base.to_string();
    // A doubled last letter: `shipp` → ship (not `bill`, `pass`, `buzz`,
    // `staff`).
    let mut last = base.chars().rev();
    if let (Some(a), Some(b)) = (last.next(), last.next())
        && a == b
        && !"flsz".contains(a)
    {
        base.pop();
    }
    (base.chars().count() >= 4).then_some(base)
}

fn qualified(t: &TableInfo) -> String {
    if t.schema.is_empty() { t.name.clone() } else { format!("{}.{}", t.schema, t.name) }
}

fn key(schema: &str, table: &str) -> (String, String) {
    (schema.to_lowercase(), table.to_lowercase())
}

/// A table's score for the question's words, and what matched.
/// Foreign-key columns don't count: `orders.customer_id` is about
/// customers, and the key brings `orders` in as their neighbour anyway.
fn score(
    question: &BTreeSet<String>,
    table: &TableInfo,
    columns: &[ColumnInfo],
    comments: &[&Comment],
    keys: &Keys,
) -> (u32, Vec<String>) {
    let mut score = 0;
    let mut matched = Vec::new();
    let hits = |of: &BTreeSet<String>| of.intersection(question).cloned().collect::<Vec<_>>();
    let by_name = hits(&words(&table.name));
    if !by_name.is_empty() {
        score += 10 * by_name.len() as u32;
        matched.push(format!("its name ({})", by_name.join(", ")));
    }
    let mut by_column = Vec::new();
    for c in columns.iter().filter(|c| keys.of(table, &c.name).is_none()) {
        if !hits(&words(&c.name)).is_empty() {
            by_column.push(c.name.clone());
        }
    }
    if !by_column.is_empty() {
        score += 3 * by_column.len().min(5) as u32;
        by_column.truncate(4);
        matched.push(format!("columns {}", by_column.join(", ")));
    }
    let said: BTreeSet<String> = comments.iter().flat_map(|c| words(&c.text)).collect();
    let by_comment = hits(&said);
    if !by_comment.is_empty() {
        score += 2 * by_comment.len().min(5) as u32;
        matched.push(format!("its comments ({})", by_comment.join(", ")));
    }
    (score, matched)
}

/// Foreign-key columns, looked up by (schema, table, column) lowercased:
/// built once per question rather than searched for each column.
struct Keys<'a>(HashMap<(String, String, String), &'a Link>);

impl<'a> Keys<'a> {
    fn of_links(links: &'a [Link]) -> Self {
        Keys(
            links
                .iter()
                .map(|l| ((l.schema.to_lowercase(), l.table.to_lowercase(), l.column.to_lowercase()), l))
                .collect(),
        )
    }

    /// Where `table.column` points, if it's a foreign key.
    fn of(&self, table: &TableInfo, column: &str) -> Option<&'a Link> {
        self.0.get(&(table.schema.to_lowercase(), table.name.to_lowercase(), column.to_lowercase())).copied()
    }
}

/// Each partition's top table: its parent, or its parent's parent when
/// partitions are partitioned again (`payment_2022_eu` → `payment_2022`
/// → `payment`).
fn roots(schema: &Schema) -> HashMap<(String, String), (String, String)> {
    let parent: HashMap<(String, String), &(String, String)> = schema
        .tables
        .iter()
        .filter_map(|(t, _)| t.partition_of.as_ref().map(|p| (key(&t.schema, &t.name), p)))
        .collect();
    parent
        .iter()
        .map(|(child, &first)| {
            let mut top = first;
            // A catalog can't loop, but a bound costs nothing.
            for _ in 0..32 {
                match parent.get(&key(&top.0, &top.1)) {
                    Some(&up) => top = up,
                    None => break,
                }
            }
            (child.clone(), top.clone())
        })
        .collect()
}

/// The links with a partition's as its top table's (`payment_p2022_01` →
/// customer as `payment` → customer), each once.
fn parents_links(schema: &Schema, roots: &HashMap<(String, String), (String, String)>) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    for l in &schema.links {
        let mut l = l.clone();
        if let Some((s, t)) = roots.get(&key(&l.schema, &l.table)) {
            (l.schema, l.table) = (s.clone(), t.clone());
        }
        if let Some((s, t)) = roots.get(&key(&l.target_schema, &l.target_table)) {
            (l.target_schema, l.target_table) = (s.clone(), t.clone());
        }
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// The tables a foreign key joins to `entry` ones, best first: each with
/// the entry tables it links, and the other such tables it links too
/// (`rental` joins `customer`, chosen, to `inventory`, a neighbour of
/// `film`), counted only between neighbours of tables matched `by_name`
/// (two neighbours of tables matched only by a column are too far from
/// the question). Linking more first, then the order of the schema.
fn neighbours(
    tables: &[&(TableInfo, Vec<ColumnInfo>)],
    links: &[Link],
    entry: &[usize],
    by_name: &[usize],
) -> Vec<(usize, BTreeSet<usize>, Vec<usize>)> {
    let index: HashMap<(String, String), usize> =
        tables.iter().enumerate().map(|(i, (t, _))| (key(&t.schema, &t.name), i)).collect();
    let pairs: Vec<(usize, usize)> = links
        .iter()
        .filter_map(|l| {
            Some((*index.get(&key(&l.schema, &l.table))?, *index.get(&key(&l.target_schema, &l.target_table))?))
        })
        .filter(|(from, to)| from != to)
        .collect();
    let mut linked: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    for &(from, to) in &pairs {
        if entry.contains(&from) && !entry.contains(&to) {
            linked.entry(to).or_default().insert(from);
        }
        if entry.contains(&to) && !entry.contains(&from) {
            linked.entry(from).or_default().insert(to);
        }
    }
    let near = |i: &usize| linked.get(i).is_some_and(|to| to.iter().any(|j| by_name.contains(j)));
    let mut beside: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    for &(from, to) in &pairs {
        if near(&from) && near(&to) {
            beside.entry(from).or_default().insert(to);
            beside.entry(to).or_default().insert(from);
        }
    }
    let mut ranked: Vec<(usize, BTreeSet<usize>, Vec<usize>)> = linked
        .into_iter()
        .map(|(i, to)| (i, to, beside.get(&i).map(|b| b.iter().copied().collect()).unwrap_or_default()))
        .collect();
    ranked.sort_by(|a, b| {
        (b.1.len() + b.2.len()).cmp(&(a.1.len() + a.2.len())).then(b.1.len().cmp(&a.1.len())).then(a.0.cmp(&b.0))
    });
    ranked
}

/// The context for `question`.
pub fn build(schema: &Schema, question: &str) -> Context {
    let question = words(question);
    let tables: Vec<&(TableInfo, Vec<ColumnInfo>)> =
        schema.tables.iter().filter(|(t, _)| !t.system && t.partition_of.is_none()).collect();
    let roots = roots(schema);
    let links = parents_links(schema, &roots);
    // A partitioned table keeps no rows of its own: its partitions do.
    let mut partition_rows: HashMap<(String, String), i64> = HashMap::new();
    for (t, _) in &schema.tables {
        if let (Some((s, top)), Some(rows)) = (roots.get(&key(&t.schema, &t.name)), t.rows) {
            *partition_rows.entry(key(s, top)).or_default() += rows.max(0);
        }
    }
    let mut by_table: HashMap<(String, String), Vec<&Comment>> = HashMap::new();
    for c in &schema.comments {
        by_table.entry(key(&c.schema, &c.table)).or_default().push(c);
    }
    let no_comments = Vec::new();
    let comments_of = |t: &TableInfo| by_table.get(&key(&t.schema, &t.name)).unwrap_or(&no_comments).as_slice();
    let keys = Keys::of_links(&links);

    // 1. Entry points, best first.
    let mut scored: Vec<(u32, usize, Vec<String>)> = tables
        .iter()
        .enumerate()
        .map(|(i, (t, cols))| {
            let (s, why) = score(&question, t, cols, comments_of(t), &keys);
            (s, i, why)
        })
        .filter(|(s, _, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut chosen: Vec<(usize, String)> =
        scored.iter().take(MAX_ENTRY).map(|(_, i, why)| (*i, format!("matches {}", why.join("; ")))).collect();
    let mut left_out = scored.len().saturating_sub(MAX_ENTRY);

    if chosen.is_empty() {
        if tables.len() <= SMALL_SCHEMA {
            chosen = (0..tables.len()).map(|i| (i, "the schema is small: every table".to_string())).collect();
        }
    } else {
        // 2. Neighbours through foreign keys.
        let entry: Vec<usize> = chosen.iter().map(|(i, _)| *i).collect();
        // Matched by name: sure enough to bring in what joins to them alone.
        let by_name: Vec<usize> = scored
            .iter()
            .take(MAX_ENTRY)
            .filter(|(_, _, why)| why.iter().any(|w| w.starts_with("its name")))
            .map(|(_, i, _)| *i)
            .collect();
        for (i, linked, also) in neighbours(&tables, &links, &entry, &by_name) {
            // One link, to a table matched only by a column or comment: too
            // far from the question.
            if linked.len() == 1 && also.is_empty() && !linked.iter().any(|j| by_name.contains(j)) {
                continue;
            }
            if chosen.len() >= MAX_TABLES {
                left_out += 1;
                continue;
            }
            let joined: Vec<String> = linked.iter().chain(&also).map(|&j| tables[j].0.name.clone()).collect();
            let why = if joined.len() > 1 {
                format!("links {} and {}", joined[..joined.len() - 1].join(", "), joined[joined.len() - 1])
            } else {
                format!("joins {}", joined[0])
            };
            chosen.push((i, why));
        }
    }

    // 3. Rendered, within the budget.
    let mut text = format!(
        "Database: {}. The tables that may matter for the question, with their columns \
         (PK: primary key; → where a foreign key points; \"…\": the database's comment).\n",
        schema.engine
    );
    let mut listed = Vec::new();
    for (i, why) in chosen {
        let (t, cols) = tables[i];
        let rows = match partition_rows.get(&key(&t.schema, &t.name)) {
            Some(parts) => Some(parts + t.rows.unwrap_or(0).max(0)),
            None => t.rows,
        };
        let block = render(t, rows, cols, comments_of(t), &keys);
        if text.len() + block.len() > MAX_CHARS && !listed.is_empty() {
            left_out += 1;
            continue;
        }
        text.push('\n');
        text.push_str(&block);
        listed.push(Chosen { name: qualified(t), why });
    }
    if listed.is_empty() {
        text = format!(
            "Database: {}. No table matched the question's words ({} tables): name a table or \
             column in the question.\n",
            schema.engine,
            tables.len()
        );
    }
    Context { text, tables: listed, left_out, facts: Vec::new() }
}

/// One table: its name, size (`rows`), comment, then a column a line.
fn render(t: &TableInfo, rows: Option<i64>, cols: &[ColumnInfo], comments: &[&Comment], keys: &Keys) -> String {
    let said = |column: Option<&str>| {
        comments
            .iter()
            .find(|c| c.column.as_deref().map(str::to_lowercase) == column.map(str::to_lowercase))
            .map(|c| format!(" \"{}\"", cut(&c.text)))
            .unwrap_or_default()
    };
    let kind = if t.is_view { " (view)" } else { "" };
    let rows =
        rows.filter(|r| *r >= 0).map(|r| format!(", ~{r} {}", if r == 1 { "row" } else { "rows" })).unwrap_or_default();
    let mut out = format!("{}{kind}{rows}{}\n", qualified(t), said(None));
    for c in cols.iter().take(MAX_COLUMNS) {
        let pk = if c.primary_key { " PK" } else { "" };
        let null = if c.nullable || c.primary_key { "" } else { " NOT NULL" };
        let fk = keys
            .of(t, &c.name)
            .map(|l| format!(" → {}.{}", l.target_table, l.target_column.as_deref().unwrap_or("(its key)")))
            .unwrap_or_default();
        let _ = writeln!(out, "  {} {}{pk}{null}{fk}{}", c.name, c.data_type, said(Some(&c.name)));
    }
    if cols.len() > MAX_COLUMNS {
        let _ = writeln!(out, "  … {} more columns", cols.len() - MAX_COLUMNS);
    }
    out
}

/// A comment on one line, cut at `MAX_COMMENT` characters.
fn cut(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ").replace('"', "'");
    if one.chars().count() <= MAX_COMMENT {
        return one;
    }
    let mut short: String = one.chars().take(MAX_COMMENT).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests;
