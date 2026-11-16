//! The gateway's methods as types: what each takes and answers, and the
//! notifications it pushes. The server reads its requests with them and
//! clients (the app, the command line) read the answers with them, so a
//! field renamed on one side doesn't compile on the other, rather than
//! quietly reading as nothing. On the wire it's JSON-RPC as before (see
//! `protocol`); fields a version doesn't know are ignored, missing ones
//! take their defaults.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::store::ProjectId;
use podelis_ai::memory::{Fact, FactId};

/// A method: its name on the wire, what it takes and what it answers.
pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send;
    type Result: Serialize + DeserializeOwned;
}

macro_rules! methods {
    ($($(#[$doc:meta])* $ty:ident = $name:literal ($params:ty) -> $result:ty;)*) => {
        $(
            $(#[$doc])*
            pub enum $ty {}
            impl Method for $ty {
                const NAME: &'static str = $name;
                type Params = $params;
                type Result = $result;
            }
        )*
    };
}

methods! {
    /// The first request on a connection: who's asking, in which protocol.
    Hello = "hello" (HelloParams) -> HelloResult;
    Status = "status" (NoParams) -> StatusResult;
    Shutdown = "shutdown" (NoParams) -> ShutdownResult;
    /// The configured models, the default first.
    Models = "models" (NoParams) -> ModelsResult;
    /// A model asked directly; its answer streams as `complete.delta`.
    Complete = "complete" (CompleteParams) -> CompleteResult;
    /// What a model would be shown of a database for a question.
    Context = "context" (ContextParams) -> ContextResult;
    /// SQL for a question (or what SQL does); streams as `ask.delta`.
    Ask = "ask" (AskParams) -> AskResult;
    /// Hands the connection to an agent's MCP session.
    McpOpen = "mcp.open" (McpOpenParams) -> McpOpenResult;
    MemoryAdd = "memory.add" (MemoryAddParams) -> Fact;
    MemoryUpdate = "memory.update" (MemoryUpdateParams) -> Fact;
    MemoryList = "memory.list" (MemoryListParams) -> MemoryListResult;
    MemoryForget = "memory.forget" (FactParams) -> FactIdResult;
    MemoryConfirm = "memory.confirm" (FactParams) -> FactIdResult;
    MemoryMove = "memory.move" (MemoryMoveParams) -> MovedResult;
    MemoryLearn = "memory.learn" (MemoryLearnParams) -> LearnResult;
    MemoryExport = "memory.export" (Scope) -> ExportResult;
    /// A connection's database as an ontology.
    Ontology = "ontology" (OntologyParams) -> OntologyResult;
    /// What an edited ontology file would change, or (`apply`) changed so.
    OntologyImport = "ontology.import" (ImportParams) -> ImportResult;
}

/// A list as sent, each entry read on its own: one this version can't
/// read (another build's) is left out, not the whole answer with it (an
/// `ask`'s SQL with a suggested fact it can't read).
fn readable<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let sent = Option::<Vec<Value>>::deserialize(d)?.unwrap_or_default();
    Ok(sent.into_iter().filter_map(|entry| serde_json::from_value(entry).ok()).collect())
}

/// A method that takes nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoParams {}

/// Which project, connection and folder a call is about: the project by
/// name or `id:<id>` (the last one opened when none), a saved connection
/// by name, and the asker's folder (a relative SQLite file is from there).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl Scope {
    pub fn project(&self) -> Option<&str> {
        given(self.project.as_deref())
    }

    pub fn connection(&self) -> Option<&str> {
        given(self.connection.as_deref())
    }

    pub fn dir(&self) -> Option<&str> {
        given(self.dir.as_deref())
    }
}

/// Text that says something: trimmed, and none when it's blank.
pub fn given(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|t| !t.is_empty())
}

/// Calls about a project (and maybe a connection) that the app makes for
/// the open one: it fills the scope in.
pub trait Scoped {
    fn scope_mut(&mut self) -> &mut Scope;
}

macro_rules! scoped {
    ($($ty:ty),*) => {
        $(impl Scoped for $ty {
            fn scope_mut(&mut self) -> &mut Scope {
                &mut self.scope
            }
        })*
    };
}

scoped!(
    MemoryAddParams,
    MemoryUpdateParams,
    MemoryListParams,
    FactParams,
    MemoryMoveParams,
    MemoryLearnParams,
    OntologyParams,
    ImportParams
);

impl Scoped for Scope {
    fn scope_mut(&mut self) -> &mut Scope {
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloParams {
    /// Who's asking (1 to 64 bytes).
    #[serde(default)]
    pub client: String,
    /// The protocol version it speaks.
    #[serde(default)]
    pub protocol: Option<u64>,
}

/// Every field has a default: another version's gateway may answer
/// otherwise, and its answer still has to be read (to stop it).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HelloResult {
    pub gateway: String,
    pub version: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusResult {
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    pub uptime_secs: u64,
    /// Connected now, the asker too.
    pub clients: usize,
    /// When it stops with no client; `None`: it keeps running.
    pub idle_stop_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShutdownResult {
    pub stopping: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsResult {
    #[serde(deserialize_with = "readable")]
    pub models: Vec<ModelInfo>,
}

/// A configured model, and where its key comes from (never the key).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelInfo {
    pub name: String,
    pub api: String,
    pub model: String,
    pub url: String,
    pub key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompleteParams {
    /// The model's name; the default one when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
}

/// The tokens a call took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompleteResult {
    pub text: String,
    /// The model's name in `config.toml`.
    pub model: String,
    /// The provider's model that answered.
    pub provider_model: String,
    pub stop_reason: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextParams {
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default)]
    pub question: String,
    /// The schema read again, not the one kept.
    #[serde(default)]
    pub refresh: bool,
}

/// A table chosen for a question, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableWhy {
    pub name: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextResult {
    pub engine: String,
    /// What a model would be shown.
    pub text: String,
    #[serde(deserialize_with = "readable")]
    pub tables: Vec<TableWhy>,
    /// Tables that qualified but were left out for the budget.
    pub left_out: usize,
    /// The team's facts shown, and those left out as stale (their lines).
    pub facts: Vec<String>,
    pub stale_facts: Vec<String>,
    pub chars: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskParams {
    #[serde(flatten)]
    pub scope: Scope,
    /// The question, or (`explain`) the SQL to say the meaning of.
    #[serde(default)]
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The user said yes to sending the schema to this model, this once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub share_once: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explain: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AskResult {
    /// The SQL, never run; none from an explanation or a model that
    /// wrote none.
    pub sql: Option<String>,
    /// What the model said besides (an explanation: all of it).
    pub said: String,
    /// What it assumed, kept as facts waiting to be confirmed.
    #[serde(deserialize_with = "readable")]
    pub suggested: Vec<Fact>,
    /// Names in the SQL the schema still doesn't have.
    #[serde(deserialize_with = "readable")]
    pub problems: Vec<String>,
    #[serde(deserialize_with = "readable")]
    pub tables: Vec<TableWhy>,
    /// Tables it looked up itself.
    #[serde(deserialize_with = "readable")]
    pub looked_at: Vec<String>,
    #[serde(deserialize_with = "readable")]
    pub facts: Vec<String>,
    #[serde(deserialize_with = "readable")]
    pub stale_facts: Vec<String>,
    pub model: String,
    pub provider_model: String,
    pub stop_reason: String,
    /// Answers it took (SQL with unknown names goes back).
    pub attempts: usize,
    pub usage: Usage,
}

/// The `data` of a `NOT_SHARED` refusal: whose schema, and where it
/// would have gone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotShared {
    pub project: String,
    pub project_id: Option<ProjectId>,
    pub model: String,
    pub url: String,
}

impl NotShared {
    /// From a refusal's `data`, if it's one.
    pub fn from_data(data: Option<&Value>) -> Option<Self> {
        serde_json::from_value(data?.clone()).ok()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpOpenParams {
    #[serde(default)]
    pub project: Option<String>,
    /// The agent's name, if it gave one.
    #[serde(default)]
    pub name: Option<String>,
    /// Its folder.
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpOpenResult {
    pub mcp: bool,
}

/// `text` as typed (`orders.status 3 = cancelled`), or its parts apart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryAddParams {
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryUpdateParams {
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default)]
    pub id: Option<FactId>,
    /// What it says now, as typed.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryListParams {
    #[serde(flatten)]
    pub scope: Scope,
    /// Only the facts matching these words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryListResult {
    pub project: String,
    #[serde(deserialize_with = "readable")]
    pub facts: Vec<Fact>,
}

/// A fact, by id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactParams {
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default)]
    pub id: Option<FactId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactIdResult {
    pub id: FactId,
}

/// Fact `id` about every database, or about the connection's alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryMoveParams {
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default)]
    pub id: Option<FactId>,
    #[serde(default)]
    pub every: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovedResult {
    pub id: FactId,
    /// Its database now; "" for every one.
    pub database: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryLearnParams {
    #[serde(flatten)]
    pub scope: Scope,
    /// A folder of migrations, as a full path (the gateway may run elsewhere).
    #[serde(default)]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnResult {
    pub files: usize,
    pub facts: usize,
    pub folder: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportResult {
    pub text: String,
    pub facts: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OntologyParams {
    #[serde(flatten)]
    pub scope: Scope,
    /// Only this form (`text`, `turtle`, `yaml`, `outline`); all when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// The catalog read again (the default); `false` takes the one kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OntologyResult {
    pub database: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turtle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<Vec<OutlineSection>>,
}

/// A section of the app's ontology browser: a table, column, term or
/// group, and its facts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutlineSection {
    pub kind: String,
    pub subject: String,
    #[serde(deserialize_with = "readable")]
    pub facts: Vec<Fact>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportParams {
    #[serde(flatten)]
    pub scope: Scope,
    /// The edited YAML file.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub apply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
}

/// What loading the file would change, or what it changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImportResult {
    Applied(ImportApplied),
    Plan(ImportPlan),
}

/// Each a fact's line. Its lists and `kept` are always sent: an answer
/// without them is neither a plan nor what changed, and is said as not
/// understood rather than taken for a plan that changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlan {
    pub add: Vec<String>,
    pub confirm: Vec<String>,
    pub forget: Vec<String>,
    #[serde(rename = "move")]
    pub moved: Vec<String>,
    /// Facts said since the file was exported, kept though it lacks them.
    pub kept: usize,
    /// The file doesn't say when it was exported: what it lacks is kept.
    #[serde(default)]
    pub undated: bool,
}

impl ImportPlan {
    /// Its sections, in the order they're shown.
    pub fn sections(&self) -> [(&'static str, &[String]); 4] {
        [("add", &self.add), ("confirm", &self.confirm), ("move", &self.moved), ("forget", &self.forget)]
    }

    /// How many lines it takes: a heading for each section with any, and
    /// a line each; and one first, when it changes something, for a file
    /// that doesn't say when it was exported.
    pub fn len(&self) -> usize {
        let lines: usize = self.sections().iter().map(|(_, items)| items.len() + usize::from(!items.is_empty())).sum();
        lines + usize::from(self.undated && lines > 0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportApplied {
    pub added: usize,
    pub confirmed: usize,
    pub forgotten: usize,
    pub moved: usize,
}

/// What the gateway pushes while a request runs, or before it stops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Note {
    /// A piece of the answer `ask` is writing.
    #[serde(rename = "ask.delta")]
    AskDelta(Delta),
    /// A piece of `complete`'s answer.
    #[serde(rename = "complete.delta")]
    CompleteDelta(Delta),
    /// The model's key is slow to come (a keychain dialog, a password
    /// manager).
    #[serde(rename = "key.waiting")]
    KeyWaiting(KeyWaiting),
    /// The gateway stops; the connection closes after it.
    #[serde(rename = "stopping")]
    Stopping(Stopping),
}

impl Note {
    /// A notification as it came, if it's one of these.
    pub fn parse(method: &str, params: &Value) -> Option<Self> {
        serde_json::from_value(serde_json::json!({ "method": method, "params": params })).ok()
    }

    /// The JSON-RPC notification's line, to send: written at once (one
    /// for each piece of an answer being written, so not made a `Value`
    /// first).
    pub fn line(&self) -> String {
        #[derive(Serialize)]
        struct Line<'a> {
            jsonrpc: &'static str,
            #[serde(flatten)]
            note: &'a Note,
        }
        let mut text = serde_json::to_string(&Line { jsonrpc: "2.0", note: self }).unwrap_or_default();
        text.push('\n');
        text
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    /// The request it's for.
    pub id: Value,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyWaiting {
    pub id: Value,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stopping {
    /// `idle`, `asked` or `interrupted`.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notes_go_as_they_did() {
        let note = Note::AskDelta(Delta { id: json!(3), text: "SEL".into() });
        let sent: Value = serde_json::from_str(&note.line()).unwrap();
        assert_eq!(sent, json!({ "jsonrpc": "2.0", "method": "ask.delta", "params": { "id": 3, "text": "SEL" } }));
        assert!(note.line().ends_with('\n'));
        assert_eq!(Note::parse("ask.delta", &json!({ "id": 3, "text": "SEL" })), Some(note));
        assert_eq!(Note::parse("something.new", &json!({})), None);
    }

    #[test]
    fn an_import_reads_as_applied_or_as_a_plan() {
        let applied: ImportResult =
            serde_json::from_value(json!({ "added": 1, "confirmed": 0, "forgotten": 2, "moved": 0 })).unwrap();
        assert!(matches!(applied, ImportResult::Applied(ImportApplied { added: 1, forgotten: 2, .. })));
        let sent = json!({ "add": ["a b c"], "confirm": [], "forget": [], "move": [], "kept": 1 });
        let ImportResult::Plan(plan) = serde_json::from_value(sent).unwrap() else { panic!("a plan") };
        assert_eq!((plan.len(), plan.kept), (2, 1));
        assert_eq!(serde_json::to_value(&plan).unwrap()["move"], json!([]));
        // What changed, with a count missing: not taken for an empty plan.
        let short = json!({ "added": 1, "confirmed": 0, "forgotten": 2 });
        assert!(serde_json::from_value::<ImportResult>(short).is_err());
    }

    #[test]
    fn a_plan_takes_a_line_per_change_and_a_heading_per_kind_that_has_some() {
        let line = |n: usize| vec![String::new(); n];
        let plan =
            ImportPlan { add: line(2), confirm: line(0), moved: line(1), forget: line(1), kept: 0, undated: false };
        assert_eq!(plan.len(), 7);
        assert_eq!(ImportPlan { undated: true, ..plan }.len(), 8, "and a line saying nothing is forgotten");
        assert!(ImportPlan::default().is_empty());
        assert!(ImportPlan { undated: true, ..ImportPlan::default() }.is_empty(), "nothing to change, still");
    }

    #[test]
    fn an_answer_with_an_entry_this_version_cant_read_keeps_the_rest() {
        let sent = json!({
            "sql": "SELECT 1",
            "suggested": [{ "subject": "no id" }, { "id": 4, "subject": "a", "relation": "is", "value": "b" }],
            "problems": ["x", 3],
        });
        let answer: AskResult = serde_json::from_value(sent).unwrap();
        assert_eq!(answer.sql.as_deref(), Some("SELECT 1"));
        assert_eq!(answer.suggested.len(), 1);
        assert_eq!(answer.problems, ["x"]);
        // And with none sent: none.
        let answer: AskResult = serde_json::from_value(json!({ "sql": null })).unwrap();
        assert!(answer.suggested.is_empty() && answer.sql.is_none());
    }

    #[test]
    fn a_scope_left_blank_says_nothing() {
        let scope: Scope = serde_json::from_value(json!({ "project": "  ", "connection": " shop " })).unwrap();
        assert_eq!((scope.project(), scope.connection(), scope.dir()), (None, Some("shop"), None));
        // What isn't given isn't sent.
        assert_eq!(serde_json::to_value(Scope::default()).unwrap(), json!({}));
    }
}
