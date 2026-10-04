#!/usr/bin/env ruby
# Static Docker checks need no daemon, image download, or Rust compilation.
require 'json'
require 'open3'
require 'set'
require 'shellwords'

ROOT = File.expand_path('../..', __dir__)
RUST = File.join(ROOT, 'rust_hft')
REGISTRY = JSON.parse(File.read(File.join(RUST, 'workspaces.json')))
ENTRANCES = REGISTRY.fetch('workspaces').map { |owner| owner.fetch('manifest') }.to_set
UNIMPLEMENTED = {
  'rust_hft/deployment/docker/Dockerfile.market-data' => 'adapter-${EXCHANGE}',
  'rust_hft/deployment/docker/Dockerfile.sentinel' => 'hft-sentinel'
}.freeze

def tracked(pattern)
  output, status = Open3.capture2('git', '-C', ROOT, 'ls-files', pattern)
  abort 'cannot read tracked Docker inputs' unless status.success?
  output.lines.map(&:strip)
end

# Cargo metadata verifies this index independently in the owning CI contract.
packages = {}
tracked('rust_hft/**/Cargo.toml').each do |relative|
  file = File.join(ROOT, relative)
  text = File.read(file)
  block = text.split(/^\[package\]\s*$/, 2)[1]
  next unless block
  block = block.split(/^\[/, 2)[0]
  name = block[/^name\s*=\s*"([^"]+)"/, 1]
  next unless name
  declared = block[/^workspace\s*=\s*"([^"]+)"/, 1]
  owner = if declared
            File.expand_path(File.join(declared, 'Cargo.toml'), File.dirname(file))
          elsif text.match?(/^\[workspace\]/)
            file
          elsif relative.start_with?('rust_hft/prediction-markets/')
            File.join(RUST, 'prediction-markets/Cargo.toml')
          else
            abort "package has no explicit owner: #{relative}"
          end
  owner = owner.delete_prefix("#{RUST}/")
  abort "unregistered owner for #{name}" unless ENTRANCES.include?(owner)
  abort "duplicate package #{name}" if packages.key?(name)
  packages[name] = owner
end

def check_recipe(path, source, packages)
  cwd = '/'
  env = {}
  directories = Set.new
  stages = {}
  stage = nil
  rust_root = nil
  builder_source = ''
  commands = 0
  source.gsub(/\\\r?\n\s*/, ' ').lines.each do |line|
    next if line.lstrip.start_with?('#') || line.strip.empty?
    instruction, body = line.strip.split(/\s+/, 2)
    body ||= ''
    case instruction
    when 'FROM'
      stages[stage] = {cwd: cwd, env: env.dup, rust_root: rust_root} if stage
      tokens = Shellwords.split(body)
      parent = stages[tokens.first]
      cwd = parent ? parent.fetch(:cwd) : '/'
      env = parent ? parent.fetch(:env).dup : {}
      rust_root = parent && parent[:rust_root]
      stage = body[/\s+AS\s+(\S+)\z/i, 1]
    when 'WORKDIR'
      cwd = File.expand_path(body, cwd)
    when 'ENV'
      Shellwords.split(body).each do |word|
        key, value = word.split('=', 2)
        env[key] = value if value
      end
    when 'COPY'
      tokens = Shellwords.split(body)
      # This root is proved by the repository-directory COPY boundary. It is
      # inherited only by stages based on this source stage.
      if tokens.length == 2 && tokens.first == 'rust_hft/'
        rust_root = File.expand_path(tokens.last, cwd)
      end
    when 'RUN'
      builder_source += "\n#{body}"
      if body.include?('cargo-scoped.sh') || body.include?('workspace-metadata.sh')
        raise "#{path}: metadata helper requires builder jq" unless builder_source.match?(/\bjq\b/)
      end
      body.to_enum(:scan, /\bcargo(?:\s+--config\s+'[^']*')?\s+(build|fetch|test)\b/).each do
        match = Regexp.last_match
        prefix = body[0...match.begin(0)]
        tokens = Shellwords.split(body[match.begin(0)..-1].split(/\s+&&\s+/, 2)[0])
        position = tokens.index('--manifest-path')
        raise "#{path}: Cargo command has no owning manifest" unless position
        manifest = tokens.fetch(position + 1)
        target = prefix.scan(/\bCARGO_TARGET_DIR=([^\s]+)/).last
        target = target ? target.first : env['CARGO_TARGET_DIR']
        raise "#{path}: Cargo target directory is implicit" unless target && target.start_with?('/')
        directories << target
        selections = tokens.each_index.select { |i| %w[-p --package].include?(tokens[i]) }.map { |i| tokens.fetch(i + 1) }
        if manifest == '$owner'
          raise "#{path}: unknown Docker TARGET is admitted" unless body.include?('exit 2')
          routes = {}
          body.scan(/([a-z|]+)\)\s+owner=([a-z-]+\/Cargo\.toml)/).each do |labels, owner|
            labels.split('|').each { |label| routes[label] = owner }
          end
          {'hft-live' => 'runtime/Cargo.toml', 'hft-paper' => 'runtime/Cargo.toml',
           'hft-collector' => 'data-pipelines/Cargo.toml'}.each do |package, expected|
            raise "#{path}: wrong dynamic owner for #{package}" unless packages[package] == expected && routes[package.delete_prefix('hft-')] == expected
          end
          raise "#{path}: dynamic Docker target builds extra binaries" unless tokens.include?('--bin') && tokens.include?('hft-${TARGET}')
        else
          absolute_manifest = File.expand_path(manifest, cwd)
          manifest = if rust_root && absolute_manifest.start_with?("#{rust_root}/")
                       absolute_manifest.delete_prefix("#{rust_root}/")
                     else
                       absolute_manifest.sub(%r{\A/(?:app|work)/}, '')
                     end
          raise "#{path}: unregistered Docker manifest #{manifest}" unless ENTRANCES.include?(manifest)
          selections.each do |package|
            raise "#{path}: #{package} has wrong owner #{manifest}" unless packages[package] == manifest
          end
        end
        raise "#{path}: Cargo resolution is unlocked" unless tokens.include?('--locked')
        commands += 1
      end
    end
    # Binary copies must use a directory that an earlier Cargo command bound.
    body.scan(%r{(?:/[A-Za-z0-9_.-]+)*/?target(?:/[A-Za-z0-9_.-]+)*/release/[^\s]+}).each do |artifact|
      absolute = File.expand_path(artifact, cwd)
      raise "#{path}: binary copy uses unbound target #{artifact}" unless directories.any? { |dir| absolute.start_with?("#{dir}/release/") }
    end
  end
  raise "#{path}: no checked Cargo build/fetch/test command" if commands.zero?
end

recipes = tracked('*Dockerfile*').select do |path|
  !path.include?('/docs/archive/') && File.read(File.join(ROOT, path)).match?(/\bcargo\s+(?:--config\s+'[^']*'\s+)?(?:build|fetch|test)\b/)
end
checked = 0
recipes.each do |path|
  source = File.read(File.join(ROOT, path))
  if UNIMPLEMENTED.key?(path)
    missing = UNIMPLEMENTED.fetch(path)
    default_exchange = source[/^ARG EXCHANGE=([^\s]+)/, 1]
    literal_package = missing.sub('${EXCHANGE}', default_exchange || '')
    abort "legacy classification became stale: #{path}" if packages.key?(literal_package)
    puts "UNIMPLEMENTED existing recipe: #{path} references #{missing}; no executable target exists"
    next
  end
  check_recipe(path, source, packages)
  checked += 1
end

rejected = 0
if ARGV == ['--self-test']
  path = 'rust_hft/docker/Dockerfile'
  source = File.read(File.join(ROOT, path))
  mutations = {
    'old copy path' => source.sub('CARGO_TARGET_DIR=/work/target', 'CARGO_TARGET_DIR=/work/runtime/target'),
    'implicit owner' => source.sub('--manifest-path runtime/Cargo.toml', ''),
    'missing builder jq' => source.sub("cargo --config 'build.rustc-wrapper=\"\"' build", './scripts/cargo-scoped.sh build')
  }
  mutations.each do |name, changed|
    abort "invalid mutation: #{name}" if changed == source
    begin
      check_recipe(path, changed, packages)
    rescue RuntimeError
      rejected += 1
      next
    end
    abort "invalid Docker contract accepted: #{name}"
  end
  dynamic_path = 'deploy/Dockerfile.hft'
  dynamic = File.read(File.join(ROOT, dynamic_path))
  swapped = dynamic.sub('live|paper) owner=runtime/Cargo.toml', 'live|paper) owner=data-pipelines/Cargo.toml')
  begin
    check_recipe(dynamic_path, swapped, packages)
  rescue RuntimeError
    rejected += 1
  else
    abort 'swapped dynamic Docker owner was admitted'
  end
  data_path = 'deployment/aliyun/research/Dockerfile.research-data'
  data_source = File.read(File.join(ROOT, data_path))
  {
    'wrong data owner' => data_source.sub('--manifest-path data-pipelines/Cargo.toml --release', '--manifest-path runtime/Cargo.toml --release'),
    'wrong test owner' => data_source.sub('cargo test --manifest-path shared/Cargo.toml', 'cargo test --manifest-path runtime/Cargo.toml'),
    'missing test owner' => data_source.sub('cargo test --manifest-path shared/Cargo.toml', 'cargo test'),
    'missing source COPY' => data_source.sub('COPY rust_hft/ rust_hft/', 'COPY unrelated/ rust_hft/')
  }.each do |name, changed|
    abort "invalid data recipe mutation: #{name}" if changed == data_source
    begin
      check_recipe(data_path, changed, packages)
    rescue RuntimeError
      rejected += 1
      next
    end
    abort "invalid data Docker contract accepted: #{name}"
  end
elsif !ARGV.empty?
  abort 'usage: test-rust-docker-workspaces.rb [--self-test]'
end
puts "PASS: #{checked} current Cargo Docker recipes bind owner, locked resolution, target copies and helper tools; #{rejected} failure cases rejected"
