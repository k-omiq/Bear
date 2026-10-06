//! Submit-then-poll GPU jobs for analysis tiles, analysis batches (one page's
//! tiles in one call, [`crate::cloud_analysis_wire::AnalysisBatch`]) and denoise
//! pages, the wire
//! mirror of `deploy/cloud/common/contract.py` (`GPU_JOB_OPERATIONS`) and the
//! `/jobs` routes of `deploy/cloud/common/api.py`.
//!
//! Modal answers a web request still running after 150 s with a 303 redirect,
//! and the desktop follows no redirect, so a tile or page that queues or runs
//! that long cannot be answered on the request that sent it. A job splits it:
//!
//! - `POST /mc/{analysis,denoise}/v1/jobs` takes the synchronous route's body
//!   plus an `attempt_id` and answers 202 with a [`JobEnvelope`] at once.
//! - `GET .../jobs/{handle}` answers the job's [`JobStatus`] and, once
//!   completed, the synchronous route's own answer as `result`.
//! - `POST .../jobs/{handle}/cancel` cancels the GPU call.
//!
//! The handle is [`job_handle`] of the kind, the request digest and the
//! attempt id, derived the same way on both sides. A submit repeated after a
//! lost answer names the job it already started, and the gateway spawns
//! nothing for it, so the page is never paid for twice.
//!
//! Negotiation is additive: a gateway that serves these routes lists
//! [`JobKind::operation`] in `GET /mc/v1/capabilities`. The protocol versions
//! of analysis and denoise do not move; an app that finds the operation
//! missing (an older gateway, Beam) uses the synchronous routes.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Shortest and longest client attempt id, of `[A-Za-z0-9_-]`.
pub const ATTEMPT_MIN: usize = 16;
pub const ATTEMPT_MAX: usize = 64;
/// Longest error code and message a job answer carries.
pub const MAX_ERROR_CODE: usize = 64;
pub const MAX_ERROR_MESSAGE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Analysis,
    AnalysisBatch,
    Denoise,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Analysis => "analysis",
            Self::AnalysisBatch => "analysis_batch",
            Self::Denoise => "denoise",
        }
    }

    /// The submit route; a job's own routes are `{route}/{handle}` and
    /// `{route}/{handle}/cancel`.
    pub fn route(self) -> &'static str {
        match self {
            Self::Analysis => "/mc/analysis/v1/jobs",
            Self::AnalysisBatch => "/mc/analysis/v1/batches",
            Self::Denoise => "/mc/denoise/v1/jobs",
        }
    }

    /// The operation `GET /mc/v1/capabilities` lists when the routes are served.
    pub fn operation(self) -> &'static str {
        match self {
            Self::Analysis => "analysis.jobs",
            Self::AnalysisBatch => "analysis.batches",
            Self::Denoise => "denoise.jobs",
        }
    }

    /// The protocol version every answer of this kind carries: the kind's own,
    /// unchanged by the job routes.
    pub fn protocol_version(self) -> &'static str {
        match self {
            Self::Analysis | Self::AnalysisBatch => crate::cloud_analysis_wire::VERSION,
            Self::Denoise => crate::cloud_denoise_wire::VERSION,
        }
    }
}

pub fn valid_attempt_id(value: &str) -> bool {
    (ATTEMPT_MIN..=ATTEMPT_MAX).contains(&value.len())
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// `<kind>-<32 hex>`: SHA-256 over the kind, NUL, the request digest, NUL and
/// the attempt id, as `gpu_job_handle` in the gateway.
pub fn job_handle(kind: JobKind, request_digest: &str, attempt_id: &str) -> String {
    let digest = Sha256::digest(format!("{}\0{request_digest}\0{attempt_id}", kind.as_str()).as_bytes());
    let hex: String = digest[..16].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{hex}", kind.as_str())
}

pub fn valid_handle(kind: JobKind, handle: &str) -> bool {
    handle.strip_prefix(kind.as_str()).and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Claimed; the GPU call is not spawned yet.
    Pending,
    /// Handed to the GPU queue or running on it.
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobFailure {
    pub error_code: String,
    pub message: String,
}

/// The one answer of every job route. `result` is the synchronous route's own
/// answer ([`crate::cloud_analysis_wire::AnalysisResult`] or
/// [`crate::cloud_denoise_wire::DenoiseResult`]), present only on a completed
/// status; the caller validates it as it validates a synchronous answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobEnvelope<R> {
    pub protocol_version: String,
    pub request_digest: String,
    pub handle: String,
    pub status: JobStatus,
    pub error: Option<JobFailure>,
    pub result: Option<R>,
}

impl<R> JobEnvelope<R> {
    /// Bound to the job asked about (kind version, digest echo, handle) and
    /// coherent: a result only when completed, a typed error when failed and
    /// never otherwise.
    pub fn validate(&self, kind: JobKind, request_digest: &str, handle: &str) -> Result<(), String> {
        if self.protocol_version != kind.protocol_version() || self.request_digest != request_digest
            || self.handle != handle
        {
            return Err("job answer identity mismatch".into());
        }
        match (&self.status, &self.error, &self.result) {
            (JobStatus::Completed, None, _) => {}
            (JobStatus::Failed, Some(error), None) => {
                if error.error_code.is_empty() || error.error_code.len() > MAX_ERROR_CODE
                    || !error.error_code.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
                    || error.message.len() > MAX_ERROR_MESSAGE
                {
                    return Err("job answer error is not a typed code".into());
                }
            }
            (JobStatus::Pending | JobStatus::Running | JobStatus::Cancelled, None, None) => {}
            _ => return Err("job answer status, error and result disagree".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTEMPT: &str = "attempt-0123456789ab";

    #[test]
    fn handle_matches_the_gateway() {
        // The same vector is in deploy/cloud/tests/test_gpu_jobs.py.
        assert_eq!(job_handle(JobKind::Denoise, &"a".repeat(64), ATTEMPT), "denoise-487b9e31a97b580d5a4a192d3b1f9e29");
        assert_eq!(job_handle(JobKind::AnalysisBatch, &"a".repeat(64), ATTEMPT),
            "analysis_batch-440222160b1226445bb30b6bf856c227");
        assert!(!valid_handle(JobKind::Analysis, &job_handle(JobKind::AnalysisBatch, "d", ATTEMPT)));
        assert_eq!(job_handle(JobKind::Analysis, &"a".repeat(64), ATTEMPT), "analysis-7dde99faa10d0190905d393880f90199");
        assert!(valid_handle(JobKind::Denoise, &job_handle(JobKind::Denoise, "d", ATTEMPT)));
        assert!(!valid_handle(JobKind::Analysis, &job_handle(JobKind::Denoise, "d", ATTEMPT)));
        assert!(!valid_handle(JobKind::Denoise, "denoise-../../gpu/stop"));
        assert!(!valid_handle(JobKind::Denoise, &format!("denoise-{}", "A".repeat(32))));
    }

    #[test]
    fn attempt_ids_are_bounded_and_plain() {
        assert!(valid_attempt_id(ATTEMPT));
        assert!(valid_attempt_id(&"f".repeat(32)));
        for bad in ["short", &"x".repeat(65), "has space in it ok", "slash/in/the/attempt"] {
            assert!(!valid_attempt_id(bad), "{bad}");
        }
    }

    fn envelope(status: &str, error: &str, result: &str) -> Result<JobEnvelope<serde_json::Value>, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"protocol_version":"1.0.0","request_digest":"{}","handle":"denoise-{}","status":"{status}","error":{error},"result":{result}}}"#,
            "a".repeat(64), "0".repeat(32)))
    }

    #[test]
    fn answers_are_bound_and_coherent() {
        let (digest, handle) = ("a".repeat(64), format!("denoise-{}", "0".repeat(32)));
        let ok = |status, error, result| envelope(status, error, result).unwrap().validate(JobKind::Denoise, &digest, &handle);
        ok("running", "null", "null").unwrap();
        ok("pending", "null", "null").unwrap();
        ok("cancelled", "null", "null").unwrap();
        ok("completed", "null", r#"{"page_png_b64":""}"#).unwrap();
        ok("failed", r#"{"error_code":"worker_timeout","message":"slow"}"#, "null").unwrap();
        assert!(ok("running", "null", r#"{"x":1}"#).is_err(), "a result before completion");
        assert!(ok("failed", "null", "null").is_err(), "a failure without a code");
        assert!(ok("failed", r#"{"error_code":"Bad Code","message":"m"}"#, "null").is_err());
        assert!(ok("completed", r#"{"error_code":"x","message":"m"}"#, "null").is_err());
        let answer = envelope("running", "null", "null").unwrap();
        assert!(answer.validate(JobKind::Analysis, &digest, &handle).is_err(), "another kind's version");
        assert!(answer.validate(JobKind::Denoise, &"b".repeat(64), &handle).is_err(), "another request");
        assert!(answer.validate(JobKind::Denoise, &digest, &format!("denoise-{}", "1".repeat(32))).is_err());
        assert!(envelope("queued", "null", "null").is_err(), "an unknown status");
        assert!(serde_json::from_str::<JobEnvelope<serde_json::Value>>(
            r#"{"protocol_version":"1.0.0","request_digest":"a","handle":"h","status":"running","error":null,"result":null,"extra":1}"#
        ).is_err(), "an unknown field");
    }
}
