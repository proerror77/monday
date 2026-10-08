#!/usr/bin/env bash
# Distinct owning-workspace targets prevent sequential cache cleaners from
# deleting another workspace's dependencies. Persist dependency bytes only.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
manifest_dir() {
  local manifest=$1
  [[ $manifest =~ ^([a-z-]+/)+Cargo\.toml$ ]] || return 1
  jq -e --arg manifest "$manifest" 'any(.workspaces[];.manifest==$manifest)' "$root/rust_hft/workspaces.json" >/dev/null || return 1
  printf '%s\n' "${manifest%/Cargo.toml}"
}
case ${1:?command required} in
  target-dir)
    directory=$(manifest_dir "${2:?manifest required}")
    printf '%s/rust_hft/target/%s\n' "$root" "${directory//\//--}"
    ;;
  binary-path)
    binary=${3:?binary required}
    [[ $binary =~ ^[a-z0-9_-]+$ ]]
    manifest=$(bash "$root/.github/scripts/research-release-products.sh" recipes "${2:?product required}" | jq -ser --arg binary "$binary" '[.[]|select(.binaries|index($binary))|.manifest] | if length==1 then .[0] else error("ambiguous binary recipe") end')
    directory=$(manifest_dir "$manifest")
    printf '%s/rust_hft/target/%s/x86_64-unknown-linux-gnu/release/%s\n' "$root" "${directory//\//--}" "$binary"
    ;;
  workspaces)
    manifests=$(jq -er '.recipes|map(.manifest)|unique | if length>0 then .[] else error("empty cache layout") end' "${2:?inputs required}")
    printf 'workspaces<<MONDAY_CACHE_LAYOUT\n'
    while IFS= read -r manifest; do
      directory=$(manifest_dir "$manifest")
      relative=$(ruby -rpathname -e 'puts Pathname.new("target/"+ARGV[0].gsub("/", "--")).relative_path_from(Pathname.new(ARGV[0]))' "$directory")
      printf 'rust_hft/%s -> %s\n' "$directory" "$relative"
    done <<<"$manifests"
    printf 'MONDAY_CACHE_LAYOUT\n'
    ;;
  cleanup)
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    while IFS= read -r manifest; do
      cargo metadata --manifest-path "$root/rust_hft/$manifest" --locked --no-deps --format-version 1 >>"$work/local.jsonl"
    done < <(jq -er '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
    jq -s --arg root "$root/" '[.[].packages[]|select(.manifest_path|startswith($root))|.name, .targets[].name]|unique' "$work/local.jsonl" >"$work/names.json"
    ruby -rjson -rfileutils - "$root" "$work/names.json" "${2:?inputs required}" <<'RUBY'
root, names_file, inputs_file = ARGV
names = JSON.parse(File.read(names_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
manifests = JSON.parse(File.read(inputs_file)).fetch('recipes').map { |r| r.fetch('manifest') }.uniq
allowed = JSON.parse(File.read("#{root}/rust_hft/workspaces.json")).fetch('workspaces').map { |w| w.fetch('manifest') }
manifests.each do |manifest|
  abort 'unadmitted target manifest' unless allowed.include?(manifest)
  directory = "#{root}/rust_hft/target/#{File.dirname(manifest).gsub('/', '--')}"
  next unless File.directory?(directory)
  abort 'cache target symlink escaped owning workspace' unless File.realpath(directory) == directory
  ["#{directory}/debug", "#{directory}/release", "#{directory}/x86_64-unknown-linux-gnu/release"].each do |profile|
    next unless File.directory?(profile)
    abort 'profile symlink escaped target' unless File.realpath(profile) == profile
    %w[build .fingerprint deps].each { |kind| abort 'artifact directory symlink escaped target' if File.symlink?("#{profile}/#{kind}") }
    # Executables already went into the immutable release; cache no local bytes.
    Dir.children(profile).each { |n| FileUtils.rm_f("#{profile}/#{n}") if File.file?("#{profile}/#{n}") || File.symlink?("#{profile}/#{n}") }
    %w[build .fingerprint deps].each do |kind|
      path = "#{profile}/#{kind}"
      next unless File.directory?(path)
      abort 'artifact directory symlink escaped target' unless File.realpath(path) == path
      Dir.children(path).each do |n|
        FileUtils.rm_rf("#{path}/#{n}") if names.any? { |name| n.start_with?("#{name}-") }
      end
    end
  end
end
RUBY
    ;;
  *) exit 2 ;;
esac
