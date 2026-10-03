#!/usr/bin/env ruby
# CI archive transport only. Scientific/runtime implementation remains Rust.
require 'rubygems/package'
require 'fileutils'

BINARIES = %w[hft-backtest alpha-harness lob-pit-materializer
  binance-market-tape-slicer binance-replay-parquet-materializer
  research-orchestrator researchctl research-prepare clickhouse-analytics-materializer
  monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot].freeze
FILES = { 'research-image-release.json' => [0o644, 1024 * 1024] }.merge(
  BINARIES.to_h { |name| ["research-bin/#{name}", [0o755, 512 * 1024 * 1024]] }
).freeze
MAX_BYTES = 1024 * 1024 * 1024

def check(condition, message)
  raise ArgumentError, message unless condition
end

def pack(archive, directory)
  check(!File.exist?(archive), 'bundle output already exists')
  actual = Dir.glob('**/*', File::FNM_DOTMATCH, base: directory).reject { |name| File.directory?(File.join(directory, name)) }
  check(actual.sort == FILES.keys.sort, 'release file set differs from fixed executable manifest')
  total = 0
  # Refuse links and special files before opening any input. CI has already
  # checked exact source/run/attempt/job and binary digests in the release.
  FILES.each do |name, (mode, maximum)|
    source = File.join(directory, name)
    info = File.lstat(source)
    check(info.file? && info.size.positive? && info.size <= maximum, "invalid release file: #{name}")
    check(!name.start_with?('research-bin/') || info.mode & 0o777 == mode, "release executable mode is not 0755: #{name}")
    total += info.size
  end
  check(total <= MAX_BYTES, 'release bundle exceeds byte budget')
  File.open(archive, File::WRONLY | File::CREAT | File::EXCL, 0o600) do |output|
    Gem::Package::TarWriter.new(output) do |bundle|
      FILES.each do |name, (mode, _maximum)|
        source = File.join(directory, name)
        bundle.add_file_simple(name, mode, File.size(source)) do |entry|
          File.open(source, 'rb') { |input| IO.copy_stream(input, entry) }
        end
      end
    end
  end
end

def read_bundle(archive)
  File.open(archive, 'rb') do |file|
    Gem::Package::TarReader.new(file) { |reader| yield reader }
  end
end

def unpack(archive, directory)
  info = File.lstat(archive)
  check(!File.exist?(directory) && info.file? && info.size.positive? && info.size <= MAX_BYTES + 32768, 'bundle/output boundary invalid')
  names = []
  total = 0
  # Inspect every header before creating output. Never extract archive paths,
  # ownership, links or metadata through a generic tar extraction API.
  read_bundle(archive) do |bundle|
    bundle.each do |entry|
      name = entry.full_name
      check(FILES.key?(name) && !names.include?(name), "unexpected/duplicate bundle path: #{name}")
      mode, maximum = FILES.fetch(name)
      check(entry.file? && entry.header.linkname.to_s.empty? && entry.header.mode == mode && entry.size.positive? && entry.size <= maximum,
        "unsafe bundle member: #{name}")
      total += entry.size
      check(total <= MAX_BYTES, 'release bundle exceeds byte budget')
      names << name
    end
  end
  check(names.sort == FILES.keys.sort, 'bundle file set is not the exact release manifest')
  FileUtils.mkdir_p(directory, mode: 0o700)
  Dir.mkdir(File.join(directory, 'research-bin'), 0o700)
  read_bundle(archive) do |bundle|
    bundle.each do |entry|
      destination = File.join(directory, entry.full_name)
      mode = FILES.fetch(entry.full_name).first
      File.open(destination, File::WRONLY | File::CREAT | File::EXCL | File::NOFOLLOW, 0o600) do |output|
        remaining = entry.size
        while remaining.positive?
          bytes = entry.read([remaining, 1024 * 1024].min)
          check(bytes && !bytes.empty?, "truncated release member: #{entry.full_name}")
          output.write(bytes)
          remaining -= bytes.bytesize
        end
      end
      File.chmod(mode, destination)
    end
  end
end

begin
  mode, archive, directory = ARGV
  check(ARGV.length == 3, 'expected pack/unpack ARCHIVE DIRECTORY')
  case mode
  when 'pack' then pack(archive, directory)
  when 'unpack' then unpack(archive, directory)
  else raise ArgumentError, 'expected pack or unpack'
  end
rescue StandardError => error
  abort "research release bundle rejected: #{error.message}"
end
