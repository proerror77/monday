# CI shares public signing trust, but each product has an independent trusted
# RAM role and exact preapproved native-plan prefixes. Never union those roles.
if (.oss_by_product | type) != "object" or (.oss_by_product | length) == 0 then
  error("per-product OSS role configuration required")
elif any(.oss_by_product | keys[]; . != "cex-runner" and . != "prediction-runner" and . != "controller") then
  error("unknown OSS product")
elif ([.oss_by_product[].role_arn] | length) != ([.oss_by_product[].role_arn] | unique | length) then
  error("products require distinct approved RAM roles")
elif (.oss_by_product[$product] | type) != "object" then
  error("selected product lacks approved OSS role")
else .oss = .oss_by_product[$product] | del(.oss_by_product)
end
