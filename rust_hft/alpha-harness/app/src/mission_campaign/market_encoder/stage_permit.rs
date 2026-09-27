//! Worker-side consumer for current, one-stage permission. No ledger or private key is mounted.
use super::{read_json, worker, MarketRequest};
use alpha_domain::campaign_stage::{
    verify_stage_permit, CampaignStageRequestV1, SignedCampaignStagePermitV1, REQUEST_SCHEMA,
    REQUEST_SECONDS,
};
use alpha_domain::market_encoder_study::MarketTrainingStageV1;
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const AUTHORIZATION_SCHEMA: &str = "monday.market_stage_authorization.v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageAuthorization {
    pub schema_version: String,
    pub permit: SignedCampaignStagePermitV1,
    /// Captured immediately before permission is consumed. Historic readback
    /// validates the original permit at this instant, never at a later expiry.
    pub accepted_at: DateTime<Utc>,
}

pub(crate) struct FileStageAuthority {
    request: MarketRequest,
    request_sha256: String,
    attempt_sha256: String,
    root_grant_sha256: String,
    job_name: String,
    pod_uid: String,
    original_grant_deadline: DateTime<Utc>,
    directory: PathBuf,
}
impl FileStageAuthority {
    pub(crate) fn from_environment(
        request: &MarketRequest,
        request_sha256: &str,
        attempt_sha256: &str,
        root_grant_sha256: &str,
        original_grant_deadline: DateTime<Utc>,
        work: &Path,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            request: request.clone(),
            request_sha256: request_sha256.into(),
            attempt_sha256: attempt_sha256.into(),
            root_grant_sha256: root_grant_sha256.into(),
            job_name: std::env::var("MONDAY_CAMPAIGN_JOB_NAME")
                .context("market stage requires its admitted Job name")?,
            pod_uid: std::env::var("MONDAY_CAMPAIGN_POD_UID")
                .context("market stage requires its current Pod UID")?,
            original_grant_deadline,
            directory: work.join("stage-authority"),
        })
    }
    pub(crate) fn validate_runtime_binding(
        &self,
        job_name: &str,
        pod_uid: &str,
    ) -> anyhow::Result<()> {
        if self.job_name != job_name || self.pod_uid != pod_uid {
            bail!("durable market stage evidence belongs to another Job or Pod; explicit recovery is required");
        }
        Ok(())
    }
    pub(crate) fn authorize(
        &self,
        stage: &MarketTrainingStageV1,
    ) -> anyhow::Result<StageAuthorization> {
        let mut bytes = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let challenge = CampaignStageRequestV1 {
            schema_version: REQUEST_SCHEMA.into(),
            request_sha256: self.request_sha256.clone(),
            attempt_sha256: self.attempt_sha256.clone(),
            root_grant_sha256: self.root_grant_sha256.clone(),
            job_name: self.job_name.clone(),
            pod_uid: self.pod_uid.clone(),
            stage: stage.key,
            nonce: hex::encode(bytes),
            requested_at: Utc::now(),
        };
        challenge.validate().map_err(anyhow::Error::msg)?;
        if challenge.requested_at >= self.original_grant_deadline {
            bail!("market stage grant expired before permission request");
        }
        let requests = self.directory.join("requests");
        std::fs::create_dir_all(&requests)?;
        let name = format!("{}.json", worker::stage_name(stage.key));
        let request_path = requests.join(&name);
        let permit_path = self.directory.join("permits").join(name);
        // An old challenge or answer is not permission to retry an interrupted
        // attempt. Recovery must explicitly account for its existing evidence.
        if request_path.try_exists()? || permit_path.try_exists()? {
            bail!(
                "market stage permission already has evidence; automatic re-request is forbidden"
            );
        }
        write_atomic_new(&request_path, &serde_json::to_vec(&challenge)?)?;
        await_permit(
            &self.request,
            &challenge,
            &permit_path,
            self.original_grant_deadline,
            Duration::from_secs(REQUEST_SECONDS as u64),
            Duration::from_secs(1),
        )
    }
}

pub(crate) fn await_permit(
    request: &MarketRequest,
    challenge: &CampaignStageRequestV1,
    path: &Path,
    original_grant_deadline: DateTime<Utc>,
    wait: Duration,
    poll: Duration,
) -> anyhow::Result<StageAuthorization> {
    let started = Instant::now();
    loop {
        let now = Utc::now();
        if now >= original_grant_deadline
            || now >= challenge.requested_at + chrono::TimeDelta::seconds(REQUEST_SECONDS)
            || started.elapsed() >= wait
        {
            bail!("market stage authority unavailable or denied before its deadline");
        }
        if path.try_exists()? {
            let permit: SignedCampaignStagePermitV1 = read_json(path)?;
            verify_stage_permit(&request.stage_authority, challenge, &permit, now)
                .map_err(anyhow::Error::msg)?;
            if permit.job_deadline_at > original_grant_deadline {
                bail!("market stage permit extends the original root deadline");
            }
            return Ok(StageAuthorization {
                schema_version: AUTHORIZATION_SCHEMA.into(),
                permit,
                accepted_at: now,
            });
        }
        std::thread::sleep(poll.min(wait.saturating_sub(started.elapsed())));
    }
}

/// Immutable evidence publication with an atomic directory entry and directory
/// fsync. The admitted persistent volume, not this function, supplies node-loss durability.
pub(crate) fn write_atomic_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let directory = path
        .parent()
        .context("stage evidence requires a parent directory")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
