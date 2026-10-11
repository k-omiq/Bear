//! Upload slots end at acceptance; remote slots end at completed metadata.
//! Neither is held by a result download. Only small durable tickets accumulate.
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Condvar};
use std::time::Instant;

const UPLOADS: usize = 2;
const REMOTE_JOBS: usize = 16;
const POLLERS: usize = 4;
const DOWNLOADS: usize = 2;

struct Gate {
    used: Mutex<usize>,
    changed: Condvar,
}
struct Permit<'a>(&'a Gate);
impl Gate {
    fn acquire(&self, cancel: &AtomicBool) -> Option<Permit<'_>> {
        let mut used = self.used.lock().unwrap();
        while *used >= REMOTE_JOBS {
            if cancel.load(Ordering::SeqCst) {
                return None;
            }
            used = self
                .changed
                .wait_timeout(used, Duration::from_millis(100))
                .unwrap()
                .0;
        }
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        *used += 1;
        Some(Permit(self))
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.used.lock().unwrap() -= 1;
        self.0.changed.notify_one();
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    version: u32,
    job: DenoiseJob,
    analysis: AnalysisAttemptRecord,
    result: Option<DenoiseResultMetadata>,
    state: String,
}
struct PageTransfer {
    at: usize,
    record: Transfer,
    path: PathBuf,
    checkpoint: PathBuf,
}
impl PageTransfer {
    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.record).map_err(|_| "journal_unavailable")?;
        write_atomic(&self.path, &bytes).map_err(|_| "journal_unavailable".into())
    }
}

fn prepare(
    batch: &Batch<'_>,
    at: usize,
    owners: &HashMap<&str, u32>,
    cancel: &AtomicBool,
) -> Result<PageTransfer, String> {
    let page = &batch.plan.pages[at];
    batch.authority()?;
    let name = batch
        .names
        .get(&page.page_index)
        .ok_or("cloud_denoise_page_missing")?;
    if owners.get(name.as_str()) != Some(&page.page_index) {
        return Err("cloud_denoise_duplicate_output".into());
    }
    let png = upload_png(&(batch.read)(page)?)?;
    let header = check_upload(&batch.plan.recipe, &png)?;
    if (header.width, header.height) != (page.width, page.height) {
        return Err("cloud_denoise_page_changed".into());
    }
    let metadata = DenoiseMetadata::new(batch.plan.recipe.clone(), &png)?;
    let scope = page_scope(batch.plan, page, &png, &metadata.request_digest)?;
    let grant = batch
        .grants
        .issue_grant_with_epoch(scope.clone(), batch.plan.profile_epoch, PAGE_GRANT_TTL, 1)
        .map_err(grant_error)?;
    batch
        .grants
        .validate_and_consume(&grant.nonce, &scope)
        .map_err(grant_error)?;
    let key = sha256_hex(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            batch.plan.profile_id,
            batch.plan.endpoint_fingerprint,
            batch.plan.chapter_id,
            page.page_index,
            page.appearance,
            metadata.request_digest
        )
        .as_bytes(),
    );
    let root = batch
        .journal
        .map(AnalysisJournal::denoise_root)
        .unwrap_or_else(|| batch.out_dir.join(".denoise-transfers"));
    std::fs::create_dir_all(&root).map_err(|_| "journal_unavailable")?;
    let path = root.join(format!("{key}.json"));
    let checkpoint = root.join(format!("{key}.part"));
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 16384) {
        return Err("journal_unavailable".into());
    }
    let prior = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 16384 => {
            Some(serde_json::from_slice::<Transfer>(&bytes).map_err(|_| "journal_unavailable")?)
        }
        Ok(_) => return Err("journal_unavailable".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("journal_unavailable".into()),
    };
    if let Some(record) = &prior {
        if !(record.version == 1
            && record.job.metadata == metadata
            && record.analysis.profile_id == batch.plan.profile_id
            && record.analysis.chapter_id.as_deref() == Some(batch.plan.chapter_id.as_str())
            && record.analysis.page_index == Some(page.page_index)
            && record.analysis.underlay_sha256 == metadata.request_digest
            && matches!(
                record.state.as_str(),
                "submitting" | "accepted" | "download_pending" | "saved" | "failed" | "cancelled"
            ))
        {
            return Err("journal_unavailable".into());
        }
        if let Some(result) = &record.result {
            result
                .validate(&metadata)
                .map_err(|_| "journal_unavailable")?;
        }
    }
    let record = match prior {
        Some(record)
            if record.result.is_some()
                || !matches!(record.state.as_str(), "failed" | "cancelled") =>
        {
            record
        }
        _ => Transfer {
            version: 1,
            job: batch.gateway.new_job(metadata.clone())?,
            result: None,
            state: "submitting".into(),
            analysis: AnalysisAttemptRecord {
                created_at_ms: analysis::now_ms(),
                schema_version: ANALYSIS_JOURNAL_SCHEMA_VERSION,
                proposal_id: analysis::random_id()?,
                provider: batch.plan.provider,
                profile_id: batch.plan.profile_id.clone(),
                capability: CAPABILITY.into(),
                source_sha256: sha256_hex(&png),
                underlay_sha256: metadata.request_digest.clone(),
                model_identity_sha256: Some(scope.hint_sha256.clone()),
                chapter_id: Some(batch.plan.chapter_id.clone()),
                page_index: Some(page.page_index),
                total_tiles: 1,
                completed_tiles: 0,
                reported_cost_usd: None,
                tile_submission_started: Some(true),
                cancel_requested: false,
                phase: AnalysisPhase::SubmittedTile { index: 0 },
            },
        },
    };
    let mut transfer = PageTransfer {
        at,
        record,
        path,
        checkpoint,
    };
    transfer.save()?; // Intent and idempotent handle durable before POST.
    if let Some(journal) = batch.journal {
        journal
            .write(&transfer.record.analysis)
            .map_err(|_| "journal_unavailable")?;
    }
    if transfer.record.result.is_none() {
        batch.authority()?;
        if let Err(code) = batch.gateway.submit_job(&transfer.record.job, &png, cancel) {
            finish_attempt(batch, &mut transfer, &Err(code.clone()))?;
            return Err(code);
        }
        transfer.record.state = "accepted".into();
        if let Err(code) = transfer.save() {
            let _ = batch.gateway.cancel_job(&transfer.record.job);
            return Err(code);
        }
    }
    // png is dropped here; neither a poller nor a download retains upload/native buffers.
    Ok(transfer)
}

fn finish_attempt(
    batch: &Batch<'_>,
    transfer: &mut PageTransfer,
    outcome: &Result<DenoisedOutput, String>,
) -> Result<(), String> {
    transfer.record.analysis.phase = match outcome {
        Ok(_) => {
            transfer.record.analysis.completed_tiles = 1;
            transfer.record.state = "saved".into();
            AnalysisPhase::RunPageComplete
        }
        Err(code) if code == CANCELLED => {
            transfer.record.analysis.cancel_requested = true;
            if transfer.record.result.is_none() {
                transfer.record.state = "cancelled".into();
            }
            AnalysisPhase::Cancelled
        }
        // A status route's HTTP error says nothing about the accepted call.
        // Retain that ticket as unknown instead of creating a paid new attempt.
        Err(code) if answered(code) && code != "gateway_error" => {
            if transfer.record.result.is_none() {
                transfer.record.state = "failed".into();
            }
            AnalysisPhase::Failed {
                code: code.split(':').next().unwrap_or(code).into(),
            }
        }
        Err(_) => AnalysisPhase::UnknownRemoteState { index: 0 },
    };
    transfer.save()?;
    if let Some(journal) = batch.journal {
        journal
            .write(&transfer.record.analysis)
            .map_err(|_| "journal_unavailable")?;
    }
    Ok(())
}

fn poll(batch: &Batch<'_>, transfer: &mut PageTransfer, cancel: &AtomicBool) -> Result<(), String> {
    if transfer.record.result.is_some() {
        return Ok(());
    }
    let began = Instant::now();
    let mut failures = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return batch
                .gateway
                .cancel_job(&transfer.record.job)
                .and(Err(CANCELLED.into()));
        }
        if let Err(code) = batch.authority() {
            let _ = batch.gateway.cancel_job(&transfer.record.job);
            return Err(code);
        }
        if began.elapsed() >= Duration::from_secs(1500) {
            let _ = batch.gateway.cancel_job(&transfer.record.job);
            return Err("gateway_unreachable".into());
        }
        match batch.gateway.poll_job(&transfer.record.job) {
            Ok(Polled::Completed(meta)) => {
                meta.validate(&transfer.record.job.metadata)
                    .map_err(|_| "gateway_protocol")?;
                let page = &batch.plan.pages[transfer.at];
                if meta.page_size(&transfer.record.job.metadata) != Some((page.width, page.height)) {
                    return Err("gateway_protocol".into());
                }
                transfer.record.result = Some(meta);
                transfer.record.state = "download_pending".into();
                return transfer.save();
            }
            Ok(Polled::Failed { code }) => {
                return Err(rejection_code(&HttpTransportError::JobFailed { code }))
            }
            Ok(Polled::Cancelled) => return Err(CANCELLED.into()),
            Ok(Polled::Waiting) => failures = 0,
            Err(code) => {
                failures += 1;
                if failures >= 5
                    || !matches!(code.as_str(), "gateway_unreachable" | "gateway_error")
                {
                    let _ = batch.gateway.cancel_job(&transfer.record.job);
                    return Err(code);
                }
            }
        }
        for _ in 0..4 {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(if cfg!(test) { 1 } else { 250 }));
        }
    }
}

fn download(
    batch: &Batch<'_>,
    transfer: &PageTransfer,
    cancel: &AtomicBool,
) -> Result<DenoisedOutput, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    batch.authority()?;
    let page = &batch.plan.pages[transfer.at];
    let meta = transfer.record.result.as_ref().ok_or("gateway_protocol")?;
    let png =
        batch
            .gateway
            .download_job(&transfer.record.job, meta, &transfer.checkpoint, cancel)?;
    let result = DenoiseResult {
        protocol_version: meta.protocol_version.clone(),
        request_digest: meta.request_digest.clone(),
        page_png_b64: String::new(),
        info: meta.info.clone(),
    };
    result
        .validate(&transfer.record.job.metadata, page.width, page.height, &png)
        .map_err(|_| "gateway_protocol")?;
    if sha256_hex(&png) != meta.png_sha256 {
        return Err("gateway_protocol".into());
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    batch.authority()?;
    // Re-read the consent-bound appearance before restoring/writing. Edits during
    // queued inference cannot attach a result onto a changed page.
    let native = (batch.read)(page)?;
    let retained = restore_denoised(&native, &png)?;
    let path = batch
        .out_dir
        .join(format!("{}.png", batch.names[&page.page_index]));
    write_atomic(&path, &retained).map_err(|e| format!("cloud_denoise_write_failed: {e}"))?;
    Ok(DenoisedOutput {
        page_index: page.page_index,
        path: path.display().to_string(),
        gray: meta.info.gray,
    })
}

pub(super) fn run(
    batch: &Batch<'_>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> CloudDenoiseReport {
    let mut owners = HashMap::new();
    for page in &batch.plan.pages {
        if let Some(name) = batch.names.get(&page.page_index) {
            owners.entry(name.as_str()).or_insert(page.page_index);
        }
    }
    let next = AtomicUsize::new(0);
    let stopped = Mutex::new(None::<String>);
    let total = batch.plan.pages.len();
    let outcomes = Mutex::new((vec![None::<Result<DenoisedOutput, String>>; total], 0usize));
    let record = |at: usize, outcome: Result<DenoisedOutput, String>| {
        if let Err(code) = &outcome {
            if stops(code) {
                *stopped.lock().unwrap() = Some(code.clone());
            }
        }
        let mut all = outcomes.lock().unwrap();
        all.0[at] = Some(outcome);
        all.1 += 1;
        progress(all.1, total);
    };
    let gate = Gate {
        used: Mutex::new(0),
        changed: Condvar::new(),
    };
    let (pending_tx, pending_rx) = mpsc::channel::<(PageTransfer, Permit<'_>)>();
    let pending_rx = Mutex::new(pending_rx);
    let (ready_tx, ready_rx) = mpsc::channel::<PageTransfer>();
    let ready_rx = Mutex::new(ready_rx);
    std::thread::scope(|threads| {
        for _ in 0..DOWNLOADS {
            let (ready_rx, record) = (&ready_rx, &record);
            threads.spawn(move || loop {
                let Ok(mut transfer) = ready_rx.lock().unwrap().recv() else {
                    break;
                };
                let mut outcome = download(batch, &transfer, cancel);
                if let Err(code) = finish_attempt(batch, &mut transfer, &outcome) {
                    outcome = Err(code);
                }
                let saved = outcome.is_ok();
                record(transfer.at, outcome);
                if saved {
                    batch.gateway.release_result(&transfer.record.job);
                }
            });
        }
        for _ in 0..POLLERS {
            let (pending_rx, record) = (&pending_rx, &record);
            let ready_tx = ready_tx.clone();
            threads.spawn(move || loop {
                let Ok((mut transfer, permit)) = pending_rx.lock().unwrap().recv() else {
                    break;
                };
                let polled = poll(batch, &mut transfer, cancel);
                drop(permit); // Completed status frees the remote slot, before ANY download.
                match polled {
                    Ok(()) => {
                        let _ = ready_tx.send(transfer);
                    }
                    Err(code) => {
                        let mut outcome = Err(code);
                        if let Err(code) = finish_attempt(batch, &mut transfer, &outcome) {
                            outcome = Err(code);
                        }
                        record(transfer.at, outcome);
                    }
                }
            });
        }
        drop(ready_tx);
        for _ in 0..UPLOADS {
            let (owners, next, stopped, record, gate) = (&owners, &next, &stopped, &record, &gate);
            let pending_tx = pending_tx.clone();
            threads.spawn(move || loop {
                let Some(permit) = gate.acquire(cancel) else {
                    break;
                };
                let at = next.fetch_add(1, Ordering::SeqCst);
                if at >= total {
                    break;
                }
                let stop = stopped.lock().unwrap().clone();
                let prepared = match stop {
                    Some(code) => Err(code),
                    None => prepare(batch, at, owners, cancel),
                };
                match prepared {
                    Ok(transfer) => {
                        if pending_tx.send((transfer, permit)).is_err() {
                            break;
                        }
                    }
                    Err(code) => {
                        drop(permit);
                        record(at, Err(code));
                    }
                }
            });
        }
        drop(pending_tx);
    });
    let (outcomes, _) = outcomes.into_inner().unwrap();
    let mut report = CloudDenoiseReport::default();
    for (page, outcome) in batch.plan.pages.iter().zip(outcomes) {
        match outcome {
            Some(Ok(output)) => report.written.push(output),
            Some(Err(code)) => report.failed.push(PageFailure {
                page_index: page.page_index,
                code,
            }),
            None => report.cancelled = true,
        }
    }
    report.cancelled |= report.failed.iter().any(|f| f.code == CANCELLED);
    report
}
