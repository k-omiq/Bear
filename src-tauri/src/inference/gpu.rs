//! Whether the selected cloud deployment has a GPU container up, and stopping it.
//!
//! Two gateway routes back this (`docs/cloud-api.md`): `GET /mc/v1/gpu` reads the
//! heartbeats GPU containers publish and never starts a GPU, and `POST /mc/v1/gpu/stop`
//! ends what is up. Both are strict on the way in, like every other `/mc/v1` answer:
//! known enums only, at most one container per role, finite non-negative times, a
//! short GPU name, and nothing from the body is ever echoed into an error.
//!
//! A deployment that predates the routes answers 404, and one that cannot report its
//! GPU (Beam today) answers `supported: false` or 501. Both reach the interface as
//! `supported: false`, a state it shows as nothing at all rather than as "no GPU".
//!
//! The commands act only on the selected cloud target, with the cloud permission on,
//! exactly as a render does. Stopping is the user's own action and sends no page, so
//! it needs no page consent.

use std::collections::BTreeMap;

use cleaner_core::engines::render::CloudProvider;
use serde::{Deserialize, Serialize};

use crate::inference::commands::{build_client_for_profile, read_inference_config, BuildClientError};
use crate::inference::http::{CloudHttpClient, HttpTransportError};

/// Largest time the wire may carry, in Unix seconds (year 5138). Anything past it is
/// a broken clock, not a container.
const MAX_WIRE_SECONDS: f64 = 1.0e11;
const MAX_IDLE_SECONDS: u32 = 86_400;
const MAX_PRICE_ENTRIES: usize = 8;
const MAX_CANCELLED_JOBS: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuRole {
    Render,
    Analysis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuState {
    Starting,
    Idle,
    Busy,
}

/// One container as `GET /mc/v1/gpu` reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuContainerWire {
    pub role: GpuRole,
    pub gpu: String,
    pub state: GpuState,
    pub started_at: f64,
    pub last_active_at: f64,
    pub idle_seconds: u32,
    pub scaledown_estimate_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuStatusWire {
    pub provider: CloudProvider,
    pub supported: bool,
    pub now: f64,
    pub containers: Vec<GpuContainerWire>,
    pub list_price_usd_per_hour: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuStopWire {
    pub provider: CloudProvider,
    pub stopped: Vec<GpuRole>,
    pub cancelled_jobs: u32,
}

/// The `POST /mc/v1/gpu/stop` body: `{"role":"render"}` for one role, `{}` for all,
/// and `"idle_only":true` added for a release that only takes an idle container.
/// A plain stop never sends the field, so a gateway that predates it still takes it.
pub fn stop_request_body(role: Option<GpuRole>, idle_only: bool) -> String {
    let mut body = serde_json::Map::new();
    if let Some(role) = role {
        body.insert("role".into(), serde_json::json!(role));
    }
    if idle_only {
        body.insert("idle_only".into(), serde_json::Value::Bool(true));
    }
    serde_json::Value::Object(body).to_string()
}

fn wire_time(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_WIRE_SECONDS).contains(&value)
}

/// A GPU name as the provider spells it: `L4`, `A10`, `L40S`, `RTX4090`.
fn gpu_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl GpuStatusWire {
    pub fn validate(&self, provider: CloudProvider) -> Result<(), HttpTransportError> {
        if self.provider != provider {
            return Err(HttpTransportError::ProviderMismatch);
        }
        let bad = || HttpTransportError::WireValidation;
        if !wire_time(self.now) || self.containers.len() > 2 {
            return Err(bad());
        }
        if !self.supported && !self.containers.is_empty() {
            return Err(bad());
        }
        let mut roles = Vec::with_capacity(2);
        for container in &self.containers {
            if roles.contains(&container.role) {
                return Err(bad());
            }
            roles.push(container.role);
            let estimate_ok = match (container.state, container.scaledown_estimate_at) {
                (GpuState::Idle, Some(at)) => wire_time(at),
                (GpuState::Idle, None) => false,
                (_, Some(_)) => false,
                (_, None) => true,
            };
            if !gpu_name(&container.gpu)
                || !wire_time(container.started_at)
                || !wire_time(container.last_active_at)
                || container.last_active_at < container.started_at
                || !(1..=MAX_IDLE_SECONDS).contains(&container.idle_seconds)
                || !estimate_ok
            {
                return Err(bad());
            }
        }
        if self.list_price_usd_per_hour.len() > MAX_PRICE_ENTRIES
            || self
                .list_price_usd_per_hour
                .iter()
                .any(|(name, price)| !gpu_name(name) || !price.is_finite() || !(0.0..=1000.0).contains(price))
        {
            return Err(bad());
        }
        Ok(())
    }
}

impl GpuStopWire {
    pub fn validate(&self, provider: CloudProvider) -> Result<(), HttpTransportError> {
        if self.provider != provider {
            return Err(HttpTransportError::ProviderMismatch);
        }
        let mut seen = self.stopped.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.stopped.len() || self.cancelled_jobs > MAX_CANCELLED_JOBS {
            return Err(HttpTransportError::WireValidation);
        }
        Ok(())
    }
}

/// Parse and validate a `GET /mc/v1/gpu` body.
pub fn parse_gpu_status(bytes: &[u8], provider: CloudProvider) -> Result<GpuStatusWire, HttpTransportError> {
    let status: GpuStatusWire =
        serde_json::from_slice(bytes).map_err(|_| HttpTransportError::JsonDeserialization)?;
    status.validate(provider)?;
    Ok(status)
}

/// Parse and validate a `POST /mc/v1/gpu/stop` body.
pub fn parse_gpu_stop(bytes: &[u8], provider: CloudProvider) -> Result<GpuStopWire, HttpTransportError> {
    let stop: GpuStopWire =
        serde_json::from_slice(bytes).map_err(|_| HttpTransportError::JsonDeserialization)?;
    stop.validate(provider)?;
    Ok(stop)
}

// ---------------------------------------------------------------------------
// What the interface receives
// ---------------------------------------------------------------------------

/// Why a deployment says nothing about its GPU. `Outdated` is one set up before the
/// routes existed: updating the cloud setup gives it them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuUnsupported {
    Outdated,
    Provider,
}

/// One container, with its times already made relative to the gateway's clock, so
/// the interface never compares the provider's clock with its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudGpuContainer {
    pub role: GpuRole,
    pub gpu: String,
    pub state: GpuState,
    pub idle_seconds: u32,
    pub up_for_ms: u64,
    /// Idle only: how long until the provider scales it down if nothing arrives.
    pub scaledown_in_ms: Option<u64>,
    pub list_price_usd_per_hour: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudGpuStatus {
    pub supported: bool,
    pub unsupported: Option<GpuUnsupported>,
    pub containers: Vec<CloudGpuContainer>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudGpuStop {
    pub supported: bool,
    pub stopped: Vec<GpuRole>,
    pub cancelled_jobs: u32,
}

fn millis(seconds: f64) -> u64 {
    // Validated finite, non-negative and bounded, so the cast cannot wrap.
    (seconds.max(0.0) * 1000.0).round() as u64
}

impl CloudGpuStatus {
    pub fn unsupported(reason: GpuUnsupported) -> Self {
        Self { supported: false, unsupported: Some(reason), containers: Vec::new() }
    }

    pub fn from_wire(wire: &GpuStatusWire) -> Self {
        if !wire.supported {
            return Self::unsupported(GpuUnsupported::Provider);
        }
        let containers = wire
            .containers
            .iter()
            .map(|c| CloudGpuContainer {
                role: c.role,
                gpu: c.gpu.clone(),
                state: c.state,
                idle_seconds: c.idle_seconds,
                up_for_ms: millis(wire.now - c.started_at),
                scaledown_in_ms: c.scaledown_estimate_at.map(|at| millis(at - wire.now)),
                list_price_usd_per_hour: wire.list_price_usd_per_hour.get(&c.gpu).copied(),
            })
            .collect();
        Self { supported: true, unsupported: None, containers }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// A stable code for the interface; never the transport's own text.
pub(crate) fn transport_code(error: &HttpTransportError) -> String {
    match error {
        HttpTransportError::UnexpectedStatus { status } if *status == 401 || *status == 403 => "gateway_unauthorized",
        HttpTransportError::UnexpectedStatus { .. } => "gateway_error",
        HttpTransportError::ConnectionError
        | HttpTransportError::Timeout
        | HttpTransportError::DnsResolutionFailed
        | HttpTransportError::DnsResolverBusy
        | HttpTransportError::NonPublicIpRejected
        | HttpTransportError::RedirectForbidden
        | HttpTransportError::JobCancelUnconfirmed
        | HttpTransportError::JobDeadline => "gateway_unreachable",
        HttpTransportError::JobFailed { .. } => "gateway_error",
        HttpTransportError::JobCancelled => "cancelled",
        HttpTransportError::InvalidCredential | HttpTransportError::CredentialProviderMismatch => "credential_missing",
        HttpTransportError::InvalidTarget
        | HttpTransportError::EndpointValidationFailed
        | HttpTransportError::InvalidProfileId => "endpoint_invalid",
        _ => "gateway_protocol",
    }
    .to_string()
}

/// A configured profile, selected or not: ongoing work can be inspected and
/// stopped without changing the default for new work.
fn configured_client(
    app: &tauri::AppHandle,
    provider: CloudProvider,
    profile_id: &str,
) -> Result<CloudHttpClient, String> {
    if !crate::inference::cloud_allowed(app) {
        return Err("cloud_disabled".into());
    }
    crate::inference::config::validate_profile_id(profile_id).map_err(|_| "target_invalid".to_string())?;
    let config = read_inference_config(app.clone()).map_err(|_| "config_unreadable".to_string())?;
    build_client_for_profile(&config, provider, profile_id).map_err(|error| match error {
        BuildClientError::Configuration(_) => "profile_missing".to_string(),
        BuildClientError::CredentialMissing(_) => "credential_missing".to_string(),
    })
}

/// A stop for one role must not report another role stopped.
fn stop_matches_role(wire: &GpuStopWire, role: Option<GpuRole>) -> bool {
    role.is_none_or(|role| wire.stopped.iter().all(|stopped| *stopped == role))
}

/// An Auto clean run's end-of-detection call: ask the gateway to release `role`'s
/// container now if it is idle, instead of billing its idle window while this
/// computer cleans. Answers the roles it released, usually none or `role`.
///
/// Not a command and not the user's stop: it cancels nothing, and the gateway leaves
/// a busy container alone. A run only logs a failure, so every `Err` is a stable code:
/// `gpu_release_unsupported` for a deployment without the route (older, Beam), and
/// `gateway_error` for one that predates `idle_only` and answers 400.
pub(crate) fn release_idle(client: &CloudHttpClient, role: GpuRole) -> Result<Vec<GpuRole>, String> {
    match client.release_idle_gpu(role).map_err(|e| transport_code(&e))? {
        Some(wire) if stop_matches_role(&wire, Some(role)) => Ok(wire.stopped),
        Some(_) => Err(transport_code(&HttpTransportError::WireValidation)),
        None => Err("gpu_release_unsupported".into()),
    }
}

/// Which GPU containers this configured deployment has up. Never starts one.
#[tauri::command]
pub async fn get_cloud_gpu_status(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
) -> Result<CloudGpuStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = configured_client(&app, provider, &profile_id)?;
        match client.get_gpu_status().map_err(|e| transport_code(&e))? {
            Some(wire) => Ok(CloudGpuStatus::from_wire(&wire)),
            None => Ok(CloudGpuStatus::unsupported(GpuUnsupported::Outdated)),
        }
    })
    .await
    .map_err(|_| "gpu_status_task_failed".to_string())?
}

/// Stop this configured deployment's GPU containers: `role`'s only, or all when it is
/// absent. Work in flight on a stopped role ends as cancelled. An unknown role never
/// gets this far: it fails to deserialize.
#[tauri::command]
pub async fn stop_cloud_gpu(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
    role: Option<GpuRole>,
) -> Result<CloudGpuStop, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = configured_client(&app, provider, &profile_id)?;
        match client.stop_gpu(role).map_err(|e| transport_code(&e))? {
            Some(wire) => {
                if !stop_matches_role(&wire, role) {
                    return Err(transport_code(&HttpTransportError::WireValidation));
                }
                Ok(CloudGpuStop { supported: true, stopped: wire.stopped, cancelled_jobs: wire.cancelled_jobs })
            }
            None => Ok(CloudGpuStop { supported: false, stopped: Vec::new(), cancelled_jobs: 0 }),
        }
    })
    .await
    .map_err(|_| "gpu_stop_task_failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{"provider":"modal","supported":true,"now":1000.0,
        "containers":[{"role":"render","gpu":"L4","state":"idle","started_at":700.0,
            "last_active_at":950.0,"idle_seconds":120,"scaledown_estimate_at":1070.0},
          {"role":"analysis","gpu":"L4","state":"busy","started_at":990.0,
            "last_active_at":995.0,"idle_seconds":120,"scaledown_estimate_at":null}],
        "list_price_usd_per_hour":{"L4":0.8}}"#;

    fn with(patch: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut value: serde_json::Value = serde_json::from_str(STATUS).unwrap();
        patch(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn gpu_status_parses_and_turns_times_relative() {
        let wire = parse_gpu_status(STATUS.as_bytes(), CloudProvider::Modal).unwrap();
        let status = CloudGpuStatus::from_wire(&wire);
        assert!(status.supported);
        assert_eq!(status.containers.len(), 2);
        let render = &status.containers[0];
        assert_eq!((render.role, render.state, render.gpu.as_str()), (GpuRole::Render, GpuState::Idle, "L4"));
        assert_eq!(render.up_for_ms, 300_000);
        assert_eq!(render.scaledown_in_ms, Some(70_000));
        assert_eq!(render.list_price_usd_per_hour, Some(0.8));
        assert_eq!(status.containers[1].scaledown_in_ms, None);
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["containers"][0]["scaledownInMs"], 70_000);
        assert_eq!(json["containers"][1]["state"], "busy");
    }

    #[test]
    fn gpu_status_rejects_anything_off_contract() {
        let cases: Vec<Vec<u8>> = vec![
            with(|v| v["extra"] = 1.into()),
            with(|v| v["containers"][0]["note"] = "hi".into()),
            with(|v| v["containers"][0]["state"] = "warm".into()),
            with(|v| v["containers"][0]["role"] = "seed".into()),
            with(|v| v["containers"][1]["role"] = "render".into()),
            with(|v| v["containers"][0]["gpu"] = "L4; drop".into()),
            with(|v| v["containers"][0]["gpu"] = "".into()),
            with(|v| v["containers"][0]["started_at"] = (-1.0).into()),
            with(|v| v["containers"][0]["started_at"] = 2.0e11.into()),
            with(|v| v["containers"][0]["last_active_at"] = 600.0.into()),
            with(|v| v["containers"][0]["idle_seconds"] = 0.into()),
            with(|v| v["containers"][0]["scaledown_estimate_at"] = serde_json::Value::Null),
            with(|v| v["containers"][1]["scaledown_estimate_at"] = 5.0.into()),
            with(|v| v["supported"] = false.into()),
            with(|v| v["now"] = serde_json::Value::Null),
            with(|v| v["list_price_usd_per_hour"]["L4"] = (-0.5).into()),
            with(|v| v["list_price_usd_per_hour"]["bad name"] = 1.0.into()),
            with(|v| {
                let first = v["containers"][0].clone();
                v["containers"].as_array_mut().unwrap().push(first);
            }),
            b"not json".to_vec(),
        ];
        for body in cases {
            assert!(parse_gpu_status(&body, CloudProvider::Modal).is_err(), "{}", String::from_utf8_lossy(&body));
        }
        assert_eq!(
            parse_gpu_status(STATUS.as_bytes(), CloudProvider::Beam).unwrap_err(),
            HttpTransportError::ProviderMismatch,
        );
    }

    #[test]
    fn an_unsupported_deployment_reports_nothing() {
        let body = br#"{"provider":"beam","supported":false,"now":5.0,"containers":[],"list_price_usd_per_hour":{}}"#;
        let status = CloudGpuStatus::from_wire(&parse_gpu_status(body, CloudProvider::Beam).unwrap());
        assert_eq!(status, CloudGpuStatus::unsupported(GpuUnsupported::Provider));
        let json = serde_json::to_value(CloudGpuStatus::unsupported(GpuUnsupported::Outdated)).unwrap();
        assert_eq!(json, serde_json::json!({"supported": false, "unsupported": "outdated", "containers": []}));
    }

    #[test]
    fn gpu_stop_parses_strictly() {
        let ok = parse_gpu_stop(br#"{"provider":"modal","stopped":["render"],"cancelled_jobs":2}"#, CloudProvider::Modal).unwrap();
        assert_eq!((ok.stopped, ok.cancelled_jobs), (vec![GpuRole::Render], 2));
        assert!(parse_gpu_stop(br#"{"provider":"modal","stopped":[],"cancelled_jobs":0}"#, CloudProvider::Modal).is_ok());
        for body in [
            &br#"{"provider":"modal","stopped":["render","render"],"cancelled_jobs":0}"#[..],
            br#"{"provider":"modal","stopped":["gpu"],"cancelled_jobs":0}"#,
            br#"{"provider":"modal","stopped":[],"cancelled_jobs":-1}"#,
            br#"{"provider":"modal","stopped":[],"cancelled_jobs":10001}"#,
            br#"{"provider":"modal","stopped":[],"cancelled_jobs":0,"message":"x"}"#,
        ] {
            assert!(parse_gpu_stop(body, CloudProvider::Modal).is_err());
        }
        assert_eq!(
            parse_gpu_stop(br#"{"provider":"beam","stopped":[],"cancelled_jobs":0}"#, CloudProvider::Modal).unwrap_err(),
            HttpTransportError::ProviderMismatch,
        );
    }

    /// One loopback answer; returns the whole request the client sent.
    fn serve_once(status: &str, body: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        let status = status.to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                // Headers and body may arrive in separate reads; stop once Content-Length is met.
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let complete = text.split_once("\r\n\r\n").is_some_and(|(head, rest)| {
                        let length = head
                            .lines()
                            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        rest.len() >= length
                    });
                    if n == 0 || complete {
                        break;
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&request).to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, rx)
    }

    fn loopback_client(port: u16) -> CloudHttpClient {
        use crate::inference::http::{BoundRuntimeCredential, CloudEndpointTarget, RuntimeCredential};
        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();
        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::ModalProxy {
                token_id: "wk-test".to_string(),
                token_secret: crate::inference::secrets::SecretValue::new("ws-test"),
            },
        )
        .unwrap();
        CloudHttpClient::new_test_client(target, cred, reqwest::blocking::Client::new())
    }

    #[test]
    fn gpu_routes_over_loopback() {
        let (port, rx) = serve_once("200 OK", r#"{"provider":"modal","supported":true,"now":10.0,"containers":[],"list_price_usd_per_hour":{"L4":0.8}}"#);
        let status = loopback_client(port).get_gpu_status().unwrap().unwrap();
        assert!(status.containers.is_empty());
        assert!(rx.recv().unwrap().starts_with("GET /mc/v1/gpu HTTP/1.1\r\n"));

        let (port, rx) = serve_once("200 OK", r#"{"provider":"modal","stopped":["analysis"],"cancelled_jobs":0}"#);
        let stop = loopback_client(port).stop_gpu(Some(GpuRole::Analysis)).unwrap().unwrap();
        assert_eq!(stop.stopped, vec![GpuRole::Analysis]);
        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /mc/v1/gpu/stop HTTP/1.1\r\n"));
        assert!(request.ends_with("\r\n\r\n{\"role\":\"analysis\"}"), "{request}");

        let (port, rx) = serve_once("200 OK", r#"{"provider":"modal","stopped":[],"cancelled_jobs":0}"#);
        loopback_client(port).stop_gpu(None).unwrap().unwrap();
        assert!(rx.recv().unwrap().ends_with("\r\n\r\n{}"));

        let (port, rx) = serve_once("200 OK", r#"{"provider":"modal","stopped":["analysis"],"cancelled_jobs":0}"#);
        assert_eq!(release_idle(&loopback_client(port), GpuRole::Analysis).unwrap(), vec![GpuRole::Analysis]);
        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /mc/v1/gpu/stop HTTP/1.1\r\n"));
        assert!(request.ends_with("\r\n\r\n{\"idle_only\":true,\"role\":\"analysis\"}"), "{request}");
    }

    /// Whatever the deployment answers, the run gets a code to log and nothing else.
    #[test]
    fn an_idle_release_fails_to_a_code_on_any_deployment_that_cannot() {
        let (port, _rx) = serve_once("400 Bad Request", r#"{"error_code":"invalid_request","message":"no"}"#);
        assert_eq!(release_idle(&loopback_client(port), GpuRole::Analysis).unwrap_err(), "gateway_error");
        let (port, _rx) = serve_once("501 Not Implemented", r#"{"error_code":"unsupported","message":"no"}"#);
        assert_eq!(release_idle(&loopback_client(port), GpuRole::Analysis).unwrap_err(), "gpu_release_unsupported");
        let (port, _rx) = serve_once("200 OK", r#"{"provider":"modal","stopped":["render"],"cancelled_jobs":0}"#);
        assert_eq!(release_idle(&loopback_client(port), GpuRole::Analysis).unwrap_err(), "gateway_protocol");
    }

    #[test]
    fn stop_role_is_strict_both_ways() {
        assert_eq!(stop_request_body(Some(GpuRole::Render), false), r#"{"role":"render"}"#);
        assert_eq!(stop_request_body(None, false), "{}");
        assert_eq!(stop_request_body(Some(GpuRole::Analysis), true), r#"{"idle_only":true,"role":"analysis"}"#);
        for bad in [r#""seed""#, r#""Render""#, r#""""#, "1"] {
            assert!(serde_json::from_str::<Option<GpuRole>>(bad).is_err(), "{bad}");
        }
        assert_eq!(serde_json::from_str::<Option<GpuRole>>("null").unwrap(), None);
        let wire = |stopped| GpuStopWire { provider: CloudProvider::Modal, stopped, cancelled_jobs: 0 };
        assert!(stop_matches_role(&wire(vec![GpuRole::Render, GpuRole::Analysis]), None));
        assert!(stop_matches_role(&wire(vec![GpuRole::Render]), Some(GpuRole::Render)));
        assert!(stop_matches_role(&wire(vec![]), Some(GpuRole::Analysis)));
        assert!(!stop_matches_role(&wire(vec![GpuRole::Render]), Some(GpuRole::Analysis)));
    }

    #[test]
    fn an_older_deployment_is_unsupported_not_an_error() {
        let (port, _rx) = serve_once("404 Not Found", r#"{"error_code":"not_found","message":"Route not found"}"#);
        assert_eq!(loopback_client(port).get_gpu_status().unwrap(), None);
        let (port, _rx) = serve_once("501 Not Implemented", r#"{"error_code":"unsupported","message":"no"}"#);
        assert_eq!(loopback_client(port).stop_gpu(None).unwrap(), None);
        let (port, _rx) = serve_once("500 Internal Server Error", r#"{"error_code":"internal_error","message":"secret detail"}"#);
        assert_eq!(
            loopback_client(port).get_gpu_status().unwrap_err(),
            HttpTransportError::UnexpectedStatus { status: 500 },
        );
    }

    #[test]
    fn transport_errors_become_stable_codes() {
        assert_eq!(transport_code(&HttpTransportError::UnexpectedStatus { status: 401 }), "gateway_unauthorized");
        assert_eq!(transport_code(&HttpTransportError::UnexpectedStatus { status: 500 }), "gateway_error");
        assert_eq!(transport_code(&HttpTransportError::Timeout), "gateway_unreachable");
        assert_eq!(transport_code(&HttpTransportError::WireValidation), "gateway_protocol");
    }
}
