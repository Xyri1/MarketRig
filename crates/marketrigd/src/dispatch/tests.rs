//! Feature SPEC §10 check 5 (the dispatcher) and check 6 (the §7 routes).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::fake::Fake;
use super::*;
use crate::store::Store;

/// A store with two READY desks and both runtimes available, plus the two
/// fakes the adapters are.
struct Rig {
    _dir: tempfile::TempDir,
    store: Store,
    dispatcher: Arc<Dispatcher>,
    codex: Arc<Fake>,
    claude: Arc<Fake>,
    events: mpsc::UnboundedReceiver<AdapterEvent>,
}

const DAEMON: &str = "01999999-0000-7000-8000-0000000000dd";

fn rig() -> Rig {
    rig_with(READY_DEADLINE)
}

fn rig_with(deadline: Duration) -> Rig {
    let (dir, store) = crate::store::open_temp();
    store
        .unit(|tx| {
            for (id, name, runtime) in [("d1", "alpha", "codex"), ("d2", "beta", "claude")] {
                tx.execute(
                    "INSERT INTO desks (id, name, state, workspace_path, created_at_ns, \
                     ready_at_ns, selected_runtime) VALUES (?1, ?2, 'READY', ?3, 1, 2, ?4)",
                    rusqlite::params![id, name, format!("/desks/{name}"), runtime],
                )?;
            }
            tx.execute(
                "UPDATE runtimes SET state = 'AVAILABLE', executable_path = '/x/rt', \
                 version = '99.0.0', validated_at_ns = 3",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (events_tx, events) = mpsc::unbounded_channel();
    let codex = Fake::new(events_tx.clone());
    let claude = Fake::new(events_tx);
    let dispatcher = Arc::new(Dispatcher {
        store: store.clone(),
        adapters: Adapters {
            codex: codex.clone(),
            claude: claude.clone(),
        },
        daemon_uuid: DAEMON.to_string(),
        notify: Arc::new(Notify::new()),
        live: Mutex::new(HashMap::new()),
        activating: Mutex::new(HashMap::new()),
        ready_deadline: deadline,
        poll: POLL,
    });
    Rig {
        _dir: dir,
        store,
        dispatcher,
        codex,
        claude,
        events,
    }
}

impl Rig {
    fn queue(&self, desk_id: &str, created_at_ns: i64) -> String {
        let id = format!("p-{desk_id}-{created_at_ns}");
        let (row, desk) = (id.clone(), desk_id.to_string());
        self.store
            .unit(move |tx| {
                tx.execute(
                    "INSERT INTO prompts (id, desk_id, kind, state, payload, created_at_ns) \
                     VALUES (?1, ?2, 'TRIGGER_RESULT', 'QUEUED', '{\"brief\":\"look\"}', ?3)",
                    rusqlite::params![row, desk, created_at_ns],
                )?;
                Ok(())
            })
            .unwrap();
        id
    }

    fn pointer(&self, desk_id: &str, runtime: &str, native: &str) {
        session::repoint(&self.store, desk_id, runtime, Some(native), "test").unwrap();
    }

    /// One pass, then every event the fakes emitted, as `run` would.
    async fn tick(&mut self) {
        self.dispatcher.pass().await;
        self.drain().await;
    }

    async fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.dispatcher.event(event).await;
        }
    }

    fn prompt(&self, id: &str) -> (String, Option<String>, Option<i64>, Option<i64>) {
        let id = id.to_string();
        self.store
            .call(move |c| {
                c.query_row(
                    "SELECT state, failure_code, attempted_at_ns, disclosed_at_ns FROM prompts \
                     WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
            })
            .unwrap()
    }

    fn events_of(&self, desk_id: &str) -> Vec<(String, String)> {
        let desk = desk_id.to_string();
        self.store
            .call(move |c| {
                c.prepare(
                    "SELECT kind, payload FROM operational_events WHERE desk_id = ?1 \
                     ORDER BY occurred_at_ns, id",
                )?
                .query_map([desk], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
            })
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// check 5 — the dispatcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fifo_one_prompt_per_pass_behind_the_orientation() {
    let mut rig = rig();
    let first = rig.queue("d1", 100);
    let second = rig.queue("d1", 200);

    // Pass one activates; the session is not ready until its event is drained,
    // so nothing is delivered yet.
    rig.dispatcher.pass().await;
    assert!(rig.codex.delivered.lock().unwrap().is_empty());
    rig.drain().await;

    // A new session heads the FIFO with its orientation, and each pass hands
    // over exactly one prompt.
    rig.tick().await;
    assert_eq!(rig.codex.kinds(), ["ORIENTATION"]);
    rig.tick().await;
    rig.tick().await;
    assert_eq!(
        rig.codex.kinds(),
        ["ORIENTATION", "TRIGGER_RESULT", "TRIGGER_RESULT"]
    );
    let handed = rig.codex.delivered.lock().unwrap().clone();
    assert_eq!(handed[1].prompt_id, first);
    assert_eq!(handed[2].prompt_id, second);
    assert!(handed[0].text.contains("marketrig://desk/alpha/quotes"));
    assert!(handed[0].text.contains("/desks/alpha/AGENTS.md"));
    assert!(
        handed[1]
            .text
            .starts_with(&format!("MarketRig TRIGGER_RESULT {first}:"))
    );

    assert_eq!(rig.prompt(&first).0, "DELIVERED");
    let kinds: Vec<String> = rig.events_of("d1").into_iter().map(|e| e.0).collect();
    assert_eq!(
        kinds,
        [
            "SESSION_STARTED",
            "SESSION_POINTER_CHANGED",
            "SESSION_READY",
            "PROMPT_DELIVERED",
            "PROMPT_DELIVERED",
            "PROMPT_DELIVERED",
        ]
    );
}

#[tokio::test]
async fn a_waiting_gate_leaves_the_row_untouched() {
    let mut rig = rig();
    let id = rig.queue("d1", 100);
    *rig.codex.outcomes.lock().unwrap() = vec![DeliverOutcome::Waiting, DeliverOutcome::Waiting];
    rig.tick().await;
    rig.tick().await;
    let (state, code, attempted, _) = rig.prompt(&id);
    assert_eq!((state.as_str(), code), ("QUEUED", None));
    assert_eq!(attempted, None, "a wait is not an attempt (§6.2, §8)");
}

#[tokio::test]
async fn an_uncertain_handoff_fails_that_prompt_only() {
    let mut rig = rig();
    let id = rig.queue("d1", 100);
    *rig.codex.outcomes.lock().unwrap() = vec![
        DeliverOutcome::Delivered, // the orientation
        DeliverOutcome::HandoffUnknown,
    ];
    for _ in 0..3 {
        rig.tick().await;
    }
    assert_eq!(
        rig.prompt(&id),
        (
            "FAILED".to_string(),
            Some("HANDOFF_UNKNOWN".to_string()),
            rig.prompt(&id).2,
            None
        )
    );
    assert!(rig.prompt(&id).2.is_some(), "the attempt is evidence");
}

#[tokio::test]
async fn an_activation_failure_fails_only_that_desks_queue() {
    let mut rig = rig();
    let mine = rig.queue("d1", 100);
    let theirs = rig.queue("d2", 110);
    *rig.codex.spawn_fails.lock().unwrap() = Some("no executable".to_string());

    for _ in 0..3 {
        rig.tick().await;
    }
    assert_eq!(
        (rig.prompt(&mine).0, rig.prompt(&mine).1),
        ("FAILED".to_string(), Some("ACTIVATION_FAILED".to_string()))
    );
    assert_eq!(rig.prompt(&theirs).0, "DELIVERED");
    assert_eq!(rig.claude.kinds(), ["ORIENTATION", "TRIGGER_RESULT"]);
}

#[tokio::test]
async fn an_unavailable_runtime_fails_the_queue_before_any_spawn() {
    let mut rig = rig();
    rig.store
        .unit(|tx| {
            tx.execute(
                "UPDATE runtimes SET state = 'UNAVAILABLE', failure_code = 'NOT_FOUND' \
                 WHERE runtime = 'codex'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let id = rig.queue("d1", 100);
    rig.tick().await;
    assert_eq!(
        (rig.prompt(&id).0, rig.prompt(&id).1),
        (
            "FAILED".to_string(),
            Some("RUNTIME_UNAVAILABLE".to_string())
        )
    );
    assert!(rig.codex.spawns.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unresumable_resume_starts_new_once_on_the_dispatcher_path() {
    let mut rig = rig();
    rig.pointer("d1", "codex", "th-1");
    let id = rig.queue("d1", 100);
    rig.codex.ready_on_spawn.store(false, Ordering::SeqCst);

    rig.tick().await;
    assert_eq!(
        rig.codex.spawns.lock().unwrap().clone(),
        [Some("th-1".to_string())]
    );
    // The TUI exits before readiness: unresumable.
    rig.dispatcher
        .event(AdapterEvent::Exited {
            desk_id: "d1".to_string(),
            reason: "EXITED",
            code: Some(1),
        })
        .await;
    assert_eq!(
        rig.codex.spawns.lock().unwrap().clone(),
        [Some("th-1".to_string()), None],
        "the pointer is dropped and a new session started once"
    );
    assert_eq!(
        session::pointers(&rig.store, "d1").unwrap()["codex"],
        "native-d1",
        "the unresumable pointer was cleared and the new session's written"
    );
    assert_eq!(rig.prompt(&id).0, "QUEUED");
    let kinds: Vec<String> = rig.events_of("d1").into_iter().map(|e| e.0).collect();
    assert_eq!(
        kinds,
        [
            "SESSION_POINTER_CHANGED",
            "SESSION_STARTED",
            "SESSION_EXITED",
            "SESSION_POINTER_CHANGED",
            "SESSION_STARTED",
            "SESSION_POINTER_CHANGED",
        ]
    );

    // The retry failing too is an activation failure, not a second retry.
    rig.dispatcher
        .event(AdapterEvent::Exited {
            desk_id: "d1".to_string(),
            reason: "EXITED",
            code: Some(2),
        })
        .await;
    assert_eq!(rig.codex.spawns.lock().unwrap().len(), 2);
    assert_eq!(
        (rig.prompt(&id).0, rig.prompt(&id).1),
        ("FAILED".to_string(), Some("ACTIVATION_FAILED".to_string()))
    );
    let failed = rig
        .events_of("d1")
        .into_iter()
        .find(|e| e.0 == "PROMPT_FAILED")
        .unwrap();
    assert!(failed.1.contains(r#""failure_detail":2"#));
}

#[tokio::test]
async fn an_explicit_continue_never_becomes_a_new_session() {
    let rig = rig();
    rig.pointer("d1", "codex", "th-1");
    rig.codex.ready_on_spawn.store(false, Ordering::SeqCst);
    rig.dispatcher.activate("d1", Mode::Continue).await.unwrap();
    rig.dispatcher
        .event(AdapterEvent::Exited {
            desk_id: "d1".to_string(),
            reason: "EXITED",
            code: Some(1),
        })
        .await;
    assert_eq!(rig.codex.spawns.lock().unwrap().len(), 1);
    assert_eq!(
        session::pointers(&rig.store, "d1").unwrap()["codex"],
        "th-1",
        "evidence only: the pointer stands"
    );
}

#[tokio::test]
async fn the_readiness_deadline_fails_the_queue() {
    let mut rig = rig_with(Duration::from_millis(1));
    let id = rig.queue("d1", 100);
    rig.codex.ready_on_spawn.store(false, Ordering::SeqCst);
    rig.tick().await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    rig.tick().await;
    assert_eq!(
        (rig.prompt(&id).0, rig.prompt(&id).1),
        ("FAILED".to_string(), Some("ACTIVATION_FAILED".to_string()))
    );
    let failed = rig
        .events_of("d1")
        .into_iter()
        .find(|e| e.0 == "PROMPT_FAILED")
        .unwrap();
    assert!(failed.1.contains(r#""failure_detail":"timeout""#));
}

#[tokio::test]
async fn a_failure_is_disclosed_once_to_the_next_new_session() {
    let mut rig = rig();
    let failed = rig.queue("d1", 100);
    *rig.codex.outcomes.lock().unwrap() =
        vec![DeliverOutcome::Delivered, DeliverOutcome::HandoffUnknown];
    for _ in 0..3 {
        rig.tick().await;
    }
    assert_eq!(rig.prompt(&failed).1, Some("HANDOFF_UNKNOWN".to_string()));

    // The session ends; the next activation is new and discloses it first.
    rig.dispatcher.exit("d1").await.unwrap();
    rig.drain().await;
    rig.queue("d1", 300);
    rig.dispatcher.activate("d1", Mode::New).await.unwrap();
    rig.drain().await;
    rig.tick().await;
    assert_eq!(
        rig.codex.kinds(),
        ["ORIENTATION", "TRIGGER_RESULT", "DISCLOSURE"],
        "the disclosure heads the new session's FIFO"
    );
    let disclosure = rig.codex.delivered.lock().unwrap()[2].text.clone();
    assert!(disclosure.contains(&format!("{failed} TRIGGER_RESULT HANDOFF_UNKNOWN")));
    assert!(
        rig.prompt(&failed).3.is_some(),
        "disclosed in the same unit"
    );

    // A third session has nothing to disclose.
    rig.dispatcher.exit("d1").await.unwrap();
    rig.drain().await;
    rig.queue("d1", 400);
    rig.dispatcher.activate("d1", Mode::New).await.unwrap();
    rig.drain().await;
    rig.tick().await;
    rig.tick().await;
    rig.tick().await;
    assert_eq!(
        rig.codex
            .kinds()
            .iter()
            .filter(|k| *k == "DISCLOSURE")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_discovered_pointer_reaches_the_row_and_the_desk() {
    let mut rig = rig();
    rig.queue("d1", 100);
    rig.tick().await;
    rig.dispatcher
        .event(AdapterEvent::PointerDiscovered {
            desk_id: "d1".to_string(),
            native_session_id: "th-9".to_string(),
        })
        .await;
    assert_eq!(
        session::pointers(&rig.store, "d1").unwrap()["codex"],
        "th-9"
    );
    // The fake already named the row, so the discovery never overwrites it.
    let process = session::live_process(&rig.store, "d1").unwrap().unwrap();
    assert_eq!(process.native_session_id.as_deref(), Some("native-d1"));
}

/// §6.1: a child that reports itself before the dispatcher has opened its row
/// is held, not dropped — readiness still lands.
#[tokio::test]
async fn readiness_inside_the_spawn_still_marks_the_row_ready() {
    let mut rig = rig();
    *rig.codex.ready_inside_spawn.lock().unwrap() = Some(Arc::downgrade(&rig.dispatcher));
    rig.queue("d1", 100);
    rig.tick().await;
    let process = session::live_process(&rig.store, "d1").unwrap().unwrap();
    assert!(process.ready_at_ns.is_some(), "the early Ready was kept");
    assert!(
        rig.events_of("d1")
            .iter()
            .any(|(kind, _)| kind == "SESSION_READY"),
        "SESSION_READY lands"
    );
}

#[tokio::test]
async fn attention_becomes_one_row() {
    let rig = rig();
    rig.dispatcher
        .event(AdapterEvent::Attention {
            desk_id: "d1".to_string(),
            kind: "system_error".to_string(),
            detail: serde_json::json!({"message": "boom"}),
        })
        .await;
    let last = rig.events_of("d1").pop().unwrap();
    assert_eq!(last.0, "SESSION_ATTENTION");
    assert!(last.1.contains(r#""kind":"system_error""#));
    assert!(last.1.contains("boom"));
}

// ---------------------------------------------------------------------------
// check 6 — the §7 session routes
// ---------------------------------------------------------------------------

const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Served {
    rig: Rig,
    base: String,
}

async fn serve(rig: Rig) -> Served {
    let roots = crate::store::Roots::resolve(Some(rig._dir.path())).unwrap();
    roots.create_dirs().unwrap();
    let state = crate::api::ApiState {
        store: rig.store.clone(),
        desks_home: std::path::PathBuf::from("/desks"),
        daemon_uuid: DAEMON.to_string(),
        credential: CREDENTIAL.to_string(),
        started_at_ns: 1,
        quit: tokio::sync::mpsc::channel(1).0,
        registry: Arc::new(crate::node::Registry::new(
            rig.store.clone(),
            Arc::new(crate::feed::MarketState::new()),
            None,
        )),
        scheduler_wake: Arc::new(Notify::new()),
        search_path: String::new(),
        terminals: crate::terminal::Manager::new().0,
        channels: Arc::new(crate::claude::Channels::default()),
        dispatch: rig.dispatcher.clone(),
        memory: Arc::new(crate::memory::seam_memory(rig.store.clone(), roots)),
        events: crate::events::Publisher::new(rig.store.clone()).unwrap(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = crate::api::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Served { rig, base }
}

impl Served {
    fn post(&self, path: &str, body: &str) -> (u16, Value) {
        let request = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build(),
        )
        .post(format!("{}{path}", self.base))
        .header("Authorization", format!("Bearer {CREDENTIAL}"))
        .header("content-type", "application/json");
        let mut response = request.send(body).unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap();
        (status, serde_json::from_str(&text).unwrap())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_session_routes_answer_section_seven() {
    let mut served = serve(rig()).await;

    // Activate: a bad mode, then NEW, then the live conflict.
    let (status, body) = served.post("/desks/d1/session/activate", r#"{"mode":"RESUME"}"#);
    assert_eq!((status, body["code"].as_str()), (400, Some("VALIDATION")));
    let (status, body) = served.post("/desks/d1/session/activate", r#"{"mode":"NEW"}"#);
    assert_eq!(status, 202);
    assert_eq!(body["process"]["runtime"], "codex");
    let (status, body) = served.post("/desks/d1/session/activate", r#"{"mode":"NEW"}"#);
    assert_eq!((status, body["code"].as_str()), (409, Some("SESSION_LIVE")));

    // Interrupt: the turn, then the runtime's refusals.
    let (status, body) = served.post("/desks/d1/session/interrupt", "");
    assert_eq!((status, body["turn_id"].as_str()), (202, Some("turn-1")));
    assert!(
        served
            .rig
            .events_of("d1")
            .iter()
            .any(|e| e.0 == "SESSION_INTERRUPTED" && e.1.contains("turn-1"))
    );
    *served.rig.codex.interrupt.lock().unwrap() =
        Err(("NO_ACTIVE_TURN", "no turn is active".to_string()));
    let (status, body) = served.post("/desks/d1/session/interrupt", "");
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("NO_ACTIVE_TURN"))
    );
    *served.rig.codex.interrupt.lock().unwrap() =
        Err(("RUNTIME_ERROR", "the app-server said no".to_string()));
    let (status, body) = served.post("/desks/d1/session/interrupt", "");
    assert_eq!(
        (status, body["code"].as_str()),
        (502, Some("RUNTIME_ERROR"))
    );

    // Exit: the row closes INTERRUPTED, and a second exit has nothing to end.
    let events = tokio::spawn({
        let dispatcher = served.rig.dispatcher.clone();
        let mut rx = std::mem::replace(&mut served.rig.events, mpsc::unbounded_channel().1);
        async move {
            while let Some(event) = rx.recv().await {
                dispatcher.event(event).await;
            }
        }
    });
    let (status, _) = served.post("/desks/d1/session/exit", "");
    assert_eq!(status, 202);
    let exited = served
        .rig
        .events_of("d1")
        .into_iter()
        .rfind(|e| e.0 == "SESSION_EXITED")
        .unwrap();
    assert!(exited.1.contains(r#""reason":"INTERRUPTED""#));
    let (status, body) = served.post("/desks/d1/session/exit", "");
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("NO_LIVE_SESSION"))
    );
    let (status, body) = served.post("/desks/d1/session/interrupt", "");
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("NO_LIVE_SESSION"))
    );

    // Switch: validation first, so a refusal costs the live session nothing.
    served.post("/desks/d1/session/activate", r#"{"mode":"NEW"}"#);
    let before = served.rig.codex.exits.load(Ordering::SeqCst);
    let (status, body) = served.post("/desks/d1/session/switch", r#"{"runtime":"nano"}"#);
    assert_eq!((status, body["code"].as_str()), (400, Some("VALIDATION")));
    let (status, body) = served.post("/desks/d1/session/switch", r#"{"runtime":"codex"}"#);
    assert_eq!((status, body["code"].as_str()), (409, Some("SAME_RUNTIME")));
    served
        .rig
        .store
        .unit(|tx| {
            tx.execute(
                "UPDATE runtimes SET state = 'UNAVAILABLE' WHERE runtime = 'claude'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (status, body) = served.post("/desks/d1/session/switch", r#"{"runtime":"claude"}"#);
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("RUNTIME_UNAVAILABLE"))
    );
    assert_eq!(
        served.rig.codex.exits.load(Ordering::SeqCst),
        before,
        "nothing was stopped for a refused switch"
    );
    assert!(
        session::live_process(&served.rig.store, "d1")
            .unwrap()
            .is_some()
    );

    // The switch that is allowed ends the session and moves the desk.
    served
        .rig
        .store
        .unit(|tx| {
            tx.execute(
                "UPDATE runtimes SET state = 'AVAILABLE' WHERE runtime = 'claude'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (status, body) = served.post("/desks/d1/session/switch", r#"{"runtime":"claude"}"#);
    assert_eq!(status, 200);
    assert_eq!(body["selected_runtime"], "claude");
    assert!(body["pointers"].is_object());
    assert!(
        session::live_process(&served.rig.store, "d1")
            .unwrap()
            .is_none()
    );
    assert!(
        served
            .rig
            .events_of("d1")
            .iter()
            .any(|e| e.0 == "RUNTIME_SWITCHED")
    );

    // A desk with no pointer cannot Continue.
    let (status, body) = served.post("/desks/d1/session/activate", r#"{"mode":"CONTINUE"}"#);
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("NO_NATIVE_SESSION"))
    );
    events.abort();
}

#[tokio::test]
async fn quit_closes_every_open_row() {
    let mut rig = rig();
    rig.queue("d1", 100);
    rig.queue("d2", 100);
    rig.tick().await;
    rig.dispatcher.quit_rows().unwrap();
    let reasons: Vec<String> = rig
        .store
        .call(|c| {
            c.prepare("SELECT exit_reason FROM agent_processes ORDER BY desk_id")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .unwrap();
    assert_eq!(reasons, ["QUIT", "QUIT"]);
    assert!(session::live_process(&rig.store, "d1").unwrap().is_none());
}
