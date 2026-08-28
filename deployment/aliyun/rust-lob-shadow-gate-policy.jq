 .schema == "monday.rust_lob_shadow_gate.v5"
 and .control_plane_version == 2
 and .passed == true
 and (.production_eligible | type == "boolean")
 and (.test_only | type == "boolean")
 and (.candidate_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_deployment_bundle_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
 and (.transition | type == "object" and .before != null
      and (.topology == "stable" or .topology == "direct-bootstrap"))
 and (.transition.after == .candidate_controller_sha256)
 and (.before | type == "object" and (.payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and (.runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$")))
 and (.resource_admission | type == "array" and length >= 3)
 and (.io_full_psi_windows | type == "array" and length >= 3)
 and (.production_assets | type == "object" and length == 4
      and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
 and (.production_process | type == "object")
 and (if .test_only then true
      else (.production_process
        | ((keys | sort) == ["spot", "usdm"]
          and all(.[]; .active == true and (.main_pid | type == "number" and . >= 1)
            and (.process_exe_sha256 | type == "string" and test("^[a-f0-9]{64}$"))))) end)
 and (.shadow_staging | type == "object"
      and (.candidate_assets | type == "object" and length == 4
        and all(.[]; type == "string" and test("^[a-f0-9]{64}$")))
      and (.restored_assets | type == "object" and length == 4
        and all(.[]; (.state == "present" and (.sha256 | type == "string" and test("^[a-f0-9]{64}$")))
          or (.state == "absent" and .sha256 == null)))
      and (.before_assets | type == "object" and length == 4)
      and (.binary | type == "object" and (.candidate_target | type == "string")
        and (.restored_present | type == "boolean")))
 and (.checks | type == "object"
      and .before_pair_unchanged == true
      and .shadow_staging_verified == true
      and .shadow_assets_restored == true
      and .resource_preflight == true
      and .oss_triplets == true
      and .strict_segment_verifier == true
      and .final_identity == true)
 and (.markets | type == "object" and ((keys | sort) == ["spot", "usdm"])
      and all(.[];
        (.segment_count | type == "number" and . >= 2)
        and (.oss_triplet_count | type == "number" and . >= 2)
        and .n_restarts == 0
        and .process_identity_verified == true
        and .installed_shadow_assets_verified == true
        and .strict_lob_continuity_readback == true
        and (.strict_aggregate_trade_continuity_readback | type == "boolean")
        and (.strict_raw_trade_continuity_readback | type == "boolean")
        and (if .market == "spot" then
          .strict_aggregate_trade_continuity_readback == true
          and .strict_raw_trade_continuity_readback == true
        else true end)))
