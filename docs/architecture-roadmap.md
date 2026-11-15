# Architecture roadmap

This roadmap turns the current architecture into a sequence of small changes.
It focuses on the root crate, where the UI, app state, gateway, CLI, transfer,
configuration, and system code share one Cargo crate. Keep `podelis-core`'s
engine adapter model and the `podelis-ai` dependency direction intact.

## Goals

- Make dependency boundaries inside the root crate visible and enforceable.
- Keep app state transitions understandable as asynchronous work completes,
  including work started on inactive tabs.
- Reduce the amount of coordination owned directly by `App`.
- Extract a new crate only when an already stable interface makes that useful.

## Phase 1: map and enforce boundaries

1. Document the intended dependencies between app, UI, gateway, agents, CLI,
   transfer, config, and system modules.
2. Extend `tests/layers.rs` only for boundaries that are clear and stable; keep
   tests free to use shared helpers as they do today.
3. Replace broad `crate::app::*` imports in touched production modules with
   explicit imports. Do this alongside behavior work, not as a repository-wide
   mechanical rewrite.

**Done when:** the dependency rules describe production dependencies accurately,
and a deliberately inverted dependency fails the layer check.

## Phase 2: use query execution as the lifecycle pilot

The query lifecycle crosses `query_screen/run.rs`, `events.rs`, `session.rs`,
and tab state. Before extracting general abstractions, trace one complete path:
start a query, receive rows, handle completion or failure, cancel it, and apply
an event for an inactive tab.

1. Identify the state transition and the asynchronous effect at each step.
2. Keep decisions about whether a query may run in the app; keep database work
   in its async task.
3. Make event identity explicit: a typed run id for query-owned work, a stable
   tab id for tab-specific connection work, a typed generation for
   connection-owned work, and request ids for replaceable background work.
   Keep request counters with the component that owns them.
4. Extract a common effect or event type only if the pilot removes duplicated
   coordination without weakening those identity guarantees.

**Done when:** each outcome has one clear owner, stale results are rejected at
the boundary, and inactive-tab results are applied to their originating tab.

## Phase 3: give cohesive state an owner

Use the pilot to identify state that changes together. Move behavior behind its
owner (for example, tab switching and parked tab state, or connection switching
and parked connection state) while keeping `App` as the coordinator. Avoid
moving fields solely to reduce the field count or splitting files solely by
size.

**Done when:** an owner can enforce its own invariants and callers no longer
need to manually coordinate the same transition across unrelated fields.

## Phase 4: clarify peer service boundaries

Review the gateway/agents integration and the app's gateway client calls. Keep
gateway process lifecycle and protocol handling separate from agent policy and
from UI state. Extract a shared service interface only where both sides need the
same operation and its inputs and outputs can be stated without app types.

**Done when:** dependency direction is explicit, the service contract has no
UI-owned types, and gateway or agent behavior can be understood without
following unrelated app state.

## Crate extraction decision

After phases 1–4, consider moving a module to a crate only if at least one
concrete need exists: independent reuse, a distinct build/test boundary, or
ownership that benefits from a separately enforced public API. Before moving
it, list its dependencies and define the smallest public interface. Do not use
crate count as the measure of modularity; Rust modules and the layer checks are
valid boundaries when they express the design clearly.

## Current review outcomes

- **Event identity:** `DbEvent` is a sum type: connection-owned events carry
  `ConnectionGeneration`; run-owned events carry `RunId` and a run-only event
  kind. The type prevents connection senders from sending query outcomes.
  Tab-specific plan and foreign-key results, error message locations, and
  pending questions carry `TabId`, which remains unique through tab closure
  and project changes. Stale results are dropped instead of being applied to
  a tab that inherits an old numeric index.
  Container searches, model asks and export estimates use `ContainerRequestId`,
  `AskId` and `ExportRequestId`, with counters owned by the state that replaces
  those requests.
- **Query effects:** `App` decides what to run and prepares state; standalone
  tasks in `query_screen/run/effects.rs` receive database inputs and an event
  sender, then report outcomes without accessing app state. Page fetches and
  cancel failures are routed by `RunId`, so they follow their originating tab
  and are discarded after that run is superseded.
- **Run options:** the one-shot watch flag, row limit, and same-result flag
  now live inside `Run`, beside the state and identity they affect.
- **Gateway and agents:** the layer check enforces the one-way integration:
  gateway code hosts `agents::mcp::serve_on` and owns the `podelis mcp` stdio
  bridge, while agent production code does not depend on the gateway.
- **Tab state:** keep `TabState` as the owner of one tab's editor, results,
  and run. Defer a wrapper around active and parked tab fields until it can
  own a real transition invariant; a wrapper that only relocates fields would
  add indirection while those transitions still coordinate connections and
  app events.
- **Connection slots:** `Conns` owns connection ID allocation and the active
  and parked slot transitions. `App` still owns the shown `Connection` value
  for direct app/UI access, while `switch_conn` coordinates generation
  invalidation, keep-alive state and catalog effects after a successful slot
  transition.
- **Connection health:** the optional heartbeat, manual `Reconnect` command,
  query failure handling and status bar already share the lost-connection
  signal, with `KeepAlive` owning health and check transitions. Keep the
  heartbeat off by default (`keepalive = 0`); while disabled,
  detection is demand-driven by a query or manual check. Generation-tagged
  events prevent a late check for a switched-away connection from changing the
  shown connection's status.
- **Crate split:** defer it. Current evidence supports stronger module
  boundaries and typed event identities, but does not yet show a stable
  independent interface that needs a separate crate.

## Review and validation

For each implementation change, update `docs/architecture.md` and
`tests/layers.rs` in the same change when the dependency contract changes.
Validate the focused behavior and layer check, then run the project checks
described in `docs/development.md` when the change is ready for broader review.
