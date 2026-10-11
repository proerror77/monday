#![cfg(feature = "control")]
use anyhow::{ensure, Result};

/// An explicitly named disposable database only, not production schema work.
#[tokio::test]
#[ignore = "requires disposable MONDAY_TEST_DATABASE_URL ending /monday_foundation_roles_test"]
async fn paused_database_roles_enforce_real_read_write_and_lock_boundaries() -> Result<()> {
    use hft_research_platform::postgres::{
        BUILD_IMPORT_ADMISSION_MIGRATION, BUILD_RELEASE_MIGRATION, MIGRATION,
        NATIVE_ADMISSION_MIGRATION, NATIVE_CAMPAIGN_INPUTS_MIGRATION,
        NATIVE_REQUEST_REVOCATION_MIGRATION, SESSION_DELIVERY_MIGRATION,
    };
    let url = std::env::var("MONDAY_TEST_DATABASE_URL")?;
    ensure!(
        url.ends_with("/monday_foundation_roles_test"),
        "test database identity mismatch"
    );
    let pool = sqlx_postgres::PgPool::connect(&url).await?;
    for migration in [
        MIGRATION,
        BUILD_RELEASE_MIGRATION,
        BUILD_IMPORT_ADMISSION_MIGRATION,
        SESSION_DELIVERY_MIGRATION,
        include_str!("../sql/artifact_gateway.sql"),
        NATIVE_ADMISSION_MIGRATION,
        NATIVE_REQUEST_REVOCATION_MIGRATION,
        NATIVE_CAMPAIGN_INPUTS_MIGRATION,
        hft_research_platform::retirement::MIGRATION,
        hft_research_platform::provider_operations::MIGRATION,
        include_str!("../../../../deployment/aliyun/research/foundation/postgres/roles.sql"),
        include_str!(
            "../../../../deployment/aliyun/research/foundation/postgres/provider_create_roles.sql"
        ),
    ] {
        sqlx_core::raw_sql::raw_sql(migration)
            .execute(&pool)
            .await?;
    }
    let mode: String = sqlx_core::query_scalar::query_scalar(
        "SELECT mode FROM research.authority WHERE singleton",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(mode, "paused");
    for role in [
        "monday_research_submitter",
        "monday_research_session_host",
        "monday_research_artifact_gateway",
        "monday_research_prepare_worker",
        "monday_research_definition_writer",
        "monday_research_release_importer",
        "monday_research_native_admission",
        "monday_research_terminal_retirement",
    ] {
        let writes: bool = sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege($1,'research.provider_create_operations','INSERT,UPDATE,DELETE')").bind(role).fetch_one(&pool).await?;
        assert!(!writes, "non-reconciler received provider create authority");
        let mut tx = pool.begin().await?;
        sqlx_core::raw_sql::raw_sql(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *tx)
            .await?;
        assert!(sqlx_core::query::query(
            "UPDATE research.provider_create_operations SET state='observed'"
        )
        .execute(&mut *tx)
        .await
        .is_err());
        tx.rollback().await?;
    }
    let reconciler: bool = sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege('monday_research_reconciler','research.provider_create_operations','SELECT') AND has_table_privilege('monday_research_reconciler','research.provider_create_operations','INSERT') AND has_table_privilege('monday_research_reconciler','research.provider_create_operations','UPDATE') AND NOT has_table_privilege('monday_research_reconciler','research.provider_create_operations','DELETE')").fetch_one(&pool).await?;
    assert!(reconciler);
    let public_access: bool = sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege('public','research.provider_create_operations','SELECT,INSERT,UPDATE,DELETE')").fetch_one(&pool).await?;
    assert!(!public_access);
    let experiment = "e".repeat(64);
    let session = "f".repeat(64);
    sqlx_core::query::query("INSERT INTO research.experiments(experiment_sha256,tenant,document) VALUES ($1,'role-fixture','{\"schema_fixture\":true}')").bind(&experiment).execute(&pool).await?;
    sqlx_core::query::query("INSERT INTO research.sessions(session_sha256,experiment_sha256,tenant,document) VALUES ($1,$2,'role-fixture','{\"schema_fixture\":true}')").bind(&session).bind(&experiment).execute(&pool).await?;
    // Schema/permission fixture only; it is not an admitted scientific Task.
    let build = "a".repeat(64);
    let run = "b".repeat(64);
    let input = "c".repeat(64);
    let task = "d".repeat(64);
    sqlx_core::query::query(
        "INSERT INTO research.build_artifacts VALUES($1,$1,'{\"schema_fixture\":true}')",
    )
    .bind(&build)
    .execute(&pool)
    .await?;
    sqlx_core::query::query(
        "INSERT INTO research.runs VALUES($1,$2,$3,'role-fixture','{\"schema_fixture\":true}')",
    )
    .bind(&run)
    .bind(&experiment)
    .bind(&build)
    .execute(&pool)
    .await?;
    sqlx_core::query::query(
        "INSERT INTO research.inputs VALUES($1,'prepared','{\"schema_fixture\":true}')",
    )
    .bind(&input)
    .execute(&pool)
    .await?;
    sqlx_core::query::query("INSERT INTO research.tasks(task_id,tenant,idempotency_key,request_sha256,run_manifest_sha256,view_manifest_sha256,state,document) VALUES($1,'role-fixture','mechanical-role-fixture',$1,$2,$3,'cancelled',$4)").bind(&task).bind(&run).bind(&input).bind(serde_json::json!({"id":task,"state":"cancelled","schema_fixture":true})).execute(&pool).await?;
    for lock in ["FOR SHARE", "FOR UPDATE"] {
        let mut tx = pool.begin().await?;
        sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_session_host")
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query(&format!(
            "SELECT document FROM research.sessions WHERE session_sha256=$1 {lock}"
        ))
        .bind(&session)
        .fetch_one(&mut *tx)
        .await?;
        tx.rollback().await?;
    }
    for query in [
        "UPDATE research.sessions SET session_sha256=session_sha256 WHERE session_sha256=$1",
        "UPDATE research.sessions SET document='{}' WHERE session_sha256=$1",
        "DELETE FROM research.sessions WHERE session_sha256=$1",
    ] {
        let mut tx = pool.begin().await?;
        sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_session_host")
            .execute(&mut *tx)
            .await?;
        let rejected = sqlx_core::query::query(query)
            .bind(&session)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        if query.contains("SET session_sha256") {
            ensure!(
                rejected.to_string().contains("immutable research record"),
                "immutable session trigger did not reject same-column write"
            );
        }
        tx.rollback().await?;
    }
    for role in [
        "monday_research_submitter",
        "monday_research_reconciler",
        "monday_research_session_host",
        "monday_research_artifact_gateway",
        "monday_research_prepare_worker",
    ] {
        let mut tx = pool.begin().await?;
        sqlx_core::raw_sql::raw_sql(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("SELECT mode FROM research.authority")
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            sqlx_core::query::query("UPDATE research.authority SET mode='postgres'")
                .execute(&mut *tx)
                .await
                .is_err()
        );
        tx.rollback().await?;
        let forbidden:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege($1,'research.admissions','INSERT') OR has_table_privilege($1,'research.native_admission_imports','INSERT') OR has_table_privilege($1,'research.revocations','INSERT') OR has_table_privilege($1,'research.backends','UPDATE')").bind(role).fetch_one(&pool).await?;
        assert!(
            !forbidden,
            "application role cannot mint or revoke authority"
        );
    }
    for role in [
        "monday_research_submitter",
        "monday_research_reconciler",
        "monday_research_session_host",
        "monday_research_artifact_gateway",
        "monday_research_prepare_worker",
        "monday_research_native_admission",
    ] {
        let forbidden:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege($1,'research.native_terminal_retirement_audits','INSERT') OR has_table_privilege($1,'research.native_terminal_retirement_events','INSERT')").bind(role).fetch_one(&pool).await?;
        assert!(
            !forbidden,
            "application/native importer cannot retire tasks"
        );
    }
    let mut tx = pool.begin().await?;
    sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_terminal_retirement")
        .execute(&mut *tx)
        .await?;
    // Row locking succeeds with the immutable identity column privilege.
    sqlx_core::query::query("SELECT document FROM research.tasks WHERE task_id=$1 FOR UPDATE")
        .bind(&task)
        .fetch_one(&mut *tx)
        .await?;
    assert!(
        sqlx_core::query::query("UPDATE research.tasks SET document='{}'")
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await?;
    let mut tx = pool.begin().await?;
    sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_terminal_retirement")
        .execute(&mut *tx)
        .await?;
    let rejected = sqlx_core::query::query("UPDATE research.tasks SET task_id=$1 WHERE task_id=$2")
        .bind("1".repeat(64))
        .bind(&task)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    ensure!(
        rejected.to_string().contains("check constraint"),
        "retirement key column changed task identity"
    );
    tx.rollback().await?;
    for table in [
        "native_terminal_retirement_audits",
        "native_terminal_retirement_events",
    ] {
        let privileges:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege('monday_research_terminal_retirement',$1,'SELECT') AND has_table_privilege('monday_research_terminal_retirement',$1,'INSERT') AND NOT has_table_privilege('monday_research_terminal_retirement',$1,'UPDATE') AND NOT has_table_privilege('monday_research_terminal_retirement',$1,'DELETE')").bind(format!("research.{table}")).fetch_one(&pool).await?;
        assert!(privileges, "retirement host must remain append-only");
    }
    for role in [
        "monday_research_submitter",
        "monday_research_reconciler",
        "monday_research_session_host",
    ] {
        let mut tx = pool.begin().await?;
        sqlx_core::raw_sql::raw_sql(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *tx)
            .await?;
        sqlx_core::query::query("SELECT mode FROM research.authority FOR SHARE")
            .fetch_one(&mut *tx)
            .await?;
        tx.rollback().await?;
    }
    let mut tx = pool.begin().await?;
    sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_prepare_worker")
        .execute(&mut *tx)
        .await?;
    sqlx_core::query::query(
        "SELECT request_sha256, tenant, expires_ms FROM research.native_admission_imports LIMIT 0",
    )
    .execute(&mut *tx)
    .await?;
    assert!(sqlx_core::query::query(
        "SELECT trust_document FROM research.native_admission_imports LIMIT 0"
    )
    .execute(&mut *tx)
    .await
    .is_err());
    tx.rollback().await?;
    for role in [
        "monday_research_artifact_gateway",
        "monday_research_prepare_worker",
        "monday_research_session_host",
        "monday_research_terminal_retirement",
    ] {
        let can_write:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege($1,'research.tasks','INSERT') OR has_column_privilege($1,'research.tasks','document','UPDATE') OR has_table_privilege($1,'research.results','INSERT')").bind(role).fetch_one(&pool).await?;
        assert!(
            !can_write,
            "reader/Session identities cannot mutate task or results"
        );
    }
    let mut tx = pool.begin().await?;
    sqlx_core::raw_sql::raw_sql("SET LOCAL ROLE monday_research_artifact_gateway")
        .execute(&mut *tx)
        .await?;
    let rejected = sqlx_core::query::query("SELECT research.artifact_write_permit($1,$2,1,1)")
        .bind("fixture")
        .bind("a".repeat(64))
        .fetch_one(&mut *tx)
        .await
        .unwrap_err();
    assert!(rejected
        .to_string()
        .contains("artifact authority is paused"));
    tx.rollback().await?;
    // The independent approval owner is not a scientific or release writer.
    for role in [
        "monday_research_submitter",
        "monday_research_reconciler",
        "monday_research_session_host",
        "monday_research_artifact_gateway",
        "monday_research_prepare_worker",
        "monday_research_definition_writer",
        "monday_research_release_importer",
        "monday_research_native_admission",
        "monday_research_terminal_retirement",
    ] {
        let writes:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege($1,'research.build_import_admissions','INSERT') OR has_table_privilege($1,'research.build_import_admissions','UPDATE') OR has_table_privilege($1,'research.build_import_admissions','DELETE') OR pg_has_role($1,'monday_research_build_import_owner','MEMBER')")
            .bind(role).fetch_one(&pool).await?;
        ensure!(!writes, "ordinary role can approve its own Build: {role}");
    }
    let owner_correct:bool=sqlx_core::query_scalar::query_scalar("SELECT has_table_privilege('monday_research_build_import_owner','research.build_import_admissions','SELECT,INSERT,UPDATE') AND NOT has_table_privilege('monday_research_build_import_owner','research.build_import_admissions','DELETE') AND NOT has_table_privilege('monday_research_build_import_owner','research.build_releases','INSERT') AND NOT has_table_privilege('monday_research_build_import_owner','research.runs','INSERT') AND NOT has_table_privilege('monday_research_build_import_owner','research.admissions','INSERT') AND NOT has_table_privilege('monday_research_build_import_owner','research.backends','UPDATE')")
        .fetch_one(&pool).await?;
    ensure!(
        owner_correct,
        "independent owner acquired release/Run/runtime authority"
    );
    let flags:bool=sqlx_core::query_scalar::query_scalar("SELECT NOT rolcanlogin AND NOT rolsuper AND NOT rolcreaterole FROM pg_roles WHERE rolname='monday_research_build_import_owner'").fetch_one(&pool).await?;
    ensure!(
        flags,
        "independent approval owner role must not provision login/admin authority"
    );
    // SQL-only schema fixture, not an admitted scientific Campaign. A resolved
    // execution cannot settle while the separately expected Secret is unknown.
    sqlx_core::query::query("UPDATE research.tasks SET state='stopping',document=$1 WHERE task_id=$2")
        .bind(serde_json::json!({"id":task,"state":"stopping","attempt":1,"fence":1,"spec":{"kind":"cex_campaign"},"schema_fixture":true}))
        .bind(&task).execute(&pool).await?;
    let settle = "UPDATE research.tasks SET state='cancelled',document=jsonb_set(document,'{state}','\"cancelled\"') WHERE task_id=$1";
    assert!(sqlx_core::query::query(settle)
        .bind(&task)
        .execute(&pool)
        .await
        .is_err());
    for kind in ["execution", "identity"] {
        sqlx_core::query::query("INSERT INTO research.provider_create_operations(task_id,attempt,fence,kind,cluster,namespace,resource_name,task_revision,state) VALUES($1,1,1,$2,'fixture','research',$2,1,'not_started')")
            .bind(&task).bind(kind).execute(&pool).await?;
        if kind == "execution" {
            assert!(sqlx_core::query::query(settle)
                .bind(&task)
                .execute(&pool)
                .await
                .is_err());
        }
    }
    sqlx_core::query::query("UPDATE research.provider_create_operations SET state='pending',request_sha256=$1,public_request='{\"kind\":\"Secret\"}',secret_data_sha256=$2 WHERE task_id=$3 AND kind='identity'")
        .bind("e".repeat(64)).bind("f".repeat(64)).bind(&task).execute(&pool).await?;
    assert!(sqlx_core::query::query(settle)
        .bind(&task)
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx_core::query::query("UPDATE research.tasks SET state='queued',document=jsonb_set(document,'{state}','\"queued\"') WHERE task_id=$1").bind(&task).execute(&pool).await.is_err());
    assert!(sqlx_core::query::query("UPDATE research.provider_create_operations SET state='not_started',request_sha256=NULL,public_request=NULL,secret_data_sha256=NULL WHERE task_id=$1 AND kind='identity'").bind(&task).execute(&pool).await.is_err());
    sqlx_core::query::query("UPDATE research.provider_create_operations SET state='observed',observed_uid='fixture-secret' WHERE task_id=$1 AND kind='identity'").bind(&task).execute(&pool).await?;
    sqlx_core::query::query(settle)
        .bind(&task)
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
