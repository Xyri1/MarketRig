//! Installation policies and the approval vocabulary.
//!
//! Contract: `sdd/features/r5-desktop-approval-controls/SPEC.md` §2 (the
//! settings row and its resource) and §3 (the approval states the two gated
//! rows carry), per R5-1 and R5-2.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::desk::append_event;
use crate::store::{Store, StoreError};
use crate::trade::TradeError;

/// The two policy values, and two of the four approval states (§2, §3).
pub const ALWAYS_ALLOW: &str = "ALWAYS_ALLOW";
pub const REQUIRE_APPROVAL: &str = "REQUIRE_APPROVAL";
pub const PENDING: &str = "PENDING";

/// The three policy columns of the one `installation_settings` row (§2). The
/// `delivery_mode` column admits only `QUEUE`, so nothing reads it back here.
pub struct Policies {
    pub trigger_code_policy: String,
    pub paper_order_policy: String,
}

/// Reads the policies inside the unit that is about to create a code snapshot
/// or a trading action, which is what keeps them honest under a concurrent
/// `PUT` (§2). No cache.
pub fn read(tx: &Transaction<'_>) -> rusqlite::Result<Policies> {
    tx.query_row(
        "SELECT trigger_code_policy, paper_order_policy FROM installation_settings WHERE id = 1",
        [],
        |r| {
            Ok(Policies {
                trigger_code_policy: r.get(0)?,
                paper_order_policy: r.get(1)?,
            })
        },
    )
}

// ---------------------------------------------------------------------------
// The policy resource (§2)
// ---------------------------------------------------------------------------

/// The `GET`/`PUT /settings/policies` body (§2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resource {
    pub trigger_code_policy: String,
    pub paper_order_policy: String,
    pub delivery_mode: String,
    /// Steering is reserved and never offered (root §11.2, per D70).
    pub steer_available: bool,
    pub updated_at_ns: i64,
}

fn resource(conn: &Connection) -> rusqlite::Result<Resource> {
    conn.query_row(
        "SELECT trigger_code_policy, paper_order_policy, delivery_mode, updated_at_ns \
         FROM installation_settings WHERE id = 1",
        [],
        |r| {
            Ok(Resource {
                trigger_code_policy: r.get(0)?,
                paper_order_policy: r.get(1)?,
                delivery_mode: r.get(2)?,
                steer_available: false,
                updated_at_ns: r.get(3)?,
            })
        },
    )
}

/// A policy-surface failure carrying a stable code the REST layer maps to the
/// R0 envelope (§2).
#[derive(Debug)]
pub enum PolicyError {
    Validation(String),
    /// `delivery_mode: "STEER"` — the reserved mode, refused as a conflict so
    /// it reads differently from a value that does not exist at all.
    SteerDisabled,
    Store(StoreError),
}

impl PolicyError {
    pub fn code(&self) -> &'static str {
        match self {
            PolicyError::Validation(_) => "VALIDATION",
            PolicyError::SteerDisabled => "STEER_DISABLED",
            PolicyError::Store(e) => e.code(),
        }
    }
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::Validation(message) => write!(f, "{message}"),
            PolicyError::SteerDisabled => write!(
                f,
                "Steering is reserved and unavailable; delivery_mode stays QUEUE."
            ),
            PolicyError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl From<StoreError> for PolicyError {
    fn from(e: StoreError) -> Self {
        PolicyError::Store(e)
    }
}

pub fn get(store: &Store) -> Result<Resource, PolicyError> {
    Ok(store.call(resource)?)
}

/// The `PUT` (§2): any subset of the three fields, one `POLICY_CHANGED` per
/// field that actually changes, all in one unit. A `PUT` that changes nothing
/// writes nothing.
pub fn put(store: &Store, body: &Value, now_ns: i64) -> Result<Resource, PolicyError> {
    let fields = body.as_object().filter(|o| !o.is_empty()).ok_or_else(|| {
        PolicyError::Validation(
            "The request body must be a JSON object naming at least one of \
             \"trigger_code_policy\", \"paper_order_policy\", and \"delivery_mode\"."
                .to_string(),
        )
    })?;
    let mut wanted: Vec<(&'static str, String)> = Vec::new();
    for (field, value) in fields {
        let value = value.as_str().unwrap_or_default();
        let column: &'static str = match field.as_str() {
            name @ ("trigger_code_policy" | "paper_order_policy") => {
                if value != ALWAYS_ALLOW && value != REQUIRE_APPROVAL {
                    return Err(PolicyError::Validation(format!(
                        "The policy \"{field}\" must be \"{ALWAYS_ALLOW}\" or \
                         \"{REQUIRE_APPROVAL}\"."
                    )));
                }
                if name == "trigger_code_policy" {
                    "trigger_code_policy"
                } else {
                    "paper_order_policy"
                }
            }
            "delivery_mode" => {
                if value == "STEER" {
                    return Err(PolicyError::SteerDisabled);
                }
                if value != "QUEUE" {
                    return Err(PolicyError::Validation(
                        "The policy \"delivery_mode\" must be \"QUEUE\".".to_string(),
                    ));
                }
                "delivery_mode"
            }
            other => {
                return Err(PolicyError::Validation(format!(
                    "The field \"{other}\" is not a policy; the policies are \
                     \"trigger_code_policy\", \"paper_order_policy\", and \"delivery_mode\"."
                )));
            }
        };
        wanted.push((column, value.to_string()));
    }
    Ok(store.unit(move |tx| {
        let mut current = resource(tx)?;
        let mut changed = false;
        for (column, to) in &wanted {
            let from = match *column {
                "trigger_code_policy" => &mut current.trigger_code_policy,
                "paper_order_policy" => &mut current.paper_order_policy,
                _ => &mut current.delivery_mode,
            };
            if from == to {
                continue;
            }
            // `column` is one of the three literals matched above, never input.
            tx.execute(
                &format!("UPDATE installation_settings SET {column} = ?1 WHERE id = 1"),
                params![to],
            )?;
            append_event(
                tx,
                "POLICY_CHANGED",
                None,
                now_ns,
                json!({ "field": column, "from": from, "to": to }),
            )?;
            *from = to.clone();
            changed = true;
        }
        if changed {
            tx.execute(
                "UPDATE installation_settings SET updated_at_ns = ?1 WHERE id = 1",
                params![now_ns],
            )?;
            current.updated_at_ns = now_ns;
        }
        Ok(current)
    })?)
}

// ---------------------------------------------------------------------------
// The decision (§3.1)
// ---------------------------------------------------------------------------

/// The one decision body: `{"decision": "APPROVE" | "DENY"}` (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Approve => "APPROVE",
            Decision::Deny => "DENY",
        }
    }

    pub fn parse(body: &Value) -> Result<Decision, PolicyError> {
        match body.get("decision").and_then(Value::as_str) {
            Some("APPROVE") => Ok(Decision::Approve),
            Some("DENY") => Ok(Decision::Deny),
            _ => Err(PolicyError::Validation(
                "The request body must be a JSON object with \"decision\": \"APPROVE\" or \"DENY\"."
                    .to_string(),
            )),
        }
    }
}

/// Deciding one pending record fails in exactly these ways (§3.1). A trigger
/// decision never produces `Trade`; an order approval that cannot reach the
/// node does, keeping `TradeError`'s own code and status.
#[derive(Debug)]
pub enum DecideError {
    /// No `PENDING` or decided row with that id under the path's desk.
    NotFound(String),
    AlreadyDecided {
        approval: String,
    },
    Store(StoreError),
    Trade(TradeError),
}

impl DecideError {
    pub fn code(&self) -> &'static str {
        match self {
            DecideError::NotFound(_) => "APPROVAL_NOT_FOUND",
            DecideError::AlreadyDecided { .. } => "APPROVAL_DECIDED",
            DecideError::Store(e) => e.code(),
            DecideError::Trade(e) => e.code(),
        }
    }
}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecideError::NotFound(id) => {
                write!(f, "There is no approval {id} on this desk.")
            }
            DecideError::AlreadyDecided { approval } => write!(
                f,
                "This approval is already {approval} and cannot be decided again."
            ),
            DecideError::Store(e) => write!(f, "{e}"),
            DecideError::Trade(e) => write!(f, "{e}"),
        }
    }
}

impl From<StoreError> for DecideError {
    fn from(e: StoreError) -> Self {
        DecideError::Store(e)
    }
}

impl From<TradeError> for DecideError {
    fn from(e: TradeError) -> Self {
        DecideError::Trade(e)
    }
}

// ---------------------------------------------------------------------------
// The listing and the decision route (§3.1)
// ---------------------------------------------------------------------------

/// The two kinds a person decides (§3.1).
pub const TRIGGER_CODE: &str = "TRIGGER_CODE";
pub const PAPER_ORDER: &str = "PAPER_ORDER";

/// One gated record of either kind, as `GET /approvals` and the decision route
/// answer it (§3.1). `id` is the `code_snapshots` or `trading_actions` row's own
/// UUID; `detail` is the kind's own object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Approval {
    pub kind: String,
    pub id: String,
    pub desk_id: String,
    pub desk_name: String,
    pub approval: String,
    pub requested_at_ns: i64,
    /// Null exactly while `PENDING`.
    pub decided_at_ns: Option<i64>,
    pub detail: Value,
}

/// The one `UNION ALL` behind both reads (§3.1). Both arms alias their row `r`,
/// so `filter` — a literal built here, never input — reads the same in each;
/// `source` is the extra `detail` member the single read adds and the listing
/// withholds, so a listing never carries every pending script.
///
/// A snapshot's trigger is the one that currently names it, and after a patch
/// attached newer code, the trigger of the last firing that ran it; neither
/// exists for a snapshot whose trigger was hard-deleted, and then it is null.
fn sql(filter: &str, source: &str) -> String {
    format!(
        "SELECT 'TRIGGER_CODE' AS kind, r.id AS id, r.desk_id AS desk_id, d.name AS desk_name, \
                r.approval AS approval, r.created_at_ns AS requested_at_ns, \
                r.decided_at_ns AS decided_at_ns, \
                json_object({source}'trigger_id', t.id, 'trigger_name', t.name, \
                            'suffix', r.suffix, 'argv', json(r.argv), \
                            'timeout_secs', r.timeout_secs, 'fingerprint', r.fingerprint, \
                            'source_bytes', length(cast(r.source AS BLOB))) AS detail \
           FROM code_snapshots r JOIN desks d ON d.id = r.desk_id \
           LEFT JOIN triggers t ON t.id = coalesce( \
                (SELECT o.id FROM triggers o WHERE o.code_snapshot_id = r.id), \
                (SELECT f.trigger_id FROM firings f WHERE f.code_snapshot_id = r.id \
                  ORDER BY f.accepted_at_ns DESC, f.id DESC LIMIT 1)) \
          WHERE {filter} \
         UNION ALL \
         SELECT 'PAPER_ORDER', r.id, r.desk_id, d.name, r.approval, r.created_at_ns, \
                r.decided_at_ns, \
                json_object('action_id', r.action_id, 'source', r.source, \
                            'trigger_id', r.trigger_id, 'firing_id', r.firing_id, \
                            'request', json(r.request), 'outcome', json(r.outcome)) \
           FROM trading_actions r JOIN desks d ON d.id = r.desk_id \
          WHERE {filter} \
          ORDER BY requested_at_ns DESC, id DESC"
    )
}

/// One row of either arm. SQLite writes every `detail` member, so the members
/// §3.1 marks optional — a snapshot with no trigger, an order with no
/// attribution, a record with no outcome — are dropped here.
fn approval_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Approval> {
    let mut detail: Value =
        serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_else(|_| json!({}));
    if let Some(members) = detail.as_object_mut() {
        members.retain(|_, value| !value.is_null());
    }
    Ok(Approval {
        kind: r.get(0)?,
        id: r.get(1)?,
        desk_id: r.get(2)?,
        desk_name: r.get(3)?,
        approval: r.get(4)?,
        requested_at_ns: r.get(5)?,
        decided_at_ns: r.get(6)?,
        detail,
    })
}

/// `GET /approvals?state=…` (§3.1), installation-wide, newest first. A record
/// that was never gated — `ALWAYS_ALLOW` — is not an approval and is listed by
/// no state.
pub fn approvals(store: &Store, state: &str) -> Result<Vec<Approval>, PolicyError> {
    let filter = match state {
        "PENDING" => "r.approval = 'PENDING'",
        "DECIDED" => "r.approval IN ('APPROVED','DENIED')",
        "ALL" => "r.approval <> 'ALWAYS_ALLOW'",
        other => {
            return Err(PolicyError::Validation(format!(
                "The query parameter \"state\" must be \"PENDING\", \"DECIDED\", or \"ALL\", \
                 not {other:?}."
            )));
        }
    };
    let sql = sql(filter, "");
    Ok(store.call(move |conn| conn.prepare(&sql)?.query_map([], approval_row)?.collect())?)
}

/// `GET /approvals/{id}` (§3.1): the same item with the snapshot's `source`.
pub fn approval(store: &Store, id: &str) -> Result<Approval, DecideError> {
    let sql = sql("r.id = ?1", "'source', r.source, ");
    let key = id.to_owned();
    store
        .call(move |conn| conn.query_row(&sql, params![key], approval_row).optional())?
        .ok_or_else(|| DecideError::NotFound(id.to_string()))
}

/// `POST /desks/{desk_id}/approvals/{id}` (§3.1): resolve the id in
/// `code_snapshots` and then in `trading_actions`, both under the path's desk —
/// a row of another desk is simply not there — apply that kind's own decision,
/// and answer the approval as it now reads. The route wakes the scheduler,
/// since an approved trigger may have become due.
pub fn decide(
    store: &Store,
    registry: &crate::node::Registry,
    desk_id: &str,
    id: &str,
    decision: Decision,
) -> Result<Approval, DecideError> {
    match crate::trigger::decide(store, desk_id, id, decision, crate::store::now_ns()) {
        Err(DecideError::NotFound(_)) => {
            crate::trade::decide(store, registry, desk_id, id, decision)?
        }
        decided => decided?,
    }
    approval(store, id)
}

// ---------------------------------------------------------------------------
// policy (feature SPEC §8 check 1)
// ---------------------------------------------------------------------------

#[cfg(test)]
fn events(store: &Store) -> Vec<(String, Value)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT kind, payload FROM operational_events WHERE desk_id IS NULL \
                 ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                ))
            })?
            .collect()
        })
        .unwrap()
}

/// The row's defaults, `PUT` validation, the refused `STEER`, one
/// `POLICY_CHANGED` per changed field, the in-unit read, and a pending record
/// no policy change touches (§2, feature SPEC §8 check 1).
#[cfg(test)]
#[test]
fn policies_resource() {
    let (_dir, store) = crate::store::open_temp();

    // The defaults are Require approval for code, Always allow for orders.
    let defaults = Resource {
        trigger_code_policy: REQUIRE_APPROVAL.to_string(),
        paper_order_policy: ALWAYS_ALLOW.to_string(),
        delivery_mode: "QUEUE".to_string(),
        steer_available: false,
        updated_at_ns: 0,
    };
    assert_eq!(get(&store).unwrap(), defaults);
    assert_eq!(
        serde_json::to_value(get(&store).unwrap()).unwrap(),
        json!({
            "trigger_code_policy": "REQUIRE_APPROVAL",
            "paper_order_policy": "ALWAYS_ALLOW",
            "delivery_mode": "QUEUE",
            "steer_available": false,
            "updated_at_ns": 0,
        })
    );

    // A body that names nothing, an unknown field, and an unknown value are all
    // VALIDATION; none of them writes.
    for body in [
        json!({}),
        json!([]),
        json!({ "trigger_code_policy": "MAYBE" }),
        json!({ "trigger_code_policy": true }),
        json!({ "paper_order_policy": "STEER" }),
        json!({ "delivery_mode": "SHOUT" }),
        json!({ "steer_available": "true" }),
    ] {
        let e = put(&store, &body, 100).expect_err("{body} must be refused");
        assert_eq!(e.code(), "VALIDATION", "{body}: {e}");
    }

    // Steering is reserved: a conflict, no row change, no event.
    let e = put(&store, &json!({ "delivery_mode": "STEER" }), 100).unwrap_err();
    assert_eq!(e.code(), "STEER_DISABLED");
    assert_eq!(get(&store).unwrap(), defaults);
    assert!(events(&store).is_empty());

    // A PUT that changes nothing writes nothing and answers the resource.
    assert_eq!(
        put(&store, &json!({ "delivery_mode": "QUEUE" }), 100).unwrap(),
        defaults
    );
    assert!(events(&store).is_empty());

    // One event per changed field: the second field already holds its value.
    let after = put(
        &store,
        &json!({ "trigger_code_policy": "ALWAYS_ALLOW", "paper_order_policy": "ALWAYS_ALLOW" }),
        200,
    )
    .unwrap();
    assert_eq!(
        after,
        Resource {
            trigger_code_policy: ALWAYS_ALLOW.to_string(),
            updated_at_ns: 200,
            ..defaults.clone()
        }
    );
    assert_eq!(get(&store).unwrap(), after);
    assert_eq!(
        events(&store),
        vec![(
            "POLICY_CHANGED".to_string(),
            json!({
                "field": "trigger_code_policy",
                "from": "REQUIRE_APPROVAL",
                "to": "ALWAYS_ALLOW",
            })
        )]
    );

    // The in-unit read is what a snapshot or action insert sees.
    let read_back = store
        .unit(|tx| {
            let p = read(tx)?;
            Ok((p.trigger_code_policy, p.paper_order_policy))
        })
        .unwrap();
    assert_eq!(
        read_back,
        (ALWAYS_ALLOW.to_string(), ALWAYS_ALLOW.to_string())
    );

    // A pending record survives a policy change untouched: only a person's
    // decision moves it (§2).
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, \
                 ready_at_ns) VALUES ('d1','alpha','READY','/desks/alpha',1,1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO code_snapshots (id, desk_id, source, suffix, argv, timeout_secs, \
                 fingerprint, approval, decided_at_ns, created_at_ns) \
                 VALUES ('s1','d1','print(1)','.py','[\"{script}\"]',300,'ff','PENDING',NULL,1)",
                [],
            )
        })
        .unwrap();
    put(
        &store,
        &json!({ "trigger_code_policy": "REQUIRE_APPROVAL" }),
        300,
    )
    .unwrap();
    let snapshot: (String, Option<i64>) = store
        .call(|c| {
            c.query_row(
                "SELECT approval, decided_at_ns FROM code_snapshots WHERE id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(snapshot, (PENDING.to_string(), None));
    assert_eq!(events(&store).len(), 2, "the change back is its own event");
}

/// The decision body and the two decision codes (§3.1).
#[cfg(test)]
#[test]
fn decision_body() {
    assert_eq!(
        Decision::parse(&json!({ "decision": "APPROVE" })).unwrap(),
        Decision::Approve
    );
    assert_eq!(Decision::Approve.as_str(), "APPROVE");
    assert_eq!(
        Decision::parse(&json!({ "decision": "DENY" })).unwrap(),
        Decision::Deny
    );
    assert_eq!(Decision::Deny.as_str(), "DENY");
    for body in [json!({}), json!({ "decision": "MAYBE" }), json!("APPROVE")] {
        assert_eq!(Decision::parse(&body).unwrap_err().code(), "VALIDATION");
    }
    assert_eq!(
        DecideError::NotFound("s1".into()).code(),
        "APPROVAL_NOT_FOUND"
    );
    assert_eq!(
        DecideError::AlreadyDecided {
            approval: "DENIED".into()
        }
        .code(),
        "APPROVAL_DECIDED"
    );
}

// ---------------------------------------------------------------------------
// policy::approvals (feature SPEC §8 check 4)
// ---------------------------------------------------------------------------

/// 2026-09-03T12:00:00Z, so the gated records are older than any `now_ns()` a
/// submit or a decision stamps and the listing's order is known.
#[cfg(test)]
const T0: i64 = 1_788_436_800_000_000_000;

/// Two `READY` desks with both policies on **Require approval**, and a registry
/// that never starts a node — nothing below reaches the sandbox.
#[cfg(test)]
fn gated_store() -> (
    tempfile::TempDir,
    Store,
    std::sync::Arc<crate::node::Registry>,
) {
    let (dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, ready_at_ns) \
                 VALUES ('d1','alpha','READY','/desks/alpha',1,1), \
                        ('d2','beta','READY','/desks/beta',1,1)",
                [],
            )
        })
        .unwrap();
    put(
        &store,
        &json!({ "paper_order_policy": REQUIRE_APPROVAL }),
        1,
    )
    .unwrap();
    let registry = std::sync::Arc::new(crate::node::Registry::new(
        store.clone(),
        std::sync::Arc::new(crate::feed::MarketState::new()),
        None,
    ));
    (dir, store, registry)
}

/// Every desk's `APPROVAL_DECIDED`, oldest first.
#[cfg(test)]
fn decided_events(store: &Store) -> Vec<(String, Value)> {
    store
        .call(|c| {
            c.prepare(
                "SELECT desk_id, payload FROM operational_events \
                 WHERE kind = 'APPROVAL_DECIDED' ORDER BY occurred_at_ns, id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                ))
            })?
            .collect()
        })
        .unwrap()
}

/// The union listing's order and shapes, the `state` filter and its refusal,
/// the single read's `source`, desk scoping, both decision refusals, and the
/// `APPROVAL_DECIDED` payload of each kind (§3.1, feature SPEC §8 check 4).
#[cfg(test)]
#[test]
fn the_approvals_listing() {
    let (_dir, store, registry) = gated_store();
    let second = 1_000_000_000;

    // A code trigger, then a patch attaching different code: the first snapshot
    // is superseded, and only the firing that ran it still names it.
    let created = crate::trigger::create(
        &store,
        "d1",
        &format!(
            r#"{{"name":"nightly","brief":"look at AAPL","schedule":{{"at":"{}"}},
                 "code":{{"source":"print(1)","suffix":".py"}}}}"#,
            chrono::DateTime::from_timestamp_nanos(T0 + 3_600 * second).to_rfc3339()
        ),
        T0,
    )
    .expect("the create is well formed");
    let trigger_id = created["id"].as_str().unwrap().to_string();
    let superseded = created["code"]["snapshot_id"].as_str().unwrap().to_string();
    let patched = crate::trigger::patch(
        &store,
        "d1",
        &trigger_id,
        r#"{"code":{"source":"print(22)","suffix":".py"}}"#,
        T0 + second,
    )
    .unwrap();
    let current = patched["code"]["snapshot_id"].as_str().unwrap().to_string();
    let (ran, owner) = (superseded.clone(), trigger_id.clone());
    store
        .unit(move |tx| {
            tx.execute(
                "INSERT INTO firings (id, desk_id, trigger_id, occurrence_ns, accepted_at_ns, \
                 trigger_revision, brief, code_snapshot_id) \
                 VALUES ('f1','d1',?1,?2,?2,1,'look at AAPL',?3)",
                params![owner, T0, ran],
            )
        })
        .unwrap();

    // A gated order on the other desk, recorded without touching the sandbox.
    let (action, submitted) = crate::trade::submit(
        &store,
        &registry,
        "d2",
        r#"{"action_id":"buy-1","instrument_id":"AAPL.XNAS",
            "side":"BUY","type":"MARKET","quantity":"10","price":null}"#,
        &crate::trade::Source::Session,
    )
    .expect("a gated order is recorded, not refused");
    assert_eq!(submitted, crate::trade::Submitted::Pending);

    // A snapshot that was never gated is not an approval at all.
    store
        .unit(|tx| {
            tx.execute(
                "INSERT INTO code_snapshots VALUES \
                 ('s0','d1','print(0)','.py','[\"{script}\"]',300,'ff','ALWAYS_ALLOW',1,1)",
                [],
            )
        })
        .unwrap();

    // --- The listing: both kinds, one query, newest first -------------------
    let pending = approvals(&store, "PENDING").unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|a| (a.kind.as_str(), a.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (PAPER_ORDER, action.id.as_str()),
            (TRIGGER_CODE, current.as_str()),
            (TRIGGER_CODE, superseded.as_str()),
        ]
    );
    let older = &pending[2];
    assert_eq!(
        serde_json::to_value(older)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [
            "approval",
            "decided_at_ns",
            "desk_id",
            "desk_name",
            "detail",
            "id",
            "kind",
            "requested_at_ns"
        ]
    );
    assert_eq!(
        (older.desk_id.as_str(), older.desk_name.as_str()),
        ("d1", "alpha")
    );
    assert_eq!(older.approval, PENDING);
    assert_eq!(older.requested_at_ns, T0);
    assert_eq!(older.decided_at_ns, None);
    let fingerprint = older.detail["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(fingerprint.len(), 64, "a hex SHA-256");
    assert_eq!(
        older.detail,
        json!({
            "trigger_id": trigger_id,
            "trigger_name": "nightly",
            "suffix": ".py",
            "argv": ["{script}"],
            "timeout_secs": 300,
            "fingerprint": fingerprint,
            "source_bytes": 8,
        }),
        "a superseded snapshot finds its trigger through the firing that ran it"
    );
    assert_eq!(pending[1].detail["trigger_id"], trigger_id.as_str());
    assert_eq!(
        pending[0].detail,
        json!({
            "action_id": "buy-1",
            "source": "SESSION",
            "request": {
                "action_id": "buy-1",
                "instrument_id": "AAPL.XNAS",
                "side": "BUY",
                "type": "MARKET",
                "quantity": "10",
                "price": null,
            },
        }),
        "a session order has no attribution and no outcome yet"
    );

    // --- The state filter ---------------------------------------------------
    assert!(approvals(&store, "DECIDED").unwrap().is_empty());
    assert_eq!(
        approvals(&store, "ALL").unwrap().len(),
        3,
        "the ungated snapshot is no approval under any state"
    );
    for state in ["", "pending", "MAYBE"] {
        assert_eq!(approvals(&store, state).unwrap_err().code(), "VALIDATION");
    }

    // --- The single read carries the source the listing withholds -----------
    let read = approval(&store, &superseded).unwrap();
    assert_eq!(read.detail["source"], "print(1)");
    assert_eq!(read.detail.as_object().unwrap().len(), 8);
    assert!(!older.detail.as_object().unwrap().contains_key("source"));
    assert_eq!(
        approval(&store, "no-such-row").unwrap_err().code(),
        "APPROVAL_NOT_FOUND"
    );

    // --- Every decision is desk-scoped --------------------------------------
    assert_eq!(
        decide(&store, &registry, "d2", &superseded, Decision::Approve)
            .unwrap_err()
            .code(),
        "APPROVAL_NOT_FOUND",
        "another desk's path never finds this snapshot"
    );
    assert_eq!(approval(&store, &superseded).unwrap().approval, PENDING);

    // --- One route, two kinds, and the decision answers the record ----------
    let denied = decide(&store, &registry, "d2", &action.id, Decision::Deny).unwrap();
    assert_eq!(denied.approval, "DENIED");
    assert!(denied.decided_at_ns.is_some());
    assert_eq!(
        denied.detail["outcome"],
        json!({ "failure_code": "DENIED" })
    );
    let approved = decide(&store, &registry, "d1", &current, Decision::Approve).unwrap();
    assert_eq!(approved.approval, "APPROVED");
    assert_eq!(approved.detail["source"], "print(22)");
    assert_eq!(
        decided_events(&store),
        vec![
            (
                "d2".to_string(),
                json!({ "kind": "PAPER_ORDER", "id": action.id, "decision": "DENY" })
            ),
            (
                "d1".to_string(),
                json!({ "kind": "TRIGGER_CODE", "id": current, "decision": "APPROVE" })
            ),
        ]
    );

    // --- A decided record names its state and stays put ---------------------
    for (desk, id, state) in [("d2", &action.id, "DENIED"), ("d1", &current, "APPROVED")] {
        let again = decide(&store, &registry, desk, id, Decision::Approve).unwrap_err();
        assert_eq!(again.code(), "APPROVAL_DECIDED");
        assert!(again.to_string().contains(state), "{again}");
    }
    assert_eq!(
        approvals(&store, "PENDING")
            .unwrap()
            .iter()
            .map(|a| a.id.clone())
            .collect::<Vec<_>>(),
        vec![superseded]
    );
    assert_eq!(approvals(&store, "DECIDED").unwrap().len(), 2);
    assert_eq!(approvals(&store, "ALL").unwrap().len(), 3);
}
