//! What AI agents may send to MongoDB (see `db::agent_constraints`): by
//! the method a statement calls (`db.<collection>.<method>(…)`), and the
//! stages and operators anywhere in it (a key in quotes too).

use crate::db::agent_constraints::AgentConstraints;

pub(in crate::db) static AGENT_CONSTRAINTS: AgentConstraints = AgentConstraints {
    reads: &["find", "findOne", "aggregate", "countDocuments", "distinct", "estimatedDocumentCount"],
    writes: &["insertOne", "insertMany", "updateOne", "updateMany", "replaceOne", "deleteOne", "deleteMany"],
    outside: &[
        ("$out", "writes the result into a collection"),
        ("$merge", "writes the result into a collection"),
        ("$where", "runs JavaScript on the server"),
        ("$function", "runs JavaScript on the server"),
        ("$accumulator", "runs JavaScript on the server"),
        ("mapReduce", "runs JavaScript on the server"),
    ],
    instead: &[("runCommand", "the schema tool tells collections and their fields")],
};
