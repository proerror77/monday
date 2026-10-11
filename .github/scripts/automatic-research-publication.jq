# main workflow code and environment administrators form the trusted boundary.
# RAM validates the environment subject; it does not validate workflow_ref.
def require($ok; $reason): if $ok then . else error($reason) end;
def positive: type == "number" and . > 0 and floor == .;
def environment_name($product):
  if $product == "cex-runner" then "monday-research-cex"
  elif $product == "controller" then "monday-research-controller"
  elif $product == "prediction-runner" then "monday-research-prediction"
  else error("unknown product") end;
. as $b | .config as $c
| require($c.schema == "monday.automatic_research_publication.v1"; "automatic publication is not configured")
| require(($c.repository_id | positive) and ($c.owner_id | positive); "invalid repository pins")
| require(($b.source_sha | type == "string" and test("^[0-9a-f]{40}$"))
    and $b.repository.full_name == $b.repository_name
    and $b.repository.id == $c.repository_id and $b.repository.owner.id == $c.owner_id
    and $b.repository.visibility == "public"; "repository identity differs")
| require($b.main.object.sha == $b.source_sha; "current main drifted")
| require($b.run.id == $b.run_id and $b.run.run_attempt == $b.run_attempt
    and $b.run.head_sha == $b.source_sha and $b.run.head_branch == "main"
    and $b.run.path == ".github/workflows/acr-publish.yml"
    and $b.run.repository.id == $c.repository_id
    and $b.run.head_repository.id == $c.repository_id
    and ($b.run.event == "workflow_run" or $b.run.event == "workflow_dispatch");
    "publisher run/source/workflow differs")
| require($b.oidc.use_default == true
    and $c.subject_prefix == ("repo:" + $b.repository.full_name)
    and (($b.oidc | has("use_immutable_subject") | not) or $b.oidc.use_immutable_subject == false)
    and (($b.oidc | has("sub_claim_prefix") | not) or $b.oidc.sub_claim_prefix == $c.subject_prefix);
    "existing OIDC template or subject prefix changed")
| ($b.products | split(",")) as $products
| require(($products | length > 0 and length <= 3 and . == (unique | sort)
    and all(.[]; . == "cex-runner" or . == "controller" or . == "prediction-runner"));
    "invalid canonical products")
| require(($c.products | keys) == ["cex-runner", "controller", "prediction-runner"]
    and ([$c.products[].environment_id] | all(.[]; positive) and length == (unique | length));
    "all product pins must be distinct")
| require(($b.environments | length) == ($products | length); "environment readback count differs")
| [$products[] as $product | $c.products[$product] as $pin
    | [$b.environments[] | select(.product == $product)] as $matches
    | require(($matches | length) == 1; "missing or duplicate product environment")
    | $matches[0] as $actual
    | require($pin.name == environment_name($product)
        and $actual.environment.id == $pin.environment_id
        and $actual.environment.name == $pin.name; "environment missing, renamed, or recreated")
    | require($actual.environment.deployment_branch_policy == {protected_branches:false,custom_branch_policies:true}
        and $actual.branches.total_count == 1 and ($actual.branches.branch_policies | length) == 1
        and $actual.branches.branch_policies[0].name == "main"
        and $actual.branches.branch_policies[0].type == "branch"; "environment must allow only branch main")
    | require(($actual.environment.protection_rules | type == "array" and length <= 1
        and all(.[]; .type == "branch_policy"));
        "existing protection requires a different approval path; never bypass it")
    | ($c.subject_prefix + ":environment:" + $pin.name) as $subject
    | require($actual.policy.oss.subject == $subject; "OSS policy subject differs from selected environment")
    | {product:$product,name:$pin.name,environment_id:$pin.environment_id,subject:$subject}]
| {schema:"monday.automatic_research_publication_admission.v1",
    repository:$b.repository_name,repository_id:$c.repository_id,owner_id:$c.owner_id,
    source_sha:$b.source_sha,run_id:$b.run_id,run_attempt:$b.run_attempt,products:$products,environments:.}
