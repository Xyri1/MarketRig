//! Endpoint discovery, verification, and the one blocking HTTP client.
//!
//! Feature SPEC `r0-workspace-desk-identity` §2 (roots), §5.2 (verification),
//! §6 (routes and envelope), §8 (timeouts and proxy rules).

use std::path::PathBuf;
use std::time::Duration;

/// A failure with the exit code it maps to (feature SPEC §8).
pub struct Fault {
    pub code: String,
    pub message: String,
    pub exit: i32,
}

impl Fault {
    /// No usable daemon: missing pointer, connection failure, `401`, or a
    /// daemon-UUID mismatch. Exit `3`; the CLI never spawns a daemon (R0-1).
    pub fn unreachable(message: impl Into<String>) -> Self {
        Self {
            code: "DAEMON_UNREACHABLE".to_string(),
            message: message.into(),
            exit: 3,
        }
    }

    /// A daemon-reported error envelope. Exit `1`.
    pub fn reported(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit: 1,
        }
    }
}

/// The application-data root (feature SPEC §2).
///
/// Deliberately duplicated from the daemon: ten lines of `std` beat a
/// dependency edge from the CLI onto `marketrigd`.
fn data_root() -> Option<PathBuf> {
    if let Some(scratch) = std::env::var_os("MARKETRIG_TEST_DATA_ROOT") {
        return Some(PathBuf::from(scratch).join("data"));
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("MarketRig"))
    }
    // R0 ships macOS and Windows only (feature SPEC §2).
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(|d| PathBuf::from(d).join("Library/Application Support/MarketRig"))
    }
}

#[derive(serde::Deserialize)]
struct EndpointFile {
    port: u16,
    credential: String,
    daemon_uuid: String,
}

#[derive(serde::Deserialize)]
struct Health {
    daemon_uuid: String,
}

/// A verified daemon endpoint. Constructing one proves §5.2 passed.
pub struct Endpoint {
    agent: ureq::Agent,
    base: String,
    credential: String,
}

impl Endpoint {
    pub fn discover() -> Result<Self, Fault> {
        let path = data_root()
            .ok_or_else(|| Fault::unreachable("Cannot resolve the MarketRig data root."))?
            .join("runtime")
            .join("endpoint.json");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| Fault::unreachable(format!("Cannot read {}: {e}.", path.display())))?;
        let file: EndpointFile = serde_json::from_str(&raw)
            .map_err(|e| Fault::unreachable(format!("Cannot parse {}: {e}.", path.display())))?;

        let endpoint = Self {
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_connect(Some(Duration::from_secs(2)))
                    .timeout_global(Some(Duration::from_secs(10)))
                    .max_redirects(0)
                    .proxy(None)
                    .http_status_as_error(false)
                    .build(),
            ),
            base: format!("http://127.0.0.1:{}", file.port),
            credential: file.credential,
        };

        let health: Health = serde_json::from_str(&endpoint.get("/health")?).map_err(|e| {
            Fault::unreachable(format!("The daemon health response is unusable: {e}."))
        })?;
        if health.daemon_uuid != file.daemon_uuid {
            return Err(Fault::unreachable(format!(
                "{} names daemon {} but the listener reports {}.",
                path.display(),
                file.daemon_uuid,
                health.daemon_uuid
            )));
        }
        Ok(endpoint)
    }

    pub fn get(&self, path: &str) -> Result<String, Fault> {
        finish(self.authorize(self.agent.get(self.uri(path))).call())
    }

    pub fn post(&self, path: &str, body: Option<serde_json::Value>) -> Result<String, Fault> {
        self.send(self.agent.post(self.uri(path)), body)
    }

    /// Partial update — the trigger group's one mutation shape (R2 feature
    /// SPEC §8), carrying the same headers and timeouts as [`Self::post`].
    pub fn patch(&self, path: &str, body: Option<serde_json::Value>) -> Result<String, Fault> {
        self.send(self.agent.patch(self.uri(path)), body)
    }

    /// Soft delete (R2 feature SPEC §8); no request body either way.
    pub fn delete(&self, path: &str) -> Result<String, Fault> {
        finish(self.authorize(self.agent.delete(self.uri(path))).call())
    }

    fn uri(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The bearer and, under a trigger, the attribution pair (§6): every
    /// request carries the same headers whatever its method.
    fn authorize<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        attribute(request.header("Authorization", format!("Bearer {}", self.credential)))
    }

    fn send(
        &self,
        request: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: Option<serde_json::Value>,
    ) -> Result<String, Fault> {
        let request = self.authorize(request);
        finish(match body {
            Some(body) => request.send_json(body),
            None => request.send_empty(),
        })
    }
}

/// The attribution pair a trigger's environment carries, or nothing — both
/// variables, both non-empty, or neither header rides (R2 feature SPEC §6).
pub fn attribution(var: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let value = |name| var(name).filter(|v| !v.is_empty());
    Some((
        value("MARKETRIG_TRIGGER_ID")?,
        value("MARKETRIG_FIRING_ID")?,
    ))
}

/// Adds that pair to a request; the daemon validates it against the firing row.
fn attribute<B>(request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
    match attribution(|name| std::env::var(name).ok()) {
        Some((trigger_id, firing_id)) => request
            .header("X-MarketRig-Trigger-Id", trigger_id)
            .header("X-MarketRig-Firing-Id", firing_id),
        None => request,
    }
}

/// Map a response to the raw success body or the §6 envelope.
fn finish(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, Fault> {
    let mut response = response
        .map_err(|e| Fault::unreachable(format!("Cannot reach the MarketRig daemon: {e}.")))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| Fault::unreachable(format!("Cannot read the daemon response: {e}.")))?;
    match status {
        200..=299 => Ok(body),
        // A 401 against the discovered endpoint is "no usable daemon" (§8).
        401 => Err(Fault::unreachable(
            "The daemon rejected the credential in runtime/endpoint.json.",
        )),
        _ => Err(envelope(status, &body)),
    }
}

fn envelope(status: u16, body: &str) -> Fault {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => match (value["code"].as_str(), value["message"].as_str()) {
            (Some(code), Some(message)) => Fault::reported(code, message),
            _ => Fault::reported(
                "INTERNAL",
                format!("The daemon answered {status} without an error envelope."),
            ),
        },
        Err(_) => Fault::reported(
            "INTERNAL",
            format!("The daemon answered {status} with an unreadable body."),
        ),
    }
}

// ---------------------------------------------------------------------------
// client::attribution_headers_from_env (R2 feature SPEC §11)
// ---------------------------------------------------------------------------

/// Both variables set means both headers; anything less means neither. Driven
/// through the pure lookup, so no test touches this process's environment.
#[cfg(test)]
#[test]
fn attribution_headers_from_env() {
    let env = |pairs: &'static [(&str, &str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    };
    assert_eq!(
        attribution(env(&[
            ("MARKETRIG_TRIGGER_ID", "t-1"),
            ("MARKETRIG_FIRING_ID", "f-1"),
        ])),
        Some(("t-1".to_string(), "f-1".to_string()))
    );
    for missing in [
        &[("MARKETRIG_TRIGGER_ID", "t-1")][..],
        &[("MARKETRIG_FIRING_ID", "f-1")][..],
        &[][..],
        // An empty value is not a value.
        &[("MARKETRIG_TRIGGER_ID", "t-1"), ("MARKETRIG_FIRING_ID", "")][..],
        &[("MARKETRIG_TRIGGER_ID", ""), ("MARKETRIG_FIRING_ID", "f-1")][..],
    ] {
        assert_eq!(attribution(env(missing)), None, "{missing:?}");
    }
}
