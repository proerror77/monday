#!/usr/bin/env bash
# RustSec reports CVSS vectors, not advisory.severity. Preserve unknown severity.
set -euo pipefail
report=${1:?audit JSON required}
audit_result=${2:?actual audit job result required}
jq -er --arg result "$audit_result" '
  .vulnerabilities as $v |
  if ($v.list | type) != "array" or $v.count != ($v.list | length) or
     $v.found != (($v.list | length) > 0) then
    error("inconsistent cargo-audit report")
  else
    "- **Cargo Audit**: result=\($result), findings=\($v.count)",
    (if $v.count > 0 then
      "- **Advisories**: \([$v.list[].advisory.id] | unique | join(", "))",
      "- **Affected packages**: \([$v.list[] | .package.name + " " + .package.version] | unique | join(", "))",
      "- **Severity metadata**: not supplied as advisory.severity; CVSS vectors available for \([$v.list[] | select(.advisory.cvss != null)] | length)/\($v.count) findings"
    else empty end)
  end
' "$report"
