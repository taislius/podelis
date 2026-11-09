//! What a write's filter keeps out, read as the document it is: whether
//! it may match every document (agents' `deleteMany`/`updateMany` and
//! `delete` commands are refused then). With the driver only, which reads
//! the values; without it no call runs, and the words decide.

use ::mongodb::bson::{Bson, Document};

/// Whether the first argument, a filter, names no field; `None` when the
/// arguments can't be read as values.
pub(super) fn first_names_no_field(args: &str) -> Option<bool> {
    let values = super::args::values(args).ok()?;
    Some(match values.first() {
        None => true,
        Some(Bson::Document(filter)) => !names_a_field(filter),
        // Not a filter: the server refuses it.
        Some(_) => false,
    })
}

/// Whether a `delete` command's deletes have one whose filter (`q`) names
/// no field; `None` when it can't be read.
pub(super) fn a_delete_names_no_field(args: &str) -> Option<bool> {
    let Some(Bson::Document(command)) = super::args::values(args).ok()?.into_iter().next() else { return None };
    let Some(Bson::Array(deletes)) = command.get("deletes") else { return Some(false) };
    Some(deletes.iter().filter_map(Bson::as_document).any(|d| match d.get("q") {
        Some(Bson::Document(q)) => !names_a_field(q),
        _ => true,
    }))
}

/// Whether a filter's own words keep any document out: a condition on a
/// field (not one only that it exists), or `$and` with one. Not `$comment`,
/// `$expr`, `$jsonSchema` or other operators alone, nor `$nor`, nor `$or`
/// unless every branch has one. A safety net, not a proof: `{x: {$ne: -1}}` can
/// match everything too; the database user's rights are the boundary.
fn names_a_field(filter: &Document) -> bool {
    filter.iter().any(|(key, value)| match key.as_str() {
        "$and" => branches(value).into_iter().any(names_a_field),
        "$or" => {
            let all = branches(value);
            !all.is_empty() && all.into_iter().all(names_a_field)
        }
        k if k.starts_with('$') => false,
        _ => !exists_only(value),
    })
}

/// The filters of `$and`'s or `$or`'s array.
fn branches(value: &Bson) -> Vec<&Document> {
    match value {
        Bson::Array(items) => items.iter().filter_map(Bson::as_document).collect(),
        _ => Vec::new(),
    }
}

/// `{$exists: true}` (or 1): met by every document with the field, and
/// every document has `_id`.
fn exists_only(value: &Bson) -> bool {
    let Some(d) = value.as_document() else { return false };
    d.len() == 1 && d.get("$exists").is_some_and(|v| !matches!(v, Bson::Boolean(false) | Bson::Int32(0) | Bson::Int64(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_that_names_no_field_may_match_everything() {
        for args in [
            "{}",
            "",
            "{$comment: 'all'}",
            "{$expr: true}",
            "{_id: {$exists: true}}",
            "{$or: [{x: 1}, {$comment: 'y'}]}",
            "{$nor: [{x: 1}]}",
            "{$and: [{$comment: 'a'}]}",
        ] {
            assert_eq!(first_names_no_field(args), Some(true), "{args}");
        }
        for args in ["{x: 1}", "{x: 1, $comment: 'c'}", "{$and: [{x: 1}, {$comment: 'a'}]}", "{$or: [{x: 1}, {y: 2}]}"]
        {
            assert_eq!(first_names_no_field(args), Some(false), "{args}");
        }
        assert_eq!(first_names_no_field("{x: "), None, "can't be read: the words decide");
    }

    #[test]
    fn a_delete_command_by_each_of_its_filters() {
        assert_eq!(a_delete_names_no_field("{delete: 't', deletes: [{q: {x: 1}, limit: 0}]}"), Some(false));
        assert_eq!(a_delete_names_no_field("{delete: 't', deletes: [{q: {x: 1}}, {q: {$comment: 'x'}}]}"), Some(true));
        assert_eq!(a_delete_names_no_field("{delete: 't', deletes: [{limit: 0}]}"), Some(true), "no q");
    }
}
