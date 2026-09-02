//! The scripted adapter the dispatcher and session-route checks drive
//! (feature SPEC §10 checks 5 and 6). No runtime, no process: every outcome is
//! set by the test.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::session::{Activation, Adapter, AdapterEvent, AdapterEvents, DeliverOutcome};

/// One delivery the fake took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handed {
    pub desk_id: String,
    pub prompt_id: String,
    pub kind: String,
    pub text: String,
}

pub struct Fake {
    pub events: AdapterEvents,
    /// The spawn fails with this detail instead of starting.
    pub spawn_fails: Mutex<Option<String>>,
    /// Every spawn emits `Ready` immediately.
    pub ready_on_spawn: AtomicBool,
    /// The `resume` argument of every spawn, in order.
    pub spawns: Mutex<Vec<Option<String>>>,
    pub delivered: Mutex<Vec<Handed>>,
    /// What the next deliveries answer, oldest first; `Delivered` once empty.
    pub outcomes: Mutex<Vec<DeliverOutcome>>,
    pub interrupt: Mutex<Result<String, (&'static str, String)>>,
    pub exits: AtomicU32,
    /// `exit` emits `Exited { reason: "INTERRUPTED" }`, as a terminal would.
    pub exit_ends: AtomicBool,
    /// When set, the spawn reports `Ready` *into the dispatcher* before it
    /// returns — a child that is up before the process row is.
    pub ready_inside_spawn: Mutex<Option<std::sync::Weak<super::Dispatcher>>>,
}

impl Fake {
    pub fn new(events: AdapterEvents) -> Arc<Fake> {
        Arc::new(Fake {
            events,
            spawn_fails: Mutex::new(None),
            ready_on_spawn: AtomicBool::new(true),
            spawns: Mutex::new(Vec::new()),
            delivered: Mutex::new(Vec::new()),
            outcomes: Mutex::new(Vec::new()),
            interrupt: Mutex::new(Ok("turn-1".to_string())),
            exits: AtomicU32::new(0),
            exit_ends: AtomicBool::new(true),
            ready_inside_spawn: Mutex::new(None),
        })
    }

    pub fn kinds(&self) -> Vec<String> {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.kind.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl Adapter for Fake {
    async fn spawn(&self, desk_id: &str, resume: Option<&str>) -> Result<Activation, String> {
        self.spawns.lock().unwrap().push(resume.map(str::to_string));
        if let Some(detail) = self.spawn_fails.lock().unwrap().clone() {
            return Err(detail);
        }
        let early = self.ready_inside_spawn.lock().unwrap().clone();
        if let Some(dispatcher) = early.and_then(|d| d.upgrade()) {
            dispatcher
                .event(AdapterEvent::Ready {
                    desk_id: desk_id.to_string(),
                })
                .await;
        } else if self.ready_on_spawn.load(Ordering::SeqCst) {
            let _ = self.events.send(AdapterEvent::Ready {
                desk_id: desk_id.to_string(),
            });
        }
        Ok(Activation {
            pid: 4242,
            native_session_id: resume
                .map(str::to_string)
                .or_else(|| Some(format!("native-{desk_id}"))),
        })
    }

    async fn deliver(
        &self,
        desk_id: &str,
        prompt_id: &str,
        kind: &str,
        text: &str,
    ) -> DeliverOutcome {
        let outcome = {
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.is_empty() {
                DeliverOutcome::Delivered
            } else {
                outcomes.remove(0)
            }
        };
        if outcome != DeliverOutcome::Waiting {
            self.delivered.lock().unwrap().push(Handed {
                desk_id: desk_id.to_string(),
                prompt_id: prompt_id.to_string(),
                kind: kind.to_string(),
                text: text.to_string(),
            });
        }
        outcome
    }

    async fn interrupt(&self, _desk_id: &str) -> Result<String, (&'static str, String)> {
        self.interrupt.lock().unwrap().clone()
    }

    async fn exit(&self, desk_id: &str) {
        self.exits.fetch_add(1, Ordering::SeqCst);
        if self.exit_ends.load(Ordering::SeqCst) {
            let _ = self.events.send(AdapterEvent::Exited {
                desk_id: desk_id.to_string(),
                reason: "INTERRUPTED",
                code: None,
            });
        }
    }
}

/// A dispatcher over two fakes, for the checks that only need `ApiState` to be
/// complete.
pub fn dispatcher(store: crate::store::Store, daemon_uuid: &str) -> Arc<super::Dispatcher> {
    let (events, rx) = tokio::sync::mpsc::unbounded_channel();
    // Nothing drains it; the fakes' evidence is not this check's subject.
    std::mem::forget(rx);
    Arc::new(super::Dispatcher {
        store,
        adapters: super::Adapters {
            codex: Fake::new(events.clone()),
            claude: Fake::new(events),
        },
        daemon_uuid: daemon_uuid.to_string(),
        notify: Arc::new(tokio::sync::Notify::new()),
        live: Mutex::new(std::collections::HashMap::new()),
        activating: Mutex::new(std::collections::HashMap::new()),
        ready_deadline: super::READY_DEADLINE,
        poll: super::POLL,
    })
}
