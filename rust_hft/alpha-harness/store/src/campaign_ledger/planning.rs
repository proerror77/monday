//! Current, read-only planning permission from the original authenticated ledger.
//! Inspection never registers a grant, reserves an Attempt, or changes a budget.
use super::*;
use alpha_domain::campaign_control::CampaignExecutionScope;

/// A process-local borrow, not a transferable credential or a restored JSON view.
pub struct VerifiedCampaignPlanningPermission<'a> {
    store: &'a AlphaStore,
    root: VerifiedCampaignRootGrant,
}

impl VerifiedCampaignPlanningPermission<'_> {
    pub fn root(&self) -> &VerifiedCampaignRootGrant {
        &self.root
    }

    /// Current authenticated Study signer, when this family has a Study. This
    /// is verification material, not another grant or a transferable permit.
    pub fn study_signer(
        &self,
    ) -> Result<Option<(String, ed25519_dalek::VerifyingKey)>, StoreError> {
        self.recheck()?;
        study::check_planning_member(
            &self.store.connection,
            &self.store.integrity_key,
            &self.root,
            Utc::now(),
        )
    }

    pub fn study_member(&self) -> Result<Option<CampaignStudyMemberV1>, StoreError> {
        self.recheck()?;
        let family = &self.root.grant().family.family_id;
        let Some(id) = self.store.campaign_study_id_for_family(family)? else {
            return Ok(None);
        };
        let signed = self
            .store
            .campaign_study_grant(&id)?
            .ok_or_else(|| err("planning Study authority is missing"))?;
        let member = signed
            .grant
            .members
            .into_iter()
            .find(|member| &member.family_id == family)
            .ok_or_else(|| err("planning Study member is missing"))?;
        Ok(Some(member))
    }

    /// Re-read current authenticated authority, rather than trusting the initial
    /// snapshot. Unrelated receipts do not invalidate an otherwise active root.
    pub fn recheck(&self) -> Result<(), StoreError> {
        self.recheck_at(Utc::now())
    }

    pub(super) fn recheck_at(&self, at: DateTime<Utc>) -> Result<(), StoreError> {
        check_planning_permission(self.store, &self.root, at)
    }
}

impl AlphaStore {
    pub fn inspect_campaign_planning_permission(
        &self,
        root: &VerifiedCampaignRootGrant,
    ) -> Result<VerifiedCampaignPlanningPermission<'_>, StoreError> {
        self.inspect_campaign_planning_permission_at(root, Utc::now())
    }

    pub(super) fn inspect_campaign_planning_permission_at(
        &self,
        root: &VerifiedCampaignRootGrant,
        at: DateTime<Utc>,
    ) -> Result<VerifiedCampaignPlanningPermission<'_>, StoreError> {
        check_planning_permission(self, root, at)?;
        Ok(VerifiedCampaignPlanningPermission {
            store: self,
            root: root.clone(),
        })
    }
}

fn check_planning_permission(
    store: &AlphaStore,
    verified: &VerifiedCampaignRootGrant,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    verified.validate_active_at(at).map_err(err)?;
    let grant = verified.grant();
    if grant.execution_scope != CampaignExecutionScope::PreHoldout {
        return Err(err("planning requires the original pre-holdout root"));
    }
    let (state, _) = load(
        &store.connection,
        &store.integrity_key,
        &grant.family.family_id,
    )?;
    if state.final_closure.is_some() {
        return Err(err(
            "family search is permanently closed for final evaluation",
        ));
    }
    let observed = state
        .roots
        .get(verified.content_sha256())
        .ok_or_else(|| err("planning root is not registered"))?;
    if observed.grant.signed_grant() != verified.signed_grant()
        || observed.grant.verifying_key() != verified.verifying_key()
        || observed.revoked_at.is_some_and(|when| at >= when)
    {
        return Err(err(
            "planning root differs from current registered authority",
        ));
    }
    validate_approval(&observed.approval, &observed.approval_hash, verified, at)?;
    if !read_effective_approval(
        &store.connection,
        &store.integrity_key,
        &observed.approval.approval_id,
    )?
    .is_active_at(at)
    {
        return Err(err("planning root approval is inactive"));
    }
    let root_usage = state.usage(Some(verified.content_sha256()))?;
    let family_usage = state.usage(None)?;
    if root_usage.accounted_trials()? >= grant.budget.max_trials
        || family_usage.accounted_trials()? >= grant.family.max_trials
        || root_usage.job_attempts >= grant.budget.max_job_attempts
        || root_usage.reserved_job_seconds >= grant.budget.max_job_seconds
    {
        return Err(err(
            "planning cannot reopen an exhausted root or family budget",
        ));
    }
    study::check_planning_member(&store.connection, &store.integrity_key, verified, at)?;
    Ok(())
}
