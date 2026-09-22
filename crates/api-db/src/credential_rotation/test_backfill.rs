//! Tests for the `*_credential_rotation_backfill` data migration.
//!
//! The migration has no Rust counterpart, so this exercises the real SQL file
//! (via `include_str!`, so the test can never silently drift from what ships)
//! against a pre-populated database.
//!
//! The `sqlx_test` harness applies every migration *before* the test body, when
//! the database is empty — so the backfill's device inserts run there as no-ops
//! (only the unconditional site-wide target rows are written). Re-applying the
//! migration here after seeding verifies the row-insertion behavior itself; the
//! statements are idempotent (`ON CONFLICT DO NOTHING`), so the second
//! application is safe and only the now-present devices get rows.

use sqlx::PgPool;

const BACKFILL_MIGRATION: &str = include_str!(
    "../../migrations.pre-squash.20260708172302/20260623130000_credential_rotation_backfill.sql"
);

const SEGMENT_ID: &str = "20000000-0000-0000-0000-000000000001";

async fn seed_segment(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO network_segments (id, name, version) VALUES ($1::uuid, 'seg', 'test')",
    )
    .bind(SEGMENT_ID)
    .execute(pool)
    .await
    .unwrap();
}

// Inserts a machine and its single BMC interface. When `bios_set` is true the
// host's UEFI password is marked as set, so the host-UEFI backfill includes it.
async fn seed_machine_with_bmc(pool: &PgPool, machine_id: &str, bmc_mac: &str, bios_set: bool) {
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

    if bios_set {
        sqlx::query("UPDATE machines SET bios_password_set_time = NOW() WHERE id = $1")
            .bind(machine_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

// Persists a SuperNIC card on `machine_id`, locked or unlocked.
async fn seed_card(pool: &PgPool, machine_id: &str, mac: &str, locked: bool) {
    let lockmode = if locked { "Locked" } else { "Unlocked" };
    sqlx::query(
        r#"INSERT INTO dpa_interfaces
               (machine_id, mac_address, device_type, pci_name, interface_type, card_state)
           VALUES ($1, $2::macaddr, 'BlueField3', $2, 'Svpc',
                   jsonb_build_object('lockmode', $3::text))"#,
    )
    .bind(machine_id)
    .bind(mac)
    .bind(lockmode)
    .execute(pool)
    .await
    .unwrap();
}

// Persists a switch (plus the expected_switches row its BMC MAC FK requires),
// optionally soft-deleted.
async fn seed_switch(pool: &PgPool, id: &str, bmc_mac: &str, deleted: bool) {
    sqlx::query(
        "INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password)
         VALUES ($1, $2::macaddr, 'admin', 'pw')",
    )
    .bind(format!("sn-{id}"))
    .bind(bmc_mac)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO switches (id, name, config, bmc_mac_address)
         VALUES ($1, $1, '{}'::jsonb, $2::macaddr)",
    )
    .bind(id)
    .bind(bmc_mac)
    .execute(pool)
    .await
    .unwrap();

    if deleted {
        sqlx::query("UPDATE switches SET deleted = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

// Persists a power shelf with a BMC MAC (plus the expected_power_shelves row
// its FK requires).
async fn seed_power_shelf(pool: &PgPool, id: &str, bmc_mac: &str) {
    sqlx::query(
        "INSERT INTO expected_power_shelves \
             (serial_number, bmc_mac_address, bmc_username, bmc_password) \
         VALUES ($1, $2::macaddr, 'admin', 'pw')",
    )
    .bind(format!("sn-{id}"))
    .bind(bmc_mac)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO power_shelves (id, name, config, bmc_mac_address) \
         VALUES ($1, $1, '{}'::jsonb, $2::macaddr)",
    )
    .bind(id)
    .bind(bmc_mac)
    .execute(pool)
    .await
    .unwrap();
}

// Persists a power shelf with no BMC MAC (e.g. not yet linked); must be skipped.
async fn seed_power_shelf_without_bmc(pool: &PgPool, id: &str) {
    sqlx::query("INSERT INTO power_shelves (id, name, config) VALUES ($1, $1, '{}'::jsonb)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

// Returns the device MACs recorded for a credential type, sorted, as text.
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
async fn backfill_records_v0_for_existing_devices(pool: PgPool) {
    seed_segment(&pool).await;

    // Two hosts (one with the UEFI password set, one without) and a DPU.
    seed_machine_with_bmc(&pool, "fm100hhost1", "02:00:00:00:01:01", true).await;
    seed_machine_with_bmc(&pool, "fm100hhost2", "02:00:00:00:02:01", false).await;
    seed_machine_with_bmc(&pool, "fm100ddpu1", "02:00:00:00:0d:01", false).await;

    // A locked and an unlocked SuperNIC card on host1.
    seed_card(&pool, "fm100hhost1", "0a:00:00:00:00:01", true).await;
    seed_card(&pool, "fm100hhost1", "0a:00:00:00:00:02", false).await;

    // A live switch and a soft-deleted switch (the latter must be skipped).
    seed_switch(&pool, "switch-live", "04:00:00:00:00:01", false).await;
    seed_switch(&pool, "switch-deleted", "04:00:00:00:00:02", true).await;

    // A power shelf with a BMC MAC and one without (the latter must be skipped).
    seed_power_shelf(&pool, "ps-live", "06:00:00:00:00:01").await;
    seed_power_shelf_without_bmc(&pool, "ps-nomac").await;

    // Apply the real migration against the now-populated database.
    sqlx::raw_sql(BACKFILL_MIGRATION)
        .execute(&pool)
        .await
        .unwrap();

    // Site-wide targets: the legacy backfill seeds bmc + UEFI + lockdown, and the
    // dpu_bmc_service seed migration adds its own site-wide row. NVOS remains
    // absent until its first target credential is stored and verified.
    let sitewide: Vec<String> = sqlx::query_scalar(
        "SELECT credential_type::text FROM sitewide_credential_rotation ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        sitewide,
        vec![
            "bmc",
            "dpu_bmc_service",
            "dpu_uefi",
            "host_uefi",
            "lockdown_ikm"
        ],
        "site-wide targets must cover bmc + dpu_bmc_service + UEFI + lockdown"
    );

    // BMC: all three machines + the live switch + the live power shelf. The
    // deleted switch and the MAC-less power shelf are excluded.
    assert_eq!(
        device_macs(&pool, "bmc").await,
        vec![
            "02:00:00:00:01:01",
            "02:00:00:00:02:01",
            "02:00:00:00:0d:01",
            "04:00:00:00:00:01",
            "06:00:00:00:00:01",
        ]
    );

    // Host UEFI: only the host with the UEFI password set (host2 unset, DPU
    // excluded by prefix), keyed by its BMC MAC.
    assert_eq!(
        device_macs(&pool, "host_uefi").await,
        vec!["02:00:00:00:01:01"]
    );

    // DPU UEFI: every DPU, keyed by its BMC MAC.
    assert_eq!(
        device_macs(&pool, "dpu_uefi").await,
        vec!["02:00:00:00:0d:01"]
    );

    // Lockdown IKM: only the currently-locked card, keyed by its NIC MAC.
    assert_eq!(
        device_macs(&pool, "lockdown_ikm").await,
        vec!["0a:00:00:00:00:01"]
    );

    // NVOS device rows are never backfilled, and every recorded device is at v0.
    let nvos_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM device_credential_rotation \
         WHERE credential_type = 'nvos'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nvos_rows, 0, "nvos must not be backfilled");

    // `IS DISTINCT FROM 0`, not `<> 0`: a plain `<>` treats `NULL <> 0` as NULL
    // and silently drops NULL rows, so a future backfill that left
    // current_version unset would slip past. Assert every row is confirmed at
    // exactly 0.
    let non_v0: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM device_credential_rotation \
         WHERE current_version IS DISTINCT FROM 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(non_v0, 0, "every backfilled device must be at v0");

    // Re-applying the migration must be idempotent (no duplicate-key errors,
    // no row-count change).
    sqlx::raw_sql(BACKFILL_MIGRATION)
        .execute(&pool)
        .await
        .unwrap();
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM device_credential_rotation")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        total, 8,
        "5 bmc + 1 host_uefi + 1 dpu_uefi + 1 lockdown_ikm"
    );
}
