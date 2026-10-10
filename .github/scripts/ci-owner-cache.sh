#!/usr/bin/env bash
# Keep external dependencies in disjoint CI targets. Rebuild local code.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
mode=${1:?expected native-input, coverage-input, cache-input or cleanup}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [[ $mode == cache-input ]]; then
  # Cargo fingerprints rebuild changed packages after a compatible restore.
  # The reviewed TOML parser is a required input. Missing support fails closed.
  ruby -I "$root/.github/scripts/vendor/tomlrb/lib" -rtomlrb -rjson -rdigest -rpathname -ropen3 - "$root" <<'RUBY'
def canonical(value)
  case value
  when Hash then value.sort.to_h.transform_values { |entry| canonical(entry) }
  when Array then value.map { |entry| canonical(entry) }
  else value
  end
end

def digest(value)
  Digest::SHA256.hexdigest(JSON.generate(canonical(value)))
end

def file_digest(path)
  Digest::SHA256.file(path).hexdigest
end

def command(*args)
  output, status = Open3.capture2(*args)
  abort "cache input command failed: #{args.first}" unless status.success?
  output.strip
end

begin
  root = ARGV.fetch(0)
  abort 'unadmitted target layout' unless ENV['MONDAY_CARGO_TARGET_LAYOUT'] == 'owning-workspace-v1'
  native = ENV.fetch('MONDAY_CI_CACHE_NATIVE')
  coverage = ENV.fetch('MONDAY_CI_CACHE_COVERAGE')
  abort 'invalid native or coverage digest' unless [native, coverage].all? { |value| value.match?(/\A[0-9a-f]{64}\z/) }
  registry = JSON.parse(File.read("#{root}/rust_hft/workspaces.json"))
  admitted = {'shared' => 'shared/Cargo.toml', 'data' => 'data-pipelines/Cargo.toml',
    'research' => 'research-core/Cargo.toml', 'control' => 'research-core/platform/Cargo.toml',
    'runtime' => 'runtime/Cargo.toml', 'prediction' => 'prediction-markets/Cargo.toml'}
  owners = registry.fetch('workspaces')
  abort 'unadmitted workspace registry' unless registry['schema'] == 'monday.cargo_workspaces.v1' &&
    owners.is_a?(Array) && owners.length == admitted.length &&
    owners.all? { |owner| owner.is_a?(Hash) && owner.keys.sort == %w[id manifest] } &&
    owners.to_h { |owner| [owner.fetch('id'), owner.fetch('manifest')] } == admitted
  profiles = {}
  targets = []
  admitted.values.sort.each do |manifest|
    path = "#{root}/rust_hft/#{manifest}"
    abort 'owner escaped repository' unless File.realpath(path).start_with?(File.realpath("#{root}/rust_hft") + '/')
    parsed = Tomlrb.load_file(path)
    abort 'invalid standalone owner profiles' unless parsed['workspace'].is_a?(Hash) &&
      parsed.fetch('profile', {}).is_a?(Hash) && parsed.fetch('profile', {}).values.all? { |profile| profile.is_a?(Hash) }
    owner_profiles = parsed.fetch('profile', {})
    known_profiles = %w[dev test release bench] + owner_profiles.keys
    owner_profiles.each do |name, profile|
      abort 'unadmitted profile name' unless name.match?(/\A[A-Za-z0-9_-]+\z/)
      parent = profile['inherits']
      abort 'unadmitted inherited profile' if parent && (!parent.is_a?(String) || !known_profiles.include?(parent))
      abort 'custom profile has no inheritance' if !%w[dev test release bench].include?(name) && !parent
      seen = [name]
      while parent
        abort 'cyclic profile inheritance' if seen.include?(parent)
        seen << parent
        parent = owner_profiles.fetch(parent, {})['inherits']
      end
    end
    # Bind custom profiles, inheritance, package overrides and build overrides.
    profiles[manifest] = parsed.fetch('profile', {})
    targets << if manifest == 'data-pipelines/Cargo.toml'
      "#{root}/rust_hft/target/debug"
    else
      "#{root}/rust_hft/#{File.dirname(manifest)}/target/debug"
    end
  end
  compiler = {'rustc' => command('rustc', '-Vv'), 'cargo' => command('cargo', '-V')}
  host = compiler.fetch('rustc')[/^host: ([a-zA-Z0-9_-]+)$/, 1]
  abort 'unadmitted compiler target' unless host == 'x86_64-unknown-linux-gnu'
  %w[rustc cargo].each { |tool| compiler["#{tool}_binary"] = file_digest(command('rustup', 'which', tool)) }
  library_root = "#{command('rustc', '--print', 'sysroot')}/lib/rustlib/#{host}/lib"
  abort 'missing compiler target libraries' unless File.directory?(library_root)
  libraries = Dir.glob("#{library_root}/**/*", File::FNM_DOTMATCH).select { |path| File.file?(path) }.sort
  abort 'empty compiler target libraries' if libraries.empty?
  compiler['target_libraries'] = libraries.to_h do |path|
    [path.delete_prefix("#{library_root}/"), file_digest(path)]
  end
  flag_names = %w[RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS
    RUSTC RUSTDOC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTC_BOOTSTRAP CARGO_INCREMENTAL]
  flags = ENV.to_h.select do |name, _|
    flag_names.include?(name) || name.match?(/\ACARGO_(?:PROFILE_.*|BUILD_.*|TARGET_.*_(?:LINKER|RUSTFLAGS))\z/)
  end
  # This lane executes dev/test commands without --target or --profile.
  abort 'unadmitted build target override' if (flags['CARGO_BUILD_TARGET'] && !flags['CARGO_BUILD_TARGET'].empty?) ||
    flags.key?('CARGO_BUILD_TARGET_DIR') || flags.key?('CARGO_BUILD_PROFILE')
  admitted_profiles = %w[DEV TEST RELEASE BENCH] + profiles.values.flat_map(&:keys).map { |name| name.upcase.tr('-', '_') }
  abort 'unadmitted profile override' if flags.keys.any? do |name|
    name.start_with?('CARGO_PROFILE_') && admitted_profiles.none? { |profile| name.start_with?("CARGO_PROFILE_#{profile}_") }
  end
  %w[RUSTC RUSTDOC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER].each do |tool|
    compiler[tool] = file_digest(command('which', flags.fetch(tool))) if flags[tool] && !flags[tool].empty?
  end
  tracked = command('git', '-C', root, 'ls-files', '-z', '--', ':(glob)**/Cargo.toml', ':(glob)**/Cargo.lock',
    ':(glob)**/.cargo/config', ':(glob)**/.cargo/config.toml', 'Cargo.toml', 'Cargo.lock', '.cargo/config', '.cargo/config.toml')
  paths = tracked.split("\0").sort
  manifests = paths.select { |path| %w[Cargo.toml Cargo.lock].include?(File.basename(path)) }.to_h do |path|
    [path, file_digest("#{root}/#{path}")]
  end
  abort 'missing owning manifest or lock' unless admitted.values.all? do |manifest|
    manifests.key?("rust_hft/#{manifest}") && manifests.key?("rust_hft/#{manifest.sub('Cargo.toml', 'Cargo.lock')}")
  end
  configs = paths.select { |path| "/#{path}".include?('/.cargo/') }.to_h { |path| [path, file_digest("#{root}/#{path}")] }
  # Bind effective untracked and user Cargo configs as well as tracked configs.
  cargo_home = ENV.fetch('CARGO_HOME', "#{Dir.home}/.cargo")
  ancestors = ["#{root}/rust_hft", root] + Pathname.new(root).ascend.drop(1).map(&:to_s)
  ancestors.each_with_index do |directory, depth|
    %w[config config.toml].each do |name|
      path = "#{directory}/.cargo/#{name}"
      configs["ancestor-#{depth}/#{name}"] = file_digest(path) if File.exist?(path)
    end
  end
  %w[config config.toml].each do |name|
    path = "#{cargo_home}/#{name}"
    configs["cargo-home/#{name}"] = file_digest(path) if File.exist?(path)
  end
  configs['rust-toolchain.toml'] = file_digest("#{root}/rust-toolchain.toml")
  helpers = ["rust_hft/scripts/cargo-scoped.sh", "rust_hft/scripts/workspace-metadata.sh",
    "rust_hft/workspaces.json", ".github/scripts/ci-owner-cache.sh",
    ".github/scripts/test-rust-workspaces.sh", ".github/scripts/check-collector-test-presence.sh",
    ".github/workflows/ci.yml", ".github/scripts/select-rust-ci-scope.sh",
    "deployment/aliyun/test-polymarket-raw-ops-control-plane.sh"]
  parser = Dir.glob("#{root}/.github/scripts/vendor/tomlrb/lib/**/*.rb").sort.to_h do |path|
    [path.delete_prefix("#{root}/"), file_digest(path)]
  end
  abort 'missing Cargo TOML parser' if parser.empty?
  compatibility = {schema: 'monday.ci-owner-cache-compat.v2', compiler: compiler, target: host,
    profile: 'dev-test', native: native, flags: flags, configs: configs, profiles: profiles, owners: admitted, parser: parser}
  prefix = "rust_hft-ci-owner-v2-#{digest(compatibility)}-"
  exact = {coverage: coverage, manifests: manifests, configs: configs,
    helpers: helpers.to_h { |path| [path, file_digest("#{root}/#{path}")] }}
  puts "compat_prefix=#{prefix}"
  puts "key=#{prefix}#{digest(exact)}"
  puts 'cache_paths<<MONDAY_CI_CACHE_PATHS'
  puts "#{cargo_home}/registry", "#{cargo_home}/git", targets
  puts 'MONDAY_CI_CACHE_PATHS'
rescue StandardError => error
  abort "invalid CI cache inputs: #{error.message}"
end
RUBY
  exit 0
fi
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
      OPENSSL_DIR OPENSSL_LIB_DIR OPENSSL_INCLUDE_DIR OPENSSL_STATIC \
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
jq -s --arg root "$root/" \
  '[.[].packages[]|select(.source != null and (.manifest_path|startswith($root)|not))|.name,.targets[].name]|unique' \
  "$work/metadata.jsonl" >"$work/external-names.json"
ruby -rjson -rfileutils - "$root" "$work/local-names.json" "$work/external-names.json" <<'RUBY'
root, names_file, external_file = ARGV
names = JSON.parse(File.read(names_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
external = JSON.parse(File.read(external_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
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
      package = name.rpartition('-').first
      # A compatible restore can contain a deleted local package. Keep only
      # current metadata-proven external names, with local names taking priority.
      FileUtils.rm_r("#{path}/#{name}") if names.include?(package) || !external.include?(package)
    end
    # Retain metadata-proven dependency build programs beside native objects.
    # They remain cache inputs, never admitted software artifacts.
    Dir.glob("#{path}/**/*", File::FNM_DOTMATCH).each do |artifact|
      next unless File.file?(artifact) && File.executable?(artifact)
      next if artifact.match?(/\.(?:so(?:\.[0-9.]+)?|dylib|dll)\z/)
      package = artifact.delete_prefix("#{path}/").split('/').first.rpartition('-').first
      next if kind == 'build' && external.include?(package) && File.basename(artifact).match?(/\Abuild[-_]script[-_]/)
      FileUtils.rm_f(artifact)
    end
  end
end
RUBY
