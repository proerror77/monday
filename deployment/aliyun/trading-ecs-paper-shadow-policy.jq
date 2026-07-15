def one_json_value:
  select(type == "array" and length == 1) | .[0];

def activation_intents($intents):
  [$intents[] | select(
    . == "StartPaper" or . == "StartShadow" or . == "StartLiveSmall"
  )];

one_json_value as $signed
| activation_intents($signed.envelope.allowed_intent_types) as $signed_starts
| activation_intents($policy[0].allowed_intent_types) as $policy_starts
| ([$signed.envelope.allowed_intent_types[]
    | select(. == "LoadFactor")]) as $signed_artifacts
| ($policy | length == 1) and
  ($signed | type == "object") and
  ($signed.envelope | type == "object") and
  ($signed.envelope.allowed_intent_types | type == "array") and
  ($policy[0] | type == "object") and
  ($policy[0].allowed_intent_types | type == "array") and
  ($policy[0].approvals | type == "array") and
  ($signed.envelope.allowed_intent_types | all(
    . == "LoadFactor" or
    . == "StartPaper" or . == "StartShadow"
  )) and
  ($policy[0].allowed_intent_types | all(
    . == "LoadFactor" or
    . == "StartPaper" or . == "StartShadow"
  )) and
  ($signed_starts | length == 1) and
  ($signed_artifacts | length == 1) and
  ($policy_starts == [$signed_starts[0]]) and
  (($policy[0].allowed_intent_types | index($signed_artifacts[0])) != null) and
  ($signed.envelope.allowed_intent_types | index("StartLiveSmall") | not) and
  ($policy[0].allowed_intent_types | index("StartLiveSmall") | not) and
  ($policy[0].runtime_paused == false) and
  (
    (($signed.envelope.allowed_intent_types | index("StartPaper")) != null and
      $signed.envelope.approval_class == "Paper")
    or
    (($signed.envelope.allowed_intent_types | index("StartShadow")) != null and
      $signed.envelope.approval_class == "Shadow")
  ) and
  ([$policy[0].approvals[]?.approval_class
    | select(. == "HumanApprovedLiveSmall" or . == "SameClassAutoLiveSmall")]
    | length == 0)
