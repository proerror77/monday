//! A fixed configuration consumer, not an adopter, grant issuer or model runner.
use anyhow::{ensure, Result};
use hft_research_agent_contracts::{
    canonical_bytes,
    consumption::{
        ConfigurationConsumptionReceiptV1, ConsumedResearcherConfigurationV1,
        ResearcherConsumptionConfigV1, ResearcherTaskContextV1,
    },
    content_sha256, ExperienceRankingOrder, CONTRACT_SCHEMA_V1,
};
use sha2::{Digest, Sha256};

/// Consume bounded canonical actual bytes addressed by the original Run config.
/// The expected SHA is a caller binding, not an adoption or authorization proof.
pub fn consume_config(
    actual_canonical_bytes: &[u8],
    expected_configuration_sha256: &str,
) -> Result<ConsumedResearcherConfigurationV1> {
    ensure!(
        !actual_canonical_bytes.is_empty() && actual_canonical_bytes.len() <= 2 * 1024 * 1024,
        "configuration bytes are absent or unbounded"
    );
    ensure!(
        format!("{:x}", Sha256::digest(actual_canonical_bytes)) == expected_configuration_sha256,
        "actual configuration bytes differ from expected Run config hash"
    );
    let config: ResearcherConsumptionConfigV1 = serde_json::from_slice(actual_canonical_bytes)?;
    ensure!(
        config.canonical_bytes()? == actual_canonical_bytes,
        "configuration is not its canonical immutable encoding"
    );
    let retrieval = &config.version.snapshot.retrieval;
    let mut experiences = config
        .corpora
        .iter()
        .flat_map(|corpus| corpus.entries.iter())
        .collect::<Vec<_>>();
    experiences.sort_unstable_by(|left, right| {
        let left_match = left.task_context_sha256 == config.query.task_context_sha256;
        let right_match = right.task_context_sha256 == config.query.task_context_sha256;
        let matching = right_match.cmp(&left_match);
        let recency = right.available_ns.cmp(&left.available_ns);
        match retrieval.ranking {
            ExperienceRankingOrder::TaskMatchThenRecency => matching.then(recency),
            ExperienceRankingOrder::RecencyThenTaskMatch => recency.then(matching),
        }
        .then_with(|| left.id.cmp(&right.id))
    });
    experiences.truncate(retrieval.top_k as usize);
    let context = ResearcherTaskContextV1 {
        prompt_text: config.version.snapshot.prompt_text.clone(),
        search_policy: config.version.snapshot.search_policy.clone(),
        query: config.query.clone(),
        query_template: retrieval.query_template.clone(),
        ordered_experiences: experiences.into_iter().cloned().collect(),
    };
    let context_bytes = canonical_bytes(&context)?;
    ensure!(
        context_bytes.len() as u64 <= retrieval.max_context_bytes,
        "complete task context exceeds frozen byte cap; truncation is forbidden"
    );
    let receipt = ConfigurationConsumptionReceiptV1 {
        schema: CONTRACT_SCHEMA_V1,
        configuration_sha256: config.id()?,
        researcher_version_sha256: config.version.id()?,
        query_sha256: config.query.id()?,
        corpus: config
            .corpora
            .iter()
            .map(|corpus| corpus.content_reference())
            .collect::<Result<Vec<_>>>()?,
        selected_experience_ids: context
            .ordered_experiences
            .iter()
            .map(|experience| experience.id.clone())
            .collect(),
        context_sha256: content_sha256(&context)?,
        context_bytes: context_bytes.len() as u64,
    };
    Ok(ConsumedResearcherConfigurationV1 { context, receipt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_research_agent_contracts::{
        consumption::{
            ExperienceEvidenceV1, ExperienceQueryV1, FrozenExperienceCorpusV1, ResearchExperienceV1,
        },
        ContentReferenceV1, ExperienceCorpusReferenceV1, ExperienceRetrievalV1,
        InformationPolicyV1, MetaTaskPhase, RankingChangeProposalV1, ResearcherChangeEvidenceV1,
        ResearcherSnapshotV1, ResearcherVersionV1,
    };

    const EPOCH: i64 = 1_700_000_000_000_000_000;
    fn hash(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn example() -> ResearcherConsumptionConfigV1 {
        let evidence = |id: &str, available_ns| ExperienceEvidenceV1 {
            content: ContentReferenceV1 {
                id: id.into(),
                content_sha256: content_sha256(&id).unwrap(),
            },
            phase: MetaTaskPhase::Development,
            data_view_sha256: hash('d'),
            available_ns,
        };
        let entry = |id: &str, task: char, time| ResearchExperienceV1 {
            id: id.into(),
            task_context_sha256: hash(task),
            observed_ns: time - 1,
            available_ns: time,
            text: format!("Actual development observation {id}"),
            source: evidence(&format!("source:{id}"), time - 2),
            evidence: vec![evidence(&format!("evidence:{id}"), time - 2)],
        };
        let corpus = FrozenExperienceCorpusV1 {
            id: "frozen-development-corpus".into(),
            entries: vec![
                entry("old-matching", 'a', EPOCH + 10),
                entry("new-unmatched", 'b', EPOCH + 20),
            ],
        };
        let version = ResearcherVersionV1 {
            schema: 1,
            parent_version_sha256: None,
            change_evidence: None,
            snapshot: ResearcherSnapshotV1 {
                prompt_text: "Use only the permitted actual development evidence.".into(),
                search_policy: serde_json::json!({"candidate_limit":4,"method":"fixed"}),
                retrieval: ExperienceRetrievalV1 {
                    ranking: ExperienceRankingOrder::TaskMatchThenRecency,
                    corpus: vec![ExperienceCorpusReferenceV1 {
                        content: corpus.content_reference().unwrap(),
                        feedback_phase: MetaTaskPhase::Development,
                    }],
                    query_template: "Match the actual task before comparing recency.".into(),
                    top_k: 1,
                    max_context_bytes: 8192,
                    information_policy: InformationPolicyV1 {
                        schema: 1,
                        proposal_feedback_phases: vec![MetaTaskPhase::Development],
                        allowed_data_view_sha256: vec![hash('d')],
                    },
                },
                source_commit: "a".repeat(40),
                build_sha256: hash('c'),
                tools: vec![ContentReferenceV1 {
                    id: "original-tool".into(),
                    content_sha256: hash('e'),
                }],
            },
        };
        let config = ResearcherConsumptionConfigV1 {
            schema: 1,
            version,
            query: ExperienceQueryV1 {
                task_context_sha256: hash('a'),
                data_view_sha256: hash('d'),
                as_of_ns: EPOCH + 100,
                query_text: "Compare sources for this fixed task.".into(),
            },
            corpora: vec![corpus],
        };
        config.validate().unwrap();
        config
    }

    fn actual(config: &ResearcherConsumptionConfigV1) -> Result<ConsumedResearcherConfigurationV1> {
        consume_config(&config.canonical_bytes()?, &config.id()?)
    }

    fn encode_declaration(config: &ResearcherConsumptionConfigV1) -> (Vec<u8>, String) {
        let bytes = canonical_bytes(config).unwrap();
        let id = format!("{:x}", Sha256::digest(&bytes));
        (bytes, id)
    }

    fn refresh_actual_corpus(config: &mut ResearcherConsumptionConfigV1) {
        for (actual, frozen) in config
            .corpora
            .iter()
            .zip(&mut config.version.snapshot.retrieval.corpus)
        {
            frozen.content = actual.content_reference().unwrap();
        }
    }

    #[test]
    fn actual_entry_consumption_changes_top_one_and_complete_context_for_ranking_only_version() {
        let incumbent = example();
        let before = actual(&incumbent).unwrap();
        assert_eq!(before.receipt.selected_experience_ids, ["old-matching"]);
        let mut challenger = incumbent.clone();
        challenger.version.parent_version_sha256 = Some(incumbent.version.id().unwrap());
        challenger.version.change_evidence = Some(ResearcherChangeEvidenceV1 {
            content: incumbent.corpora[0].entries[0].evidence[0].content.clone(),
            phase: MetaTaskPhase::Development,
        });
        challenger.version.snapshot.retrieval.ranking =
            ExperienceRankingOrder::RecencyThenTaskMatch;
        RankingChangeProposalV1 {
            schema: 1,
            incumbent_version_sha256: incumbent.version.id().unwrap(),
            challenger: challenger.version.clone(),
        }
        .validate_against(&incumbent.version)
        .unwrap();
        let after = actual(&challenger).unwrap();
        assert_eq!(after.receipt.selected_experience_ids, ["new-unmatched"]);
        assert_eq!(before.receipt.corpus, after.receipt.corpus);
        assert_eq!(before.receipt.query_sha256, after.receipt.query_sha256);
        assert_eq!(
            incumbent.version.snapshot.build_sha256,
            challenger.version.snapshot.build_sha256
        );
        assert_ne!(
            before.receipt.configuration_sha256,
            after.receipt.configuration_sha256
        );
        assert_ne!(before.receipt.context_sha256, after.receipt.context_sha256);
        assert_eq!(
            after.context.prompt_text,
            challenger.version.snapshot.prompt_text
        );
        assert_eq!(
            after.context.search_policy,
            challenger.version.snapshot.search_policy
        );
        assert_eq!(
            after.context.ordered_experiences[0],
            challenger.corpora[0].entries[1]
        );
        assert_eq!(
            after.receipt.context_bytes,
            canonical_bytes(&after.context).unwrap().len() as u64
        );
    }

    #[test]
    fn canonically_rebound_hidden_future_or_foreign_evidence_is_rejected_even_below_top_k() {
        let original = example();
        actual(&original).unwrap();
        for fault in [
            "hidden_source",
            "hidden_evidence",
            "foreign_view",
            "future_entry",
            "future_source",
            "future_evidence",
            "observed_after_available",
            "source_after_entry",
            "duplicate_entry",
            "conflicting_source",
            "empty_evidence",
            "negative_clock",
        ] {
            let mut changed = original.clone();
            let entry = &mut changed.corpora[0].entries[1];
            match fault {
                "hidden_source" => entry.source.phase = MetaTaskPhase::Selection,
                "hidden_evidence" => entry.evidence[0].phase = MetaTaskPhase::Certification,
                "foreign_view" => entry.evidence[0].data_view_sha256 = hash('f'),
                "future_entry" => entry.available_ns = changed.query.as_of_ns + 1,
                "future_source" => entry.source.available_ns = changed.query.as_of_ns + 1,
                "future_evidence" => entry.evidence[0].available_ns = changed.query.as_of_ns + 1,
                "observed_after_available" => entry.observed_ns = entry.available_ns + 1,
                "source_after_entry" => entry.source.available_ns = entry.available_ns + 1,
                "duplicate_entry" => entry.id = "old-matching".into(),
                "conflicting_source" => {
                    entry.evidence[0].content.id = entry.source.content.id.clone();
                    entry.evidence[0].content.content_sha256 = hash('f');
                }
                "empty_evidence" => entry.evidence.clear(),
                "negative_clock" => entry.observed_ns = -1,
                _ => unreachable!(),
            }
            refresh_actual_corpus(&mut changed);
            let (bytes, id) = encode_declaration(&changed);
            assert!(consume_config(&bytes, &id).is_err(), "{fault}");
        }
        let mut changed = original.clone();
        changed.query.data_view_sha256 = hash('f');
        let (bytes, id) = encode_declaration(&changed);
        assert!(consume_config(&bytes, &id).is_err());
    }

    #[test]
    fn actual_bytes_full_corpus_and_complete_context_caps_cannot_be_laundered() {
        let original = example();
        let output = actual(&original).unwrap();
        let bytes = original.canonical_bytes().unwrap();
        assert!(consume_config(&bytes, &hash('f')).is_err());
        let pretty = serde_json::to_vec_pretty(&original).unwrap();
        let pretty_id = format!("{:x}", Sha256::digest(&pretty));
        assert!(consume_config(&pretty, &pretty_id).is_err());
        let mut changed = original.clone();
        changed.corpora[0].entries[1].text.push('!');
        let (bytes, id) = encode_declaration(&changed);
        assert!(consume_config(&bytes, &id).is_err());
        changed = original.clone();
        changed.version.snapshot.retrieval.max_context_bytes = output.receipt.context_bytes - 1;
        let (bytes, id) = encode_declaration(&changed);
        assert!(consume_config(&bytes, &id).is_err());
        changed = original.clone();
        changed.version.snapshot.retrieval.max_context_bytes = output.receipt.context_bytes;
        actual(&changed).unwrap();
        let mut forged = serde_json::to_value(&original).unwrap();
        forged["adopted"] = serde_json::json!(true);
        let bytes = canonical_bytes(&forged).unwrap();
        let id = format!("{:x}", Sha256::digest(&bytes));
        assert!(consume_config(&bytes, &id).is_err());
    }

    #[test]
    fn fixed_query_and_cross_corpus_identity_are_bound_with_deterministic_ties() {
        let original = example();
        let before = actual(&original).unwrap();
        let mut changed = original.clone();
        changed.query.query_text.push('!');
        let after = actual(&changed).unwrap();
        assert_ne!(
            before.receipt.configuration_sha256,
            after.receipt.configuration_sha256
        );
        assert_ne!(before.receipt.context_sha256, after.receipt.context_sha256);
        changed = original.clone();
        let mut second = changed.corpora[0].clone();
        second.id = "second-corpus".into();
        changed
            .version
            .snapshot
            .retrieval
            .corpus
            .push(ExperienceCorpusReferenceV1 {
                content: second.content_reference().unwrap(),
                feedback_phase: MetaTaskPhase::Development,
            });
        changed.corpora.push(second);
        let (bytes, id) = encode_declaration(&changed);
        assert!(consume_config(&bytes, &id).is_err());
        changed = original.clone();
        changed.version.snapshot.retrieval.top_k = 2;
        changed.corpora[0].entries[1].task_context_sha256 =
            changed.query.task_context_sha256.clone();
        changed.corpora[0].entries[1].available_ns = changed.corpora[0].entries[0].available_ns;
        changed.corpora[0].entries[1].observed_ns = changed.corpora[0].entries[0].observed_ns;
        changed.corpora[0].entries[1].source.available_ns =
            changed.corpora[0].entries[0].source.available_ns;
        changed.corpora[0].entries[1].evidence[0].available_ns =
            changed.corpora[0].entries[0].evidence[0].available_ns;
        refresh_actual_corpus(&mut changed);
        let first = actual(&changed).unwrap();
        changed.corpora[0].entries.reverse();
        refresh_actual_corpus(&mut changed);
        let second = actual(&changed).unwrap();
        assert_eq!(
            first.receipt.selected_experience_ids,
            ["new-unmatched", "old-matching"]
        );
        assert_eq!(first.receipt.context_sha256, second.receipt.context_sha256);
    }
}
