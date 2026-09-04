//! Installation policies and the approval vocabulary.
//!
//! Contract: `sdd/features/r5-desktop-approval-controls/SPEC.md` §2 (the
//! settings row and its resource) and §3 (the approval states the two gated
//! rows carry), per R5-1 and R5-2.

use rusqlite::{Connection, Transaction, params};
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
