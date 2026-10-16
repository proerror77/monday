#!/usr/bin/env bash
# Cache lookup binds all native bytes. Normalize only a fresh CI source tree.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
[[ ${GITHUB_ACTIONS:-false} == true ]] || exit 0
cd "$root"
source=rust_hft/native/libduckdb-sys
[[ -z $(git -c safe.directory="$root" status --porcelain --untracked-files=all -- "$source") ]] || {
  echo 'native source must be committed and clean in CI' >&2; exit 1;
}
# Never hide an input change from an existing local or restored Cargo target.
for target in rust_hft/target rust_hft/research-core/target rust_hft/data-pipelines/target; do
  [[ ! -e $target ]] || { echo "native timestamp preparation must precede target creation: $target" >&2; exit 1; }
done
ruby -ropen3 - "$source" <<'RUBY'
files, status = Open3.capture2('git', '-c', "safe.directory=#{Dir.pwd}", 'ls-files', '-z', '--', ARGV.fetch(0))
abort 'native source inventory failed' unless status.success? && !files.empty?
files.split("\0").each do |file|
  abort 'native source must contain regular files' unless File.file?(file) && !File.symlink?(file)
  File.utime(Time.at(946684800), Time.at(946684800), file)
end
RUBY
