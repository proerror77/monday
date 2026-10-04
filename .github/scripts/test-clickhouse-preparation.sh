#!/usr/bin/env bash
# Only a disposable loopback CI server. Tiny synthetic contract fixtures; no
# source archive, remote database, model fitting or real strategy replay.
set -euo pipefail
cd "$(dirname "$0")/../.."
endpoint=${MONDAY_TEST_CH_ENDPOINT:?explicit fixture endpoint required}
[[ $endpoint == http://127.0.0.1:18123 ]] || { echo 'test server is not disposable loopback' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ruby - "$endpoint" "$work" <<'RUBY'
require 'uri'
endpoint,work=ARGV
params={param_view_id:'1'*64,param_venue:'fixture',param_market:'usdm',param_instrument:'fixture',param_depth:'2',param_start_ns:'100',param_end_ns:'1000',param_lookback_ns:'50',param_horizons_ns:'[70,150]',param_label_tolerance_ns:'20',param_fit_cutoff_ns:'1000',param_sources:"['#{'a'*64}']"}
File.write("#{work}/url", "#{endpoint}/?#{URI.encode_www_form(params)}")
%w[clickhouse prepare].each do |name|
  File.read("rust_hft/research-core/platform/sql/#{name}.sql").split(';').each_with_index do |sql,i|
    next unless sql.match?(/\b(?:CREATE|INSERT)\b/)
    File.write("#{work}/#{name}-#{i}.sql",sql)
  end
end
RUBY
query() {
  local status=0
  curl --fail-with-body --silent --show-error --max-time 30 \
    --user "fixture:${MONDAY_TEST_CH_PASSWORD:?}" --data-binary @- \
    --output "$work/response" "$(cat "$work/url")" || status=$?
  if ((status != 0)); then cat "$work/response" >&2; return "$status"; fi
  cat "$work/response"
}
for file in "$work"/clickhouse-*.sql; do query <"$file" >/dev/null; done
source_sha=$(printf a%.0s {1..64})
cat <<SQL | query >/dev/null
INSERT INTO research.normalized_books VALUES
('$source_sha','fixture','fixture','usdm','A',0,99,100,[99],[1],[101],[1]),
('$source_sha','fixture','fixture','usdm','A',1,199,200,[100],[1],[102],[1]),
('$source_sha','fixture','fixture','usdm','A',2,275,300,[104],[1],[106],[1]),
('$source_sha','fixture','fixture','usdm','A',3,275,310,[998],[1],[1000],[1]),
('$source_sha','fixture','fixture','usdm','A',4,355,370,[109],[1],[111],[1]),
('$source_sha','fixture','fixture','usdm','B',0,445,446,[499],[1],[501],[1]),
('$source_sha','fixture','fixture','usdm','C',5,899,900,[100],[1],[102],[1]),
('$source_sha','fixture','fixture','usdm','C',6,1055,1056,[200],[1],[202],[1])
SQL
for file in "$work"/prepare-*.sql; do query <"$file" >/dev/null; done
printf '%s' "SELECT horizon_ns,target_event_ns,mature_ns FROM research.prepared_labels WHERE view_id={view_id:String} AND segment='A' AND ordinal=1 ORDER BY horizon_ns FORMAT TabSeparated" | query >"$work/labels"
diff -u <(printf '70\t275\t300\n150\t355\t370\n') "$work/labels"
count=$(printf '%s' "SELECT count() FROM research.prepared_labels WHERE view_id={view_id:String} AND ((segment='A' AND ordinal=4) OR (segment='C' AND ordinal=5)) FORMAT TabSeparated" | query)
[[ $count == 0 ]]
# Native typed output exists: this is neither a JSONL spool nor an OSS s3 scan.
printf '%s' "SELECT segment,ordinal,event_ns,available_ns,values FROM research.prepared_features WHERE view_id={view_id:String} ORDER BY available_ns,segment,ordinal FORMAT RowBinary" | query >"$work/features.rowbinary"
test -s "$work/features.rowbinary"
MONDAY_TEST_CH_ROWBINARY="$work/features.rowbinary" cargo test --manifest-path rust_hft/research-core/platform/Cargo.toml -p hft-research-platform --features control --lib --locked clickhouse::tests::actual_clickhouse_rowbinary_preserves_typed_feature_contract -- --ignored --exact
printf 'PASS: CH SQL multi-horizon time join, deterministic ties, gap and split isolation\n'
