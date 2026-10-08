# CI shares public signing trust, but each product has an independent trusted
# RAM role and exact preapproved native-plan prefixes. Never union those roles.
if (.oss_by_product | type) != "object" or (.oss_by_product | length) == 0 then
  error("MONDAY_RESEARCH_RELEASE_POLICY lacks oss_by_product mapping; this does not establish missing Alibaba resources or roles. Review existing role configuration and use .github/scripts/migrate-research-oss-policy.sh; real RAM permissions require independent verification.")
elif any(.oss_by_product | keys[]; . != "cex-runner" and . != "prediction-runner" and . != "controller") then
  error("unknown OSS product")
elif ([.oss_by_product[].role_arn] | length) != ([.oss_by_product[].role_arn] | unique | length) then
  error("products require distinct approved RAM roles")
elif (.oss_by_product[$product] | type) != "object" then
  error("MONDAY_RESEARCH_RELEASE_POLICY.oss_by_product lacks the selected product mapping; inspect existing roles before configuring it, without creating resources or expanding permissions automatically")
else .oss = .oss_by_product[$product] | del(.oss_by_product)
end
