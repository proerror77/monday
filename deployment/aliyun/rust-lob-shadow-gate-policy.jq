. as $root
| ([
    "binance-lob-archiver-recovery@.service",
    "binance-lob-archiver-recovery@.timer",
    "host-rust-lob-recovery-queue.sh",
    "host-rust-lob-readback.sh",
    "host-rust-lob-shadow-gate.sh",
    "host-rust-lob-cutover.sh",
    "host-rust-lob-restore.sh",
    "host-rust-lob-controller-release.sh",
    "monday-collector-health.sh",
    "rust-lob-control-plane-lib.sh",
    "rust-lob-runtime-health-policy.jq",
    "rust-lob-shadow-gate-policy.jq"
  ] | sort) as $controller_asset_keys
| ([
    "binance-lob-archiver-production@.service",
    "binance-lob-archiver-upload@.service",
    "binance-lob-archiver-production-spot.env",
    "binance-lob-archiver-production-usdm.env"
  ] | sort) as $production_asset_keys
| ([
    "binance-lob-archiver-rust@.service",
    "binance-lob-archiver-rust-upload@.service",
    "binance-lob-archiver-rust-spot.env",
    "binance-lob-archiver-rust-usdm.env"
  ] | sort) as $shadow_asset_keys
| $root.schema == "monday.rust_lob_shadow_gate.v5"
and .control_plane_version == 2
and .passed == true
and (.production_eligible | type == "boolean")
and (.test_only | type == "boolean")
and (if .test_only then .production_eligible == false else .production_eligible == true end)
and (.source_mode == "stable" or .source_mode == "direct")
and (.from_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
and (.candidate_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
and (.candidate_deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
and (.candidate_deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
and (.candidate_control_bytes | type == "object"
  and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
  and (.assets | type == "object" and (keys | sort) == $controller_asset_keys
    and all(.[]; type == "string" and test("^[a-f0-9]{64}$"))))
and (.transition | type == "object"
  and (.before | test("^[a-f0-9]{64}$"))
  and (.after == $root.candidate_controller_sha256)
  and (.topology == "stable" or .topology == "direct-bootstrap"))
and (if .source_mode == "direct" then
  .transition.topology == "direct-bootstrap"
  and .from_controller_sha256 == .transition.before
 else
  .source_mode == "stable"
  and .from_controller_sha256 == .transition.before
end)
and (.before | type == "object"
  and .controller == $root.transition.before
  and (.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
  and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
  and (.production_projection | type == "string" and length > 0)
  and (.production_assets | type == "object" and (keys | sort) == $production_asset_keys
    and all(.[]; type == "string" and test("^[a-f0-9]{64}$"))))
and (.production_assets | type == "object" and (keys | sort) == $production_asset_keys
  and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
and (.production_runtime | type == "object"
  and .schema == "monday.rust_lob_production_runtime.v1"
  and .type == "simple"
  and .exec_start == "/opt/monday/bin/binance-lob-archiver"
  and .environment_file == "/etc/monday/binance-lob-archiver-production-%i.env"
  and .user == "hftcollector"
  and .group == "hftcollector"
  and .restart == "always"
  and .restart_sec == 5
  and .runtime_max_sec == 21600
  and .kill_mode == "mixed"
  and .timeout_start_sec == 120
  and .timeout_stop_sec == 600
  and .cpu_quota == "80%"
  and .memory_high == "2048M"
  and .memory_max == "2560M"
  and (.sandbox | type == "object"
    and .no_new_privileges == true
    and .private_tmp == true
    and .protect_system == "strict"
    and .protect_home == true
    and .protect_kernel_tunables == true
    and .protect_kernel_modules == true
    and .protect_control_groups == true
    and .lock_personality == true
    and .restrict_suidsgid == true
    and .state_directory == "hft-collector"
    and .read_write_paths == ["/data/monday/spool/binance-lob", "/data/monday/spool/binance-lob-recovery"])
  and (.upload | type == "object"
    and .type == "oneshot"
    and .exec_start == "/opt/monday/bin/binance-lob-archiver --upload-only"
    and .environment_file == "/etc/monday/binance-lob-archiver-production-%i.env"
    and .cpu_quota == "80%"
    and .memory_high == "384M"
    and .memory_max == "512M"
    and .timeout_start_sec == 0)
  and (.unit_sha256 | type == "object"
    and (.collector | type == "string" and test("^[a-f0-9]{64}$"))
    and (.upload | type == "string" and test("^[a-f0-9]{64}$")))
  and (.unit_semantics_sha256 | type == "object"
    and (.collector | type == "string" and test("^[a-f0-9]{64}$"))
    and (.upload | type == "string" and test("^[a-f0-9]{64}$")))
  and (.env_sha256 | type == "object"
    and (.spot | type == "string" and test("^[a-f0-9]{64}$"))
    and (.usdm | type == "string" and test("^[a-f0-9]{64}$")))
  and (.markets | type == "object" and (keys | sort) == ["spot", "usdm"]
    and (.spot | type == "object"
      and .market == "spot" and .dataset == "spot_all" and .symbols == "ALL"
      and .shard_id == "all" and .spool_dir == "/data/monday/spool/binance-lob/spot"
      and .oss_bucket == "monday-lob-apne1-1045353359"
      and .oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com"
      and .oss_region == "ap-northeast-1" and .aliyun_profile == "ecs-role")
    and (.usdm | type == "object"
      and .market == "usdm" and .dataset == "usdm_perpetual_top100_lob"
      and .shard_id == "all" and .ws_shard_size == 25
      and .spool_dir == "/data/monday/spool/binance-lob/usdm"
      and .oss_bucket == "monday-lob-apne1-1045353359"
      and .oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com"
      and .oss_region == "ap-northeast-1" and .aliyun_profile == "ecs-role")
    and (.usdm.symbols | type == "string")
    and ((.usdm.symbols | split(",")) | length == 100)
    and ((.usdm.symbols | split(",") | unique) | length == 100)))
and (.production_process | type == "object")
and (if .test_only then true else
  (.production_process | ((keys | sort) == ["spot", "usdm"]
    and all(.[]; .active == true
      and (.main_pid | type == "number" and . >= 1)
      and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))))) end)
and (.resource_admission | type == "array" and length >= 3
  and ((["preflight","shadow-spot","strict-verifier-spot","upload-drain-spot","shadow-usdm","strict-verifier-usdm","upload-drain-usdm","oss-readback-spot","oss-readback-usdm"]
    - (map(.phase) | unique)) | length == 0)
  and all(.[]; . as $r
    | (.phase | type == "string" and length > 0)
    and (.started_at | type == "string" and length > 0)
    and (.ended_at | type == "string" and length > 0)
    and (.samples | type == "number" and . >= 1)
    and (.host_memory_available_bytes | type == "number" and . >= 0)
    and (.max_memory_available_bytes | type == "number" and . >= 0)
    and (.current_memory_available_bytes | type == "number" and . >= 0)
    and (.breach | type == "boolean" and . == false)
    and ($r.required_bytes | type == "number" and . > 0 and . <= $r.host_memory_available_bytes)
    and (.phase_memory_max_bytes | type == "number" and . > 0)))
and (.io_full_psi_windows | type == "array" and length >= 3
  and all(.[]; . as $p
    | (.phase | type == "string" and length > 0)
    and (.stage | type == "string" and length > 0)
    and (.hit | type == "boolean")
    and ($p.consecutive_hits | type == "number" and . >= 0)
    and (if $p.stage == "calibration"
         then ($p.delta_us | type == "number" and . >= 0)
           and ($p.ratio | type == "number" and . >= 0)
         else true end)))
and (.shadow_staging | type == "object"
  and .mode == "run-scoped"
  and (.run_unit_root | type == "string" and test("/run/monday/rust-lob-gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
  and (.spool_root | type == "string" and test("/data/monday/spool/binance-lob-rust-shadow/gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
  and (.units | type == "object" and (keys | sort) == ["spot", "usdm"]
    and (.spot | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-spot\\.service$"))
    and (.usdm | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-usdm\\.service$")))
  and (.upload_units | type == "object" and (keys | sort) == ["spot", "usdm"]
    and all(.[]; type == "object"
      and (.unit | type == "string" and test("^monday-rust-lob-gate-[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-(spot|usdm)-upload\\.service$"))
      and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))))
  and (.candidate_assets | type == "object" and (keys | sort) == $shadow_asset_keys
    and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
  and (.restored_assets | type == "object" and (keys | sort) == $shadow_asset_keys
    and all(.[]; ((.state == "present"
      and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
      or (.state == "absent" and .sha256 == null)
      or (.state == "projection"
        and (.target | type == "string" and length > 0)
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))))))
  and (.before_assets | type == "object" and (keys | sort) == $shadow_asset_keys
    and all(.[]; ((.state == "present"
      and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
      or (.state == "absent" and .sha256 == null)
      or (.state == "projection"
        and (.target | type == "string" and length > 0)
        and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))))))
  and .restored_assets == .before_assets
  and (.binary | type == "object"
    and (.path | type == "string" and length > 0)
    and (.candidate_target | type == "string" and (contains("/opt/monday/bin/") | not))
    and (.candidate_target | type == "string" and length > 0)
    and (.restored_present | type == "boolean")
    and ((.restored_target_sha256 == null)
      or (.restored_target_sha256 | type == "string" and test("^[a-f0-9]{64}$")))))
  and (.binary.path == .run_unit_root)
and (.checks | type == "object"
  and .before_pair_unchanged == true
  and .shadow_staging_verified == true
  and .production_runtime_verified == true
  and .shadow_assets_restored == true
  and .resource_preflight == true
  and .oss_triplets == true
  and .strict_segment_verifier == true
  and .final_identity == true
  and .controller_control_bytes == true
  and .shadow_link_restored == true
  and .health_freshness == true)
and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"])
  and (to_entries | all(.[]; .value.market == .key))
  and all(.[]; . as $m
    | (.market | type == "string")
    and (.dataset | type == "string" and length > 0)
    and (.session_id | type == "string" and length > 0)
    and (.spool_dir | type == "string" and test("/data/monday/spool/binance-lob-rust-shadow/gate/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/(spot|usdm)$"))
    and (.shard_id == "all")
    and (.oss_bucket == "monday-lob-apne1-1045353359")
    and (.oss_endpoint == "oss-ap-northeast-1-internal.aliyuncs.com")
    and (.oss_region == "ap-northeast-1")
    and (.aliyun_profile == "ecs-role")
    and (.expected_oss_bucket | type == "string" and length > 0)
    and (.expected_oss_prefix | type == "string" and length > 0)
    and ($m.segment_count | type == "number" and . >= 2 and . == ($m.segments | length))
    and ($m.oss_triplet_count | type == "number" and . >= 2 and . == ($m.triplets | length))
    and (.n_restarts | type == "number" and . == 0)
    and .process_identity_verified == true
    and .installed_shadow_assets_verified == true
    and .strict_lob_continuity_readback == true
    and (.strict_aggregate_trade_continuity_readback | type == "boolean")
    and (.strict_raw_trade_continuity_readback | type == "boolean")
    and (if .market == "spot" then
      .strict_aggregate_trade_continuity_readback == true
      and .strict_raw_trade_continuity_readback == true
    else true end)
    and (.segments | type == "array" and length >= 2
      and all(.[];
        (.file | type == "string" and test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))
        and (.path | type == "string" and length > 0)
        and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.start_received_at_ns | type == "number" and . >= 0)
        and (.end_received_at_ns | type == "number")
        and (.end_received_at_ns >= .start_received_at_ns)
        and (.session_id | type == "string" and . == $m.session_id)))
    and (.triplets | type == "array" and length >= 2
      and all(.[];
        (.market | type == "string" and . == $m.market)
        and (.dataset | type == "string" and . == $m.dataset)
        and (.data_uri | type == "string"
          and startswith(("oss://" + $m.expected_oss_bucket + "/" + $m.expected_oss_prefix + "/"))
          and test("^oss://[^/]+/.+\\.jsonl\\.zst$"))
        and (.manifest_uri | type == "string" and test("^oss://[^/]+/.+\\.manifest\\.json$"))
        and (.manifest_uri == (.data_uri + ".manifest.json"))
        and (.success_uri | type == "string" and test("^oss://[^/]+/.+\\._SUCCESS$"))
        and (.success_uri == (.data_uri + "._SUCCESS"))
        and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.success_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
        and (.success_content == (.data_sha256 + "\n"))
        and (.start_received_at_ns | type == "number" and . >= 0)
        and (.end_received_at_ns | type == "number")
        and (.end_received_at_ns >= .start_received_at_ns)
        and (.observed_at | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
        and (.session_id | type == "string" and . == $m.session_id)
        and (.catalog_sha256 | type == "string" and . == $m.health.frozen_catalog_sha256)))
    and (.health | type == "object"
      and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.session_id | type == "string" and length > 0)
      and (.frozen_catalog_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.frozen_symbol_count | type == "number" and . >= 1)
      and (.max_health_silence_seconds | type == "number" and . >= 0 and . <= 120)
      and (.samples | type == "number" and . >= 1)
      and .session_id == $m.session_id)))
