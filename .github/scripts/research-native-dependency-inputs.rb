#!/usr/bin/env ruby
# Cargo resolves native features. This identity excludes application source and packaging scripts.
require 'json'
require 'open3'
require 'digest'

root, recipes_file = ARGV
abort 'root and recipes required' unless root && recipes_file
$LOAD_PATH.unshift("#{root}/.github/scripts/vendor/tomlrb/lib")
require 'tomlrb'
records = JSON.parse(File.read(recipes_file)).flat_map do |recipe|
  manifest = recipe.fetch('manifest')
  args = ['cargo', 'metadata', '--manifest-path', "#{root}/rust_hft/#{manifest}",
          '--locked', '--format-version', '1', '--filter-platform', 'x86_64-unknown-linux-gnu']
  lock = Tomlrb.load_file("#{root}/rust_hft/#{File.dirname(manifest)}/Cargo.lock").fetch('package')
  features = recipe.fetch('features')
  unless features.empty?
    args += ['--features', features.split(',').map { |feature| "#{recipe.fetch('package')}/#{feature}" }.join(',')]
  end
  output, status = Open3.capture2(*args, chdir: "#{root}/rust_hft")
  abort "native dependency resolution failed: #{manifest}" unless status.success?
  metadata = JSON.parse(output)
  packages = metadata.fetch('packages').to_h { |package| [package.fetch('id'), package] }
  nodes = metadata.fetch('resolve').fetch('nodes').to_h { |node| [node.fetch('id'), node] }
  native_roots = packages.values.select { |package| package.fetch('name') == 'libduckdb-sys' }
  native_roots.map do |native|
    pending = [native.fetch('id')]
    visited = {}
    until pending.empty?
      id = pending.shift
      next if visited.key?(id)
      package = packages.fetch(id)
      node = nodes.fetch(id)
      source = package['source']
      checksum = nil
      if source&.start_with?('registry+')
        entry = lock.find { |item| item['name'] == package['name'] && item['version'] == package['version'] && item['source'] == source }
        checksum = entry&.fetch('checksum', nil)
        abort 'native registry source has no checksum' unless checksum&.match?(/\A[0-9a-f]{64}\z/)
      end
      # Registry versions and git revisions are immutable Cargo identities.
      # Local overrides also bind source bytes, including the native build script.
      local = nil
      unless source
        directory = File.dirname(package.fetch('manifest_path'))
        files = Dir.glob("#{directory}/**/*", File::FNM_DOTMATCH).select { |file| File.file?(file) }
        abort 'native local dependency contains a symlink' if files.any? { |file| File.symlink?(file) }
        local = files.sort.to_h { |file| [file.delete_prefix(directory + '/'), Digest::SHA256.file(file).hexdigest] }
      end
      visited[id] = { 'name' => package.fetch('name'), 'version' => package.fetch('version'),
                      'source' => source, 'checksum' => checksum,
                      'features' => node.fetch('features').sort, 'local' => local }
      pending.concat(node.fetch('dependencies'))
    end
    { 'owner' => manifest, 'packages' => visited.values.sort_by { |package| JSON.generate(package) } }
  end
end
puts JSON.generate({ 'schema' => 'monday.native-dependencies.v1', 'closures' => records.uniq.sort_by { |record| JSON.generate(record) } })
