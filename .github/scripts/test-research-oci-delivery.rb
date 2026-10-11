#!/usr/bin/env ruby
require 'minitest/autorun'
require_relative 'research-oci-delivery'

class ResearchOciDeliveryTest < Minitest::Test
  SOURCE = 'a'*40
  IMAGE_REPOSITORY = 'fixture.invalid/wildcard0923/research-runner'

  class FixtureReader < ResearchOciDelivery::Reader
    attr_accessor :responses, :calls
    def initialize
      super('fixture/repo')
      @responses = {}; @calls = []
    end
    def command(*args, limit: ResearchOciDelivery::MAX_BYTES*8)
      return '1791680000' if args[0..2]==['git','log','-1']
      return '' if args[0..2]==['git','merge-base','--is-ancestor']
      return super unless args[0]=='gh'
      path = args.last
      @calls << path
      value = @responses.fetch(path) { raise "unexpected fixture request" }
      raise 'fixture request denied' if value == :failure
      value.is_a?(String) ? value : JSON.generate(value)
    end
  end

  def setup
    @env = ENV.to_h
    ENV.delete('GITHUB_RUN_ID'); ENV.delete('GITHUB_RUN_ATTEMPT')
    @reader = FixtureReader.new
    @run = {'id'=>100,'run_number'=>2,'run_attempt'=>1,'head_sha'=>SOURCE,'head_branch'=>'main',
      'head_repository'=>{'full_name'=>'fixture/repo'},'event'=>'workflow_run',
      'path'=>'.github/workflows/acr-publish.yml','status'=>'completed','conclusion'=>'failure'}
    @job = {'id'=>200,'run_id'=>100,'run_attempt'=>1,'head_sha'=>SOURCE,
      'name'=>'Publish OCI research-runner','status'=>'completed','conclusion'=>'success'}
    @receipt = {'schema'=>'monday.research-oci-delivery.v1','repository'=>'fixture/repo',
      'source_sha'=>SOURCE,'product'=>'cex-runner','image_repository'=>IMAGE_REPOSITORY,
      'image'=>IMAGE_REPOSITORY+'@sha256:'+'b'*64,'oci_config_digest'=>'sha256:'+'c'*64,
      'delivery'=>{'run_id'=>100,'run_attempt'=>1,'job_id'=>200},
      'software'=>{'run_id'=>300,'run_attempt'=>2,'job_id'=>400},'software_manifest_sha256'=>'d'*64}
    @dir = Dir.mktmpdir('oci-delivery-test')
    File.write(File.join(@dir,'research-oci-delivery.json'),JSON.generate(@receipt))
    zip = File.join(@dir,'receipt.zip')
    assert system('zip','-q',zip,'research-oci-delivery.json',chdir: @dir)
    @reader.responses["repos/fixture/repo/actions/artifacts/500/zip"] = File.binread(zip)
    @artifact = {'id'=>500,'name'=>"research-oci-delivery-cex-runner-#{SOURCE}-1",
      'expired'=>false,'size_in_bytes'=>File.size(zip),'workflow_run'=>{'id'=>100,'head_sha'=>SOURCE}}
    runs_path = "repos/fixture/repo/actions/workflows/acr-publish.yml/runs?branch=main&status=completed&head_sha=#{SOURCE}&created=%3E%3D2026-10-11T00:53:20Z&per_page=100"
    @runs_path = runs_path
    @reader.responses[runs_path] = [{'total_count'=>1,'workflow_runs'=>[@run]}]
    @reader.responses['repos/fixture/repo/actions/runs/100'] = @run
    jobs([@job])
    artifacts([@artifact])
  end

  def teardown
    ENV.replace(@env)
    FileUtils.remove_entry(@dir)
  end

  def jobs(entries, attempt=1)
    @reader.responses["repos/fixture/repo/actions/runs/100/attempts/#{attempt}/jobs?per_page=100"] =
      [{'total_count'=>entries.size,'jobs'=>entries}]
  end

  def artifacts(entries)
    @reader.responses['repos/fixture/repo/actions/runs/100/artifacts?per_page=100'] =
      [{'total_count'=>entries.size,'artifacts'=>entries}]
  end

  def find
    @reader.find(SOURCE,'cex-runner',IMAGE_REPOSITORY)
  end

  def test_image_reused_after_signed_archive_failure
    assert_equal @receipt, find
    assert_equal 'failure', @run['conclusion']
  end

  def test_current_run_consumes_completed_oci_job_before_archive
    ENV['GITHUB_RUN_ID']='100'; ENV['GITHUB_RUN_ATTEMPT']='1'
    @run['status']='in_progress'; @run['conclusion']=nil
    assert_equal @receipt, @reader.find(SOURCE,'cex-runner',IMAGE_REPOSITORY,current: true)
  end

  def test_complete_empty_history_allows_first_image_build
    @reader.responses[@runs_path] = [{'total_count'=>0,'workflow_runs'=>[]}]
    assert_nil find
  end

  def test_unreadable_or_incomplete_history_never_means_first_build
    @reader.responses[@runs_path] = :failure
    assert_raises(RuntimeError) { find }
    @reader.responses[@runs_path] = [{'total_count'=>2,'workflow_runs'=>[@run]}]
    assert_raises(RuntimeError) { find }
  end

  def test_expired_or_missing_positive_receipt_denies_repush_fallback
    @artifact['expired']=true
    assert_raises(RuntimeError) { find }
    artifacts([])
    assert_raises(RuntimeError) { find }
  end

  def test_failed_oci_job_is_not_delivery
    @job['conclusion']='failure'
    assert_nil find
  end

  def test_partial_product_success_does_not_prove_another_product
    assert_nil @reader.find(SOURCE,'controller','fixture.invalid/wildcard0923/campaign-cycle-controller')
    assert_equal @receipt, find
  end

  def test_ambiguous_producer_and_artifact_fail_closed
    jobs([@job,@job.merge('id'=>201)])
    assert_raises(RuntimeError) { find }
    jobs([@job]); artifacts([@artifact,@artifact.merge('id'=>501)])
    assert_raises(RuntimeError) { find }
  end

  def test_foreign_repository_source_attempt_and_job_fail_closed
    changes = [{'head_sha'=>'e'*40},{'run_attempt'=>2},{'run_id'=>101}]
    changes.each do |change|
      jobs([@job.merge(change)])
      assert_raises(RuntimeError) { find }
    end
    jobs([@job]); @run['head_repository']={'full_name'=>'foreign/repo'}
    assert_raises(RuntimeError) { find }
  end

  def test_new_attempt_reuses_old_receipt_without_relabeling_it
    @run['run_attempt']=2
    jobs([],2)
    assert_equal({'run_id'=>100,'run_attempt'=>1,'job_id'=>200},find.fetch('delivery'))
  end

  def test_bad_digest_product_and_producer_receipts_are_rejected
    [@receipt.merge('product'=>'controller'), @receipt.merge('source_sha'=>'e'*40),
     @receipt.merge('image'=>IMAGE_REPOSITORY+':latest'),
     @receipt.merge('delivery'=>@receipt['delivery'].merge('job_id'=>201)),
     @receipt.merge('software'=>{'run_id'=>0,'run_attempt'=>2,'job_id'=>400})].each do |bad|
      assert_raises(RuntimeError) { @reader.validate(bad,100,1,200,SOURCE,'cex-runner',IMAGE_REPOSITORY) }
    end
  end

  def test_archive_failure_cancellation_or_success_cannot_refill_allowance
    archive_history
    %w[failure cancelled success].each do |conclusion|
      jobs([@job,@job.merge('id'=>201,'name'=>'Publish research-runner','conclusion'=>conclusion)])
      assert @reader.archive_started?(SOURCE,'cex-runner')
    end
    jobs([@job,@job.merge('id'=>201,'name'=>'Publish research-runner','conclusion'=>'skipped')])
    refute @reader.archive_started?(SOURCE,'cex-runner')
  end

  def archive_history
    ENV['GITHUB_RUN_ID']='101'; ENV['GITHUB_RUN_NUMBER']='3'; ENV['GITHUB_RUN_ATTEMPT']='1'
    ENV['MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY']=JSON.generate(
      'history_anchor_run_id'=>99,'history_anchor_run_number'=>1,'not_before'=>1791680000)
    anchor=@run.merge('id'=>99,'run_number'=>1,'head_sha'=>'e'*40,'created_at'=>'2026-10-11T00:40:00Z')
    current=@run.merge('id'=>101,'run_number'=>3,'status'=>'in_progress','conclusion'=>nil)
    @history_path='repos/fixture/repo/actions/workflows/acr-publish.yml/runs?per_page=100'
    @reader.responses[@history_path]=[{'total_count'=>3,'workflow_runs'=>[anchor,@run,current]}]
  end

  def test_partial_failed_archive_reserves_only_its_fixed_product_share
    archive_history
    [['research-runner','cex-runner','controller'],
     ['campaign-cycle-controller','controller','cex-runner']].each do |repository,spent,unused|
      %w[failure cancelled success].each do |conclusion|
        jobs([@job,@job.merge('id'=>201,'name'=>"Publish #{repository}",'conclusion'=>conclusion)])
        assert @reader.archive_started?(SOURCE,spent)
        refute @reader.archive_started?(SOURCE,unused)
      end
    end
  end

  def test_archive_unknown_or_in_progress_prior_job_reserves_authority
    archive_history
    %w[queued in_progress unknown].each do |status|
      jobs([@job.merge('name'=>'Publish research-runner','status'=>status,'conclusion'=>nil)])
      assert @reader.archive_started?(SOURCE,'cex-runner')
    end
  end

  def test_deleted_history_gaps_old_ordinals_and_anchor_drift_are_pending
    archive_history
    history=@reader.responses[@history_path]
    complete=Marshal.load(Marshal.dump(history))
    history[0]['workflow_runs'].delete_at(1); history[0]['total_count']=2
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
    @reader.responses[@history_path]=Marshal.load(Marshal.dump(complete))
    ENV['GITHUB_RUN_NUMBER']='2'
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
    ENV['GITHUB_RUN_NUMBER']='3'
    @reader.responses[@history_path][0]['workflow_runs'][0]['id']=98
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
  end

  def test_operating_anchor_cannot_renew_authority_for_its_own_source
    archive_history
    @reader.responses[@history_path][0]['workflow_runs'][0]['head_sha']=SOURCE
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
  end

  def test_prior_same_source_run_with_unknown_state_or_no_jobs_is_pending
    archive_history
    %w[queued in_progress unknown].each do |status|
      @run['status']=status
      jobs([])
      assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
    end
    @run['status']='completed'; @run['conclusion']=nil
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
    @run['conclusion']='failure'
    assert_raises(RuntimeError) { @reader.archive_started?(SOURCE,'cex-runner') }
  end

  def test_reanchoring_does_not_erase_earlier_retained_source_consumption
    archive_history
    history=@reader.responses[@history_path][0]
    earlier=@run.merge('id'=>98,'run_number'=>1)
    history['workflow_runs'][0]['run_number']=2
    @run['run_number']=3
    history['workflow_runs'][2]['run_number']=4
    history['workflow_runs'] << earlier; history['total_count']=4
    ENV['GITHUB_RUN_NUMBER']='4'
    ENV['MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY']=JSON.generate(
      'history_anchor_run_id'=>99,'history_anchor_run_number'=>2,'not_before'=>1791680000)
    @reader.responses['repos/fixture/repo/actions/runs/98/attempts/1/jobs?per_page=100']=[{
      'total_count'=>1,'jobs'=>[@job.merge('id'=>198,'run_id'=>98,'name'=>'Publish research-runner')]}]
    jobs([@job])
    assert @reader.archive_started?(SOURCE,'cex-runner')
  end

  def test_duplicate_pagination_ids_and_totals_are_rejected
    @reader.responses[@runs_path] = [{'total_count'=>2,'workflow_runs'=>[@run,@run]}]
    assert_raises(RuntimeError) { find }
  end

  def test_extra_receipt_file_is_rejected
    File.write(File.join(@dir,'foreign'),'foreign')
    zip = File.join(@dir,'extra.zip')
    assert system('zip','-q',zip,'research-oci-delivery.json','foreign',chdir: @dir)
    @reader.responses['repos/fixture/repo/actions/artifacts/500/zip'] = File.binread(zip)
    assert_raises(RuntimeError) { find }
  end
end
