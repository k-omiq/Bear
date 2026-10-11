//! Metadata-only polling and durable, bounded denoise image transfers.
use super::*;
use cleaner_core::cloud_denoise_wire::{
    DenoiseResultMetadata, RESULT_CHUNK_BYTES, RESULT_OPERATION,
};
use cleaner_core::ingest::sha256_hex;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DenoiseJob {
    pub metadata: DenoiseMetadata,
    pub attempt_id: String,
    pub handle: String,
    pub profile_id: String,
    pub origin_fingerprint: String,
}

impl CloudHttpClient {
    pub(crate) fn serves_denoise_results(&self) -> Result<bool, HttpTransportError> {
        Ok(self.serves_gpu_jobs(JobKind::Denoise)?
            && self
                .operations
                .get()
                .is_some_and(|ops| ops.iter().any(|op| op == RESULT_OPERATION)))
    }

    pub(crate) fn new_denoise_job(
        &self,
        metadata: DenoiseMetadata,
    ) -> Result<DenoiseJob, HttpTransportError> {
        let attempt_id = attempt_id()?;
        Ok(DenoiseJob {
            handle: job_wire::job_handle(JobKind::Denoise, &metadata.request_digest, &attempt_id),
            metadata,
            attempt_id,
            profile_id: self.target.profile_id.clone(),
            origin_fingerprint: self.target.canonical_origin_fingerprint.clone(),
        })
    }

    fn validate_denoise_job(&self, job: &DenoiseJob) -> Result<(), HttpTransportError> {
        if job.profile_id != self.target.profile_id
            || job.origin_fingerprint != self.target.canonical_origin_fingerprint
            || !job_wire::valid_attempt_id(&job.attempt_id)
            || job.handle
                != job_wire::job_handle(
                    JobKind::Denoise,
                    &job.metadata.request_digest,
                    &job.attempt_id,
                )
        {
            return Err(HttpTransportError::WireValidation);
        }
        Ok(())
    }

    /// The caller persists this exact ticket BEFORE submit, so a lost answer or
    /// restart repeats the same idempotent attempt, never another GPU inference.
    pub(crate) fn submit_denoise_job(
        &self,
        job: &DenoiseJob,
        png: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), HttpTransportError> {
        self.validate_denoise_job(job)?;
        job.metadata
            .validate(png)
            .map_err(|_| HttpTransportError::WireValidation)?;
        let body = serde_json::to_vec(&serde_json::json!({"metadata":job.metadata,
            "page_png_b64":base64::engine::general_purpose::STANDARD.encode(png), "attempt_id":job.attempt_id}))
            .map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > DENOISE_MAX_BODY_BYTES {
            return Err(HttpTransportError::BodySizeLimitExceeded);
        }
        let check = |_: &DenoiseResultMetadata| Ok(());
        let transport = HttpGpuJob {
            client: self,
            kind: JobKind::Denoise,
            body,
            request_digest: &job.metadata.request_digest,
            handle: job.handle.clone(),
            max_answer_bytes: 16384,
            check: &check,
            reject: denoise_rejection_error,
        };
        for attempt in 0..gpu_jobs::DENOISE_SCHEDULE.submit_attempts {
            if cancel.load(Ordering::SeqCst) {
                if attempt > 0 && self.cancel_denoise_job(job).is_err() {
                    return Err(HttpTransportError::JobCancelUnconfirmed);
                }
                return Err(HttpTransportError::JobCancelled);
            }
            match transport.submit(gpu_jobs::SUBMIT_TIMEOUT) {
                Ok(()) => return Ok(()),
                Err(HttpTransportError::ConnectionError | HttpTransportError::Timeout)
                    if attempt + 1 < gpu_jobs::DENOISE_SCHEDULE.submit_attempts => {}
                Err(
                    error @ (HttpTransportError::ConnectionError | HttpTransportError::Timeout),
                ) => {
                    let _ = self.cancel_denoise_job(job);
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(HttpTransportError::ConnectionError)
    }

    fn denoise_job_route(&self, job: &DenoiseJob, suffix: &str) -> Result<Url, HttpTransportError> {
        self.validate_denoise_job(job)?;
        let mut url = self.job_url(JobKind::Denoise, Some(&job.handle), false);
        url.set_path(&format!("{}/{suffix}", url.path()));
        Ok(url)
    }

    pub(crate) fn denoise_job_status(
        &self,
        job: &DenoiseJob,
    ) -> Result<Polled<DenoiseResultMetadata>, HttpTransportError> {
        let (status, bytes) = self.job_request(
            self.client.get(self.denoise_job_route(job, "metadata")?),
            Duration::from_secs(30),
            16384,
        )?;
        if status != StatusCode::OK {
            return Err(HttpTransportError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }
        let envelope: JobEnvelope<DenoiseResultMetadata> =
            serde_json::from_slice(&bytes).map_err(|_| HttpTransportError::JsonDeserialization)?;
        envelope
            .validate(JobKind::Denoise, &job.metadata.request_digest, &job.handle)
            .map_err(|_| HttpTransportError::WireValidation)?;
        Ok(match envelope.status {
            JobStatus::Completed => {
                let meta = envelope.result.ok_or(HttpTransportError::WireValidation)?;
                meta.validate(&job.metadata)
                    .map_err(|_| HttpTransportError::WireValidation)?;
                Polled::Completed(meta)
            }
            JobStatus::Pending | JobStatus::Running => Polled::Waiting,
            JobStatus::Failed => Polled::Failed {
                code: envelope.error.unwrap().error_code,
            },
            JobStatus::Cancelled => Polled::Cancelled,
        })
    }

    pub(crate) fn cancel_denoise_job(&self, job: &DenoiseJob) -> Result<(), HttpTransportError> {
        self.validate_denoise_job(job)?;
        let (status, bytes) = self.job_request(
            self.client
                .post(self.job_url(JobKind::Denoise, Some(&job.handle), true)),
            gpu_jobs::CANCEL_TIMEOUT,
            16384,
        )?;
        if status != StatusCode::OK {
            return Err(HttpTransportError::JobCancelUnconfirmed);
        }
        let view: JobEnvelope<serde_json::Value> =
            serde_json::from_slice(&bytes).map_err(|_| HttpTransportError::WireValidation)?;
        view.validate(JobKind::Denoise, &job.metadata.request_digest, &job.handle)
            .map_err(|_| HttpTransportError::WireValidation)?;
        if !view.status.is_terminal() {
            return Err(HttpTransportError::JobCancelUnconfirmed);
        }
        Ok(())
    }

    pub(crate) fn download_denoise_job(
        &self,
        job: &DenoiseJob,
        meta: &DenoiseResultMetadata,
        checkpoint: &Path,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, HttpTransportError> {
        self.validate_denoise_job(job)?;
        meta.validate(&job.metadata)
            .map_err(|_| HttpTransportError::WireValidation)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(checkpoint)
            .map_err(|_| HttpTransportError::DenoiseStorage)?;
        let mut have = file
            .metadata()
            .map_err(|_| HttpTransportError::DenoiseStorage)?
            .len() as usize;
        if have > meta.png_bytes {
            return Err(HttpTransportError::BodySizeLimitExceeded);
        }
        let mut stalled = 0;
        let started = Instant::now();
        while have < meta.png_bytes {
            if cancel.load(Ordering::SeqCst) {
                return Err(HttpTransportError::JobCancelled);
            }
            let before = have;
            let index = have / RESULT_CHUNK_BYTES;
            let offset = have % RESULT_CHUNK_BYTES;
            let chunk_size = (meta.png_bytes - index * RESULT_CHUNK_BYTES).min(RESULT_CHUNK_BYTES);
            let mut req = self
                .client
                .get(self.denoise_job_route(job, &format!("result/{index}"))?)
                .timeout(Duration::from_secs(30))
                .header(ACCEPT, "application/octet-stream");
            if offset != 0 {
                req = req.header(RANGE, format!("bytes={offset}-"));
            }
            let transfer = (|| {
                let mut response = self
                    .attach_auth(req)?
                    .send()
                    .map_err(|_| HttpTransportError::ConnectionError)?;
                if response.status().is_redirection() || response.headers().contains_key(LOCATION) {
                    return Err(HttpTransportError::RedirectForbidden);
                }
                let expected_status = if offset == 0 {
                    StatusCode::OK
                } else {
                    StatusCode::PARTIAL_CONTENT
                };
                if response.status() != expected_status {
                    return Err(HttpTransportError::UnexpectedStatus {
                        status: response.status().as_u16(),
                    });
                }
                Self::verify_content_type(&response, "application/octet-stream")?;
                if offset != 0
                    && response
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        != Some(format!("bytes {offset}-{}/{chunk_size}", chunk_size - 1).as_str())
                {
                    return Err(HttpTransportError::WireValidation);
                }
                if response
                    .content_length()
                    .is_some_and(|n| n != (chunk_size - offset) as u64)
                {
                    return Err(HttpTransportError::WireValidation);
                }
                let mut got = offset;
                let mut buffer = [0u8; 4096];
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        return Err(HttpTransportError::JobCancelled);
                    }
                    let count = response
                        .read(&mut buffer)
                        .map_err(|_| HttpTransportError::ConnectionError)?;
                    if count == 0 {
                        break;
                    }
                    if got + count > chunk_size {
                        return Err(HttpTransportError::BodySizeLimitExceeded);
                    }
                    file.write_all(&buffer[..count])
                        .map_err(|_| HttpTransportError::DenoiseStorage)?;
                    have += count;
                    got += count;
                }
                if got != chunk_size {
                    return Err(HttpTransportError::ConnectionError);
                }
                file.sync_data()
                    .map_err(|_| HttpTransportError::DenoiseStorage)?;
                Ok(())
            })();
            match transfer {
                Ok(()) => stalled = 0,
                Err(
                    error @ (HttpTransportError::ConnectionError
                    | HttpTransportError::Timeout
                    | HttpTransportError::UnexpectedStatus {
                        status: 404 | 500..=504,
                    }),
                ) => {
                    stalled = if have > before { 0 } else { stalled + 1 };
                    if stalled >= RESULT_STALLED_TRIES
                        || started.elapsed() > Duration::from_secs(30 * 60)
                    {
                        return Err(error);
                    }
                    thread::sleep(RESULT_RETRY_PAUSE);
                }
                Err(error) => return Err(error),
            }
        }
        drop(file);
        let png = fs::read(checkpoint).map_err(|_| HttpTransportError::DenoiseStorage)?;
        if sha256_hex(&png) != meta.png_sha256 {
            // Keep the accepted handle; discard a corrupt local checkpoint so
            // retrieval can be retried without another GPU submission.
            fs::write(checkpoint, []).map_err(|_| HttpTransportError::DenoiseStorage)?;
            return Err(HttpTransportError::WireValidation);
        }
        // The metadata names the answer's own size; the page's is that over
        // the recipe's output scale.
        let (width, height) = meta.page_size(&job.metadata).ok_or(HttpTransportError::WireValidation)?;
        DenoiseResult {
            protocol_version: meta.protocol_version.clone(),
            request_digest: meta.request_digest.clone(),
            page_png_b64: String::new(),
            info: meta.info.clone(),
        }
        .validate(&job.metadata, width, height, &png)
        .map_err(|_| HttpTransportError::WireValidation)?;
        Ok(png)
    }

    pub(crate) fn release_denoise_result(&self, job: &DenoiseJob) {
        if let Ok(url) = self.denoise_job_route(job, "result/release") {
            let _ = self.job_request(self.client.post(url), Duration::from_secs(20), 16384);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::cloud_denoise_wire::{preset, DenoiseInfo, DenoiseStepTiming};
    use std::net::TcpListener;

    fn noisy_png() -> Vec<u8> {
        let mut out = Vec::new();
        let mut data = Vec::new();
        let mut seed = 42u32;
        for _ in 0..256 * 256 * 3 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            data.push((seed >> 24) as u8);
        }
        let mut encoder = png::Encoder::new(&mut out, 256, 256);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&data)
            .unwrap();
        out
    }
    fn client(base: &str) -> CloudHttpClient {
        let target =
            CloudEndpointTarget::new_test_target(CloudProvider::Modal, "denoise-test", base)
                .unwrap();
        let credential = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();
        CloudHttpClient::new_test_client(
            target,
            credential,
            Client::builder().redirect(Policy::none()).build().unwrap(),
        )
    }
    fn result(job: &DenoiseJob, png: &[u8]) -> DenoiseResultMetadata {
        DenoiseResultMetadata {
            protocol_version: job.metadata.protocol_version.clone(),
            request_digest: job.metadata.request_digest.clone(),
            png_sha256: sha256_hex(png),
            png_bytes: png.len(),
            info: DenoiseInfo {
                width: 256,
                height: 256,
                gray: false,
                steps: job
                    .metadata
                    .recipe
                    .resolve()
                    .unwrap()
                    .iter()
                    .map(|step| DenoiseStepTiming {
                        model_id: step.model_id.clone(),
                        ms: 1,
                    })
                    .collect(),
            },
        }
    }
    fn chunk_server(
        png: Vec<u8>,
        cut: Option<usize>,
        existing: usize,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let thread = thread::spawn(move || {
            let mut offset = existing;
            let mut first = true;
            let mut headers = Vec::new();
            while offset < png.len() {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "client did not request the remaining chunk"
                            );
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                // macOS can inherit the listener's nonblocking mode. Request
                // reads must block until the client actually sends its header.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.push(byte[0]);
                }
                let head = String::from_utf8(head).unwrap().to_lowercase();
                let index = offset / RESULT_CHUNK_BYTES;
                let in_chunk = offset % RESULT_CHUNK_BYTES;
                let end = ((index + 1) * RESULT_CHUNK_BYTES).min(png.len());
                assert!(head.starts_with("get /mc/denoise/v1/jobs/denoise-"));
                assert!(head.contains(&format!("/result/{index} http/1.1")));
                if in_chunk != 0 {
                    assert!(head.contains(&format!("range: bytes={in_chunk}-")));
                } else {
                    assert!(!head.contains("range:"));
                }
                assert!(head.contains("modal-key: tid"));
                assert!(head.contains("modal-secret: tsec"));
                headers.push(head);
                let chunk_size = end - index * RESULT_CHUNK_BYTES;
                let range = if in_chunk > 0 {
                    format!(
                        "Content-Range: bytes {in_chunk}-{}/{chunk_size}\r\n",
                        chunk_size - 1
                    )
                } else {
                    String::new()
                };
                write!(stream,"HTTP/1.1 {} OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n{range}Connection: close\r\n\r\n",
                    if in_chunk>0 {206}else {200},end-offset).unwrap();
                let send_end = if first {
                    cut.map(|n| offset + n).unwrap_or(end)
                } else {
                    end
                };
                first = false;
                stream.write_all(&png[offset..send_end]).unwrap();
                offset = send_end;
            }
            headers
        });
        (format!("http://{address}/mc/v1"), thread)
    }
    fn transfer(cut: Option<usize>, existing: usize) {
        let png = noisy_png();
        assert!(png.len() > 2 * RESULT_CHUNK_BYTES);
        let (base, server) = chunk_server(png.clone(), cut, existing);
        let http = client(&base);
        let metadata =
            DenoiseMetadata::new(preset("realcugan-2x-conservative").unwrap().recipe(), &png)
                .unwrap();
        let job = http.new_denoise_job(metadata).unwrap();
        // Persist/reload the accepted ticket, then use a newly constructed client.
        let job: DenoiseJob = serde_json::from_slice(&serde_json::to_vec(&job).unwrap()).unwrap();
        let http = client(&base);
        let checkpoint = std::env::temp_dir().join(format!(
            "mc-chunk-{}-{}.part",
            std::process::id(),
            job.attempt_id
        ));
        fs::write(&checkpoint, &png[..existing]).unwrap();
        let downloaded = http.download_denoise_job(
            &job,
            &result(&job, &png),
            &checkpoint,
            &AtomicBool::new(false),
        );
        let headers = server.join().unwrap();
        assert_eq!(downloaded.unwrap(), png);
        assert_eq!(fs::read(&checkpoint).unwrap(), png);
        assert_eq!(
            headers.len(),
            png.len().div_ceil(RESULT_CHUNK_BYTES) + usize::from(cut.is_some())
        );
        fs::remove_file(checkpoint).unwrap();
    }
    #[test]
    fn interrupted_chunk_keeps_bytes_and_requests_only_its_missing_tail() {
        transfer(Some(12345), 0);
    }
    #[test]
    fn new_client_resumes_a_previous_process_checkpoint() {
        transfer(None, 12345);
    }
    #[test]
    fn recovered_job_cannot_send_credentials_to_another_profile_or_handle() {
        let http = client("http://127.0.0.1:12345/mc/v1");
        let png = noisy_png();
        let metadata =
            DenoiseMetadata::new(preset("realcugan-2x-conservative").unwrap().recipe(), &png)
                .unwrap();
        let mut job = http.new_denoise_job(metadata).unwrap();
        job.profile_id = "another".into();
        assert_eq!(
            http.denoise_job_status(&job).unwrap_err(),
            HttpTransportError::WireValidation
        );
        job.profile_id = "denoise-test".into();
        job.handle = "../other".into();
        assert_eq!(
            http.denoise_job_status(&job).unwrap_err(),
            HttpTransportError::WireValidation
        );
    }

    #[test]
    fn a_corrupt_checkpoint_is_rejected_and_cleared_without_resubmitting() {
        let http = client("http://127.0.0.1:12345/mc/v1");
        let png = noisy_png();
        let metadata =
            DenoiseMetadata::new(preset("realcugan-2x-conservative").unwrap().recipe(), &png)
                .unwrap();
        let job = http.new_denoise_job(metadata).unwrap();
        let mut corrupt = png.clone();
        corrupt[100] ^= 1;
        let checkpoint = std::env::temp_dir().join(format!("mc-bad-chunk-{}.part", job.attempt_id));
        fs::write(&checkpoint, corrupt).unwrap();
        assert_eq!(
            http.download_denoise_job(
                &job,
                &result(&job, &png),
                &checkpoint,
                &AtomicBool::new(false)
            )
            .unwrap_err(),
            HttpTransportError::WireValidation
        );
        assert_eq!(fs::metadata(&checkpoint).unwrap().len(), 0);
        fs::remove_file(checkpoint).unwrap();
    }
}
