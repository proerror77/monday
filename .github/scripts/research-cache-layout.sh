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
cache_recipes() {
  # Selection changes the exact build recipe, never the cache path/version.
  jq -ce --slurpfile registry "$root/rust_hft/workspaces.json" '
    def name: type == "string" and test("^[A-Za-z0-9_-]+$");
    if .schema != "monday.research-products.v2" or (.recipes | type != "array" or length == 0) or
       (.recipes | all(.[]; (.manifest as $manifest | $registry[0].workspaces | any(.manifest == $manifest)) and
         (.package | name) and (.features | type == "string") and
         (.binaries | type == "array" and length > 0 and all(.[];name))) | not) or
       (.recipes | map(.manifest) | unique) != ["data-pipelines/Cargo.toml", "prediction-markets/Cargo.toml",
         "research-core/Cargo.toml", "research-core/platform/Cargo.toml"]
    then error("unadmitted research cache catalog") else .recipes end' "$root/.github/scripts/research-release-products.json"
}
cache_manifests() {
  cache_recipes | jq -r 'map(.manifest)|unique|.[]'
}
validate_recipes() {
  jq -e --argjson catalog "$(cache_recipes)" '
    def name: type == "string" and test("^[A-Za-z0-9_-]+$");
    .recipes | type == "array" and length>0 and all(.[];
      (.manifest as $manifest | $catalog | any(.manifest == $manifest)) and
      (.package|name) and (.features|type == "string") and
      (.binaries|type == "array" and length>0 and all(.[];name)))' "$1" >/dev/null
}
case ${1:?command required} in
  compatibility-inputs)
    # Keep local dependency choices in provenance and Cargo's exact cache suffix.
    # The restore prefix still binds every admitted compiler, native and profile input.
    registry="$root/rust_hft/workspaces.json"
    manifests=$(jq -er 'if .schema == "monday.cargo_workspaces.v1" and (.workspaces|length)>0
      then .workspaces[].manifest else error("invalid workspace registry") end' "$registry")
    owner_profiles='{}'
    while IFS= read -r manifest; do
      manifest_dir "$manifest" >/dev/null
      digest=$(sha256sum "$root/rust_hft/$manifest" | awk '{print $1}')
      owner_profiles=$(jq -c --arg manifest "$manifest" --arg digest "$digest" '. + {($manifest):$digest}' <<<"$owner_profiles")
    done <<<"$manifests"
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    # Validate full provenance before deriving a narrower restore identity.
    jq -Se --argjson owner_profiles "$owner_profiles" '
      def digest: type == "string" and test("^[0-9a-f]{64}$");
      def name: type == "string" and test("^[A-Za-z0-9_-]+$");
      if .schema != "monday.compilation-inputs.v3" or
        ([.compiler,.native,.flags,.profiles,.recipe] | all(digest) | not) or
        (.target | name | not) or (.profile | name | not) or
        (.builder_image | type != "string" or (test("^.+@sha256:[0-9a-f]{64}$") | not)) or
        .workspace_profiles != $owner_profiles or
        (.locks | type != "object") or
        ((.locks | keys) != ($owner_profiles | keys | map(sub("Cargo.toml$";"Cargo.lock")) | sort)) or
        (.locks | all(.[];digest) | not) or
        (.recipes | type != "array" or length == 0) or
        (.recipes | all(.[];
          (.manifest as $manifest | $owner_profiles | has($manifest)) and
          (.package | name) and (.features | type == "string") and
          (.binaries | type == "array" and length > 0 and all(.[];name))) | not)
      then error("invalid compilation inputs for dependency cache compatibility")
      else . end' "${2:?inputs required}" >"$work/validated.json"
    validate_recipes "$work/validated.json"
    cache_recipes >"$work/cache-recipes.json"
    ruby -I "$root/.github/scripts/vendor/tomlrb/lib" -rtomlrb -rjson -rdigest -rpathname - "$root" "$work/cache-recipes.json" >"$work/owner-profiles.json" <<'RUBY'
root, recipes_file = ARGV
begin
  registry = JSON.parse(File.read("#{root}/rust_hft/workspaces.json"))
  abort 'unsupported workspace registry' unless registry['schema'] == 'monday.cargo_workspaces.v1'
  owners = registry.fetch('workspaces')
  abort 'unsupported workspace entry' if owners.empty? || owners.any? { |owner| owner.keys.sort != %w[id manifest] }
  by_manifest = owners.to_h { |owner| [owner.fetch('manifest'), owner] }
  abort 'duplicate workspace entry' unless by_manifest.length == owners.length && owners.map { |owner| owner.fetch('id') }.uniq.length == owners.length
  selected = JSON.parse(File.read(recipes_file)).map { |recipe| recipe.fetch('manifest') }.uniq.sort
  abort 'unadmitted cache owner' if selected.empty? || selected.any? { |manifest| !by_manifest.key?(manifest) }
  profiles = selected.to_h do |manifest|
    path = "#{root}/rust_hft/#{manifest}"
    abort 'cache owner escaped repository' unless File.realpath(path).start_with?(File.realpath("#{root}/rust_hft") + '/')
    parsed = Tomlrb.load_file(path)
    abort 'cache owner is not a standalone workspace with valid profiles' unless parsed['workspace'].is_a?(Hash) &&
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
    # Bind inherited, custom, package and build-override profiles together.
    [manifest, parsed.fetch('profile', {})]
  end
  parser_root = "#{root}/.github/scripts/vendor/tomlrb/lib"
  parser_files = Dir.glob("#{parser_root}/**/*.rb").sort
  abort 'empty Cargo TOML parser' if parser_files.empty?
  parser = Digest::SHA256.hexdigest(parser_files.map { |path| "#{Pathname.new(path).relative_path_from(Pathname.new(parser_root))}:#{Digest::SHA256.file(path).hexdigest}\n" }.join)
  canonical = lambda do |value|
    case value
    when Hash then value.keys.sort.to_h { |key| [key, canonical.call(value.fetch(key))] }
    when Array then value.map { |item| canonical.call(item) }
    else value
    end
  end
  puts JSON.generate(canonical.call({'schema'=>'monday.cargo-cache-profiles.v2', 'owners'=>selected.map { |manifest| by_manifest.fetch(manifest) }, 'profiles'=>profiles, 'parser_sha256'=>parser}))
rescue StandardError => error
  warn "invalid Cargo cache profile inputs: #{error.class}"
  exit 1
end
RUBY
    jq -Se --slurpfile owner_profiles "$work/owner-profiles.json" '
      {schema:"monday.dependency-cache-compat.v3",target,profile,compiler,native,flags,
        builder_image,recipe,owner_profiles:$owner_profiles[0]}' "$work/validated.json"
    ;;
  manifest-inputs)
    # Include local path/patch/default-feature manifests beyond recipe roots.
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    # Container checkouts may belong to the host runner user. Scope trust to
    # this invocation, and propagate Git failure before processing its output.
    git -c safe.directory="$root" -C "$root" ls-files -z -- ':(glob)**/Cargo.toml' Cargo.toml >"$work/manifests"
    result='{}'
    while IFS= read -r -d '' manifest; do
      digest=$(sha256sum "$root/$manifest" | awk '{print $1}')
      result=$(jq -c --arg manifest "$manifest" --arg digest "$digest" '. + {($manifest):$digest}' <<<"$result")
    done <"$work/manifests"
    jq -Se 'if length>0 then . else error("no tracked local manifests") end' <<<"$result"
    ;;
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
  cache-paths)
    # Cache dependency registries and disjoint targets, never tool executables.
    validate_recipes "${2:?inputs required}"
    manifests=$(cache_manifests)
    printf 'cache_paths<<MONDAY_CACHE_PATHS\n'
    printf '%s\n' "${CARGO_HOME:-$HOME/.cargo}/registry" "${CARGO_HOME:-$HOME/.cargo}/git"
    while IFS= read -r manifest; do
      directory=$(manifest_dir "$manifest")
      printf '%s/rust_hft/target/%s\n' "$root" "${directory//\//--}"
    done <<<"$manifests"
    printf 'MONDAY_CACHE_PATHS\n'
    ;;
  cleanup)
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    cache_recipes >"$work/cache-recipes.json"
    validate_recipes "${2:?inputs required}"
    while IFS= read -r manifest; do
      cargo metadata --manifest-path "$root/rust_hft/$manifest" --locked --no-deps --format-version 1 >>"$work/local.jsonl"
    done < <(jq -er '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
    # A subset restore also contains the other owners. Inspect every admitted
    # recipe separately to clean those bytes; this never builds a feature union.
    recipes=$(jq -ec '.[]' "$work/cache-recipes.json")
    while IFS= read -r recipe; do
      manifest=$(jq -er .manifest <<<"$recipe")
      manifest_dir "$manifest" >/dev/null
      features=$(jq -r .features <<<"$recipe")
      package=$(jq -er .package <<<"$recipe")
      metadata_args=()
      if [[ -n $features ]]; then
        scoped_features=$(jq -nr --arg package "$package" --arg features "$features" '$features|split(",")|map($package+"/"+.)|join(",")')
        metadata_args+=(--features "$scoped_features")
      fi
      cargo metadata --manifest-path "$root/rust_hft/$manifest" --locked --format-version 1 "${metadata_args[@]}" >>"$work/local.jsonl"
    done <<<"$recipes"
    jq -s --arg root "$root/" '[.[].packages[]|select(.source == null or (.manifest_path|startswith($root)))|.name, .targets[].name]|unique|if length>0 then . else error("no local compilation packages") end' "$work/local.jsonl" >"$work/names.json"
    jq -s --arg root "$root/" '[.[].packages[]|select(.source != null and (.manifest_path|startswith($root)|not))|.name, .targets[].name]|unique' "$work/local.jsonl" >"$work/external.json"
    ruby -rjson -rfileutils - "$root" "$work/names.json" "$work/external.json" "$work/cache-recipes.json" <<'RUBY'
root, names_file, external_file, recipes_file = ARGV
names = JSON.parse(File.read(names_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
external = JSON.parse(File.read(external_file)).flat_map { |n| [n, n.tr('-', '_'), "lib#{n.tr('-', '_')}"] }.uniq
manifests = JSON.parse(File.read(recipes_file)).map { |r| r.fetch('manifest') }.uniq
allowed = JSON.parse(File.read("#{root}/rust_hft/workspaces.json")).fetch('workspaces').map { |w| w.fetch('manifest') }
manifests.each do |manifest|
  abort 'unadmitted target manifest' unless allowed.include?(manifest)
  directory = "#{root}/rust_hft/target/#{File.dirname(manifest).gsub('/', '--')}"
  abort 'invalid cache target' if File.symlink?(directory) || (File.exist?(directory) && !File.directory?(directory))
  next unless File.directory?(directory)
  abort 'cache target symlink escaped owning workspace' unless File.realpath(directory) == directory
  Dir.children(directory).each do |name|
    FileUtils.rm_rf("#{directory}/#{name}") unless %w[debug release x86_64-unknown-linux-gnu].include?(name)
  end
  triple = "#{directory}/x86_64-unknown-linux-gnu"
  abort 'cache target triple symlink escaped owning workspace' if File.symlink?(triple)
  if File.exist?(triple)
    abort 'cache target triple escaped owning workspace' unless File.directory?(triple) && File.realpath(triple) == triple
    Dir.children(triple).each { |name| FileUtils.rm_rf("#{triple}/#{name}") unless name == 'release' }
  end
  ["#{directory}/debug", "#{directory}/release", "#{directory}/x86_64-unknown-linux-gnu/release"].each do |profile|
    abort 'invalid cache profile' if File.symlink?(profile) || (File.exist?(profile) && !File.directory?(profile))
    next unless File.directory?(profile)
    abort 'profile symlink escaped target' unless File.realpath(profile) == profile
    %w[build .fingerprint deps incremental].each do |kind|
      path = "#{profile}/#{kind}"
      abort 'invalid artifact directory or symlink escaped target' if File.symlink?(path) || (File.exist?(path) && !File.directory?(path))
    end
    # Executables already went into the immutable release; cache no local bytes.
    Dir.children(profile).each { |n| FileUtils.rm_rf("#{profile}/#{n}") unless %w[build .fingerprint deps].include?(n) }
    %w[build .fingerprint deps].each do |kind|
      path = "#{profile}/#{kind}"
      next unless File.directory?(path)
      abort 'artifact directory symlink escaped target' unless File.realpath(path) == path
      Dir.children(path).each do |n|
        # Cargo appends a hash after the complete package or target name.
        # A local target named data must not match the data-encoding dependency.
        package = n.rpartition('-').first
        # Removed local packages are absent from current metadata. Reject all
        # unknown bytes, with local names taking priority over external aliases.
        FileUtils.rm_rf("#{path}/#{n}") if names.include?(package) || !external.include?(package)
      end
      Dir.glob("#{path}/**/*", File::FNM_DOTMATCH).each do |artifact|
        next unless File.file?(artifact) && File.executable?(artifact)
        next if artifact.match?(/\.(?:so(?:\.[0-9.]+)?|dylib|dll)\z/)
        package = artifact.delete_prefix("#{path}/").split('/').first.rpartition('-').first
        # Native code generators in out/ are also external build outputs. Cargo
        # can mark the build script fresh and still require those programs.
        next if kind == 'build' && external.include?(package)
        FileUtils.rm_f(artifact)
      end
    end
  end
end
RUBY
    ;;
  *) exit 2 ;;
esac
