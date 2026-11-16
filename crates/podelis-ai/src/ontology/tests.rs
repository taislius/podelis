use super::*;
use crate::db::{ColumnInfo, Link, TableInfo};

fn table(name: &str, columns: &[&str]) -> (TableInfo, Vec<ColumnInfo>) {
    let info = TableInfo {
        schema: "public".into(),
        name: name.into(),
        is_view: false,
        rows: None,
        bytes: None,
        system: false,
        partition_of: None,
    };
    let cols = columns
        .iter()
        .map(|c| ColumnInfo { name: (*c).into(), data_type: "text".into(), nullable: true, primary_key: *c == "id" })
        .collect();
    (info, cols)
}

fn link(table: &str, column: &str, target: &str) -> Link {
    Link {
        schema: "public".into(),
        table: table.into(),
        column: column.into(),
        target_schema: "public".into(),
        target_table: target.into(),
        target_column: Some("id".into()),
    }
}

fn shop() -> Schema {
    Schema {
        engine: "PostgreSQL".into(),
        dialect: crate::db::Engine::Postgres,
        tables: vec![
            table("customers", &["id", "name", "city"]),
            table("orders", &["id", "customer_id", "status", "total"]),
            table("trips", &["id", "from_city", "to_city"]),
            table("cities", &["id", "name"]),
        ],
        links: vec![
            link("orders", "customer_id", "customers"),
            link("trips", "from_city", "cities"),
            link("trips", "to_city", "cities"),
        ],
        comments: Vec::new(),
    }
}

fn fact(id: i64, subject: &str, relation: &str, value: &str, confirmed: bool) -> Fact {
    Fact {
        id: crate::memory::FactId::from_written(id),
        subject: subject.into(),
        relation: relation.into(),
        value: value.into(),
        source: if confirmed { "user".into() } else { "model".into() },
        confirmed,
        at: "2026-11-10T08:00:00+02:00".into(),
        database: String::new(),
    }
}

fn id(text: &str) -> ConceptId {
    ConceptId::parse(text)
}

#[test]
fn the_catalog_gives_tables_columns_and_keys() {
    let o = Ontology::from_catalog(&shop());
    assert_eq!(o.concepts.values().filter(|c| c.kind == Kind::Table).count(), 4);
    assert!(o.concepts.contains_key(&id("column:public.orders.status")));
    let orders = &o.concepts[&id("table:public.orders")];
    assert_eq!(orders.labels[0].text, "orders");
    // Two keys to one table stay two relations.
    let to_cities: Vec<&Relation> = o
        .relations
        .iter()
        .filter(|r| r.to == id("table:public.cities") && r.from == id("table:public.trips"))
        .collect();
    assert_eq!(to_cities.len(), 2);
}

#[test]
fn a_facts_subject_names_a_table_a_column_a_term_or_nothing_here() {
    let o = Ontology::from_catalog(&shop());
    assert_eq!(o.resolve("orders"), Resolved::Concept(id("table:public.orders")));
    assert_eq!(o.resolve("Orders.Status"), Resolved::Concept(id("column:public.orders.status")));
    assert_eq!(o.resolve("public.orders"), Resolved::Concept(id("table:public.orders")));
    assert_eq!(o.resolve("public.orders.total"), Resolved::Concept(id("column:public.orders.total")));
    assert_eq!(o.resolve("revenue"), Resolved::Term("revenue".into()));
    // As people say it: the singular finds the table, and its columns.
    assert_eq!(o.resolve("customer"), Resolved::Concept(id("table:public.customers")));
    assert_eq!(o.resolve("order.status"), Resolved::Concept(id("column:public.orders.status")));
    // Gone, or never there: stale.
    assert_eq!(o.resolve("orders.discount"), Resolved::Missing);
    assert_eq!(o.resolve("invoices.state"), Resolved::Missing);
    // A schema the catalog doesn't name: the table by its own name (SQLite has none).
    assert_eq!(o.resolve("other.orders.status"), Resolved::Concept(id("column:public.orders.status")));
}

#[test]
fn a_subject_is_about_a_table_or_its_columns() {
    let o = Ontology::from_catalog(&shop());
    for subject in ["orders", "Orders.status", "public.orders", "public.orders.status", "order.total"] {
        assert_eq!(o.table_of(subject), Some(id("table:public.orders")), "{subject}");
    }
    assert_eq!(o.table_of("revenue"), None, "a term");
    assert_eq!(o.table_of("public"), None, "a schema");
    assert_eq!(o.table_of("orders.discount"), None, "not here");
}

#[test]
fn facts_become_labels_assertions_terms_or_stale() {
    let facts = [
        fact(1, "customers", "also called", "client", true),
        fact(2, "orders.status", "means", "3 = cancelled", true),
        fact(3, "revenue", "defined as", "sum(orders.total)", true),
        fact(4, "orders", "excluded by default", "status = 3", true),
        fact(5, "orders.discount", "means", "percent off", true),
        fact(6, "orders.total", "is", "with VAT", false),
    ];
    let o = Ontology::from_catalog(&shop()).with_facts(&facts);
    // `also called` is another name: "client" finds customers.
    let named: Vec<&str> = o.named("Client").iter().map(|c| c.name.as_str()).collect();
    assert_eq!(named, ["public.customers"]);
    // A word naming no table is a term, with what's said of it.
    let revenue = id("term:revenue");
    assert_eq!(o.concepts[&revenue].kind, Kind::Term);
    assert_eq!(o.about(&revenue).next().unwrap().says, Says::DefinedAs("sum(orders.total)".into()));
    assert_eq!(o.about(&id("table:public.orders")).next().unwrap().says, Says::ExcludedByDefault("status = 3".into()));
    assert_eq!(
        o.about(&id("column:public.orders.status")).next().unwrap().says,
        Says::Means { verb: "means".into(), what: "3 = cancelled".into() }
    );
    // A column that isn't there: stale, not asserted.
    assert_eq!(o.stale.iter().map(|f| f.id.get()).collect::<Vec<_>>(), [5]);
    assert_eq!(o.assertions().len(), 4);
}

#[test]
fn another_name_names_the_table_in_facts_too() {
    let facts = [
        fact(1, "customers", "also called", "client", true),
        fact(2, "client.city", "means", "where they live", true),
        fact(3, "client", "means", "someone who has ordered", true),
        fact(4, "orders.total", "also called", "amount", true),
        fact(5, "amount", "is", "with VAT", true),
    ];
    let o = Ontology::from_catalog(&shop()).with_facts(&facts);
    // Not stale, and not a term of its own: about customers.
    assert!(o.stale.is_empty(), "{:?}", o.stale);
    assert!(!o.concepts.contains_key(&id("term:client")));
    let city = id("column:public.customers.city");
    assert_eq!(o.about(&city).count(), 1);
    assert_eq!(o.about(&id("table:public.customers")).count(), 1);
    assert_eq!(o.about(&id("column:public.orders.total")).count(), 1, "a column's other name too");
    assert_eq!(o.table_of("client.city"), Some(id("table:public.customers")));
    // A table's own name stays its own: another name can't take it.
    let o = Ontology::from_catalog(&shop()).with_facts(&[fact(1, "customers", "also called", "orders", true)]);
    assert_eq!(o.resolve("orders"), Resolved::Concept(id("table:public.orders")));
    // The catalog alone, as a question sees it: the names are taken in only
    // when there are some.
    let catalog = Ontology::from_catalog(&shop());
    assert!(matches!(catalog.with_aliases(&facts[1..3]), std::borrow::Cow::Borrowed(_)));
    assert_eq!(catalog.with_aliases(&facts).table_of("client"), Some(id("table:public.customers")));
}

#[test]
fn another_name_doesnt_take_a_schemas_tables() {
    // Schema `client` has `invoices`; `client` is also another name of customers.
    let mut schema = shop();
    let (mut invoices, columns) = table("invoices", &["id", "total"]);
    invoices.schema = "client".into();
    schema.tables.push((invoices, columns));
    let o = Ontology::from_catalog(&schema).with_facts(&[fact(1, "customers", "also called", "client", true)]);
    assert_eq!(o.resolve("client.invoices"), Resolved::Concept(id("table:client.invoices")));
    assert_eq!(o.resolve("client.invoices.total"), Resolved::Concept(id("column:client.invoices.total")));
    assert_eq!(o.resolve("client.city"), Resolved::Concept(id("column:public.customers.city")));
}

#[test]
fn names_taken_in_twice_add_nothing() {
    let facts = [
        fact(1, "customers", "also called", "client", true),
        // About a table that's gone.
        fact(2, "public.oldtable", "also called", "legacy", true),
        fact(3, "client", "means", "someone who has ordered", true),
    ];
    let aliased = Ontology::from_catalog(&shop()).with_aliases(&facts).into_owned();
    // Kept with them: asked again, it's that one, not a copy.
    assert!(matches!(aliased.with_aliases(&facts), std::borrow::Cow::Borrowed(_)));
    let o = aliased.with_facts(&facts);
    let labels = &o.concepts[&id("table:public.customers")].labels;
    assert_eq!(labels.iter().filter(|l| l.kind == LabelKind::Alternative).count(), 1, "{labels:?}");
    assert_eq!(o.stale.len(), 1, "{:?}", o.stale);
}

#[test]
fn what_only_a_table_has_said_of_a_word_that_names_none_is_stale() {
    // `invoices` was dropped: its default filter and history are about a
    // table that's gone, not a business term called "invoices".
    let facts = [
        fact(1, "invoices", "excluded by default", "void", true),
        fact(2, "invoices", "history", "dropped in 007_cleanup.sql", true),
        fact(3, "revenue", "means", "what customers paid", true),
    ];
    let o = Ontology::from_catalog(&shop()).with_facts(&facts);
    assert_eq!(o.stale.iter().map(|f| f.id.get()).collect::<Vec<_>>(), [1, 2]);
    assert!(!o.concepts.contains_key(&id("term:invoices")));
    assert!(o.concepts.contains_key(&id("term:revenue")), "a word that means something is a term");
    // A table that's here keeps them.
    let o = Ontology::from_catalog(&shop()).with_facts(&[fact(1, "orders", "excluded by default", "void", true)]);
    assert!(o.stale.is_empty());
}

#[test]
fn described_and_exported() {
    let facts = [
        fact(1, "customers", "also called", "client", true),
        fact(2, "revenue", "defined as", "sum(orders.total)", true),
        fact(3, "orders.total", "is", "with \"VAT\"", false),
        fact(4, "orders.discount", "means", "percent off", true),
    ];
    let o = Ontology::from_catalog(&shop()).with_facts(&facts);
    let text = o.describe();
    assert!(
        text.starts_with("4 tables, 0 views, 12 columns, 1 terms; 3 foreign keys; 2 facts, 1 other names, 1 stale"),
        "{text}"
    );
    assert!(text.contains("table public.customers\n  also called: client"), "{text}");
    assert!(text.contains("references public.customers (customer_id → id)"), "{text}");
    assert!(text.contains("column public.orders.total\n  is with \"VAT\" (suggested, not confirmed)"), "{text}");
    assert!(
        text.contains(
            "Stale: the table or column they name isn't in this database\n  orders.discount means percent off"
        ),
        "{text}"
    );

    let turtle = o.to_turtle("urn:podelis:shop/");
    assert!(turtle.contains("<urn:podelis:shop/table/public.customers> a skos:Concept"), "{turtle}");
    assert!(turtle.contains("skos:altLabel \"client\""), "{turtle}");
    assert!(turtle.contains("podelis:definedAs \"sum(orders.total)\""), "{turtle}");
    assert!(turtle.contains("<urn:podelis:shop/column/public.orders.total> a skos:Concept"), "{turtle}");
    assert!(turtle.contains("skos:broader <urn:podelis:shop/table/public.orders>"), "{turtle}");
    // A suggestion isn't the team's yet: not exported.
    assert!(!turtle.contains("VAT"), "{turtle}");
}

#[test]
fn names_an_iri_cant_hold_are_encoded_in_turtle() {
    let mut schema = shop();
    schema.tables.push(table("order items", &["id", "a>b"]));
    let turtle = Ontology::from_catalog(&schema).to_turtle("urn:podelis:shop/");
    assert!(turtle.contains("<urn:podelis:shop/table/public.order%20items> a skos:Concept"), "{turtle}");
    assert!(turtle.contains("<urn:podelis:shop/column/public.order%20items.a%3Eb>"), "{turtle}");
    // No IRI with a space or `>` in it.
    for iri in turtle.split('<').skip(1).filter_map(|t| t.split('>').next()) {
        assert!(!iri.contains(' ') && !iri.contains('"'), "{iri}");
    }
    assert_eq!(super::iri_part("žąsis"), "%C5%BE%C4%85sis");
}

mod file {
    use super::*;
    use crate::ontology::file::{export, read};
    use crate::ontology::plan::plan;

    fn facts() -> Vec<Fact> {
        let mut all = vec![
            fact(1, "customers", "also called", "client", true),
            fact(2, "customers", "also called", "buyer", true),
            fact(3, "orders.status", "means", "3 = cancelled", true),
            fact(4, "orders", "excluded by default", "status = 3", true),
            fact(5, "revenue", "defined as", "sum(orders.total)", true),
            fact(6, "orders.total", "is", "with VAT", false),
            fact(7, "orders.discount", "means", "percent off", true),
            // Written in the singular: the same table, under its own name.
            fact(8, "customer", "means", "a person who bought", true),
        ];
        // About this database, but revenue: about every one.
        for f in &mut all {
            if f.id.get() != 5 {
                f.database = "shop".into();
            }
        }
        all
    }

    fn onto() -> Ontology {
        Ontology::from_catalog(&shop()).with_facts(&facts())
    }

    fn exported() -> String {
        let o = Ontology::from_catalog(&shop()).with_facts(&facts());
        export(&o, &facts(), "shop", "Podėlis ontology · shop")
    }

    #[test]
    fn a_term_is_written_once_however_its_facts_name_it() {
        let facts = [
            fact(1, "Revenue", "defined as", "sum(amount)", true),
            fact(2, "revenue", "also called", "sales", true),
            fact(3, "sales", "means", "gross", true),
        ];
        let o = Ontology::from_catalog(&shop()).with_facts(&facts);
        let text = export(&o, &facts, "shop", "test");
        assert_eq!(text.matches("evenue:").count(), 1, "one entry, all three facts under it: {text}");
        assert!(!text.contains("sales:"), "not under its other name: {text}");
    }

    #[test]
    fn exported_as_tables_columns_terms_suggestions_and_stale() {
        let text = exported();
        assert!(text.starts_with("# Podėlis ontology · shop\n"), "{text}");
        assert_eq!(text.matches("  customer").count(), 1, "one table, however facts name it:\n{text}");
        let expected = [
            "database: shop",
            "  customers:",
            "    means: a person who bought",
            "    also_called:",
            "    - client",
            "    - buyer",
            "  orders:",
            "    excluded_by_default: status = 3",
            "    columns:",
            "      status:",
            "        means: 3 = cancelled",
            "every_database:",
            "  terms:",
            "    revenue:",
            "      defined_as: sum(orders.total)",
            "suggested:",
            "- about: orders.total",
            "  relation: is",
            "  value: with VAT",
            "stale:",
            "- about: orders.discount",
            "  value: percent off",
        ];
        for line in expected {
            assert!(text.lines().any(|l| l == line), "no `{line}` in:\n{text}");
        }
    }

    #[test]
    fn read_back_unchanged_it_changes_nothing() {
        let file = read(&exported()).unwrap();
        assert_eq!(file.database.as_deref(), Some("shop"));
        assert!(file.exported.is_some(), "the file says when it was written");
        let wanted = file.wanted;
        assert!(plan(&onto(), &facts(), &wanted, None).is_empty(), "{:?}", plan(&onto(), &facts(), &wanted, None));
    }

    #[test]
    fn edits_add_confirm_and_forget() {
        let edited = exported()
            // Another name, and a new term about this database.
            .replace("    - buyer\n", "    - buyer\n    - patron\n")
            .replace("every_database:\n", "terms:\n  margin:\n    defined_as: revenue minus cost\nevery_database:\n")
            // The model's guess confirmed: moved up under its column.
            .replace("suggested:\n- about: orders.total\n  relation: is\n  value: with VAT\n", "")
            .replace("      status:\n", "      total:\n        is: with VAT\n      status:\n")
            // The stale one forgotten.
            .replace("stale:\n- about: orders.discount\n  relation: means\n  value: percent off\n", "");
        let file = read(&edited).unwrap();
        let p = plan(&onto(), &facts(), &file.wanted, file.exported.as_deref());
        let added: Vec<String> = p.add.iter().map(|w| format!("{} {} {}", w.subject, w.relation, w.value)).collect();
        assert_eq!(added, ["customers also called patron", "margin defined as revenue minus cost"], "{edited}");
        assert_eq!(p.add[1].everywhere, Some(false), "added under `terms`: this database's");
        assert_eq!(p.confirm.iter().map(|f| f.id.get()).collect::<Vec<_>>(), [6]);
        assert_eq!(p.forget.iter().map(|f| f.id.get()).collect::<Vec<_>>(), [7]);
        assert!(p.moved.is_empty());
    }

    #[test]
    fn moved_between_this_database_and_every_one() {
        // Revenue down to this database's terms; customers' names up to every database's.
        let edited = exported()
            .replace("every_database:\n  terms:\n    revenue:\n      defined_as: sum(orders.total)\n", "")
            .replace("database: shop\n", "database: shop\nterms:\n  revenue:\n    defined_as: sum(orders.total)\n");
        let wanted = read(&edited).unwrap().wanted;
        let p = plan(&onto(), &facts(), &wanted, None);
        assert_eq!(p.moved.iter().map(|(f, every)| (f.id.get(), *every)).collect::<Vec<_>>(), [(5, false)], "{edited}");
        assert!(p.add.is_empty() && p.forget.is_empty(), "{p:?}");
    }

    #[test]
    fn a_wrong_file_says_what_is_wrong() {
        let unknown = read("tables:\n  orders:\n    meaning: typo\n").unwrap_err();
        assert!(unknown.contains("meaning"), "{unknown}");
        assert!(read("tables: [").is_err());
        // A table twice: an error, not one of them quietly dropped.
        assert!(read("tables:\n  orders:\n    means: a\n  orders:\n    means: b\n").is_err());
        let nested = read("terms:\n  revenue:\n    columns:\n      x:\n        means: y\n").unwrap_err();
        assert!(nested.contains("a term has no columns"), "{nested}");
        // What YAML reads as a number or a yes/no is the text written.
        let wanted =
            read("tables:\n  orders:\n    columns:\n      status:\n        means: [3, y, 1.5]\n").unwrap().wanted;
        let values: Vec<&str> = wanted.iter().map(|w| w.value.as_str()).collect();
        assert_eq!(values, ["3", "y", "1.5"]);
    }
}

#[test]
fn outlined_by_table_then_terms_suggestions_and_stale() {
    let facts = [
        fact(1, "customer", "also called", "client", true),
        fact(2, "orders.status", "means", "3 = cancelled", true),
        fact(3, "revenue", "defined as", "sum(orders.total)", true),
        fact(4, "orders.total", "is", "with VAT", false),
        fact(5, "orders.discount", "means", "percent off", true),
    ];
    let o = Ontology::from_catalog(&shop()).with_facts(&facts);
    let outline = crate::ontology::outline::outline(&o, &facts);
    let shape: Vec<(&str, &str, usize)> =
        outline.iter().map(|s| (s.kind.as_str(), s.subject.as_str(), s.facts.len())).collect();
    assert_eq!(
        shape,
        [
            ("table", "cities", 0),
            ("table", "customers", 1),
            ("table", "orders", 0),
            ("column", "orders.status", 1),
            ("table", "trips", 0),
            ("term", "revenue", 1),
            ("suggested", "", 1),
            ("stale", "", 1),
        ]
    );
}

#[test]
fn a_fact_about_a_system_table_or_an_unlisted_column_is_outlined_as_what_it_is() {
    let mut schema = shop();
    let (mut info, columns) = table("pg_class", &["relname"]);
    info.system = true;
    schema.tables.push((info, columns));
    // A table whose catalog gave no columns: any column of it resolves.
    schema.tables.push(table("events", &[]));
    let facts = [fact(1, "pg_class", "means", "the catalog", true), fact(2, "events.kind", "means", "click", true)];
    let o = Ontology::from_catalog(&schema).with_facts(&facts);
    let outline = crate::ontology::outline::outline(&o, &facts);
    let kinds: Vec<(&str, &str)> =
        outline.iter().filter(|s| !s.facts.is_empty()).map(|s| (s.kind.as_str(), s.subject.as_str())).collect();
    assert_eq!(kinds, [("column", "events.kind"), ("table", "pg_class")], "not terms");
}

mod review {
    use super::*;
    use crate::ontology::file::{export, read};
    use crate::ontology::plan::plan;

    fn onto(facts: &[Fact]) -> Ontology {
        Ontology::from_catalog(&shop()).with_facts(facts)
    }

    fn round_trip(facts: &[Fact]) -> crate::ontology::plan::Plan {
        let text = export(&onto(facts), facts, "shop", "test");
        let file = read(&text).unwrap();
        plan(&onto(facts), facts, &file.wanted, file.exported.as_deref())
    }

    #[test]
    fn an_unchanged_file_changes_nothing_whatever_its_facts_say() {
        let mut facts = vec![
            // A history (a relation the file has no key for), stale.
            fact(1, "old.col", "history", "created in 001", true),
            // A relation the file has no key for, confirmed.
            fact(2, "orders", "note", "kept   with  spaces", true),
            // A suggestion with a relation of its own.
            fact(3, "orders.total", "history", "was gross", false),
            fact(4, "orders.status", "means", "line one\nline two", true),
        ];
        facts[1].database = "shop".into();
        let p = round_trip(&facts);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn a_fact_said_for_this_database_and_for_every_one_stays_two() {
        let mut facts =
            vec![fact(1, "orders.total", "is", "with VAT", true), fact(2, "orders.total", "is", "with VAT", true)];
        facts[1].database = "shop".into();
        assert!(round_trip(&facts).is_empty(), "{:?}", round_trip(&facts));
        // Deleting the every-database copy forgets that one, not the other.
        let text = export(&onto(&facts), &facts, "shop", "test");
        let edited = text.replace(
            "every_database:\n  tables:\n    orders:\n      columns:\n        total:\n          is: with VAT\n",
            "",
        );
        assert_ne!(edited, text);
        let file = read(&edited).unwrap();
        let p = plan(&onto(&facts), &facts, &file.wanted, file.exported.as_deref());
        assert_eq!(p.forget.iter().map(|f| f.id.get()).collect::<Vec<_>>(), [1], "{p:?}");
        assert!(p.moved.is_empty() && p.add.is_empty());
    }

    #[test]
    fn a_fact_added_since_the_export_is_kept() {
        let facts = vec![fact(1, "orders.status", "means", "3 = cancelled", true)];
        let text = export(&onto(&facts), &facts, "shop", "test");
        let file = read(&text).unwrap();
        let mut now = facts;
        let mut later = fact(2, "orders", "is", "new since", false);
        later.at = "2999-01-01T00:00:00+00:00".into();
        now.push(later);
        let p = plan(&onto(&now), &now, &file.wanted, file.exported.as_deref());
        assert!(p.forget.is_empty(), "{p:?}");
        assert_eq!(p.kept, 1);
        assert!(!p.undated);
        // Without the export's time (a file written by hand): what it lacks
        // can't be told from what came since, so nothing is forgotten.
        let p = plan(&onto(&now), &now, &[], None);
        assert!(p.forget.is_empty() && p.undated, "{p:?}");
        assert_eq!(p.kept, 2);
    }
}

/// Whatever the facts say, a file exported and read back unchanged
/// changes nothing: every fact finds its own line again, however its
/// subject is written, wherever it's filed (tables, columns, terms, other
/// names, suggestions, stale, other relations, this database or every one).
mod round_trip_property {
    use super::*;
    use crate::ontology::file::{export, read};
    use crate::ontology::plan::plan;
    use proptest::prelude::*;

    fn subject() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "orders",
            "Orders",
            "order",
            "public.orders",
            "orders.status",
            "Customers.City",
            "customer.name",
            "public.trips.to_city",
            "revenue",
            "Margin",
            "client",
            "client.city",
            "invoices",
            "orders.discount",
            "other.orders.total",
        ])
        .prop_map(str::to_string)
    }

    fn relation() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "means",
            "is",
            "are",
            "equals",
            "also called",
            "defined as",
            "excluded by default",
            "history",
            "note",
        ])
        .prop_map(str::to_string)
    }

    fn value() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["3", "y", "with VAT", "client", "sum(x) - 1", "a: b", "\"quoted\"", "x # y"])
            .prop_map(str::to_string)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

        #[test]
        fn an_unchanged_file_plans_nothing(
            said in prop::collection::vec((subject(), relation(), value(), any::<bool>(), any::<bool>()), 0..12)
        ) {
            let facts: Vec<Fact> = said
                .iter()
                .enumerate()
                .map(|(i, (subject, relation, value, confirmed, every))| {
                    let mut f = fact(i64::try_from(i).unwrap() + 1, subject, relation, value, *confirmed);
                    if !*every {
                        f.database = "shop".into();
                    }
                    f
                })
                .collect();
            let o = Ontology::from_catalog(&shop()).with_facts(&facts);
            let text = export(&o, &facts, "shop", "test");
            let file = read(&text).map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
            let p = plan(&o, &facts, &file.wanted, file.exported.as_deref());
            prop_assert!(p.is_empty(), "{p:?}\n{text}");
        }
    }
}
