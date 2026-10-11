# Catalog and native payload limits define a conservative source-local inventory.
def integer($min; $max): type == "number" and floor == . and . >= $min and . <= $max;
def require($condition; $message): if $condition then . else error($message) end;
def inventory:
  . as $catalog |
  require(.schema == "monday.research-products.v2" and (.products|type)=="object" and
    (.products|keys)==["cex-runner","controller","prediction-runner"] and
    (.recipes|type)=="array"; "unrecognized research catalog") |
  [$products|split(",")[]] as $selected |
  require(($selected|length)>0 and ($selected|sort|unique)==$selected and
    all($selected[]; . as $p | $catalog.products|has($p)); "invalid product selection") |
  [$selected[] as $p |
    $catalog.products[$p] as $names |
    require(($names|type)=="array" and ($names|length)>0 and
      ($names|unique|length)==($names|length) and
      all($names[]; type=="string" and test("^[a-z0-9][a-z0-9-]*$")); "invalid product binaries") |
    [$catalog.recipes[] | .binaries = [.binaries[]|select(. as $b | $names|index($b))] |
      select(.binaries|length>0)] as $recipes |
    require(([$recipes[].binaries[]]|sort)==($names|sort); "ambiguous or missing executable owner") |
    ($names|length) as $e | ($recipes|length) as $b |
    (1 + $e + 3*$b) as $n |
    ($source_bytes + $e*536870912 + 3*$b*1048576) as $upload |
    {product:$p, executables:($names|sort), executable_count:$e, build_count:$b,
     object_count:$n, native_requests:(3*$n+6), oss_requests:(3*$n+2), put_requests:$n, get_requests:(2*$n+2),
     request_body_bytes:($upload + 262144),
     response_body_bytes:($upload + 1048576 + (2*$n+1)*4096 + 262144),
     new_storage_bytes:$upload, recipes:$recipes}] as $allocations |
  {schema:"monday.research-publication-estimate.v1", repository:$repository,
   source_sha:$source_sha, source_archive:{bytes:$source_bytes,sha256:$source_digest},
   source_committed_at:$source_committed_at,
   products:$selected, basis:"catalog-static-upper-bound", executable_bound_bytes:536870912,
   metadata_bound_bytes:1048576, small_response_bound_bytes:4096,
   allocations:$allocations,
   total:($allocations|{object_count:(map(.object_count)|add),
      native_requests:(map(.native_requests)|add), oss_requests:(map(.oss_requests)|add), put_requests:(map(.put_requests)|add),
      get_requests:(map(.get_requests)|add), request_body_bytes:(map(.request_body_bytes)|add),
      response_body_bytes:(map(.response_body_bytes)|add), new_storage_bytes:(map(.new_storage_bytes)|add)})};
def price:
  # Whole decimal GB and integer micro-CNY avoid underestimating or float money.
  .total as $t |
  (($t.response_body_bytes + $t.oss_requests*65536)/1000000000|ceil) as $egress_gb |
  ($t.new_storage_bytes/1000000000|ceil) as $storage_gb |
  ($t.oss_requests*10000) as $request |
  ($egress_gb*812000) as $egress |
  ($storage_gb*$storage_hours*198) as $storage |
  . + {pricing:{model:"tokyo-cny-20261011-conservative-v1", currency:"CNY", region:"ap-northeast-1",
    storage_class:"Standard-LRS", storage_hours:$storage_hours,
    storage_horizon_is_deletion:false, request_provider_units_per_attempt:1,
    request_micro_cny:$request, egress_model_gb:$egress_gb, egress_micro_cny:$egress,
    storage_model_gb:$storage_gb, storage_micro_cny:$storage,
    estimated_micro_cny:($request+$egress+$storage),
    continued_storage_micro_cny_per_hour:($storage_gb*198),
    free_quota_assumed:false, invoice_hard_cap:false,
    response_overhead_model_bytes_per_request:65536}};
def admit_policy($p):
  . as $estimate |
  require(($p|type)=="object" and
    ($p|keys)==["currency","expires_at","max_estimated_micro_cny","max_new_storage_bytes",
      "max_oss_requests","max_request_body_bytes","max_response_body_bytes","price_model",
      "products","publisher_run_attempt","publisher_run_number","publisher_workflow","repository","schema","source_sha","storage_hours"];
    "one exact budget approval required") |
  require($p.schema=="monday.research-publication-budget-policy.v1" and $p.repository==$repository and
    $p.source_sha==$source_sha and $p.products==.products and $p.currency=="CNY" and
    $p.price_model==.pricing.model and $p.storage_hours==$storage_hours;
    "budget source, products, repository or price model differs") |
  require(($run_id|integer(1;9007199254740991)) and ($run_number|integer(1;9007199254740991)) and $attempt==1 and
    $p.publisher_run_number==$run_number and $p.publisher_run_attempt==$attempt and
    $p.publisher_workflow==".github/workflows/acr-publish.yml";
    "budget cannot renew in another run or attempt") |
  require(($p.expires_at|integer(1;1794268800)) and $p.expires_at>$now and
    $p.expires_at<=($now+604800) and $now<1794268800;
    "budget approval or price model expired") |
  require(($p.max_estimated_micro_cny|integer(1;1000000000)) and
    ($p.max_oss_requests|integer(1;1000)) and
    ($p.max_request_body_bytes|integer(1;107374182400)) and
    ($p.max_response_body_bytes|integer(1;107374182400)) and
    ($p.max_new_storage_bytes|integer(1;107374182400)); "invalid budget limits") |
  require(.pricing.estimated_micro_cny<=$p.max_estimated_micro_cny and
    .total.oss_requests<=$p.max_oss_requests and
    .total.request_body_bytes<=$p.max_request_body_bytes and
    .total.response_body_bytes<=$p.max_response_body_bytes and
    .total.new_storage_bytes<=$p.max_new_storage_bytes;
    "publication budget insufficient before cloud requests") |
  . + {admission:{schema:"monday.research-publication-budget-admission.v1",
      publisher_run_id:$run_id,publisher_run_number:$run_number,publisher_run_attempt:$attempt,expires_at:$p.expires_at,
      single_run:true,invoice_hard_cap:false,approved_limits:$p}};
def native_budget:
  {schema:"monday.oss-publication-budget.v1",repository,source_sha,
   publisher_run_id:.admission.publisher_run_id,publisher_run_attempt:.admission.publisher_run_attempt,
   expires_at_ms:(.admission.expires_at*1000),
   publication_namespaces:["research/builds/","research/sources/"],
   limits:{requests:.total.native_requests,request_payload_bytes:.total.request_body_bytes,
     response_payload_bytes:.total.response_body_bytes},
   allocations:[.allocations[]|{product,limits:{requests:.native_requests,
     request_payload_bytes:.request_body_bytes,response_payload_bytes:.response_body_bytes}}]};
def admit_operations:
  # Ongoing software operating authorization has no human-guessed SHA/run IDs.
  # It is a bound per new source, not a global account or research/trading cap.
  . as $estimate | $policy as $p |
  require(($p|type)=="object" and ($p|keys)==["currency","expires_at","history_anchor_run_id",
    "history_anchor_run_number","history_retention_required","max_estimated_micro_cny",
    "max_new_storage_bytes","max_oss_requests","max_request_body_bytes","max_response_body_bytes",
    "not_before","price_model","products","publisher_workflow","repository","schema","storage_hours"];
    "one software operating allowance required") |
  require($p.schema=="monday.research-publication-operations-policy.v1" and $p.repository==$repository and
    $p.publisher_workflow==".github/workflows/acr-publish.yml" and $p.currency=="CNY" and
    $p.price_model==.pricing.model and $p.storage_hours==$storage_hours and
    ($p.products|type)=="array" and ($p.products|sort|unique)==$p.products and
    ($p.products|length)>0 and all($p.products[]; .=="cex-runner" or .=="controller" or .=="prediction-runner") and
    all(.products[]; . as $product | $p.products|index($product)); "operating scope or price model differs") |
  require(($p.history_anchor_run_id|integer(1;9007199254740991)) and
    ($p.history_anchor_run_number|integer(1;9007199254740991)) and $p.history_retention_required==true;
    "retained publication history anchor required") |
  require(($p.not_before|integer(1;1794268800)) and ($p.expires_at|integer(1;1794268800)) and
    $p.not_before<=$now and $p.expires_at>$now and $p.expires_at>$p.not_before and
    $p.expires_at-$p.not_before<=604800 and $now<1794268800; "operating allowance or price model expired") |
  require((.source_committed_at|integer(1;1794268800)) and
    .source_committed_at>=$p.not_before and .source_committed_at<=$now;
    "pre-window or future source requires manual reconciliation") |
  # Adapt public authorization to the existing strict native envelope. Runtime
  # identities are captured here, before any OIDC/STS/OSS request.
  ($p + {schema:"monday.research-publication-budget-policy.v1",source_sha:$source_sha,
    products:$estimate.products,publisher_run_number:$run_number,publisher_run_attempt:$attempt}
    | del(.not_before,.history_anchor_run_id,.history_anchor_run_number,.history_retention_required)) as $bound |
  admit_policy($bound) |
  .admission.approval_kind="software-operating-allowance" |
  .admission.operating_policy=$p |
  .admission.aggregate_invoice_cap=false |
  .admission.retry_reconciliation_basis="retained-monotonic-github-history";
if $mode=="estimate" then inventory | price
elif $mode=="admit" then admit_policy($policy)
elif $mode=="admit-operations" then admit_operations
elif $mode=="native" then native_budget
else error("invalid budget mode") end
