-- Offline installation AFTER the foundation migrations, as the trusted schema
-- owner. These NOLOGIN roles do not provision credentials or change authority.
-- Separate login identities and membership belong to the deployment Gate.
CREATE ROLE monday_research_submitter NOLOGIN;
CREATE ROLE monday_research_reconciler NOLOGIN;
CREATE ROLE monday_research_session_host NOLOGIN;
CREATE ROLE monday_research_artifact_gateway NOLOGIN;
CREATE ROLE monday_research_prepare_worker NOLOGIN;
CREATE ROLE monday_research_definition_writer NOLOGIN;
CREATE ROLE monday_research_release_importer NOLOGIN;
CREATE ROLE monday_research_build_import_owner NOLOGIN;
CREATE ROLE monday_research_native_admission NOLOGIN;
CREATE ROLE monday_research_terminal_retirement NOLOGIN;
GRANT USAGE ON SCHEMA research TO monday_research_submitter,
  monday_research_reconciler, monday_research_session_host,
  monday_research_artifact_gateway, monday_research_prepare_worker,
  monday_research_definition_writer, monday_research_release_importer,
  monday_research_native_admission, monday_research_terminal_retirement,
  monday_research_build_import_owner;
GRANT SELECT ON research.authority, research.inputs, research.backends,
  research.build_artifacts, research.build_releases, research.runs,
  research.admissions, research.revocations, research.tasks
  TO monday_research_submitter;
GRANT INSERT ON research.tasks TO monday_research_submitter;
GRANT SELECT ON research.results TO monday_research_submitter;
-- SELECT FOR SHARE/UPDATE requires UPDATE on a column. These immutable keys
-- permit row locking but their triggers prohibit writes; authority.singleton
-- is checked true and cannot change mode, proofs or concurrency.
GRANT UPDATE(singleton) ON research.authority TO monday_research_submitter;
GRANT SELECT ON research.authority, research.inputs, research.views, research.backends,
  research.experiments, research.build_artifacts, research.build_releases, research.runs,
  research.sessions, research.subscriptions, research.tasks, research.admissions,
  research.revocations, research.events, research.results, research.completion_intents
  TO monday_research_reconciler;
GRANT INSERT, UPDATE ON research.tasks TO monday_research_reconciler;
GRANT INSERT ON research.events, research.results, research.inputs,
  research.views, research.completion_intents TO monday_research_reconciler;
GRANT USAGE ON SEQUENCE research.events_event_id_seq TO monday_research_reconciler;
GRANT UPDATE(singleton) ON research.authority TO monday_research_reconciler;
GRANT UPDATE(request_sha256) ON research.admissions TO monday_research_reconciler;
GRANT SELECT ON research.authority, research.experiments, research.runs,
  research.sessions, research.session_snapshots, research.subscriptions,
  research.tasks, research.completion_intents, research.completion_deliveries
  TO monday_research_session_host;
GRANT INSERT ON research.sessions, research.session_snapshots,
  research.subscriptions, research.completion_deliveries, research.completion_intents TO monday_research_session_host;
GRANT UPDATE(singleton) ON research.authority TO monday_research_session_host;
GRANT UPDATE(task_id) ON research.tasks TO monday_research_session_host;
GRANT UPDATE(session_sha256) ON research.sessions TO monday_research_session_host;
GRANT UPDATE(intent_sha256) ON research.completion_intents TO monday_research_session_host;
GRANT SELECT ON research.authority, research.tasks, research.admissions,
  research.revocations TO monday_research_artifact_gateway;
GRANT EXECUTE ON FUNCTION research.artifact_write_permit(text,text,integer,bigint)
  TO monday_research_artifact_gateway;
GRANT SELECT ON research.authority, research.tasks, research.admissions,
  research.revocations, research.inputs, research.views TO monday_research_prepare_worker;
GRANT SELECT ON research.authority, research.experiments, research.build_artifacts,
  research.build_releases, research.runs TO monday_research_definition_writer;
GRANT INSERT ON research.experiments, research.runs TO monday_research_definition_writer;
GRANT UPDATE(singleton) ON research.authority TO monday_research_definition_writer;
GRANT SELECT ON research.authority, research.build_artifacts, research.build_releases
  TO monday_research_release_importer;
GRANT INSERT ON research.build_artifacts, research.build_releases TO monday_research_release_importer;
GRANT UPDATE(singleton) ON research.authority TO monday_research_release_importer;
-- The importer cannot install, revoke or restore its own independent approval.
GRANT SELECT ON research.build_import_admissions TO monday_research_release_importer;
GRANT SELECT, INSERT, UPDATE ON research.build_import_admissions TO monday_research_build_import_owner;
GRANT SELECT ON research.build_import_admission_audits TO monday_research_build_import_owner;
-- Never give importer membership in this owner role; no DELETE or schema ownership.
-- Only independently verified native grants can be projected by this identity.
-- Neither Agent tools, workers, Session nor reconciler inherit this role.
GRANT SELECT, INSERT ON research.admissions, research.revocations
  TO monday_research_native_admission;
GRANT SELECT, INSERT ON research.native_admission_imports
  TO monday_research_native_admission;
GRANT SELECT ON research.native_admission_imports TO monday_research_submitter,
  monday_research_reconciler, monday_research_session_host,
  monday_research_artifact_gateway;
-- Preparation lease checks consume only the exact tenant and expiry projection.
GRANT SELECT(request_sha256, tenant, expires_ms) ON research.native_admission_imports
  TO monday_research_prepare_worker;

-- Readonly effective revocation and immutable campaign input facts. Only the
-- independently verified native importer can append source constraint records.
GRANT SELECT ON research.native_request_revocations TO monday_research_reconciler,
  monday_research_submitter, monday_research_session_host,
  monday_research_artifact_gateway, monday_research_prepare_worker;
GRANT SELECT, INSERT ON research.native_request_revocations TO monday_research_native_admission;
GRANT SELECT ON research.native_campaign_inputs TO monday_research_reconciler,
  monday_research_submitter, monday_research_session_host,
  monday_research_definition_writer;
GRANT SELECT, INSERT ON research.native_campaign_inputs TO monday_research_native_admission;

-- A separate trusted mechanical retirement host. The ordinary reconciler,
-- Agent, Session and worker receive no retirement write or inherited role.
GRANT SELECT ON research.tasks, research.runs, research.native_admission_imports,
  research.events, research.results, research.native_campaign_inputs,
  research.inputs, research.backends TO monday_research_terminal_retirement;
GRANT SELECT, INSERT ON research.native_terminal_retirement_audits,
  research.native_terminal_retirement_events TO monday_research_terminal_retirement;
-- Any column UPDATE permits FOR UPDATE. This key cannot change independently
-- of request/document identity, which this role cannot write.
GRANT UPDATE(task_id) ON research.tasks TO monday_research_terminal_retirement;
