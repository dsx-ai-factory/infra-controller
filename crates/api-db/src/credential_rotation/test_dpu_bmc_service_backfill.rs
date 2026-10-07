//! Tests for the `dpu_bmc_service` seed + backfill migration.
//!
//! Exercises the real SQL file via `include_str!` (so the test cannot silently
//! drift from what ships) against a pre-populated database. As with
//! [`super::test_backfill`], the `sqlx_test` harness applies every migration
//! first against an empty database -- there the device insert is a no-op and
//! only the site-wide `dpu_bmc_service` target row is written. Seeding rows and
//! re-applying the migration here exercises the enrollment itself; the
//! statements are idempotent (`ON CONFLICT DO NOTHING`), so the second
//! application is safe and only enrolls the now-present BF4 DPUs.

use sqlx::PgPool;

const SEED_MIGRATION: &str =
    include_str!("../../migrations/20260826143513_seed_dpu_bmc_service_rotation.sql");

const SEGMENT_ID: &str = "20000000-0000-0000-0000-000000000002";

async fn seed_segment(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO network_segments (id, name, version) VALUES ($1::uuid, 'seg', 'test')",
    )
    .bind(SEGMENT_ID)
    .execute(pool)
    .await
    .unwrap();
}

// Persists a machine and its single BMC interface.
async fn seed_machine_with_bmc(pool: &PgPool, machine_id: &str, bmc_mac: &str) {
    sqlx::query(
        r#"INSERT INTO machines (id, dpf)
           VALUES ($1, '{"enabled": true, "used_for_ingestion": false}'::jsonb)"#,
    )
    .bind(machine_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO machine_interfaces
               (machine_id, segment_id, mac_address, primary_interface, hostname,
                association_type, interface_type)
           VALUES ($1, $2::uuid, $3::macaddr, false, $4, 'Machine', 'Bmc')"#,
    )
    .bind(machine_id)
    .bind(SEGMENT_ID)
    .bind(bmc_mac)
    .bind(format!("{machine_id}-bmc"))
    .execute(pool)
    .await
    .unwrap();
}

// Persists a machine, its single BMC interface, and a topology row carrying the
// DMI `product_name` the backfill classifies BF4-ness on.
async fn seed_dpu(pool: &PgPool, machine_id: &str, bmc_mac: &str, product_name: &str) {
    seed_machine_with_bmc(pool, machine_id, bmc_mac).await;
    let topology = serde_json::json!({
        "discovery_data": { "Info": { "dmi_data": { "product_name": product_name } } },
    });
    sqlx::query("INSERT INTO machine_topologies (machine_id, topology) VALUES ($1, $2::jsonb)")
        .bind(machine_id)
        .bind(topology)
        .execute(pool)
        .await
        .unwrap();
}

// Persists a DPU machine with a BMC interface but no topology row at all, to
// prove such a machine is simply skipped rather than erroring the backfill.
async fn seed_dpu_without_topology(pool: &PgPool, machine_id: &str, bmc_mac: &str) {
    seed_machine_with_bmc(pool, machine_id, bmc_mac).await;
}

async fn device_macs(pool: &PgPool, credential_type: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT device_mac::text FROM device_credential_rotation \
         WHERE credential_type = $1::credential_rotation_type ORDER BY 1",
    )
    .bind(credential_type)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[crate::sqlx_test]
async fn backfill_enrolls_only_bf4_dpus_at_v0(pool: PgPool) {
    seed_segment(&pool).await;

    // A BF4 DPU (agent DMI `product_name`), a BF4 DPU still on the Redfish-derived
    // SKU string (`B4240V`, matched via the `B4` substring), a BF3 DPU (does not
    // match), a BF4 DPU whose topology carries the pre-discovery `Unspecified
    // Model` placeholder (must be skipped), and a BF4 DPU with no topology row at
    // all (must be skipped, not error).
    seed_dpu(
        &pool,
        "fm100dbf4",
        "02:00:00:00:0d:01",
        "BlueField-4 SmartNIC Main Card",
    )
    .await;
    seed_dpu(&pool, "fm100dsku", "02:00:00:00:0d:02", "B4240V").await;
    seed_dpu(
        &pool,
        "fm100dbf3",
        "02:00:00:00:0d:03",
        "BlueField-3 SmartNIC Main Card",
    )
    .await;
    seed_dpu(&pool, "fm100dplc", "02:00:00:00:0d:04", "Unspecified Model").await;
    seed_dpu_without_topology(&pool, "fm100dnot", "02:00:00:00:0d:05").await;

    // Apply the real migration against the now-populated database.
    sqlx::raw_sql(SEED_MIGRATION).execute(&pool).await.unwrap();

    // The site-wide target row exists (seeded when migrations first ran).
    let target: i32 = sqlx::query_scalar(
        "SELECT target_version FROM sitewide_credential_rotation \
         WHERE credential_type = 'dpu_bmc_service'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(target, 0, "the site-wide service target must seed at v0");

    // Only the two BF4 DPUs with a BF4-identifying DMI product_name are enrolled.
    assert_eq!(
        device_macs(&pool, "dpu_bmc_service").await,
        vec!["02:00:00:00:0d:01", "02:00:00:00:0d:02"],
        "only BF4 DPUs with a BF4 DMI product_name get a dpu_bmc_service row"
    );

    let version: Option<i32> = sqlx::query_scalar(
        "SELECT current_version FROM device_credential_rotation \
         WHERE credential_type = 'dpu_bmc_service' AND device_mac = '02:00:00:00:0d:01'::macaddr",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(version, Some(0), "an enrolled BF4 DPU records v0");
}
