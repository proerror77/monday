#!/usr/bin/env bash
# Keep external dependencies in disjoint CI targets. Rebuild local code.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
mode=${1:?expected native-input, coverage-input or cleanup}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [[ $mode == coverage-input ]]; then
  # Bind only the commands this Rust job executes, excluding source/event IDs.
  ruby -rjson -rdigest <<'RUBY'
plan = JSON.parse(ENV.fetch('MONDAY_CI_CACHE_PLAN'))
flags = %w[handoff json ondo collector control focused loop]
coverage = flags.to_h do |name|
  value = plan.fetch(name)
  abort 'invalid Rust cache coverage flag' unless %w[true false].include?(value)
  [name, value]
end
packages = %w[owning focused loop].to_h do |name|
  selected = plan.fetch("#{name}_packages").split(',').reject(&:empty?)
  abort 'invalid Rust cache coverage package' unless selected.all? { |p| p.match?(/\A[a-zA-Z0-9_-]+\z/) }
  [name, selected.uniq.sort]
end
coverage['owning_packages'] = packages.fetch('owning')
# Direct-package Clippy omits loop members, even when the loop stage is inactive.
coverage['owning_clippy_packages'] = packages.fetch('owning') - packages.fetch('loop')
%w[focused loop].each do |name|
  abort 'active Rust cache coverage has no packages' if coverage.fetch(name) == 'true' && packages.fetch(name).empty?
  coverage["#{name}_packages"] = coverage.fetch(name) == 'true' ? packages.fetch(name) : []
end
puts "coverage=#{Digest::SHA256.hexdigest(JSON.generate(coverage))}"
RUBY
  exit 0
fi
if [[ $mode == native-input ]]; then
  [[ $(uname -s) == Linux ]]
  {
    uname -ms
    cat /etc/os-release
    dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' | LC_ALL=C sort
    for command in cc c++ clang mold protoc ldd; do "$command" --version; done
    for variable in CC CXX AR LD CFLAGS CXXFLAGS LDFLAGS LIBRARY_PATH LD_LIBRARY_PATH \
      CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH PKG_CONFIG PKG_CONFIG_PATH \
      PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR BINDGEN_EXTRA_CLANG_ARGS \
      PROTOC PROTOC_INCLUDE CMAKE_TOOLCHAIN_FILE; do
      printf '%s=%s\n' "$variable" "${!variable-}"
    done
  } >"$work/native"
  printf 'native=%s\n' "$(sha256sum "$work/native" | awk '{print $1}')"
  exit 0
fi
[[ $mode == cleanup ]]
[[ ${MONDAY_CARGO_TARGET_LAYOUT:?layout required} == owning-workspace-v1 ]]
manifests=$(jq -er '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
while IFS= read -r manifest; do
  [[ $manifest =~ ^([a-z-]+/)+Cargo\.toml$ ]]
  # Metadata only. Match cache discovery without compiling a feature union.
  cargo metadata --manifest-path "$root/rust_hft/$manifest" \
    --locked --all-features --format-version 1 >>"$work/metadata.jsonl"
done <<<"$manifests"
jq -se --arg root "$root/" \
  '[.[].packages[]|select(.source == null or (.manifest_path|startswith($root)))|.name,.targets[].name]|unique|
   if length>0 then . else error("no repository-local compilation packages") end' \
  "$work/metadata.jsonl" >"$work/local-names.json"
ruby -rjson -rfileutils - "$root" "$work/local-names.json" <<'RUBY'
root, names_file = ARGV
names = JSON.parse(File.read(names_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
manifests = JSON.parse(File.read("#{root}/rust_hft/workspaces.json")).fetch('workspaces').map { |w| w.fetch('manifest') }
targets = manifests.map do |manifest|
  abort 'unadmitted CI cache owner' unless manifest.match?(%r{\A(?:[a-z-]+/)+Cargo\.toml\z})
  manifest == 'data-pipelines/Cargo.toml' ? "#{root}/rust_hft/target" : "#{root}/rust_hft/#{File.dirname(manifest)}/target"
end
abort 'CI cache targets overlap' unless targets.uniq == targets && targets.none? { |p| targets.any? { |q| p != q && p.start_with?("#{q}/") } }
targets.each do |target|
  next unless File.directory?(target)
  abort 'CI cache target escaped owner' unless File.realpath(target) == target
  profile = "#{target}/debug"
  next unless File.directory?(profile)
  abort 'CI cache profile escaped target' unless File.realpath(profile) == profile
  %w[build .fingerprint deps incremental].each do |kind|
    abort 'CI cache artifact directory escaped target' if File.symlink?("#{profile}/#{kind}")
  end
  Dir.children(profile).each do |name|
    path = "#{profile}/#{name}"
    FileUtils.rm_r(path) unless %w[build .fingerprint deps].include?(name)
  end
  %w[build .fingerprint deps].each do |kind|
    path = "#{profile}/#{kind}"
    next unless File.directory?(path)
    Dir.children(path).each do |name|
      # Match the full name before Cargo's hash, not a local package prefix.
      FileUtils.rm_r("#{path}/#{name}") if names.include?(name.rpartition('-').first)
    end
  end
end
RUBY
