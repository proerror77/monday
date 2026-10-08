#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
migrate="$script_dir/migrate-research-oss-policy.sh"
jq -n '{trust:{schema:1,keys:{old:"public-key"}},key_id:"old",builder_image:"existing-builder",image_repositories:{controller:"existing-acr"},tls:{preserve:true},oss:{preserve:"existing-native-policy"}}' > "$work/old.json"
jq -n --arg source "$(printf '%040d' 1)" --arg build "$(printf '%064d' 2)" '
  {controller:{bucket:"existing",region:"ap-northeast-1",endpoint:"https://existing.oss-ap-northeast-1.aliyuncs.com/",role_arn:"acs:ram::123:role/existing-controller",oidc_provider_arn:"acs:ram::123:oidc-provider/existing",audience:"existing-aud",subject:"existing-sub",repository_id:1,owner_id:2,role_prefixes:[("research/builds/"+$build+"/"),("research/sources/"+$source+"/")]}}' > "$work/map.json"
bash "$migrate" "$work/old.json" "$work/map.json" > "$work/new.json"
jq -e --slurpfile old "$work/old.json" --slurpfile map "$work/map.json" 'del(.oss_by_product)==$old[0] and .oss_by_product==$map[0]' "$work/new.json" >/dev/null
bash "$migrate" "$work/new.json" "$work/map.json" > "$work/retry.json"
cmp "$work/new.json" "$work/retry.json"
jq '.["prediction-runner"] = (.controller | .role_arn="acs:ram::123:role/existing-prediction")' "$work/map.json" > "$work/added-map.json"
bash "$migrate" "$work/new.json" "$work/added-map.json" > "$work/added-policy.json"
jq -e --slurpfile old "$work/new.json" --slurpfile map "$work/added-map.json" \
  'del(.oss_by_product)==($old[0]|del(.oss_by_product)) and .oss_by_product==$map[0] and .oss_by_product.controller==$old[0].oss_by_product.controller' "$work/added-policy.json" >/dev/null
bash "$migrate" "$work/added-policy.json" "$work/added-map.json" > "$work/added-retry.json"
cmp "$work/added-policy.json" "$work/added-retry.json"
expect_reject() {
  if bash "$migrate" "$1" "$2" > "$work/rejected.json" 2> "$work/error"; then
    printf 'invalid migration accepted\n' >&2; exit 1
  fi
  test ! -s "$work/rejected.json"
}
for expression in \
  'del(.controller.subject)' \
  '.controller.repository_id="1"' \
  '.controller.extra="unsupported"' \
  '.controller.bucket="Existing"' \
  '.controller.endpoint="http://existing.oss-ap-northeast-1.aliyuncs.com/"' \
  '.controller.endpoint="https://foreign.oss-ap-northeast-1.aliyuncs.com/"' \
  '.controller.endpoint="https://existing.oss-cn-hangzhou.aliyuncs.com/"' \
  '.controller.endpoint+="?redirect=elsewhere"' \
  '.controller.role_prefixes=["research/builds/*"]' \
  '.controller.role_prefixes|=reverse' \
  '.controller.role_prefixes+= [.controller.role_prefixes[0]]' \
  '.foreign=.controller|del(.controller)' \
  '.["cex-runner"] = .controller'; do
  jq "$expression" "$work/map.json" > "$work/invalid-map.json"
  expect_reject "$work/old.json" "$work/invalid-map.json"
done
jq '.oss_by_product.controller.subject="changed"' "$work/new.json" > "$work/conflict.json"
expect_reject "$work/conflict.json" "$work/map.json"
expect_reject "$work/added-policy.json" "$work/map.json"
jq 'del(.controller)' "$work/added-map.json" > "$work/removed-map.json"
expect_reject "$work/added-policy.json" "$work/removed-map.json"
jq '.oss_by_product="invalid"' "$work/new.json" > "$work/invalid-existing.json"
expect_reject "$work/invalid-existing.json" "$work/map.json"
cat "$work/old.json" "$work/old.json" > "$work/multiple-policy.json"
expect_reject "$work/multiple-policy.json" "$work/map.json"
cat "$work/map.json" "$work/map.json" > "$work/multiple-map.json"
expect_reject "$work/old.json" "$work/multiple-map.json"
if jq -e --arg product controller -f "$script_dir/select-research-oss-policy.jq" "$work/old.json" > "$work/selected.json" 2> "$work/error"; then exit 1; fi
grep -Fq 'does not establish missing Alibaba resources or roles' "$work/error"
jq -e --arg product controller -f "$script_dir/select-research-oss-policy.jq" "$work/new.json" > "$work/selected.json"
jq -e --slurpfile map "$work/map.json" '.oss==$map[0].controller' "$work/selected.json" >/dev/null
printf 'Existing policy preservation, additive products, idempotence and migration negative cases passed\n'
