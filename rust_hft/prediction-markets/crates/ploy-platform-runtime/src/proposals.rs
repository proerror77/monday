use crate::next_proposal_id;
use chrono::Utc;
use ploy_operator_contracts::{
    ProposalActionKind, ProposalCreateRequest, ProposalDecisionRequest, ProposalStatus,
    SafetyProposal,
};
use rust_decimal::Decimal;
use std::io;

#[derive(Debug, Default, Clone)]
pub struct ProposalStore {
    proposals: Vec<SafetyProposal>,
}

#[derive(Debug, Clone)]
pub struct ProposalExecutionPlan {
    pub proposal_id: String,
    pub action_kind: ProposalActionKind,
    pub target_deployment_id: String,
    pub proposed_max_gross_exposure: Option<Decimal>,
    pub decision_note: Option<String>,
}

impl ProposalStore {
    #[must_use]
    pub fn all(&self) -> Vec<SafetyProposal> {
        self.proposals.clone()
    }

    pub fn replace(&mut self, proposals: Vec<SafetyProposal>) {
        self.proposals = proposals;
    }

    pub fn create(&mut self, request: ProposalCreateRequest) -> io::Result<SafetyProposal> {
        if matches!(request.action_kind, ProposalActionKind::ReduceMaxExposure)
            && request.proposed_max_gross_exposure.is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reduce_max_exposure proposals require proposed_max_gross_exposure",
            ));
        }

        let proposal = SafetyProposal {
            proposal_id: next_proposal_id(&request.target_deployment_id),
            action_kind: request.action_kind,
            target_deployment_id: request.target_deployment_id,
            status: ProposalStatus::Pending,
            rationale: request.rationale,
            evidence: request.evidence,
            source_run_id: request.source_run_id,
            proposed_max_gross_exposure: request.proposed_max_gross_exposure,
            created_at: Utc::now(),
            decided_at: None,
            decision_note: None,
        };
        self.proposals.push(proposal.clone());
        Ok(proposal)
    }

    pub fn prepare_approval(
        &self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<ProposalExecutionPlan>> {
        let Some(proposal) = self
            .proposals
            .iter()
            .find(|proposal| proposal.proposal_id == proposal_id)
        else {
            return Ok(None);
        };

        if proposal.status != ProposalStatus::Pending {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("proposal `{proposal_id}` is no longer pending"),
            ));
        }

        Ok(Some(ProposalExecutionPlan {
            proposal_id: proposal.proposal_id.clone(),
            action_kind: proposal.action_kind,
            target_deployment_id: proposal.target_deployment_id.clone(),
            proposed_max_gross_exposure: proposal.proposed_max_gross_exposure,
            decision_note: request
                .decision_note
                .or_else(|| Some("approved by operator".to_string())),
        }))
    }

    pub fn mark_approved(
        &mut self,
        proposal_id: &str,
        decision_note: Option<String>,
    ) -> io::Result<SafetyProposal> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|proposal| proposal.proposal_id == proposal_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("proposal `{proposal_id}` was not found"),
                )
            })?;
        proposal.status = ProposalStatus::Approved;
        proposal.decided_at = Some(Utc::now());
        proposal.decision_note = decision_note;
        Ok(proposal.clone())
    }

    pub fn mark_failed(&mut self, proposal_id: &str, error: &io::Error) -> io::Result<()> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|proposal| proposal.proposal_id == proposal_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("proposal `{proposal_id}` was not found"),
                )
            })?;
        proposal.status = ProposalStatus::Failed;
        proposal.decided_at = Some(Utc::now());
        proposal.decision_note = Some(error.to_string());
        Ok(())
    }

    pub fn reject(
        &mut self,
        proposal_id: &str,
        request: ProposalDecisionRequest,
    ) -> io::Result<Option<SafetyProposal>> {
        let Some(proposal) = self
            .proposals
            .iter_mut()
            .find(|proposal| proposal.proposal_id == proposal_id)
        else {
            return Ok(None);
        };

        if proposal.status != ProposalStatus::Pending {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("proposal `{proposal_id}` is no longer pending"),
            ));
        }

        proposal.status = ProposalStatus::Rejected;
        proposal.decided_at = Some(Utc::now());
        proposal.decision_note = request
            .decision_note
            .or_else(|| Some("rejected by operator".to_string()));
        Ok(Some(proposal.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::ProposalStore;
    use ploy_operator_contracts::{
        ProposalActionKind, ProposalCreateRequest, ProposalDecisionRequest, ProposalStatus,
    };
    use rust_decimal::Decimal;

    #[test]
    fn create_and_reject_proposal() {
        let mut store = ProposalStore::default();
        let proposal = store
            .create(ProposalCreateRequest {
                action_kind: ProposalActionKind::PauseDeployment,
                target_deployment_id: "example.paper".to_string(),
                rationale: "drift".to_string(),
                evidence: vec!["drawdown".to_string()],
                source_run_id: Some("run-1".to_string()),
                proposed_max_gross_exposure: None,
            })
            .expect("create");

        let rejected = store
            .reject(
                &proposal.proposal_id,
                ProposalDecisionRequest {
                    decision_note: Some("no".to_string()),
                },
            )
            .expect("reject")
            .expect("proposal");

        assert_eq!(rejected.status, ProposalStatus::Rejected);
    }

    #[test]
    fn prepare_and_approve_reduce_exposure_proposal() {
        let mut store = ProposalStore::default();
        let proposal = store
            .create(ProposalCreateRequest {
                action_kind: ProposalActionKind::ReduceMaxExposure,
                target_deployment_id: "example.live".to_string(),
                rationale: "cap".to_string(),
                evidence: vec![],
                source_run_id: None,
                proposed_max_gross_exposure: Some(Decimal::new(500, 2)),
            })
            .expect("create");

        let plan = store
            .prepare_approval(
                &proposal.proposal_id,
                ProposalDecisionRequest {
                    decision_note: None,
                },
            )
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.action_kind, ProposalActionKind::ReduceMaxExposure);

        let approved = store
            .mark_approved(&proposal.proposal_id, plan.decision_note)
            .expect("approve");
        assert_eq!(approved.status, ProposalStatus::Approved);
    }
}
