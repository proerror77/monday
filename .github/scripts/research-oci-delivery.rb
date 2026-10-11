#!/usr/bin/env ruby
# Software delivery evidence only. No OSS credentials, grants or registry tags.
require 'json'
require 'digest'
require 'tmpdir'
require 'timeout'
require 'time'

module ResearchOciDelivery
  REPOSITORIES = {'cex-runner'=>'research-runner', 'controller'=>'campaign-cycle-controller',
                  'prediction-runner'=>'prediction-research-runner'}.freeze
  MAX_BYTES = 1_048_576

  class Reader
    def initialize(repository)
      raise 'invalid repository' unless repository.match?(%r{\A[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\z})
      @repository = repository
      @deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 180
    end

    def command(*args, limit: MAX_BYTES * 8)
      remaining = @deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
      raise 'OCI evidence deadline exhausted' unless remaining.positive?
      data = +''
      IO.pipe do |reader, writer|
        pid = Process.spawn(*args, out: writer, err: File::NULL, pgroup: true)
        writer.close
        begin
          Timeout.timeout([remaining, 45].min) do
            loop do
              data << reader.readpartial(65_536)
              raise 'OCI evidence response too large' if data.bytesize > limit
            rescue EOFError
              break
            end
            _, status = Process.wait2(pid)
            raise 'OCI evidence request failed' unless status.success?
          end
        rescue StandardError
          Process.kill('KILL', -pid) rescue Errno::ESRCH
          Process.wait(pid) rescue Errno::ECHILD
          raise
        end
      end
      data
    end

    def api(path, collection = nil)
      args = ['gh', 'api', '--method', 'GET']
      args += ['--paginate', '--slurp'] if collection
      result = JSON.parse(command(*args, "repos/#{@repository}/#{path}"))
      return result unless collection
      raise 'invalid OCI evidence pages' unless result.is_a?(Array) && !result.empty? &&
        result.all? { |p| p.is_a?(Hash) && p[collection].is_a?(Array) && p['total_count'].is_a?(Integer) }
      entries = result.flat_map { |p| p.fetch(collection) }
      ids = entries.map { |e| e.is_a?(Hash) && e['id'] }
      raise 'incomplete OCI evidence pages' unless result.all? { |p| p['total_count'] == entries.size } &&
        ids.all? { |id| id.is_a?(Integer) && id.positive? } && ids.uniq == ids
      entries
    end

    def run_valid?(run, source)
      run['head_sha'] == source && run['head_branch'] == 'main' &&
        run.dig('head_repository','full_name') == @repository &&
        run['path'] == '.github/workflows/acr-publish.yml' &&
        %w[workflow_run workflow_dispatch].include?(run['event']) &&
        run['id'].is_a?(Integer) && run['id'].positive? &&
        run['run_attempt'].is_a?(Integer) && run['run_attempt'].between?(1,100)
    end

    def jobs(run, attempt, source)
      entries = api("actions/runs/#{run}/attempts/#{attempt}/jobs?per_page=100", 'jobs')
      raise 'OCI job identity mismatch' unless entries.all? { |j| j['run_id']==run &&
        j['run_attempt']==attempt && j['head_sha']==source && j['name'].is_a?(String) }
      entries
    end

    def candidates(source, current: false)
      if current
        run = api("actions/runs/#{Integer(ENV.fetch('GITHUB_RUN_ID'))}")
        raise 'current OCI run identity mismatch' unless run_valid?(run, source) &&
          run['run_attempt'] == Integer(ENV.fetch('GITHUB_RUN_ATTEMPT'))
        return [[run, run['run_attempt']]]
      end
      # A failed signed archive does not erase a successful image job.
      migration = command('git','log','-1','--format=%ct','-G','monday.research-products.v2',
                          '--','.github/scripts/research-release-products.json').strip
      since = Time.at(Integer(migration)).utc.strftime('%Y-%m-%dT%H:%M:%SZ')
      runs = api("actions/workflows/acr-publish.yml/runs?branch=main&status=completed&head_sha=#{source}&created=%3E%3D#{since}&per_page=100", 'workflow_runs')
      runs.each { |r| raise 'history run identity mismatch' unless run_valid?(r,source) && r['status']=='completed' }
      pairs = runs.sort_by { |r| -r['id'] }.flat_map { |r| r['run_attempt'].downto(1).map { |a| [r,a] } }
      # During an Actions rerun, the run itself is in progress. Inspect previous
      # attempts explicitly; never attribute their jobs to the new attempt.
      if ENV['GITHUB_RUN_ID'] && ENV['GITHUB_RUN_ATTEMPT'].to_i > 1
        run = api("actions/runs/#{Integer(ENV.fetch('GITHUB_RUN_ID'))}")
        raise 'rerun identity mismatch' unless run_valid?(run,source)
        (Integer(ENV.fetch('GITHUB_RUN_ATTEMPT'))-1).downto(1) do |attempt|
          pairs << [run,attempt] unless pairs.any? { |r,a| r['id']==run['id'] && a==attempt }
        end
      end
      pairs
    end

    def receipt(run, attempt, job, source, product, image_repository)
      name = "research-oci-delivery-#{product}-#{source}-#{attempt}"
      artifacts = api("actions/runs/#{run['id']}/artifacts?per_page=100", 'artifacts').select { |a| a['name']==name }
      raise 'missing or ambiguous OCI receipt' unless artifacts.size==1
      artifact = artifacts.first
      raise 'expired or foreign OCI receipt' unless artifact['expired']==false &&
        artifact['size_in_bytes'].is_a?(Integer) && artifact['size_in_bytes'].between?(1,MAX_BYTES) &&
        artifact.dig('workflow_run','id')==run['id'] && artifact.dig('workflow_run','head_sha')==source
      receipt = Dir.mktmpdir('research-oci-receipt') do |dir|
        zip = File.join(dir,'receipt.zip')
        File.binwrite(zip,command('gh','api','--method','GET',"repos/#{@repository}/actions/artifacts/#{artifact['id']}/zip",limit: MAX_BYTES))
        raise 'unexpected OCI receipt files' unless command('unzip','-Z1',zip).lines.map(&:strip)==['research-oci-delivery.json']
        JSON.parse(command('unzip','-p',zip,'research-oci-delivery.json',limit: MAX_BYTES))
      end
      validate(receipt,run['id'],attempt,job['id'],source,product,image_repository)
      reread = api("actions/runs/#{run['id']}")
      raise 'OCI receipt run changed during read' unless run_valid?(reread,source) && reread['run_attempt']==run['run_attempt']
      again = jobs(run['id'],attempt,source).find { |j| j['id']==job['id'] }
      raise 'OCI receipt job changed during read' unless again==job
      receipt
    end

    def validate(receipt, run, attempt, job, source, product, image_repository)
      raise 'OCI delivery receipt identity mismatch' unless receipt.is_a?(Hash) &&
        receipt['schema']=='monday.research-oci-delivery.v1' && receipt['repository']==@repository &&
        receipt['source_sha']==source && receipt['product']==product &&
        receipt['delivery']=={'run_id'=>run,'run_attempt'=>attempt,'job_id'=>job} &&
        receipt['image_repository']==image_repository &&
        receipt['image'].is_a?(String) && receipt['image'].match?(%r{\A#{Regexp.escape(image_repository)}@sha256:[0-9a-f]{64}\z}) &&
        receipt['oci_config_digest'].is_a?(String) && receipt['oci_config_digest'].match?(/\Asha256:[0-9a-f]{64}\z/) &&
        receipt['software_manifest_sha256'].is_a?(String) && receipt['software_manifest_sha256'].match?(/\A[0-9a-f]{64}\z/) &&
        receipt['software'].is_a?(Hash) && %w[run_id run_attempt job_id].all? { |k| receipt['software'][k].is_a?(Integer) && receipt['software'][k].positive? }
      receipt
    end

    def find(source, product, image_repository, current: false)
      candidates(source,current: current).each do |run,attempt|
        found = jobs(run['id'],attempt,source).select { |j| j['name']=="Publish OCI #{REPOSITORIES.fetch(product)}" }
        raise 'ambiguous OCI producer job' if found.size>1
        job = found.first
        next unless job && job['status']=='completed' && job['conclusion']=='success'
        return receipt(run,attempt,job,source,product,image_repository)
      end
      raise 'this product OCI delivery is incomplete' if current
      nil
    end

    def archive_started?(source, product)
      policy = JSON.parse(ENV.fetch('MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY'))
      current_id = Integer(ENV.fetch('GITHUB_RUN_ID'))
      current_number = Integer(ENV.fetch('GITHUB_RUN_NUMBER'))
      current_attempt = Integer(ENV.fetch('GITHUB_RUN_ATTEMPT'))
      anchor_id = policy.fetch('history_anchor_run_id')
      anchor_number = policy.fetch('history_anchor_run_number')
      # Read every workflow ordinal, including queued/failed/non-main runs.
      # A missing/deleted visible ordinal never means unspent authority. This
      # requires administrators to preserve monotonic history in the approved
      # window; a deleted higher tail cannot be proved from Actions alone.
      runs = api('actions/workflows/acr-publish.yml/runs?per_page=100','workflow_runs')
      raise 'publication history has unknown ordinals' unless runs.all? { |r|
        r['run_number'].is_a?(Integer) && r['run_number'].positive? &&
        r['path']=='.github/workflows/acr-publish.yml' && r.dig('head_repository','full_name')==@repository }
      window = runs.select { |r| r['run_number']>=anchor_number }
      ordinals = window.map { |r| r['run_number'] }.sort
      raise 'publication history is incomplete, deleted or a newer run exists' unless
        anchor_number<=current_number && ordinals==(anchor_number..current_number).to_a
      anchor = window.find { |r| r['run_number']==anchor_number }
      current = window.find { |r| r['id']==current_id }
      raise 'publication anchor or current run differs' unless anchor && anchor['id']==anchor_id &&
        run_valid?(anchor,anchor['head_sha']) && anchor['status']=='completed' &&
        Time.iso8601(anchor.fetch('created_at')).to_i<=policy.fetch('not_before') &&
        current && current['run_number']==current_number && run_valid?(current,source) &&
        current['run_attempt']==current_attempt && current['status']=='in_progress'
      # Approval starts after an existing source anchor, so earlier sources
      # cannot receive fresh authority by moving the history query boundary.
      raise 'operating allowance requires a new source after its anchor' if anchor['head_sha']==source
      command('git','merge-base','--is-ancestor',anchor.fetch('head_sha'),source)
      # Retained earlier same-source runs also count. Moving an approval anchor
      # must not erase their consumption.
      runs.select { |r| r['head_sha']==source }.any? do |run|
        raise 'archive history run identity mismatch' unless run_valid?(run,source)
        unless run['id']==current_id
          raise 'prior archive run is not demonstrably terminal' unless run['status']=='completed' &&
            %w[success failure cancelled timed_out action_required neutral skipped stale startup_failure].include?(run['conclusion'])
        end
        run['run_attempt'].downto(1).any? do |attempt|
          next false if run['id']==current_id && attempt==current_attempt
          entries = jobs(run['id'],attempt,source)
          raise 'prior archive jobs are unavailable' if entries.empty?
          entries.any? { |j| j['name']=="Publish #{REPOSITORIES.fetch(product)}" &&
            !(j['status']=='completed' && j['conclusion']=='skipped') }
        end
      end
    end

    def record(source, product, image_repository)
      run_id = Integer(ENV.fetch('GITHUB_RUN_ID')); attempt = Integer(ENV.fetch('GITHUB_RUN_ATTEMPT'))
      run = api("actions/runs/#{run_id}")
      raise 'delivery run identity mismatch' unless run_valid?(run,source) && run['run_attempt']==attempt && run['status']=='in_progress'
      matches = jobs(run_id,attempt,source).select { |j| j['name']=="Publish OCI #{REPOSITORIES.fetch(product)}" }
      raise 'ambiguous active OCI job' unless matches.size==1 && matches.first['status']=='in_progress'
      root = ENV.fetch('RUNNER_TEMP')
      record = JSON.parse(File.read(File.join(root,'acr-release-record.json')))
      manifest_path = File.join(root,'research-release','research-image-release.json')
      software = JSON.parse(File.read(manifest_path))
      raise 'verified ACR record differs' unless record['source_sha']==source &&
        record['run_id']==run_id.to_s && record['run_attempt']==attempt.to_s && software['source_sha']==source
      receipt = {'schema'=>'monday.research-oci-delivery.v1','repository'=>@repository,
        'source_sha'=>source,'product'=>product,'image_repository'=>image_repository,
        'image'=>record.fetch('published_image'),'oci_config_digest'=>record.fetch('base_image_digest'),
        'delivery'=>{'run_id'=>run_id,'run_attempt'=>attempt,'job_id'=>matches.first.fetch('id')},
        'software'=>{'run_id'=>Integer(software.fetch('workflow_run_id')),
          'run_attempt'=>software.fetch('workflow_run_attempt'),'job_id'=>software.fetch('workflow_job_id')},
        'software_manifest_sha256'=>Digest::SHA256.file(manifest_path).hexdigest}
      prior = File.join(root,'research-oci-prior.json')
      receipt['reused_delivery'] = JSON.parse(File.read(prior)).fetch('delivery') if File.file?(prior)
      validate(receipt,run_id,attempt,matches.first['id'],source,product,image_repository)
    end
  end

  def self.main(args)
    mode, source, product, image_repository, output, env_output = args
    raise 'invalid OCI delivery arguments' unless %w[record reuse current archive-history].include?(mode) &&
      source&.match?(/\A[0-9a-f]{40}\z/) && REPOSITORIES.key?(product) &&
      image_repository&.match?(%r{\A[a-z0-9.-]+/wildcard0923/#{REPOSITORIES.fetch(product)}\z})
    reader = Reader.new(ENV.fetch('GITHUB_REPOSITORY'))
    if mode=='archive-history'
      raise 'previous signed archive requires reconciliation; no allowance refill' if reader.archive_started?(source,product)
      return
    end
    receipt = mode=='record' ? reader.record(source,product,image_repository) :
      reader.find(source,product,image_repository,current: mode=='current')
    if receipt
      File.write(output,JSON.pretty_generate(receipt)+"\n",mode: File::WRONLY|File::CREAT|File::EXCL)
      File.open(env_output,'a') { |f| f.puts "image=#{receipt.fetch('image')}" } if env_output
    end
  rescue StandardError => e
    # Do not expose subprocess stderr, token/header diagnostics or receipt data.
    warn "Research OCI evidence rejected: #{e.message}"
    exit 1
  end
end

ResearchOciDelivery.main(ARGV) if $PROGRAM_NAME == __FILE__
