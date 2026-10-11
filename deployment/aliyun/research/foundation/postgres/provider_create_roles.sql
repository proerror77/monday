-- Additive grants AFTER provider_create_operations.sql and the existing roles.
-- Apply only within the separately verified paused migration Gate.
GRANT SELECT, INSERT, UPDATE ON research.provider_create_operations TO monday_research_reconciler;
-- No worker, submitter, publisher, Session, admission or retirement writer gets
-- create-operation writes. Nobody receives DELETE or a terminal override.
