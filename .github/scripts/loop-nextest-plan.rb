#!/usr/bin/env ruby
# Move measured outliers from their hash shard. Preserve every test identity.
require 'json'

def tests(path)
  JSON.parse(File.read(path)).fetch('rust-suites').flat_map do |id, suite|
    next [] unless [nil, 'listed'].include?(suite['status'])
    suite.fetch('testcases').filter_map do |name, test|
      [id, name] if !test['ignored'] && test.fetch('filter-match').fetch('status') == 'matches'
    end
  end.sort
end

def exact(value)
  value.gsub(/([\\),])/) { |char| "\\#{char}" }.gsub("\n", '\\n').gsub("\r", '\\r').gsub("\t", '\\t')
end

work = ARGV.fetch(0)
full = tests("#{work}/nextest-list.json")
shards = (1..4).map { |i| tests("#{work}/hash-#{i}.json") }
abort 'hash partitions overlap or omit tests' unless shards.flatten(1).sort == full && full.uniq == full
mode = ENV.fetch('LOOP_PARTITION_MODE', 'balanced-v1')
abort 'invalid partition mode' unless %w[balanced-v1 hash].include?(mode)
# Run 37731276078: shard 2 has 2511 test-seconds; other shards have 856–966.
# These three tests account for 404, 378 and 358 seconds in that shard.
# Exact identity matching makes renamed or absent outliers ordinary hash tests.
{
  'mission_campaign::tests::execute_retains_paired_mlp_training_diagnostics' => 1,
  'mission_campaign::tests::execute_native_prepared_development_retains_exact_round_readbacks' => 3,
  'representation_plan::tests::review_production_tool_closure_routes_readers_and_model_contracts' => 4
}.each do |name, destination|
  next if mode == 'hash'
  matched = full.select { |_, test| test == name }
  abort "ambiguous outlier #{name}" if matched.length > 1
  next if matched.empty?
  identity = matched.fetch(0)
  # Only rebalance the measured overloaded shard. Source drift can change hashes.
  next unless shards.fetch(1).include?(identity)
  shards.fetch(1).delete(identity)
  shards.fetch(destination - 1) << identity
end
abort 'balanced partitions overlap or omit tests' unless shards.flatten(1).sort == full
shards.each_with_index do |identities, index|
  # Group predicates by binary to keep the command below Linux's argument limit.
  expression = identities.group_by(&:first).sort.map do |id, rows|
    "(binary_id(=#{exact(id)}) & (#{rows.sort.map { |_, name| "test(=#{exact(name)})" }.join(' | ')}))"
  end.join(' | ')
  expression = 'none()' if expression.empty?
  abort 'filterset exceeds safe argument size' if expression.bytesize > 100_000
  File.write("#{work}/filter-#{index + 1}.txt", expression)
end
expected = JSON.parse(File.read("#{work}/expected-counts.json"))
expected['partition'] = 'balanced-v1'
expected['mode'] = mode
expected['tests'] = full
expected['shard_tests'] = shards.map(&:sort)
File.write("#{work}/expected-counts.json", JSON.pretty_generate(expected) + "\n")
