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
    python3 - "$root" "$work/validated.json" >"$work/owner-profiles.json" <<'PYTHON'
"""Bind selected owning-workspace profiles without hashing dependency declarations."""

import json
from pathlib import Path
import sys
import tomllib


def profile_inputs(root: Path, inputs: dict) -> dict:
    registry = json.loads((root / "rust_hft/workspaces.json").read_text())
    if registry.get("schema") != "monday.cargo_workspaces.v1":
        raise ValueError("unsupported workspace registry")
    owners = registry["workspaces"]
    if not owners or any(set(owner) != {"id", "manifest"} for owner in owners):
        raise ValueError("unsupported workspace entry")
    by_manifest = {owner["manifest"]: owner for owner in owners}
    if len(by_manifest) != len(owners) or len({owner["id"] for owner in owners}) != len(owners):
        raise ValueError("duplicate workspace entry")
    selected = sorted({recipe["manifest"] for recipe in inputs["recipes"]})
    if not selected or any(manifest not in by_manifest for manifest in selected):
        raise ValueError("unadmitted cache owner")
    profiles = {}
    for manifest in selected:
        path = root / "rust_hft" / manifest
        if not path.resolve().is_relative_to((root / "rust_hft").resolve()):
            raise ValueError("cache owner escaped repository")
        with path.open("rb") as source:
            parsed = tomllib.load(source)
        if not isinstance(parsed.get("workspace"), dict) or not isinstance(parsed.get("profile", {}), dict):
            raise ValueError("cache owner is not a standalone workspace with valid profiles")
        # Every custom profile, inherited profile, package override and build
        # override remains bound. Cargo resolves dependencies from the exact key.
        profiles[manifest] = parsed.get("profile", {})
    return {"schema": "monday.cargo-cache-profiles.v1",
            "owners": [by_manifest[manifest] for manifest in selected], "profiles": profiles}


if __name__ == "__main__":
    try:
        result = profile_inputs(Path(sys.argv[1]), json.loads(Path(sys.argv[2]).read_text()))
        output = json.dumps(result, sort_keys=True, separators=(",", ":"), allow_nan=False)
    except (ValueError, KeyError, TypeError, OSError, IndexError) as error:
        sys.exit(f"invalid Cargo cache profile inputs: {error}")
    print(output)
PYTHON
    jq -Se --slurpfile owner_profiles "$work/owner-profiles.json" '
      {schema:"monday.dependency-cache-compat.v2",target,profile,compiler,native,flags,
        builder_image,recipe,recipes,owner_profiles:$owner_profiles[0]}' "$work/validated.json"
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
    manifests=$(jq -er '.recipes|map(.manifest)|unique | if length>0 then .[] else error("empty cache layout") end' "${2:?inputs required}")
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
    while IFS= read -r manifest; do
      cargo metadata --manifest-path "$root/rust_hft/$manifest" --locked --no-deps --format-version 1 >>"$work/local.jsonl"
    done < <(jq -er '.workspaces[].manifest' "$root/rust_hft/workspaces.json")
    # Include any local path/patch dependencies outside workspace membership.
    # Metadata uses each recipe's exact features; this never compiles a union.
    recipes=$(jq -ec '.recipes[]' "${2:?inputs required}")
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
    %w[build .fingerprint deps incremental].each { |kind| abort 'artifact directory symlink escaped target' if File.symlink?("#{profile}/#{kind}") }
    FileUtils.rm_rf("#{profile}/incremental")
    # Executables already went into the immutable release; cache no local bytes.
    Dir.children(profile).each { |n| FileUtils.rm_f("#{profile}/#{n}") if File.file?("#{profile}/#{n}") || File.symlink?("#{profile}/#{n}") }
    %w[build .fingerprint deps].each do |kind|
      path = "#{profile}/#{kind}"
      next unless File.directory?(path)
      abort 'artifact directory symlink escaped target' unless File.realpath(path) == path
      Dir.children(path).each do |n|
        # Cargo appends a hash after the complete package or target name.
        # A local target named data must not match the data-encoding dependency.
        FileUtils.rm_rf("#{path}/#{n}") if names.include?(n.rpartition('-').first)
      end
    end
  end
end
RUBY
    ;;
  *) exit 2 ;;
esac
