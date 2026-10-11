#!/usr/bin/env ruby
# CI tooling provenance only; signing and independent PG import stay native Rust.
require 'json'
require 'digest'
require 'yaml'

ROOT = File.expand_path('../..', __dir__)
RECIPE = JSON.parse(File.read(File.join(__dir__, 'research-release-tools.json'))).freeze
MANIFEST = 'research-release-tools.json'
INPUTS = %w[
  .github/scripts/research-release-tools.json
  .github/scripts/build-research-release-tools.sh
  .github/scripts/research-release-tools-artifact.rb
  .github/scripts/research-release-bundle.rb
  .github/scripts/research-release-source-sha.sh
  rust_hft/research-core/platform/Cargo.toml
  rust_hft/research-core/platform/Cargo.lock
].freeze

def check(value, message)
  raise ArgumentError, message unless value
end

def input_hashes
  paths = INPUTS + %w[.cargo/config.toml rust_hft/.cargo/config.toml].select { |p| File.exist?(File.join(ROOT, p)) }
  paths.to_h { |path| [path, Digest::SHA256.file(File.join(ROOT, path)).hexdigest] }
end

def binaries(directory)
  check(File.lstat(directory).directory? && !File.symlink?(directory), 'invalid tools directory')
  folder = File.join(directory, 'release-tools')
  check(File.lstat(folder).directory? && !File.symlink?(folder), 'invalid binary directory')
  check(Dir.children(folder).sort == RECIPE.fetch('binaries'), 'unexpected tools file set')
  RECIPE.fetch('binaries').map do |name|
    path = File.join(folder, name)
    info = File.lstat(path)
    check(info.file? && info.mode & 0o777 == 0o755 && info.size.positive? && info.size <= 512 * 1024 * 1024,
      "unsafe tool: #{name}")
    { 'file' => name, 'sha256' => Digest::SHA256.file(path).hexdigest }
  end
end

def build_inputs_valid(inputs, workflow)
  check(RECIPE == {
    'schema' => 'monday.research-release-tools-recipe.v1',
    'manifest' => 'rust_hft/research-core/platform/Cargo.toml',
    'package' => 'hft-research-platform', 'features' => 'publisher',
    'default_features' => false, 'profile' => 'release',
    'target' => 'x86_64-unknown-linux-gnu',
    'binaries' => %w[research-release-capability research-release-publisher]
  }, 'unadmitted tools build recipe')
  check(%w[.github/workflows/ploy-ci.yml .github/workflows/acr-publish.yml].include?(workflow), 'unexpected producer workflow')
  jobs = YAML.safe_load(File.read(File.join(ROOT, workflow))).fetch('jobs').values.select { |j| j['name'] == 'Research release tools' }
  check(jobs.length == 1, 'ambiguous tools producer')
  builder = jobs.first.fetch('container').fetch('image')
  check(builder.match?(/@sha256:[0-9a-f]{64}\z/), 'unpinned tools builder')
  check(inputs.keys.sort == %w[builder_image compiler flags_sha256 inputs native_sha256 recipe].sort, 'invalid build input fields')
  check(inputs['builder_image'] == builder && inputs['recipe'] == RECIPE && inputs['inputs'] == input_hashes,
    'tools recipe, profile, lock, builder or source inputs differ')
  compiler = inputs.fetch('compiler')
  check(compiler.keys.sort == %w[cargo_sha256 cargo_version rustc_sha256 rustc_version sysroot_sha256].sort,
    'invalid compiler evidence')
  check(compiler['rustc_version'].start_with?("rustc 1.98.1 ") && compiler['cargo_version'].start_with?("cargo 1.98.1 "),
    'wrong tools compiler version')
  hashes = %w[cargo_sha256 rustc_sha256 sysroot_sha256].map { |key| compiler[key] } +
    %w[flags_sha256 native_sha256].map { |key| inputs[key] }
  check(hashes.all? { |value| value.is_a?(String) && value.match?(/\A[0-9a-f]{64}\z/) }, 'invalid compiler/native/flags hash')
end

begin
  mode, directory, source, run, attempt, job, workflow, evidence = ARGV
  check(%w[create verify].include?(mode) && [7, 8].include?(ARGV.length), 'expected create/verify DIRECTORY SOURCE RUN ATTEMPT JOB WORKFLOW [COMPILER_EVIDENCE]')
  check(source&.match?(/\A[0-9a-f]{40}\z/) && [run, attempt, job].all? { |n| n&.match?(/\A[1-9][0-9]*\z/) }, 'invalid source/producer identity')
  path = File.join(directory, MANIFEST)
  identity = { 'schema' => 'monday.research-release-tools.v1', 'source_sha' => source,
    'workflow_run_id' => run, 'workflow_run_attempt' => attempt.to_i,
    'workflow_job_id' => job.to_i, 'workflow_path' => workflow,
    'target' => RECIPE.fetch('target'), 'profile' => RECIPE.fetch('profile') }
  if mode == 'create'
    check(Dir.children(directory).sort == ['release-tools'], 'tools output already contains metadata or extra files')
    build = JSON.parse(File.read(evidence))
    build['inputs'] = input_hashes
    build['recipe'] = RECIPE
    build_inputs_valid(build, workflow)
    data = identity.merge('build_inputs' => build, 'binaries' => binaries(directory))
    File.write(path, JSON.pretty_generate(data) + "\n", mode: File::WRONLY | File::CREAT | File::EXCL, perm: 0o644)
  else
    check(Dir.children(directory).sort == [MANIFEST, 'release-tools'].sort, 'extra tools artifact files')
    info = File.lstat(path)
    check(info.file? && info.size.positive? && info.size <= 1024 * 1024, 'unsafe tools manifest')
    data = JSON.parse(File.read(path))
    check(data.keys.sort == (identity.keys + %w[build_inputs binaries]).sort, 'invalid tools manifest fields')
    check(identity.all? { |key, value| data[key] == value }, 'tools source/producer/profile mismatch')
    build_inputs_valid(data.fetch('build_inputs'), workflow)
    check(data.fetch('binaries') == binaries(directory), 'tools executable hash mismatch')
  end
rescue StandardError => error
  abort "release tools rejected: #{error.message}"
end
