 .schema == "monday.rust_lob_shadow_gate.v5"
 and .control_plane_version == 2
 and .passed == true
 and (.candidate_controller_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_payload_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.candidate_runtime_contract_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
 and (.transition | type == "object")
 and (.checks | type == "object")
