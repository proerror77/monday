//! Publication request and application payload reservations. These are not billable wire bytes.
//! A durable reservation precedes each send. Failures never refund it.
use crate::{
    orchestrator::Artifact,
    release::{ReleaseProducer, SourceArchive},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const JSON_LIMIT: u64 = 1024 * 1024;
pub const SMALL_RESPONSE_LIMIT: u64 = 4096;
pub const CONTROL_RESPONSE_LIMIT: u64 = 64 * 1024;
pub const STS_REQUEST_LIMIT: u64 = 128 * 1024;
const LEDGER_LIMIT: u64 = 2 * 1024 * 1024;
const NAMESPACES: [&str; 2] = ["research/builds/", "research/sources/"];

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub requests: u64,
    pub request_payload_bytes: u64,
    pub response_payload_bytes: u64,
}
impl Limits {
    pub fn checked_add(self, other: Self) -> Result<Self> {
        Ok(Self {
            requests: self
                .requests
                .checked_add(other.requests)
                .context("budget request overflow")?,
            request_payload_bytes: self
                .request_payload_bytes
                .checked_add(other.request_payload_bytes)
                .context("budget request payload overflow")?,
            response_payload_bytes: self
                .response_payload_bytes
                .checked_add(other.response_payload_bytes)
                .context("budget response payload overflow")?,
        })
    }
    fn fits(self, limit: Self) -> bool {
        self.requests <= limit.requests
            && self.request_payload_bytes <= limit.request_payload_bytes
            && self.response_payload_bytes <= limit.response_payload_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub repository: String,
    pub source_sha: String,
    pub product: String,
    pub image_repository: String,
    pub software_run_id: u64,
    pub publisher_run_id: u64,
    pub publisher_run_attempt: u32,
    pub publisher_job_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: String,
    pub repository: String,
    pub source_sha: String,
    pub publisher_run_id: u64,
    pub publisher_run_attempt: u32,
    pub expires_at_ms: u64,
    pub publication_namespaces: Vec<String>,
    pub limits: Limits,
    pub allocations: Vec<Allocation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub product: String,
    pub limits: Limits,
}
impl Envelope {
    pub fn validate(&self, binding: &Binding, now: u64) -> Result<Limits> {
        ensure!(
            self.schema == "monday.oss-publication-budget.v1"
                && self.repository == binding.repository
                && self.source_sha == binding.source_sha
                && self.source_sha.len() == 40
                && self
                    .source_sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && self.publisher_run_id > 0
                && self.publisher_run_id == binding.publisher_run_id
                && self.publisher_run_attempt == 1
                && binding.publisher_run_attempt == 1
                && self.expires_at_ms > now
                && self.expires_at_ms < (1u64 << 53)
                && self.publication_namespaces == NAMESPACES
                && binding.software_run_id > 0
                && binding.publisher_job_id > 0,
            "publication budget has wrong source, run, attempt, expiry or scope"
        );
        ensure!(
            !self.allocations.is_empty()
                && self.allocations.len() <= 3
                && self
                    .allocations
                    .windows(2)
                    .all(|w| w[0].product < w[1].product),
            "product allocations required"
        );
        let mut sum = Limits::default();
        for allocation in &self.allocations {
            let product = &allocation.product;
            let limits = &allocation.limits;
            ensure!(
                matches!(
                    product.as_str(),
                    "cex-runner" | "controller" | "prediction-runner"
                ) && limits.requests > 0,
                "unknown or empty product budget"
            );
            sum = sum.checked_add(*limits)?;
        }
        ensure!(
            sum.fits(self.limits),
            "product allocations exceed execution envelope"
        );
        self.allocations
            .iter()
            .find(|a| a.product == binding.product)
            .map(|a| a.limits)
            .context("selected product has no allocation")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub schema: u32,
    pub repository: String,
    pub product: String,
    pub source: SourceArchive,
    pub software_producer: ReleaseProducer,
    pub publisher_prefixes: Vec<String>,
    pub program_objects: Vec<Artifact>,
    pub build_count: u64,
}
impl Inventory {
    pub fn validate(&self, binding: &Binding) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.repository == binding.repository
                && self.product == binding.product
                && self.source.schema == 1
                && self.source.code_commit == binding.source_sha
                && self.source.archive.key
                    == format!("research/sources/{}/source.tar", binding.source_sha)
                && self.software_producer.repository == binding.repository
                && self.software_producer.source_sha == binding.source_sha
                && self.software_producer.run_id == binding.software_run_id
                && self.software_producer.run_attempt > 0
                && self.software_producer.job_id > 0
                && matches!(
                    self.software_producer.workflow_path.as_str(),
                    ".github/workflows/ploy-ci.yml" | ".github/workflows/acr-publish.yml"
                )
                && self.build_count > 0
                && self.build_count <= 255
                && self.publisher_prefixes.len() as u64 == self.build_count + 1
                && self.publisher_prefixes.windows(2).all(|w| w[0] < w[1])
                && self
                    .publisher_prefixes
                    .iter()
                    .all(|p| crate::release_oss::exact_prefix(p))
                && self
                    .publisher_prefixes
                    .contains(&format!("research/sources/{}/", binding.source_sha))
                && !self.program_objects.is_empty()
                && self.program_objects.windows(2).all(|w| w[0].key < w[1].key),
            "budget inventory has foreign or incomplete native provenance"
        );
        for object in std::iter::once(&self.source.archive).chain(&self.program_objects) {
            ensure!(
                crate::valid_digest(&object.sha256)
                    && (1..=512 * 1024 * 1024).contains(&object.bytes)
                    && self
                        .publisher_prefixes
                        .iter()
                        .any(|p| object.key.starts_with(p)),
                "budget object is outside actual source/Build plan"
            );
        }
        for prefix in self
            .publisher_prefixes
            .iter()
            .filter(|p| p.starts_with("research/builds/"))
        {
            ensure!(
                self.program_objects
                    .iter()
                    .any(|a| a.key.starts_with(prefix)),
                "budget Build prefix has no measured executable"
            );
        }
        for object in &self.program_objects {
            ensure!(
                object.key.starts_with("research/builds/") && object.key.split('/').count() == 4,
                "budget program is not an exact Build executable"
            );
        }
        Ok(())
    }
    pub fn publication_limits(&self) -> Result<Limits> {
        let metadata = self
            .build_count
            .checked_mul(3)
            .context("metadata count overflow")?;
        let objects = 1u64
            .checked_add(self.program_objects.len() as u64)
            .and_then(|n| n.checked_add(metadata))
            .context("object count overflow")?;
        let mut uploaded = self.source.archive.bytes;
        let mut read = self.source.archive.bytes.max(SMALL_RESPONSE_LIMIT);
        for object in &self.program_objects {
            uploaded = uploaded
                .checked_add(object.bytes)
                .context("object size overflow")?;
            read = read
                .checked_add(object.bytes.max(SMALL_RESPONSE_LIMIT))
                .context("read size overflow")?;
        }
        let metadata_bytes = metadata
            .checked_mul(JSON_LIMIT)
            .context("metadata byte overflow")?;
        Ok(Limits {
            requests: objects
                .checked_mul(3)
                .and_then(|n| n.checked_add(6))
                .context("request count overflow")?,
            request_payload_bytes: uploaded
                .checked_add(metadata_bytes)
                .and_then(|n| n.checked_add(2 * STS_REQUEST_LIMIT))
                .context("upload budget overflow")?,
            response_payload_bytes: read
                .checked_add(metadata_bytes)
                .and_then(|n| {
                    objects
                        .checked_mul(2)
                        .and_then(|n| n.checked_add(1))
                        .and_then(|n| n.checked_mul(SMALL_RESPONSE_LIMIT))
                        .and_then(|small| n.checked_add(small))
                })
                .and_then(|n| n.checked_add(4 * CONTROL_RESPONSE_LIMIT))
                .context("response budget overflow")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub ledger_path: PathBuf,
    pub envelope_sha256: String,
    pub binding_sha256: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: String,
    envelope: Envelope,
    binding: Binding,
    inventory: Inventory,
    policy_sha256: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Source,
    Publish,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    OssRead,
    OssWrite,
    Oidc,
    Sts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Claim {
        name: String,
    },
    Reserve {
        phase: Phase,
        service: Service,
        resource: String,
        limits: Limits,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sequence: u64,
    previous_sha256: String,
    action: Action,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCounts {
    pub oss_read: u64,
    pub oss_write: u64,
    pub oidc: u64,
    pub sts: u64,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub schema: &'static str,
    pub identity: PublicIdentity,
    pub envelope_sha256: String,
    pub inventory_sha256: String,
    pub limits: Limits,
    pub reserved: Limits,
    pub services: ServiceCounts,
    pub claims: BTreeSet<String>,
}
#[derive(Debug, Serialize)]
pub struct PublicIdentity {
    pub repository: String,
    pub source_sha: String,
    pub product: String,
    pub software_run_id: u64,
    pub publisher_run_id: u64,
    pub publisher_run_attempt: u32,
    pub publisher_job_id: u64,
}

pub struct Ledger {
    reference: Reference,
}
struct State {
    header: Header,
    records: u64,
    previous: String,
    reserved: Limits,
    services: ServiceCounts,
    claims: BTreeSet<String>,
    controls: BTreeSet<String>,
    source_reads: u64,
    anchors: Vec<String>,
}
fn open(path: &Path, create: bool) -> Result<std::fs::File> {
    ensure!(path.is_absolute(), "budget ledger requires absolute path");
    let parent = path.parent().context("budget parent required")?;
    ensure!(
        parent.canonicalize()? == parent && parent.metadata()?.permissions().mode() & 0o077 == 0,
        "budget parent must be canonical and private"
    );
    let mut flags = rustix::fs::OFlags::RDWR
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK;
    if create {
        flags |= rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL;
    }
    let file = std::fs::File::from(
        rustix::fs::open(path, flags, rustix::fs::Mode::from_raw_mode(0o600)).map_err(|_| {
            anyhow::anyhow!("private budget ledger unavailable or already initialized")
        })?,
    );
    let m = file.metadata()?;
    ensure!(
        m.is_file()
            && m.nlink() == 1
            && m.permissions().mode() & 0o077 == 0
            && m.len() <= LEDGER_LIMIT,
        "invalid private budget ledger"
    );
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(file)
}
fn now() -> Result<u64> {
    u64::try_from(chrono::Utc::now().timestamp_millis()).context("invalid budget clock")
}
fn initialization_path(path: &Path) -> PathBuf {
    path.with_extension("initialized")
}
fn require_initialization(path: &Path, state: &State) -> Result<()> {
    let mut marker = open(&initialization_path(path), false)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut marker)
        .take(LEDGER_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes == format!("{}\n", state.anchors.join("\n")).as_bytes(),
        "budget initialization marker missing or changed"
    );
    Ok(())
}
impl State {
    fn apply(&mut self, action: &Action, check_expiry: bool) -> Result<()> {
        let allocation = self
            .header
            .envelope
            .validate(&self.header.binding, if check_expiry { now()? } else { 0 })?;
        match action {
            Action::Claim { name } => {
                let required = match name.as_str() {
                    "source_exchange" => None,
                    "source_ready" => Some("source_exchange"),
                    "preflight" => Some("source_ready"),
                    "preflight_ready" => Some("preflight"),
                    "publish_exchange" => Some("preflight_ready"),
                    "publisher_ready" => Some("publish_exchange"),
                    "publication" => Some("publisher_ready"),
                    "publication_ready" => Some("publication"),
                    _ => anyhow::bail!("unknown publication budget phase"),
                };
                ensure!(
                    required.is_none_or(|p| self.claims.contains(p))
                        && self.claims.insert(name.clone()),
                    "publication phase already consumed or prerequisite missing"
                );
                if name == "source_ready" || name == "publisher_ready" {
                    let phase = if name == "source_ready" {
                        "Source"
                    } else {
                        "Publish"
                    };
                    ensure!(
                        self.controls.contains(&format!("{phase}:Oidc"))
                            && self.controls.contains(&format!("{phase}:Sts")),
                        "publication exchange was not fully reserved"
                    );
                }
                if name == "preflight_ready" {
                    ensure!(self.source_reads == 2, "preflight reservations incomplete");
                }
            }
            Action::Reserve {
                phase,
                service,
                resource,
                limits,
            } => {
                ensure!(
                    limits.requests == 1,
                    "one reservation must represent one native call"
                );
                match service {
                    Service::Oidc | Service::Sts => {
                        let (begin, done) = if *phase == Phase::Source {
                            ("source_exchange", "source_ready")
                        } else {
                            ("publish_exchange", "publisher_ready")
                        };
                        ensure!(
                            self.claims.contains(begin)
                                && !self.claims.contains(done)
                                && limits.response_payload_bytes <= CONTROL_RESPONSE_LIMIT
                                && limits.request_payload_bytes
                                    <= if *service == Service::Oidc {
                                        0
                                    } else {
                                        STS_REQUEST_LIMIT
                                    }
                                && resource
                                    == if *service == Service::Oidc {
                                        "oidc"
                                    } else {
                                        "sts"
                                    }
                                && self.controls.insert(format!("{phase:?}:{service:?}")),
                            "control phase was consumed or exceeds payload limit"
                        );
                    }
                    Service::OssRead | Service::OssWrite => {
                        let (begin, done) = if *phase == Phase::Source {
                            ("preflight", "preflight_ready")
                        } else {
                            ("publication", "publication_ready")
                        };
                        ensure!(
                            self.claims.contains(begin)
                                && !self.claims.contains(done)
                                && (*phase != Phase::Source || *service == Service::OssRead)
                                && (resource.is_empty()
                                    || self
                                        .header
                                        .inventory
                                        .publisher_prefixes
                                        .iter()
                                        .any(|p| resource.starts_with(p))),
                            "OSS request is outside active budget phase or scope"
                        );
                        if *phase == Phase::Source {
                            ensure!(
                                self.source_reads < 2
                                    && limits.request_payload_bytes == 0
                                    && (resource.is_empty()
                                        || resource == &self.header.inventory.source.archive.key),
                                "source preflight cannot repeat or read another object"
                            );
                            self.source_reads += 1;
                        }
                    }
                }
                let next = self.reserved.checked_add(*limits)?;
                ensure!(
                    next.fits(allocation),
                    "publication request or payload budget exhausted before send"
                );
                self.reserved = next;
                match service {
                    Service::OssRead => self.services.oss_read += 1,
                    Service::OssWrite => self.services.oss_write += 1,
                    Service::Oidc => self.services.oidc += 1,
                    Service::Sts => self.services.sts += 1,
                }
            }
        }
        Ok(())
    }
}
impl Ledger {
    pub fn create(
        path: &Path,
        envelope: Envelope,
        binding: Binding,
        inventory: Inventory,
        policy_sha256: String,
    ) -> Result<Self> {
        let limit = envelope.validate(&binding, now()?)?;
        inventory.validate(&binding)?;
        ensure!(
            inventory.publication_limits()?.fits(limit) && crate::valid_digest(&policy_sha256),
            "allocation cannot fund the complete native publication"
        );
        let header = Header {
            schema: "monday.oss-budget-ledger.v1".into(),
            envelope,
            binding,
            inventory,
            policy_sha256,
        };
        let bytes = serde_json::to_vec(&header)?;
        ensure!(
            bytes.len() < LEDGER_LIMIT as usize,
            "budget header exceeds bound"
        );
        // Burn initialization before creating the journal. A lost journal cannot reborrow.
        let mut marker = open(&initialization_path(path), true)?;
        marker.write_all(format!("{}\n", crate::identity(&header)?).as_bytes())?;
        marker.sync_all()?;
        std::fs::File::open(path.parent().context("budget parent")?)?.sync_all()?;
        let mut file = open(path, true)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::File::open(path.parent().context("budget parent")?)?.sync_all()?;
        let m = file.metadata()?;
        Ok(Self {
            reference: Reference {
                ledger_path: path.into(),
                envelope_sha256: crate::identity(&header.envelope)?,
                binding_sha256: crate::identity(&header)?,
                device: m.dev(),
                inode: m.ino(),
            },
        })
    }
    pub fn from_path(path: &Path) -> Result<Self> {
        let mut file = open(path, false)?;
        let state = Self::read(&mut file)?;
        require_initialization(path, &state)?;
        let m = file.metadata()?;
        Ok(Self {
            reference: Reference {
                ledger_path: path.into(),
                envelope_sha256: crate::identity(&state.header.envelope)?,
                binding_sha256: crate::identity(&state.header)?,
                device: m.dev(),
                inode: m.ino(),
            },
        })
    }
    pub fn from_reference(reference: &Reference) -> Result<Self> {
        let ledger = Self {
            reference: reference.clone(),
        };
        ledger.locked()?;
        Ok(ledger)
    }
    pub fn reference(&self) -> Reference {
        self.reference.clone()
    }
    fn read(file: &mut std::fs::File) -> Result<State> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        Read::by_ref(file)
            .take(LEDGER_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= LEDGER_LIMIT && bytes.ends_with(b"\n"),
            "budget ledger missing, truncated or too large"
        );
        let mut lines = bytes[..bytes.len() - 1].split(|b| *b == b'\n');
        let header: Header = serde_json::from_slice(lines.next().context("budget header missing")?)
            .map_err(|_| anyhow::anyhow!("invalid budget header"))?;
        ensure!(
            header.schema == "monday.oss-budget-ledger.v1",
            "unknown budget ledger schema"
        );
        header.envelope.validate(&header.binding, 0)?;
        header.inventory.validate(&header.binding)?;
        ensure!(
            crate::valid_digest(&header.policy_sha256),
            "invalid budget policy binding"
        );
        let mut state = State {
            previous: crate::identity(&header)?,
            anchors: vec![crate::identity(&header)?],
            header,
            records: 0,
            reserved: Limits::default(),
            services: ServiceCounts::default(),
            claims: BTreeSet::new(),
            controls: BTreeSet::new(),
            source_reads: 0,
        };
        for line in lines {
            let record: Record = serde_json::from_slice(line)
                .map_err(|_| anyhow::anyhow!("invalid budget reservation"))?;
            ensure!(
                record.sequence == state.records + 1 && record.previous_sha256 == state.previous,
                "budget reservation sequence or digest changed"
            );
            state.apply(&record.action, false)?;
            state.records = record.sequence;
            state.previous = crate::identity(&record)?;
            state.anchors.push(state.previous.clone());
        }
        Ok(state)
    }
    fn locked(&self) -> Result<(std::fs::File, State)> {
        let mut file = open(&self.reference.ledger_path, false)?;
        let m = file.metadata()?;
        ensure!(
            m.dev() == self.reference.device && m.ino() == self.reference.inode,
            "budget ledger replaced"
        );
        let state = Self::read(&mut file)?;
        require_initialization(&self.reference.ledger_path, &state)?;
        ensure!(
            crate::identity(&state.header.envelope)? == self.reference.envelope_sha256
                && crate::identity(&state.header)? == self.reference.binding_sha256,
            "budget allocation or binding changed"
        );
        Ok((file, state))
    }
    fn append(&self, action: Action) -> Result<()> {
        let (mut file, mut state) = self.locked()?;
        state.apply(&action, true)?;
        let record = Record {
            sequence: state
                .records
                .checked_add(1)
                .context("budget sequence overflow")?,
            previous_sha256: state.previous,
            action,
        };
        let bytes = serde_json::to_vec(&record)?;
        ensure!(
            file.metadata()?
                .len()
                .checked_add(bytes.len() as u64 + 1)
                .is_some_and(|n| n <= LEDGER_LIMIT),
            "budget ledger capacity exhausted"
        );
        // Commit the intention first. Losing a complete journal record cannot reset it.
        let mut marker = open(&initialization_path(&self.reference.ledger_path), false)?;
        let anchor = format!("{}\n", crate::identity(&record)?);
        ensure!(
            marker
                .metadata()?
                .len()
                .checked_add(anchor.len() as u64)
                .is_some_and(|n| n <= LEDGER_LIMIT),
            "budget anchor capacity exhausted"
        );
        marker.seek(SeekFrom::End(0))?;
        marker.write_all(anchor.as_bytes())?;
        marker.sync_all()?;
        file.seek(SeekFrom::End(0))?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }
    pub fn claim(&self, name: &str) -> Result<()> {
        self.append(Action::Claim { name: name.into() })
    }
    pub fn reserve(
        &self,
        phase: Phase,
        service: Service,
        resource: &str,
        request_payload_bytes: u64,
        response_payload_bytes: u64,
    ) -> Result<()> {
        self.append(Action::Reserve {
            phase,
            service,
            resource: resource.into(),
            limits: Limits {
                requests: 1,
                request_payload_bytes,
                response_payload_bytes,
            },
        })
    }
    pub fn require_binding(&self, binding: &Binding, policy_sha256: &str) -> Result<()> {
        let (_, state) = self.locked()?;
        ensure!(
            state.header.binding == *binding && state.header.policy_sha256 == policy_sha256,
            "budget has foreign context or OSS policy"
        );
        state.header.envelope.validate(binding, now()?)?;
        Ok(())
    }
    pub fn require_inventory(&self, inventory: &Inventory) -> Result<()> {
        let (_, state) = self.locked()?;
        inventory.validate(&state.header.binding)?;
        ensure!(
            state.header.inventory == *inventory,
            "budget does not match recomputed native object inventory"
        );
        Ok(())
    }
    pub fn require_publish_funding(&self) -> Result<()> {
        let (_, state) = self.locked()?;
        let allocation = state
            .header
            .envelope
            .validate(&state.header.binding, now()?)?;
        let total = state.header.inventory.publication_limits()?;
        let remaining = Limits {
            requests: total
                .requests
                .checked_sub(4)
                .context("publication request bound")?,
            request_payload_bytes: total
                .request_payload_bytes
                .checked_sub(STS_REQUEST_LIMIT)
                .context("publication upload bound")?,
            response_payload_bytes: total
                .response_payload_bytes
                .checked_sub(2 * CONTROL_RESPONSE_LIMIT + SMALL_RESPONSE_LIMIT)
                .context("publication response bound")?,
        };
        ensure!(
            state.reserved.checked_add(remaining)?.fits(allocation),
            "remaining budget cannot fund complete publication before exchange"
        );
        Ok(())
    }
    pub fn require_session(
        &self,
        publisher: bool,
        prefixes: &[String],
        policy_sha256: &str,
    ) -> Result<()> {
        let (_, state) = self.locked()?;
        let expected = if publisher {
            state.header.inventory.publisher_prefixes.clone()
        } else {
            vec![format!(
                "research/sources/{}/",
                state.header.binding.source_sha
            )]
        };
        ensure!(
            prefixes == expected && state.header.policy_sha256 == policy_sha256,
            "session budget has foreign scope or policy"
        );
        state
            .header
            .envelope
            .validate(&state.header.binding, now()?)?;
        Ok(())
    }
    pub fn status(&self) -> Result<Status> {
        let (_, state) = self.locked()?;
        let b = state.header.binding;
        Ok(Status {
            schema: "monday.oss-publication-budget-usage.v1",
            limits: state.header.envelope.validate(&b, 0)?,
            envelope_sha256: self.reference.envelope_sha256.clone(),
            inventory_sha256: crate::identity(&state.header.inventory)?,
            identity: PublicIdentity {
                repository: b.repository,
                source_sha: b.source_sha,
                product: b.product,
                software_run_id: b.software_run_id,
                publisher_run_id: b.publisher_run_id,
                publisher_run_attempt: b.publisher_run_attempt,
                publisher_job_id: b.publisher_job_id,
            },
            reserved: state.reserved,
            services: state.services,
            claims: state.claims,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture(
        product: &str,
        builds: u64,
        programs: u64,
    ) -> (Binding, Inventory, Envelope) {
        let binding = Binding {
            repository: "owner/repo".into(),
            source_sha: "a".repeat(40),
            product: product.into(),
            image_repository: format!("registry/{product}"),
            software_run_id: 1,
            publisher_run_id: 2,
            publisher_run_attempt: 1,
            publisher_job_id: 3,
        };
        let mut prefixes: Vec<_> = (1..=builds)
            .map(|n| format!("research/builds/{n:064x}/"))
            .collect();
        prefixes.push(format!("research/sources/{}/", binding.source_sha));
        let inventory = Inventory {
            schema: 1,
            repository: binding.repository.clone(),
            product: product.into(),
            source: SourceArchive {
                schema: 1,
                code_commit: binding.source_sha.clone(),
                archive: Artifact {
                    key: format!("research/sources/{}/source.tar", binding.source_sha),
                    sha256: "b".repeat(64),
                    bytes: 8192,
                },
            },
            software_producer: ReleaseProducer {
                repository: binding.repository.clone(),
                source_sha: binding.source_sha.clone(),
                run_id: 1,
                run_attempt: 2,
                job_id: 4,
                workflow_path: ".github/workflows/ploy-ci.yml".into(),
            },
            publisher_prefixes: prefixes,
            program_objects: (0..programs)
                .map(|n| Artifact {
                    key: format!("research/builds/{:064x}/program{n}", n % builds + 1),
                    sha256: "c".repeat(64),
                    bytes: 8192,
                })
                .collect(),
            build_count: builds,
        };
        let mut inventory = inventory;
        inventory.program_objects.sort_by(|a, b| a.key.cmp(&b.key));
        let limits = inventory.publication_limits().unwrap();
        let envelope = Envelope {
            schema: "monday.oss-publication-budget.v1".into(),
            repository: binding.repository.clone(),
            source_sha: binding.source_sha.clone(),
            publisher_run_id: 2,
            publisher_run_attempt: 1,
            expires_at_ms: now().unwrap() + 60_000,
            publication_namespaces: NAMESPACES.map(str::to_owned).to_vec(),
            limits,
            allocations: vec![Allocation {
                product: product.into(),
                limits,
            }],
        };
        (binding, inventory, envelope)
    }
    fn directory() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        Ok(dir)
    }
    fn ledger(dir: &Path) -> Result<Ledger> {
        let (b, i, e) = fixture("controller", 1, 1);
        Ledger::create(&dir.canonicalize()?.join("ledger"), e, b, i, "d".repeat(64))
    }
    fn exchange(l: &Ledger, p: Phase) -> Result<()> {
        let (begin, done) = if p == Phase::Source {
            ("source_exchange", "source_ready")
        } else {
            ("publish_exchange", "publisher_ready")
        };
        l.claim(begin)?;
        l.reserve(p, Service::Oidc, "oidc", 0, CONTROL_RESPONSE_LIMIT)?;
        l.reserve(p, Service::Sts, "sts", 64, CONTROL_RESPONSE_LIMIT)?;
        l.claim(done)
    }
    fn ready(l: &Ledger) -> Result<()> {
        exchange(l, Phase::Source)?;
        l.claim("preflight")?;
        l.reserve(Phase::Source, Service::OssRead, "", 0, 4096)?;
        l.reserve(
            Phase::Source,
            Service::OssRead,
            &format!("research/sources/{}/source.tar", "a".repeat(40)),
            0,
            0,
        )?;
        l.claim("preflight_ready")?;
        l.require_publish_funding()?;
        exchange(l, Phase::Publish)?;
        l.claim("publication")
    }
    #[test]
    fn budget_actual_catalogue_counts_and_aggregate_are_finite() -> Result<()> {
        let mut aggregate = Limits::default();
        let mut alloc = Vec::new();
        for (p, b, e, calls) in [
            ("cex-runner", 4, 7, 66),
            ("controller", 3, 5, 51),
            ("prediction-runner", 3, 5, 51),
        ] {
            let (binding, inventory, envelope) = fixture(p, b, e);
            inventory.validate(&binding)?;
            let limits = inventory.publication_limits()?;
            assert_eq!(limits.requests, calls);
            aggregate = aggregate.checked_add(limits)?;
            alloc.push(Allocation {
                product: p.into(),
                limits,
            });
            assert_eq!(envelope.validate(&binding, now()?)?, limits);
        }
        let (binding, _, mut envelope) = fixture("controller", 1, 1);
        envelope.allocations = alloc;
        envelope.limits = aggregate;
        assert_eq!(aggregate.requests, 168);
        envelope.validate(&binding, now()?)?;
        envelope.limits.requests -= 1;
        assert!(envelope.validate(&binding, now()?).is_err());
        Ok(())
    }
    #[test]
    fn budget_schema_denies_foreign_identity_scope_expiry_overflow_and_replay() -> Result<()> {
        let (binding, _, envelope) = fixture("controller", 1, 1);
        let original = serde_json::to_value(&envelope)?;
        for (name, bad) in [
            ("schema", serde_json::json!("v2")),
            ("repository", serde_json::json!("foreign/repo")),
            ("source_sha", serde_json::json!("b".repeat(40))),
            ("publisher_run_id", serde_json::json!(3)),
            ("publisher_run_attempt", serde_json::json!(2)),
            ("expires_at_ms", serde_json::json!(0)),
            ("publication_namespaces", serde_json::json!(["research/"])),
            (
                "allocations",
                serde_json::json!({"controller":envelope.limits}),
            ),
        ] {
            let mut value = original.clone();
            value[name] = bad;
            assert!(
                serde_json::from_value::<Envelope>(value)
                    .and_then(|e| e
                        .validate(&binding, now().unwrap())
                        .map_err(serde::de::Error::custom))
                    .is_err(),
                "accepted {name}"
            );
        }
        for name in ["unknown", "expires_at_ms"] {
            let mut value = original.clone();
            if name == "unknown" {
                value[name] = serde_json::json!(1)
            } else {
                value.as_object_mut().unwrap().remove(name);
            }
            assert!(serde_json::from_value::<Envelope>(value).is_err());
        }
        let mut bad = envelope.clone();
        bad.allocations.push(bad.allocations[0].clone());
        assert!(bad.validate(&binding, now()?).is_err());
        bad.allocations[0].product = "cex-runner".into();
        bad.allocations[0].limits.requests = u64::MAX;
        assert!(bad.validate(&binding, now()?).is_err());
        bad = envelope.clone();
        bad.expires_at_ms = 1u64 << 53;
        assert!(bad.validate(&binding, now()?).is_err());
        let mut foreign = binding.clone();
        foreign.publisher_job_id = 0;
        assert!(envelope.validate(&foreign, now()?).is_err());
        assert!(Limits {
            requests: u64::MAX,
            ..Limits::default()
        }
        .checked_add(Limits {
            requests: 1,
            ..Limits::default()
        })
        .is_err());
        Ok(())
    }
    #[test]
    fn budget_inventory_and_underfunding_deny_before_creation() -> Result<()> {
        let dir = directory()?;
        let path = dir.path().canonicalize()?.join("ledger");
        let (binding, inventory, envelope) = fixture("controller", 1, 1);
        for dimension in 0..3 {
            let mut e = envelope.clone();
            let l = &mut e.allocations[0].limits;
            match dimension {
                0 => l.requests -= 1,
                1 => l.request_payload_bytes -= 1,
                _ => l.response_payload_bytes -= 1,
            }
            assert!(
                Ledger::create(&path, e, binding.clone(), inventory.clone(), "d".repeat(64))
                    .is_err()
            );
            assert!(!path.exists());
        }
        let mut bad = inventory.clone();
        bad.program_objects[0].key = "research/builds/foreign/program".into();
        assert!(bad.validate(&binding).is_err());
        bad = inventory.clone();
        bad.software_producer.workflow_path = ".github/workflows/foreign.yml".into();
        assert!(bad.validate(&binding).is_err());
        bad = inventory.clone();
        bad.program_objects[0].bytes = 0;
        assert!(bad.validate(&binding).is_err());
        let l = Ledger::create(
            &path,
            envelope,
            binding.clone(),
            inventory.clone(),
            "d".repeat(64),
        )?;
        bad = inventory;
        bad.program_objects[0].sha256 = "e".repeat(64);
        assert!(l.require_inventory(&bad).is_err());
        let mut other = binding;
        other.publisher_job_id += 1;
        assert!(l.require_binding(&other, &"d".repeat(64)).is_err());
        assert!(l.require_binding(&other, &"e".repeat(64)).is_err());
        Ok(())
    }
    #[test]
    fn budget_reservations_survive_restart_and_failed_or_interrupted_send() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        let reference = l.reference();
        l.claim("source_exchange")?;
        l.reserve(
            Phase::Source,
            Service::Oidc,
            "oidc",
            0,
            CONTROL_RESPONSE_LIMIT,
        )?;
        drop(l); // No send or completion: reservation remains burned.
        let resumed = Ledger::from_reference(&reference)?;
        assert_eq!(resumed.status()?.reserved.requests, 1);
        assert!(resumed.claim("source_exchange").is_err());
        assert!(resumed
            .reserve(
                Phase::Source,
                Service::Oidc,
                "oidc",
                0,
                CONTROL_RESPONSE_LIMIT
            )
            .is_err());
        assert!(resumed.claim("preflight").is_err());
        assert!(ledger(dir.path()).is_err());
        assert_eq!(
            Ledger::from_path(&reference.ledger_path)?
                .status()?
                .services
                .oidc,
            1
        );
        let summary = serde_json::to_string(&resumed.status()?)?;
        for forbidden in [
            "ledger_path",
            "policy_sha256",
            "image_repository",
            "binding_sha256",
            "device",
            "inode",
            "access_key",
            "security_token",
        ] {
            assert!(!summary.contains(forbidden), "summary leaked {forbidden}");
        }
        Ok(())
    }
    #[test]
    fn budget_phase_order_foreign_scope_and_exhaustion_fail_closed() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        assert!(l
            .reserve(Phase::Source, Service::Oidc, "oidc", 0, 64)
            .is_err());
        exchange(&l, Phase::Source)?;
        l.claim("preflight")?;
        assert!(l
            .reserve(Phase::Source, Service::OssWrite, "", 1, 0)
            .is_err());
        assert!(l
            .reserve(
                Phase::Source,
                Service::OssRead,
                "research/sources/foreign/source.tar",
                0,
                1
            )
            .is_err());
        l.reserve(Phase::Source, Service::OssRead, "", 0, 4096)?;
        l.reserve(
            Phase::Source,
            Service::OssRead,
            &format!("research/sources/{}/source.tar", "a".repeat(40)),
            0,
            0,
        )?;
        assert!(l
            .reserve(Phase::Source, Service::OssRead, "", 0, 4096)
            .is_err());
        l.claim("preflight_ready")?;
        exchange(&l, Phase::Publish)?;
        l.claim("publication")?;
        let key = format!("research/builds/{:064x}/program0", 1);
        assert!(l
            .reserve(
                Phase::Publish,
                Service::OssRead,
                "research/builds/foreign/program",
                0,
                0
            )
            .is_err());
        let limit = l.status()?.limits.requests;
        while l.status()?.reserved.requests < limit {
            l.reserve(Phase::Publish, Service::OssRead, &key, 0, 0)?;
        }
        assert!(l
            .reserve(Phase::Publish, Service::OssRead, &key, 0, 0)
            .is_err());
        assert_eq!(l.status()?.reserved.requests, limit);
        l.claim("publication_ready")?;
        assert!(l
            .reserve(Phase::Publish, Service::OssRead, &key, 0, 0)
            .is_err());
        Ok(())
    }
    #[test]
    fn budget_payload_dimensions_cannot_be_bypassed_by_small_request_count() -> Result<()> {
        for dimension in 0..2 {
            let dir = directory()?;
            let l = ledger(dir.path())?;
            ready(&l)?;
            let status = l.status()?;
            let request = if dimension == 0 {
                status.limits.request_payload_bytes - status.reserved.request_payload_bytes + 1
            } else {
                0
            };
            let response = if dimension == 1 {
                status.limits.response_payload_bytes - status.reserved.response_payload_bytes + 1
            } else {
                0
            };
            assert!(l
                .reserve(
                    Phase::Publish,
                    Service::OssWrite,
                    &format!("research/builds/{:064x}/program0", 1),
                    request,
                    response
                )
                .is_err());
            assert_eq!(l.status()?.reserved.requests, status.reserved.requests);
        }
        Ok(())
    }
    #[test]
    fn budget_expired_ledger_still_reports_history_but_cannot_charge_or_claim() -> Result<()> {
        let dir = directory()?;
        let (b, i, mut e) = fixture("controller", 1, 1);
        e.expires_at_ms = now()? + 1000;
        let ready = Ledger::create(
            &dir.path().canonicalize()?.join("ready"),
            e.clone(),
            b.clone(),
            i.clone(),
            "d".repeat(64),
        )?;
        exchange(&ready, Phase::Source)?;
        let l = Ledger::create(
            &dir.path().canonicalize()?.join("ledger"),
            e,
            b.clone(),
            i,
            "d".repeat(64),
        )?;
        l.claim("source_exchange")?;
        l.reserve(Phase::Source, Service::Oidc, "oidc", 0, 64)?;
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(l.status()?.reserved.requests, 1);
        // All prerequisites exist; only expiry prevents this new claim.
        assert!(ready.claim("preflight").is_err());
        assert!(l
            .reserve(Phase::Source, Service::Sts, "sts", 64, 64)
            .is_err());
        assert!(l.require_binding(&b, &"d".repeat(64)).is_err());
        assert_eq!(
            Ledger::from_path(&l.reference().ledger_path)?
                .status()?
                .reserved
                .requests,
            1
        );
        Ok(())
    }
    #[test]
    fn budget_private_ledger_rejects_symlink_hardlink_replacement_and_torn_writes() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        let r = l.reference();
        let linked = dir.path().join("link");
        std::os::unix::fs::symlink(&r.ledger_path, &linked)?;
        assert!(Ledger::from_path(&linked).is_err());
        std::fs::remove_file(&linked)?;
        std::fs::hard_link(&r.ledger_path, &linked)?;
        assert!(Ledger::from_reference(&r).is_err());
        std::fs::remove_file(&linked)?;
        l.claim("source_exchange")?;
        let original = std::fs::read(&r.ledger_path)?;
        std::fs::write(&r.ledger_path, &original[..original.len() - 1])?;
        assert!(Ledger::from_path(&r.ledger_path).is_err());
        std::fs::write(&r.ledger_path, &original)?;
        let other = dir.path().join("replacement");
        std::fs::write(&other, &original)?;
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(other, &r.ledger_path)?;
        assert!(Ledger::from_reference(&r).is_err());
        Ok(())
    }
    #[test]
    fn budget_missing_journal_cannot_reinitialize_the_same_allocation() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        let path = l.reference().ledger_path;
        std::fs::remove_file(&path)?;
        assert!(Ledger::from_path(&path).is_err());
        assert!(ledger(dir.path()).is_err());
        assert!(!path.exists());
        assert!(initialization_path(&path).exists());
        Ok(())
    }
    #[test]
    fn budget_complete_record_removal_and_interrupted_append_cannot_refund() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        let path = l.reference().ledger_path;
        l.claim("source_exchange")?;
        let before = std::fs::read(&path)?;
        l.reserve(Phase::Source, Service::Oidc, "oidc", 0, 64)?;
        let after = std::fs::read(&path)?;
        std::fs::write(&path, &before)?; // Entire last line lost; still valid JSONL.
        assert!(Ledger::from_path(&path).is_err());
        assert!(l
            .reserve(Phase::Source, Service::Oidc, "oidc", 0, 64)
            .is_err());
        assert!(ledger(dir.path()).is_err());
        std::fs::write(&path, &after)?;
        assert_eq!(l.status()?.reserved.requests, 1);
        // Simulate the durable intent completing before a crashed journal append.
        let mut marker = std::fs::OpenOptions::new()
            .append(true)
            .open(initialization_path(&path))?;
        marker.write_all(format!("{}\n", "e".repeat(64)).as_bytes())?;
        marker.sync_all()?;
        assert!(Ledger::from_path(&path).is_err());
        Ok(())
    }
    #[test]
    fn budget_record_tampering_and_public_permissions_are_rejected() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        l.claim("source_exchange")?;
        let path = l.reference().ledger_path;
        let data = String::from_utf8(std::fs::read(&path)?)?;
        std::fs::write(&path, data.replace("\"sequence\":1", "\"sequence\":2"))?;
        assert!(Ledger::from_path(&path).is_err());
        std::fs::write(&path, &data)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(Ledger::from_path(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;
        assert!(Ledger::from_path(&path).is_err());
        Ok(())
    }
    #[test]
    #[ignore = "subprocess fixture; launched by budget_cross_process_reservation_is_single_use"]
    fn budget_child_reserve() -> Result<()> {
        let path = PathBuf::from(std::env::var("MONDAY_TEST_BUDGET_LEDGER")?);
        Ledger::from_path(&path)?.reserve(Phase::Source, Service::Oidc, "oidc", 0, 64)
    }
    #[test]
    fn budget_cross_process_reservation_is_single_use() -> Result<()> {
        let dir = directory()?;
        let l = ledger(dir.path())?;
        l.claim("source_exchange")?;
        let command = || {
            let mut c = std::process::Command::new(std::env::current_exe().unwrap());
            c.args([
                "--exact",
                "release_budget::tests::budget_child_reserve",
                "--ignored",
            ])
            .env("MONDAY_TEST_BUDGET_LEDGER", l.reference().ledger_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
            c
        };
        let mut a = command().spawn()?;
        let mut b = command().spawn()?;
        assert_ne!(a.wait()?.success(), b.wait()?.success());
        assert_eq!(l.status()?.reserved.requests, 1);
        Ok(())
    }
}
