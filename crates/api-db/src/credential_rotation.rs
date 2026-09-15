//! Runtime writer for the credential-rotation bookkeeping tables.
//!
//! `device_credential_rotation` records, per device and credential type, the
//! last confirmed version of the site-wide credential applied on the hardware --
//! the convergence marker the rotation engine drives toward
//! `sitewide_credential_rotation.target_version`.
//!
//! Already-ingested devices are populated once by the
//! `*_credential_rotation_backfill` data migration. This module records the
//! same fact at runtime, at the moment NICo actually sets a credential on a
//! device (factory -> site-wide at ingestion), so the table does not go stale
//! as new sites and new hardware are adopted. The migration handles "before the
//! upgrade"; these hooks handle "ever after".
//!
//! Ingestion hooks wired today (calling [`record_device_converged`]) record the
//! fact at the moment NICo writes the credential, not when the device row is
//! later persisted:
//!
//! * `bmc` -- at `site-explorer` `BmcEndpointExplorer::set_bmc_root_credentials`,
//!   the single point where every host, DPU, switch, and power-shelf BMC is
//!   moved onto (or confirmed on) the site-wide root and its per-device secret
//!   is written.
//! * `host_uefi` -- when the host UEFI password is set on the device
//!   (`api-core` `set_host_uefi_password` and the machine-controller UEFI-setup
//!   state, alongside stamping `machines.bios_password_set_time`).
//! * `dpu_uefi` -- when the DPU UEFI password is set on the device, at the
//!   machine-controller `DpuInitState::WaitingForPlatformConfiguration` state
//!   right after `uefi_setup(dpu = true)` succeeds (keyed by the DPU BMC MAC,
//!   mirroring the backfill).
//! * `lockdown_ikm` -- staged as a two-phase rotation keyed by the card (NIC)
//!   MAC, so the recorded convergence version is always the one the hardware was
//!   actually locked under rather than the (mutable) site-wide target re-read at
//!   observation time. When api-core issues the lock command it stamps the IKM
//!   version the lock key was derived from as the in-flight marker via
//!   [`mark_device_rotating_to_version`] (`rotating_to_version`); when
//!   dpa-manager `handle_locking` sees the card report Locked
//!   (`card_state.lockmode == Locked`) it promotes that exact value to
//!   `current_version` via [`promote_rotating_to_current`]. A card with no staged
//!   marker (locked before this flow shipped, already at v0 from the backfill)
//!   falls back to [`record_device_converged`] at the site-wide target. The lock
//!   command derives from the staged site-wide target when lockdown rotation is
//!   enabled (otherwise from the card's current tracked version, so a staged
//!   target does not migrate cards until the cutover flip); the staged
//!   `rotating_to_version` is exactly the crash-safety marker that keeps a
//!   mid-flight advance from mis-recording a card as converged to a version it
//!   was never locked under.
//!
//! NVOS stages its target before dispatch and records an optional backend job.
//! Missing or terminal job observations retry that exact target. Completion is
//! promoted only after per-device credential readback, with target, attempt, and
//! job comparisons fencing stale responses.
//!
//! Teardown hooks (calling [`delete_device_converged`]) remove a marker when the
//! credential it tracks is torn down, keeping the table honest:
//!
//! * `bmc` -- at `api-core` `delete_bmc_root_credentials_by_mac`, alongside
//!   deleting the per-device BMC secret. Once NICo discards the
//!   secret it can no longer authenticate or rotate, so the marker is meaningless.
//! * `host_uefi` -- in the `api-core` force-delete path, right after
//!   `clear_host_uefi_password` resets the password on the device: the host no
//!   longer carries the site-wide UEFI value, so the marker is false.
//!
//! Markers NICo does *not* tear down (the device keeps the site-wide credential,
//! or NICo keeps the secret) are left to the rotation engine, which must always
//! join `device_credential_rotation` to the live device tables when selecting
//! work so a row orphaned by device deletion is never acted on.

use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Duration, Utc};
use mac_address::MacAddress;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, DatabaseError};

/// Backoff floor: the first failed rotation attempt quarantines the device for
/// this long before the engine will retry it. Set to 15 minutes so retries are
/// spaced far enough apart -- from the very first failure -- that repeated failed
/// BMC logins cannot cluster tightly enough to trip a device's account-lockout
/// policy (whose failure counters reset on the order of minutes).
const BACKOFF_BASE_SECS: i64 = 900;
/// Backoff ceiling: the quarantine window never grows past this (~1 hour), so a
/// permanently failing device is still retried periodically.
const BACKOFF_CAP_SECS: i64 = 3600;

/// Exponential, capped backoff window: `now + min(BASE * 2^prior_attempts, CAP)`.
///
/// `prior_attempts` is the failure count recorded *before* the attempt being
/// quarantined, so the first failure (`prior_attempts = 0`) waits [`BACKOFF_BASE_SECS`],
/// the second waits twice that, and so on until [`BACKOFF_CAP_SECS`]. The engine
/// passes the result to [`increment_rotate_attempt`] as `quarantined_until`.
///
/// Saturating arithmetic makes this total for any `i32` input: a huge attempt
/// count saturates `2^exp` (and the multiply) at `i64::MAX`, which [`BACKOFF_CAP_SECS`]
/// then clamps back to the ceiling -- so no separate exponent limit is needed.
pub fn backoff_until(prior_attempts: i32, now: DateTime<Utc>) -> DateTime<Utc> {
    let exp = prior_attempts.max(0) as u32;
    let secs = BACKOFF_BASE_SECS
        .saturating_mul(2_i64.saturating_pow(exp))
        .min(BACKOFF_CAP_SECS);
    now + Duration::seconds(secs)
}

/// Mirrors the `credential_rotation_type` Postgres enum
/// (`20260623120000_credential_rotation.sql`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "credential_rotation_type", rename_all = "snake_case")]
pub enum CredentialRotationType {
    Bmc,
    HostUefi,
    DpuUefi,
    Nvos,
    LockdownIkm,
    DpuBmcService,
}

/// Records that `device_mac` now carries the current site-wide `credential_type`
/// credential, i.e. it has converged to the active `target_version`.
///
/// Call this right after NICo sets the credential on the device (the factory ->
/// site-wide step at ingestion). The recorded `current_version` is the
/// credential type's current site-wide `target_version`, so a device ingested
/// during or after a rotation is recorded at the version it actually received.
///
/// Requires a `sitewide_credential_rotation` row for `credential_type` to
/// already exist; the backfill migration seeds one for every active type. If it
/// is absent this returns [`DatabaseError::MissingSitewideRotationTarget`]
/// rather than guessing a version -- see the body for why guessing is unsafe.
///
/// Idempotent: an existing row (re-ingestion, retry, or the backfill migration)
/// is left untouched, so this never clobbers a version the rotation engine is
/// tracking -- the engine owns all subsequent version transitions.
pub async fn record_device_converged(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
) -> Result<(), DatabaseError> {
    // Resolve the site-wide target up front and fail loudly if it is missing.
    //
    // For every credential type wired today (bmc, host_uefi, dpu_uefi,
    // lockdown_ikm) the backfill data migration unconditionally seeds a
    // `sitewide_credential_rotation` row, so an absent row is never a normal
    // condition -- it is a corrupted invariant. The previous COALESCE(..., 0)
    // masked that: it recorded `current_version = 0`, which may be the wrong
    // convergence version (the device actually received whatever the live
    // target was), and the `ON CONFLICT DO NOTHING` below then froze that wrong
    // value forever (the engine owns transitions and never clobbers it). Once
    // the site-wide row was restored the engine would see `0 < target` and
    // drive a spurious rotation of a security credential. Erroring instead
    // surfaces the broken state and lets it self-heal once the row exists.
    //
    // Resolving the target here and recording it is correct only when the
    // device is known to carry the *current* site-wide credential -- i.e. NICo
    // just set factory -> site-wide. A caller that locked/derived against a
    // specific version it captured earlier must instead stage that version with
    // [`mark_device_rotating_to_version`] and promote it on confirmation with
    // [`promote_rotating_to_current`], so a target advancing between derivation
    // and confirmation cannot mis-record the convergence version.
    let select = "SELECT target_version FROM sitewide_credential_rotation \
                  WHERE credential_type = $1";
    let target_version: i32 = sqlx::query_scalar(select)
        .bind(credential_type)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(select, e))?
        .ok_or(DatabaseError::MissingSitewideRotationTarget(
            credential_type,
        ))?;

    // Idempotent: an existing row (re-ingestion, retry, or the backfill
    // migration) is left untouched, so we never clobber a version the rotation
    // engine is tracking -- the engine owns all subsequent transitions.
    let insert = "INSERT INTO device_credential_rotation \
                      (device_mac, credential_type, current_version) \
                  VALUES ($1, $2, $3) \
                  ON CONFLICT (device_mac, credential_type) DO NOTHING";
    sqlx::query(insert)
        .bind(device_mac)
        .bind(credential_type)
        .bind(target_version)
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(insert, e))
}

/// Stages an in-flight rotation: records that `device_mac` is being moved to
/// `rotating_to_version` of `credential_type`, without touching `current_version`.
///
/// This is phase one of a two-phase convergence for credentials NICo derives
/// against a specific version it must remember (the lockdown-IKM lock flow is the
/// motivating case): api-core calls this when it *issues* the lock command,
/// capturing the exact IKM version the key was derived from. dpa-manager then
/// calls [`promote_rotating_to_current`] when the hardware confirms, so the
/// recorded convergence version is the one the card was actually locked under
/// rather than the site-wide target re-read at observation time (which may have
/// advanced in between).
///
/// Upserts so it works whether or not a row exists yet, and is idempotent across
/// retry scout cycles: the lock command is re-derived from the same version every
/// cycle, so the conditional `DO UPDATE` only writes when the staged value
/// actually changes. `current_version` is left as-is (NULL "not yet established"
/// for a first lock, or the prior converged value for a real rotation). The
/// non-negative CHECK on the column is the final guard against a bad version.
pub async fn mark_device_rotating_to_version(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO device_credential_rotation \
                     (device_mac, credential_type, rotating_to_version) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (device_mac, credential_type) \
                 DO UPDATE SET rotating_to_version = EXCLUDED.rotating_to_version \
                 WHERE device_credential_rotation.rotating_to_version \
                       IS DISTINCT FROM EXCLUDED.rotating_to_version";
    sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// `NoStagedCredentialRotation` means the device row is missing or has no
/// `rotating_to_version` to promote. Both cases share the caller's fallback
/// policy; this does not describe the hardware's credential state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoStagedCredentialRotation;

/// Completes an in-flight rotation: promotes a staged `rotating_to_version` to
/// `current_version` for `(device_mac, credential_type)` and clears the in-flight
/// marker. Returns `Applied(())` if a staged rotation was promoted, or
/// `NotApplied(NoStagedCredentialRotation)` if there was nothing to promote.
///
/// Phase two of the flow started by [`mark_device_rotating_to_version`]: called
/// when the hardware confirms the new credential. Because the promoted value is
/// the exact version staged at derivation time, a site-wide target that advanced
/// in between cannot make us record a version the hardware was never on.
///
/// Idempotent: a second call (e.g. a re-observed lock) finds `rotating_to_version`
/// already cleared and is a no-op, leaving the promoted `current_version` intact.
/// A `NotApplied` return lets the caller fall back to [`record_device_converged`]
/// for devices that were converged before this staged flow shipped (no marker).
/// The query does not compare the staged value against the completed external
/// operation's target; callers must not interpret promotion as that check.
///
/// Promotion also clears the failure bookkeeping ([`increment_rotate_attempt`]
/// writes `rotate_attempts`, `rotate_quarantined_until`, and
/// `rotate_last_error_redacted`): a device that just converged carries no stale
/// error text into `GetCredentialRotationStatus`, and, crucially, a converged
/// row never retains a future backoff window -- so the [`rotation_status`]
/// converged and quarantined buckets stay disjoint.
pub async fn promote_rotating_to_current(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
) -> Result<ConditionalWrite<(), NoStagedCredentialRotation>, DatabaseError> {
    let query = "UPDATE device_credential_rotation \
                 SET current_version = rotating_to_version, \
                     rotating_to_version = NULL, \
                     rotate_attempts = 0, \
                     rotate_quarantined_until = NULL, \
                     rotate_last_error_redacted = NULL \
                 WHERE device_mac = $1 AND credential_type = $2 \
                       AND rotating_to_version IS NOT NULL";
    let result = sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .execute(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(if result.rows_affected() > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(NoStagedCredentialRotation)
    })
}

/// Records that `device_mac` is now *unlocked* for `credential_type`: NULLs
/// `current_version` (the truth column's "no credential established" value, per
/// the `lockdown_ikm` unlock contract) and clears the in-flight `rotating_to_version`
/// marker plus all failure bookkeeping.
// Only applicable for NIC lockdown input-key-material rotation today (other rotations dont unlock the device).
pub async fn record_device_unlocked(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
) -> Result<bool, DatabaseError> {
    let query = "UPDATE device_credential_rotation \
                 SET current_version = NULL, \
                     rotating_to_version = NULL, \
                     rotate_attempts = 0, \
                     rotate_quarantined_until = NULL, \
                     rotate_last_error_redacted = NULL, \
                     rotate_job_id = NULL \
                 WHERE device_mac = $1 AND credential_type = $2";
    let result = sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .execute(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result.rows_affected() > 0)
}

/// Records a failed rotation attempt for `(device_mac, credential_type)`: bumps
/// `rotate_attempts`, stamps `rotate_last_attempt_at = now()`, stores the
/// already-redacted `error_redacted`, and sets the backoff window
/// `rotate_quarantined_until` so the engine skips this device until it expires.
///
/// This is the BMC engine's synchronous failure recorder, distinct from the
/// NVOS job-based [`record_device_rotation_failed`] (which marks a *terminal*
/// backend rejection under attempt CAS). The in-flight `rotating_to_version`
/// crash marker is deliberately left in place: once the window passes the engine
/// re-enters and its two-candidate recovery reconciles the hardware.
///
/// The caller derives `quarantined_until` from [`backoff_until`] (exponential,
/// capped). Only the failure path writes these columns; a successful
/// convergence is recorded via [`promote_rotating_to_current`], which clears
/// them again -- so a converged row never carries a future backoff window, which
/// keeps the [`rotation_status`] quarantined and converged buckets disjoint.
///
/// Operates on the existing convergence row -- a device under management always
/// has one (seeded at ingestion or by the backfill), and the engine only acts on
/// devices [`device_rotation_status`] returned a row for -- so a missing row is a
/// no-op rather than an error.
///
/// `error_redacted` MUST already have secrets removed (see the engine's redactor
/// and `carbide_redfish::libredfish::redact_password`); it is surfaced verbatim
/// by `GetCredentialRotationStatus`.
pub async fn increment_rotate_attempt(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    error_redacted: &str,
    quarantined_until: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    let query = "UPDATE device_credential_rotation \
                 SET rotate_attempts = rotate_attempts + 1, \
                     rotate_last_attempt_at = now(), \
                     rotate_last_error_redacted = $3, \
                     rotate_quarantined_until = $4 \
                 WHERE device_mac = $1 AND credential_type = $2";
    sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(error_redacted)
        .bind(quarantined_until)
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// `RotationStartNotEligible` means the site-wide target is missing or differs
/// from the requested version, or the device is converged, quarantined, or has
/// work that cannot be replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationStartNotEligible;

/// `record_device_rotation_started` stages a target before password dispatch.
///
/// Returns the new attempt number in `Applied`. `NotApplied` means the target
/// is missing or changed, the device converged, quarantine is active, or
/// unresolved work cannot be replaced. Only a later target may replace a
/// pre-dispatch rejection without a backend job. A negative target returns
/// `DatabaseError::InvalidArgument`.
pub async fn record_device_rotation_started(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
) -> Result<ConditionalWrite<i32, RotationStartNotEligible>, DatabaseError> {
    if rotating_to_version < 0 {
        return Err(DatabaseError::InvalidArgument(format!(
            "rotating_to_version must be non-negative, got {rotating_to_version}"
        )));
    }

    let query = "WITH target AS ( \
                     SELECT target_version \
                     FROM sitewide_credential_rotation \
                     WHERE credential_type = $2 \
                     FOR UPDATE \
                 ) \
                 INSERT INTO device_credential_rotation \
                     (device_mac, credential_type, rotating_to_version, \
                      rotate_attempts, rotate_last_attempt_at) \
                 SELECT $1, $2, $3, 1, now() \
                 FROM target \
                 WHERE target_version = $3 \
                 ON CONFLICT (device_mac, credential_type) DO UPDATE SET \
                     rotating_to_version = EXCLUDED.rotating_to_version, \
                     rotate_job_id = NULL, \
                     rotate_attempts = device_credential_rotation.rotate_attempts + 1, \
                     rotate_last_attempt_at = now(), \
                     rotate_last_error_redacted = NULL, \
                     rotate_quarantined_until = NULL \
                 WHERE (device_credential_rotation.rotating_to_version IS NULL \
                            AND device_credential_rotation.rotate_last_error_redacted IS NULL \
                        OR device_credential_rotation.rotating_to_version \
                               < EXCLUDED.rotating_to_version \
                            AND device_credential_rotation.rotate_last_error_redacted IS NOT NULL \
                            AND device_credential_rotation.rotate_job_id IS NULL) \
                       AND (device_credential_rotation.current_version IS NULL \
                            OR device_credential_rotation.current_version \
                               < EXCLUDED.rotating_to_version) \
                       AND (device_credential_rotation.rotate_quarantined_until IS NULL \
                            OR device_credential_rotation.rotate_quarantined_until <= now()) \
                 RETURNING rotate_attempts";

    let attempt = sqlx::query_scalar::<_, i32>(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match attempt {
        Some(attempt) => ConditionalWrite::Applied(attempt),
        None => ConditionalWrite::NotApplied(RotationStartNotEligible),
    })
}

/// `RotationAttemptNotEligible` means the device row is missing or fails
/// the writer's checks for the target, attempt, job, terminal error, or quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationAttemptNotEligible;

/// `record_device_rotation_submitted` attaches a backend job to a staged attempt.
///
/// Returns `NotApplied(RotationAttemptNotEligible)` if the row is missing,
/// the target or attempt differs, another job is recorded, or the attempt
/// has a terminal error. Recording the same job again returns `Applied(())`.
/// An empty `job_id` returns `DatabaseError::InvalidArgument`.
pub async fn record_device_rotation_submitted(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
    expected_attempt: i32,
    job_id: &str,
) -> Result<ConditionalWrite<(), RotationAttemptNotEligible>, DatabaseError> {
    if job_id.is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "rotation job ID must not be empty".to_string(),
        ));
    }

    let query = "UPDATE device_credential_rotation \
                 SET rotate_job_id = $5 \
                 WHERE device_mac = $1 AND credential_type = $2 \
                       AND rotating_to_version = $3 \
                       AND rotate_attempts = $4 \
                       AND (rotate_job_id IS NULL OR rotate_job_id = $5) \
                       AND rotate_last_error_redacted IS NULL";

    let result = sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .bind(expected_attempt)
        .bind(job_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(if result.rows_affected() > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(RotationAttemptNotEligible)
    })
}

/// `record_device_rotation_retry_started` starts another observation attempt
/// for the same unresolved target.
///
/// `Applied(attempt)` contains the new attempt number; the target never changes.
/// Returns `NotApplied(RotationAttemptNotEligible)` if the row is missing,
/// the target, attempt, or job differs, a terminal error is recorded, or
/// quarantine is active.
pub async fn record_device_rotation_retry_started(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
    expected_attempt: i32,
    expected_job_id: Option<&str>,
) -> Result<ConditionalWrite<i32, RotationAttemptNotEligible>, DatabaseError> {
    let query = "UPDATE device_credential_rotation \
                 SET rotate_job_id = NULL, \
                     rotate_attempts = rotate_attempts + 1, \
                     rotate_last_attempt_at = now(), \
                     rotate_last_error_redacted = NULL, \
                     rotate_quarantined_until = NULL \
                 WHERE device_mac = $1 AND credential_type = $2 \
                       AND rotating_to_version = $3 \
                       AND rotate_attempts = $4 \
                       AND rotate_job_id IS NOT DISTINCT FROM $5 \
                       AND rotate_last_error_redacted IS NULL \
                       AND (rotate_quarantined_until IS NULL \
                            OR rotate_quarantined_until <= now()) \
                 RETURNING rotate_attempts";

    let attempt = sqlx::query_scalar::<_, i32>(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .bind(expected_attempt)
        .bind(expected_job_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match attempt {
        Some(attempt) => ConditionalWrite::Applied(attempt),
        None => ConditionalWrite::NotApplied(RotationAttemptNotEligible),
    })
}

/// `record_device_rotation_failed` marks a definitive backend rejection
/// without changing the staged target.
///
/// Use only when no mutation was accepted. A row with a backend job is retained
/// because we may not know whether its password changed.
/// Returns `NotApplied(RotationAttemptNotEligible)` if the row is missing,
/// the target or attempt differs, or a job or terminal error is recorded.
pub async fn record_device_rotation_failed(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
    expected_attempt: i32,
    error_redacted: &str,
) -> Result<ConditionalWrite<(), RotationAttemptNotEligible>, DatabaseError> {
    let query = "UPDATE device_credential_rotation \
                 SET rotate_last_error_redacted = $5 \
                 WHERE device_mac = $1 AND credential_type = $2 \
                       AND rotating_to_version = $3 \
                       AND rotate_attempts = $4 \
                       AND rotate_job_id IS NULL \
                       AND rotate_last_error_redacted IS NULL";

    let result = sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .bind(expected_attempt)
        .bind(error_redacted)
        .execute(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(if result.rows_affected() > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(RotationAttemptNotEligible)
    })
}

/// `record_device_rotation_succeeded` promotes a target after its matching
/// backend operation confirmed convergence.
///
/// The caller must first write and read back the per-device target.
/// Returns `NotApplied(RotationAttemptNotEligible)` if the row is missing,
/// the target, attempt, or job differs, or a terminal error is recorded.
/// An empty `expected_job_id` returns `DatabaseError::InvalidArgument`.
pub async fn record_device_rotation_succeeded(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
    rotating_to_version: i32,
    expected_attempt: i32,
    expected_job_id: &str,
) -> Result<ConditionalWrite<(), RotationAttemptNotEligible>, DatabaseError> {
    if expected_job_id.is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "rotation job ID must not be empty".to_string(),
        ));
    }

    let query = "UPDATE device_credential_rotation \
                 SET current_version = rotating_to_version, \
                     rotating_to_version = NULL, \
                     rotate_job_id = NULL, \
                     rotate_last_error_redacted = NULL, \
                     rotate_quarantined_until = NULL \
                 WHERE device_mac = $1 AND credential_type = $2 \
                       AND rotating_to_version = $3 \
                       AND rotate_attempts = $4 \
                       AND rotate_job_id = $5 \
                       AND rotate_last_error_redacted IS NULL";

    let result = sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .bind(rotating_to_version)
        .bind(expected_attempt)
        .bind(expected_job_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(if result.rows_affected() > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(RotationAttemptNotEligible)
    })
}

/// Deletes the convergence row for `(device_mac, credential_type)`, if present.
///
/// Call this when NICo tears down the credential the row tracks -- either by
/// discarding its only copy (the per-device BMC secret deleted on force-delete
/// or `DeleteCredential`) or by changing it back on the device (the
/// host UEFI password cleared on force-delete). Once the credential the marker
/// depends on is gone, the marker is false and must not linger for the rotation
/// engine to act on. Idempotent: deleting a missing row is a no-op.
pub async fn delete_device_converged(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    credential_type: CredentialRotationType,
) -> Result<(), DatabaseError> {
    let query = "DELETE FROM device_credential_rotation \
                 WHERE device_mac = $1 AND credential_type = $2";
    sqlx::query(query)
        .bind(device_mac)
        .bind(credential_type)
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// The current site-wide rotation target for `credential_type`, or `None` if no
/// target row exists. A missing row means the credential type has not published
/// its initial target yet.
///
/// `RotateCredential` reads this to learn the version it must write the
/// rotate-TO secret at (`current + 1`) before publishing the new target with
/// [`set_next_target_version`].
pub async fn current_target_version(
    conn: &mut PgConnection,
    credential_type: CredentialRotationType,
) -> Result<Option<i32>, DatabaseError> {
    let query = "SELECT target_version FROM sitewide_credential_rotation \
                 WHERE credential_type = $1";
    sqlx::query_scalar::<_, i32>(query)
        .bind(credential_type)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// A site-wide rotation target published by [`set_initial_target_version`] or
/// [`set_next_target_version`].
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StagedRotation {
    /// Newly published site-wide target version.
    pub target_version: i32,

    /// Time at which the target became visible to rotation controllers.
    pub started_at: DateTime<Utc>,
}

/// `RotationTargetNotCurrent` means publication did not match the expected
/// target version or expected absence of a target. Advancing a target does not
/// distinguish a missing row from a different version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationTargetNotCurrent;

/// `set_initial_target_version` publishes version zero when no target row exists.
///
/// The caller must create and read back the immutable version-zero credential
/// before calling this function. The insert is a compare-and-set on row absence:
/// `Applied` returns the published target, and `NotApplied` means another
/// request already initialized it.
pub async fn set_initial_target_version(
    conn: &mut PgConnection,
    credential_type: CredentialRotationType,
    request_meta: serde_json::Value,
) -> Result<ConditionalWrite<StagedRotation, RotationTargetNotCurrent>, DatabaseError> {
    let query = "INSERT INTO sitewide_credential_rotation \
                     (credential_type, target_version, started_at, request_meta) \
                 VALUES ($1, 0, now(), $2) \
                 ON CONFLICT (credential_type) DO NOTHING \
                 RETURNING target_version, started_at";

    let staged = sqlx::query_as::<_, StagedRotation>(query)
        .bind(credential_type)
        .bind(request_meta)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match staged {
        Some(staged) => ConditionalWrite::Applied(staged),
        None => ConditionalWrite::NotApplied(RotationTargetNotCurrent),
    })
}

/// `set_next_target_version` atomically advances the site-wide rotation target
/// for `credential_type` from `expected_current` to `expected_current + 1`,
/// stamping `started_at = now()` and recording `request_meta`.
///
/// Returns the new [`StagedRotation`] in `Applied`, or `NotApplied` if no row
/// matched `expected_current` -- either the target differs or the row is missing.
///
/// The handler writes the rotate-TO secret at the predicted next version
/// *before* calling this, so publishing the target last guarantees a device is
/// never recorded as converged to a version whose secret has not been written
/// yet. The model is table-driven: the current site-wide credential is whichever
/// version this `target_version` names, so the bump alone makes the new version
/// current (no unversioned alias is maintained). On `NotApplied`, the caller
/// decides whether to read the target again and retry publication.
pub async fn set_next_target_version(
    conn: &mut PgConnection,
    credential_type: CredentialRotationType,
    expected_current: i32,
    request_meta: serde_json::Value,
) -> Result<ConditionalWrite<StagedRotation, RotationTargetNotCurrent>, DatabaseError> {
    let query = "UPDATE sitewide_credential_rotation \
                 SET target_version = target_version + 1, \
                     started_at = now(), \
                     request_meta = $3 \
                 WHERE credential_type = $1 AND target_version = $2 \
                 RETURNING target_version, started_at";
    let staged = sqlx::query_as::<_, StagedRotation>(query)
        .bind(credential_type)
        .bind(expected_current)
        .bind(request_meta)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match staged {
        Some(staged) => ConditionalWrite::Applied(staged),
        None => ConditionalWrite::NotApplied(RotationTargetNotCurrent),
    })
}

/// Aggregate convergence status for a site-wide rotation: how many devices have
/// reached the current target, how many are still pending, and how many are
/// quarantined (plus the quarantined MACs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationStatus {
    /// Current site-wide target version.
    pub target_version: i32,

    /// Number of live devices confirmed at or beyond the target.
    pub converged: i64,

    /// Number of live devices that are neither converged nor quarantined.
    pub pending: i64,

    /// Number of live devices whose work-claim delay is active.
    pub quarantined: i64,

    /// MAC addresses for devices counted as quarantined.
    pub quarantined_device_macs: Vec<String>,

    /// Time at which the current target was published.
    pub started_at: DateTime<Utc>,
}

/// Just the counted columns; the quarantined MAC list is gathered separately so
/// the aggregate stays a single grouped row.
#[derive(sqlx::FromRow)]
struct RotationCounts {
    target_version: i32,
    converged: i64,
    pending: i64,
    quarantined: i64,
    started_at: DateTime<Utc>,
}

/// Live switches that cannot bootstrap from complete expected-switch credentials.
pub async fn nvos_credential_source_gaps(
    conn: impl DbReader<'_>,
) -> Result<Vec<(SwitchId, Option<MacAddress>, bool)>, DatabaseError> {
    let query = "SELECT s.id AS switch_id, s.bmc_mac_address, \
                        es.nvos_username IS NOT NULL \
                            OR es.nvos_password IS NOT NULL AS malformed_expected_credentials \
                 FROM switches s \
                 LEFT JOIN expected_switches es \
                   ON es.bmc_mac_address = s.bmc_mac_address \
                 WHERE s.deleted IS NULL \
                   AND (NULLIF(es.nvos_username, '') IS NULL \
                        OR NULLIF(es.nvos_password, '') IS NULL) \
                 ORDER BY s.id";

    sqlx::query_as::<_, (SwitchId, Option<MacAddress>, bool)>(query)
        .fetch_all(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Convergence status for `credential_type`'s current site-wide target.
///
/// A device counts as `converged` once its `current_version` reaches the target,
/// `quarantined` while its backoff window is in the future, and `pending`
/// otherwise (including "not yet established", `current_version IS NULL`).
/// Errors with [`DatabaseError::MissingSitewideRotationTarget`] when no target
/// row exists.
pub async fn rotation_status(
    conn: &mut PgConnection,
    credential_type: CredentialRotationType,
) -> Result<RotationStatus, DatabaseError> {
    if credential_type == CredentialRotationType::Nvos {
        return nvos_rotation_status(conn).await;
    }

    // count(d.device_mac) ignores the synthetic all-NULL row a LEFT JOIN
    // produces when a type has zero devices, so every bucket is 0 in that case
    // while the site-wide row still yields target_version / started_at.
    let counts_query = "SELECT s.target_version, \
                            count(d.device_mac) FILTER ( \
                                WHERE d.current_version >= s.target_version) AS converged, \
                            count(d.device_mac) FILTER ( \
                                WHERE (d.current_version IS NULL \
                                       OR d.current_version < s.target_version) \
                                  AND (d.rotate_quarantined_until IS NULL \
                                       OR d.rotate_quarantined_until <= now())) AS pending, \
                            count(d.device_mac) FILTER ( \
                                WHERE d.rotate_quarantined_until > now()) AS quarantined, \
                            s.started_at \
                        FROM sitewide_credential_rotation s \
                        LEFT JOIN device_credential_rotation d \
                            ON d.credential_type = s.credential_type \
                        WHERE s.credential_type = $1 \
                        GROUP BY s.target_version, s.started_at";

    let counts = sqlx::query_as::<_, RotationCounts>(counts_query)
        .bind(credential_type)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(counts_query, e))?
        .ok_or(DatabaseError::MissingSitewideRotationTarget(
            credential_type,
        ))?;

    let macs_query = "SELECT device_mac::text FROM device_credential_rotation \
                      WHERE credential_type = $1 AND rotate_quarantined_until > now() \
                      ORDER BY device_mac";
    let quarantined_device_macs = sqlx::query_scalar::<_, String>(macs_query)
        .bind(credential_type)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(macs_query, e))?;

    Ok(RotationStatus {
        target_version: counts.target_version,
        converged: counts.converged,
        pending: counts.pending,
        quarantined: counts.quarantined,
        quarantined_device_macs,
        started_at: counts.started_at,
    })
}

/// NVOS convergence is reported over live switches.
///
/// The database cannot see credential-store-only per-device NVOS secrets, so
/// filtering by expected-switch credential columns could report false
/// convergence. Every live switch counts once; a switch without a BMC MAC or
/// actionable credential remains pending until its inventory is corrected or it
/// is removed from the live set.
async fn nvos_rotation_status(conn: &mut PgConnection) -> Result<RotationStatus, DatabaseError> {
    let counts_query = "WITH live_devices AS ( \
                            SELECT id AS switch_id, bmc_mac_address AS device_mac \
                            FROM switches \
                            WHERE deleted IS NULL \
                        ) \
                        SELECT s.target_version, \
                            count(ld.switch_id) FILTER ( \
                                WHERE d.current_version >= s.target_version) AS converged, \
                            count(ld.switch_id) FILTER ( \
                                WHERE (d.current_version IS NULL \
                                       OR d.current_version < s.target_version) \
                                  AND (d.rotate_quarantined_until IS NULL \
                                       OR d.rotate_quarantined_until <= now())) AS pending, \
                            count(ld.switch_id) FILTER ( \
                                WHERE d.rotate_quarantined_until > now()) AS quarantined, \
                            s.started_at \
                        FROM sitewide_credential_rotation s \
                        LEFT JOIN live_devices ld ON TRUE \
                        LEFT JOIN device_credential_rotation d \
                            ON d.credential_type = s.credential_type \
                           AND d.device_mac = ld.device_mac \
                        WHERE s.credential_type = $1 \
                        GROUP BY s.target_version, s.started_at";

    let counts = sqlx::query_as::<_, RotationCounts>(counts_query)
        .bind(CredentialRotationType::Nvos)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(counts_query, e))?
        .ok_or(DatabaseError::MissingSitewideRotationTarget(
            CredentialRotationType::Nvos,
        ))?;

    let macs_query = "SELECT DISTINCT d.device_mac::text AS device_mac \
                      FROM device_credential_rotation d \
                      JOIN switches s \
                        ON s.bmc_mac_address = d.device_mac \
                       AND s.deleted IS NULL \
                      WHERE d.credential_type = $1 \
                        AND d.rotate_quarantined_until > now() \
                      ORDER BY device_mac";

    let quarantined_device_macs = sqlx::query_scalar::<_, String>(macs_query)
        .bind(CredentialRotationType::Nvos)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(macs_query, e))?;

    Ok(RotationStatus {
        target_version: counts.target_version,
        converged: counts.converged,
        pending: counts.pending,
        quarantined: counts.quarantined,
        quarantined_device_macs,
        started_at: counts.started_at,
    })
}

/// Per-device convergence detail for `credential_type`'s current site-wide
/// target. The `converged` / `quarantined` flags are evaluated server-side
/// against the same `now()` the aggregate [`rotation_status`] uses, so a single
/// device's status is consistent with the site-wide counts.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DeviceRotationStatus {
    /// Site-wide target this device is converging to (from the sitewide row).
    pub target_version: i32,

    /// When the current site-wide target was staged.
    pub started_at: DateTime<Utc>,

    /// Device MAC address associated with this rotation row.
    pub device_mac: String,

    /// Last confirmed hardware version; `None` means "not yet established".
    pub current_version: Option<i32>,

    /// Non-`None` while a rotation is mid-flight on this device.
    pub rotating_to_version: Option<i32>,

    /// `current_version >= target_version` (false when `current_version` is NULL).
    pub converged: bool,

    /// In a backoff window (`rotate_quarantined_until > now()`).
    pub quarantined: bool,

    /// End of the current backoff window, when one is active.
    pub quarantined_until: Option<DateTime<Utc>>,

    /// Number of mutation attempts recorded for this device and credential.
    pub rotate_attempts: i32,

    /// Time the latest mutation attempt was staged.
    pub rotate_last_attempt_at: Option<DateTime<Utc>>,

    /// Redacted reason the current operation is blocked or failed.
    pub rotate_last_error_redacted: Option<String>,
}

/// Query row for NVOS status while the site-wide target may be unpublished.
///
/// The query starts from the live switch and left-joins the target, so these
/// target fields must be nullable.
#[derive(sqlx::FromRow)]
struct NvosDeviceRotationStatusRow {
    target_version: Option<i32>,
    started_at: Option<DateTime<Utc>>,
    device_mac: String,
    current_version: Option<i32>,
    rotating_to_version: Option<i32>,
    converged: bool,
    quarantined: bool,
    quarantined_until: Option<DateTime<Utc>>,
    rotate_attempts: i32,
    rotate_last_attempt_at: Option<DateTime<Utc>>,
    rotate_last_error_redacted: Option<String>,
}

/// Durable per-device operation fields used to resume rotation reconciliation.
///
/// The staged target identifies unresolved mutation work, the optional job ID
/// selects backend polling, the attempt number rejects stale results, and
/// terminal failure metadata prevents that work from being mistaken for a safe
/// new submission after restart.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DeviceRotationOperationState {
    /// Credential version last confirmed on the device.
    pub current_version: Option<i32>,

    /// Target version staged before backend dispatch.
    pub rotating_to_version: Option<i32>,

    /// Opaque backend job ID, when dispatch returned one.
    pub rotate_job_id: Option<String>,

    /// Monotonic operation CAS token. PostgreSQL has no unsigned integer type,
    /// so the non-negative `integer` column maps directly to `i32`.
    pub rotate_attempts: i32,

    /// Database time when the current attempt was staged.
    pub rotate_last_attempt_at: Option<DateTime<Utc>>,

    /// Redacted reason a definitive failed attempt is blocked.
    pub rotate_last_error_redacted: Option<String>,
}

/// Returns the restart-safe operation state for one device credential row.
///
/// A controller reads this before choosing whether to establish a baseline,
/// submit work, poll an existing backend job, preserve a terminal failure, or
/// treat the device as converged.
pub async fn device_rotation_operation_state(
    conn: impl DbReader<'_>,
    credential_type: CredentialRotationType,
    device_mac: MacAddress,
) -> Result<Option<DeviceRotationOperationState>, DatabaseError> {
    let query = "SELECT current_version, \
                        rotating_to_version, \
                        rotate_job_id, \
                        rotate_attempts, \
                        rotate_last_attempt_at, \
                        rotate_last_error_redacted \
                 FROM device_credential_rotation \
                 WHERE credential_type = $1 AND device_mac = $2";

    sqlx::query_as::<_, DeviceRotationOperationState>(query)
        .bind(credential_type)
        .bind(device_mac)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Convergence status for a single device's `credential_type` credential.
///
/// Returns `None` when the device does not belong to the credential type's
/// device universe; the caller surfaces that as `NotFound` rather than
/// fabricating a status for an unknown device. For NVOS, a live switch remains
/// distinguishable from an unknown device before the initial target is
/// published, and produces [`DatabaseError::MissingSitewideRotationTarget`].
pub async fn device_rotation_status(
    conn: &mut PgConnection,
    credential_type: CredentialRotationType,
    device_mac: MacAddress,
) -> Result<Option<DeviceRotationStatus>, DatabaseError> {
    if credential_type == CredentialRotationType::Nvos {
        return nvos_device_rotation_status(conn, device_mac).await;
    }

    // COALESCE guards the two derived booleans: `current_version >= target` is
    // NULL when current_version is NULL ("not yet established"), and the
    // quarantine comparison is NULL when the window is unset -- both mean false.
    let query = "SELECT s.target_version, \
                        s.started_at, \
                        d.device_mac::text AS device_mac, \
                        d.current_version, \
                        d.rotating_to_version, \
                        COALESCE(d.current_version >= s.target_version, false) AS converged, \
                        COALESCE(d.rotate_quarantined_until > now(), false) AS quarantined, \
                        d.rotate_quarantined_until AS quarantined_until, \
                        d.rotate_attempts, \
                        d.rotate_last_attempt_at, \
                        d.rotate_last_error_redacted \
                 FROM sitewide_credential_rotation s \
                 JOIN device_credential_rotation d \
                     ON d.credential_type = s.credential_type \
                 WHERE s.credential_type = $1 AND d.device_mac = $2";
    sqlx::query_as::<_, DeviceRotationStatus>(query)
        .bind(credential_type)
        .bind(device_mac)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

async fn nvos_device_rotation_status(
    conn: &mut PgConnection,
    device_mac: MacAddress,
) -> Result<Option<DeviceRotationStatus>, DatabaseError> {
    let query = "WITH live_device AS ( \
                     SELECT DISTINCT bmc_mac_address AS device_mac \
                     FROM switches \
                     WHERE deleted IS NULL AND bmc_mac_address = $2 \
                 ) \
                 SELECT s.target_version, \
                        s.started_at, \
                        ld.device_mac::text AS device_mac, \
                        d.current_version, \
                        d.rotating_to_version, \
                        COALESCE(d.current_version >= s.target_version, false) AS converged, \
                        COALESCE(d.rotate_quarantined_until > now(), false) AS quarantined, \
                        d.rotate_quarantined_until AS quarantined_until, \
                        COALESCE(d.rotate_attempts, 0) AS rotate_attempts, \
                        d.rotate_last_attempt_at, \
                        d.rotate_last_error_redacted \
                 FROM live_device ld \
                 LEFT JOIN sitewide_credential_rotation s \
                     ON s.credential_type = $1 \
                 LEFT JOIN device_credential_rotation d \
                     ON d.credential_type = $1 \
                    AND d.device_mac = ld.device_mac";

    let status = sqlx::query_as::<_, NvosDeviceRotationStatusRow>(query)
        .bind(CredentialRotationType::Nvos)
        .bind(device_mac)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let Some(status) = status else {
        return Ok(None);
    };

    let (Some(target_version), Some(started_at)) = (status.target_version, status.started_at)
    else {
        return Err(DatabaseError::MissingSitewideRotationTarget(
            CredentialRotationType::Nvos,
        ));
    };

    Ok(Some(DeviceRotationStatus {
        target_version,
        started_at,
        device_mac: status.device_mac,
        current_version: status.current_version,
        rotating_to_version: status.rotating_to_version,
        converged: status.converged,
        quarantined: status.quarantined,
        quarantined_until: status.quarantined_until,
        rotate_attempts: status.rotate_attempts,
        rotate_last_attempt_at: status.rotate_last_attempt_at,
        rotate_last_error_redacted: status.rotate_last_error_redacted,
    }))
}

// Tests for the SQL-only `*_credential_rotation_backfill` data migration. It has
// no Rust counterpart to host an inline `mod tests`, so it lives as a sibling
// child module here (mirroring `machine_interface::test_duplicate_mac`) rather
// than as a standalone top-level module.
#[cfg(test)]
mod test_backfill;

#[cfg(test)]
mod test_dpu_bmc_service_backfill;

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use mac_address::MacAddress;
    use sqlx::{PgConnection, PgPool};

    use super::{
        BACKOFF_CAP_SECS, CredentialRotationType, NoStagedCredentialRotation,
        RotationAttemptNotEligible, RotationStartNotEligible, RotationTargetNotCurrent,
        backoff_until, current_target_version, delete_device_converged,
        device_rotation_operation_state, device_rotation_status, increment_rotate_attempt,
        mark_device_rotating_to_version, promote_rotating_to_current, record_device_converged,
        record_device_rotation_failed, record_device_rotation_retry_started,
        record_device_rotation_started, record_device_rotation_submitted,
        record_device_rotation_succeeded, record_device_unlocked, rotation_status,
        set_initial_target_version, set_next_target_version,
    };
    use crate::ConditionalWrite::{Applied, NotApplied};

    // Inserts a device convergence row with an explicit current_version (and no
    // quarantine) for the status-counting tests.
    async fn insert_device(conn: &mut PgConnection, mac: &str, ctype: &str, current: Option<i32>) {
        sqlx::query(
            "INSERT INTO device_credential_rotation \
                 (device_mac, credential_type, current_version) \
             VALUES ($1::macaddr, $2::credential_rotation_type, $3)",
        )
        .bind(mac)
        .bind(ctype)
        .bind(current)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    async fn insert_switch_with_nvos_credentials(
        conn: &mut PgConnection,
        id: &str,
        bmc_mac: &str,
        deleted: bool,
        has_nvos_credentials: bool,
    ) {
        sqlx::query(
            "INSERT INTO expected_switches \
                 (serial_number, bmc_mac_address, bmc_username, bmc_password, \
                  nvos_username, nvos_password) \
             VALUES ($1, $2::macaddr, 'admin', 'pw', $3, $4)",
        )
        .bind(format!("sn-{id}"))
        .bind(bmc_mac)
        .bind(has_nvos_credentials.then_some("nvos-admin"))
        .bind(has_nvos_credentials.then_some("nvos-password"))
        .execute(&mut *conn)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO switches (id, name, config, bmc_mac_address, deleted) \
             VALUES ($1, $1, '{}'::jsonb, $2::macaddr, \
                     CASE WHEN $3 THEN now() ELSE NULL END)",
        )
        .bind(id)
        .bind(bmc_mac)
        .bind(deleted)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    async fn insert_switch(conn: &mut PgConnection, id: &str, bmc_mac: &str, deleted: bool) {
        insert_switch_with_nvos_credentials(conn, id, bmc_mac, deleted, true).await;
    }

    async fn insert_credentialless_switch(
        conn: &mut PgConnection,
        id: &str,
        bmc_mac: &str,
        deleted: bool,
    ) {
        insert_switch_with_nvos_credentials(conn, id, bmc_mac, deleted, false).await;
    }

    // current_version for a (mac, type) row, or None if no row exists.
    async fn version_of(conn: &mut PgConnection, mac: &str, credential_type: &str) -> Option<i32> {
        let row: Option<Option<i32>> = sqlx::query_scalar(
            "SELECT current_version FROM device_credential_rotation \
             WHERE device_mac = $1::macaddr \
               AND credential_type = $2::credential_rotation_type",
        )
        .bind(mac)
        .bind(credential_type)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
        row.flatten()
    }

    // rotating_to_version for a (mac, type) row, or None if no row exists or no
    // rotation is staged.
    async fn rotating_version_of(
        conn: &mut PgConnection,
        mac: &str,
        credential_type: &str,
    ) -> Option<i32> {
        let row: Option<Option<i32>> = sqlx::query_scalar(
            "SELECT rotating_to_version FROM device_credential_rotation \
             WHERE device_mac = $1::macaddr \
               AND credential_type = $2::credential_rotation_type",
        )
        .bind(mac)
        .bind(credential_type)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
        row.flatten()
    }

    // Simulates the target publication that production performs only after the
    // corresponding immutable credential has been stored and verified.
    async fn publish_nvos_target(conn: &mut PgConnection, target_version: i32) {
        sqlx::query(
            "INSERT INTO sitewide_credential_rotation \
                 (credential_type, target_version) \
             VALUES ('nvos', $1)",
        )
        .bind(target_version)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    #[crate::sqlx_test]
    async fn records_current_target_and_is_idempotent(pool: PgPool) {
        let mac1: MacAddress = "02:00:00:00:00:01".parse().unwrap();
        let mac2: MacAddress = "02:00:00:00:00:02".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        // The backfill migration seeds the bmc site-wide target at version 0, so
        // a device recorded now converges at 0.
        record_device_converged(&mut conn, mac1, CredentialRotationType::Bmc)
            .await
            .unwrap();
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:01", "bmc").await,
            Some(0)
        );

        // Bump the site-wide target. An already-recorded device must NOT be
        // clobbered -- the engine owns version transitions, not this hook.
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 3 \
             WHERE credential_type = 'bmc'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        record_device_converged(&mut conn, mac1, CredentialRotationType::Bmc)
            .await
            .unwrap();
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:01", "bmc").await,
            Some(0),
            "existing row must be preserved on re-ingestion"
        );

        // A device first seen after the bump records the current target (3).
        record_device_converged(&mut conn, mac2, CredentialRotationType::Bmc)
            .await
            .unwrap();
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:02", "bmc").await,
            Some(3),
            "a newly ingested device records the current site-wide target"
        );
    }

    #[crate::sqlx_test]
    async fn stages_and_promotes_rotation_ignoring_sitewide_target(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:0a".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        // Advance the site-wide lockdown target so it differs from the version we
        // stage. Promotion must land exactly the staged version (the one the card
        // was locked under), never the live target -- this is the TOCTOU the
        // two-phase lock flow guards against.
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 5 \
             WHERE credential_type = 'lockdown_ikm'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();

        // Phase one (issue): stage the in-flight rotation. current_version stays
        // NULL ("not yet established") until the hardware confirms.
        mark_device_rotating_to_version(&mut conn, mac, CredentialRotationType::LockdownIkm, 2)
            .await
            .unwrap();

        assert_eq!(
            rotating_version_of(&mut conn, "02:00:00:00:00:0a", "lockdown_ikm").await,
            Some(2),
            "issue must stage the derived version as the in-flight marker"
        );

        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:0a", "lockdown_ikm").await,
            None,
            "current_version must not advance until the lock is confirmed"
        );

        // Phase two (confirm): promote the staged version to current and clear
        // the in-flight marker.
        let promoted =
            promote_rotating_to_current(&mut conn, mac, CredentialRotationType::LockdownIkm)
                .await
                .unwrap();
        assert_eq!(
            promoted,
            Applied(()),
            "a staged rotation must report as promoted"
        );
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:0a", "lockdown_ikm").await,
            Some(2),
            "must promote the staged version (2), not the site-wide target (5)"
        );
        assert_eq!(
            rotating_version_of(&mut conn, "02:00:00:00:00:0a", "lockdown_ikm").await,
            None,
            "the in-flight marker must be cleared on promotion"
        );

        // Idempotent: a re-observed lock finds nothing staged, so it is a no-op
        // that leaves the promoted version intact.
        let promoted_again =
            promote_rotating_to_current(&mut conn, mac, CredentialRotationType::LockdownIkm)
                .await
                .unwrap();
        assert_eq!(
            promoted_again,
            NotApplied(NoStagedCredentialRotation),
            "a second promotion with nothing staged must report no-op"
        );
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:0a", "lockdown_ikm").await,
            Some(2),
            "current_version must be preserved when there is nothing to promote"
        );
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let now = Utc::now();

        // prior_attempts = 0 is the first failure: the base floor (15 minutes).
        assert_eq!(backoff_until(0, now), now + Duration::minutes(15));
        // Each subsequent failure doubles the window until it reaches the cap.
        assert_eq!(backoff_until(1, now), now + Duration::minutes(30));
        assert_eq!(backoff_until(2, now), now + Duration::minutes(60));
        // A large attempt count saturates at the cap rather than overflowing.
        assert_eq!(
            backoff_until(1_000, now),
            now + Duration::seconds(BACKOFF_CAP_SECS)
        );
        // i32::MAX exercises the extreme exponent path without a separate clamp.
        assert_eq!(
            backoff_until(i32::MAX, now),
            now + Duration::seconds(BACKOFF_CAP_SECS)
        );
        // A negative count (never produced in practice) floors at the base.
        assert_eq!(backoff_until(-5, now), now + Duration::minutes(15));
    }

    #[crate::sqlx_test]
    async fn increment_rotate_attempt_accumulates_and_quarantines(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:07".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        // A device under management already carries a convergence row (bmc is
        // seeded at target 0), behind which we model a failing rotation.
        record_device_converged(&mut conn, mac, CredentialRotationType::Bmc)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 1 \
             WHERE credential_type = 'bmc'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();

        let until = backoff_until(0, Utc::now());
        increment_rotate_attempt(
            &mut conn,
            mac,
            CredentialRotationType::Bmc,
            "redacted boom",
            until,
        )
        .await
        .unwrap();

        let status = device_rotation_status(
            &mut conn,
            CredentialRotationType::Bmc,
            "02:00:00:00:00:07".parse().unwrap(),
        )
        .await
        .unwrap()
        .expect("a recorded device must have a status");
        assert_eq!(status.rotate_attempts, 1);
        assert!(
            status.quarantined,
            "a future window must read as quarantined"
        );
        assert!(status.quarantined_until.is_some());
        assert!(status.rotate_last_attempt_at.is_some());
        assert_eq!(
            status.rotate_last_error_redacted.as_deref(),
            Some("redacted boom")
        );
        assert!(!status.converged, "the device is still behind the target");

        // A second failure accumulates the attempt count and overwrites the last
        // error, leaving convergence untouched.
        increment_rotate_attempt(
            &mut conn,
            mac,
            CredentialRotationType::Bmc,
            "second boom",
            backoff_until(1, Utc::now()),
        )
        .await
        .unwrap();
        let status = device_rotation_status(
            &mut conn,
            CredentialRotationType::Bmc,
            "02:00:00:00:00:07".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(status.rotate_attempts, 2);
        assert_eq!(
            status.rotate_last_error_redacted.as_deref(),
            Some("second boom")
        );

        // Incrementing a device with no row is a harmless no-op.
        increment_rotate_attempt(
            &mut conn,
            "02:00:00:00:00:fe".parse().unwrap(),
            CredentialRotationType::Bmc,
            "no row",
            backoff_until(0, Utc::now()),
        )
        .await
        .unwrap();
    }

    #[crate::sqlx_test]
    async fn promote_clears_quarantine_and_error_on_success(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:08".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        // Advance the bmc target and stage an in-flight rotation to version 1.
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 1 \
             WHERE credential_type = 'bmc'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        mark_device_rotating_to_version(&mut conn, mac, CredentialRotationType::Bmc, 1)
            .await
            .unwrap();

        // Model a prior failed attempt: attempts bumped, error stored, and a
        // future backoff window recorded.
        increment_rotate_attempt(
            &mut conn,
            mac,
            CredentialRotationType::Bmc,
            "transient boom",
            backoff_until(0, Utc::now()),
        )
        .await
        .unwrap();

        // A later attempt succeeds and promotes the staged version.
        let promoted = promote_rotating_to_current(&mut conn, mac, CredentialRotationType::Bmc)
            .await
            .unwrap();
        assert_eq!(
            promoted,
            Applied(()),
            "a staged rotation must report as promoted"
        );

        let status = device_rotation_status(
            &mut conn,
            CredentialRotationType::Bmc,
            "02:00:00:00:00:08".parse().unwrap(),
        )
        .await
        .unwrap()
        .expect("a recorded device must have a status");

        // The version advanced to the staged target, and the failure bookkeeping
        // is wiped so a converged row carries no stale error or backoff window.
        assert_eq!(status.current_version, Some(1));
        assert!(status.converged);
        assert_eq!(status.rotating_to_version, None);
        assert_eq!(status.rotate_attempts, 0);
        assert!(!status.quarantined);
        assert!(status.quarantined_until.is_none());
        assert!(status.rotate_last_error_redacted.is_none());
    }

    #[crate::sqlx_test]
    async fn record_unlocked_nulls_version_and_clears_markers(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:0d".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        // A card converged at v2 with an in-flight relock staged to v3 and a
        // prior failed attempt (attempts bumped, error stored, backoff window).
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 3 \
             WHERE credential_type = 'lockdown_ikm'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        insert_device(&mut conn, "02:00:00:00:00:0d", "lockdown_ikm", Some(2)).await;
        mark_device_rotating_to_version(&mut conn, mac, CredentialRotationType::LockdownIkm, 3)
            .await
            .unwrap();
        increment_rotate_attempt(
            &mut conn,
            mac,
            CredentialRotationType::LockdownIkm,
            "transient boom",
            backoff_until(0, Utc::now()),
        )
        .await
        .unwrap();

        // Observing the card unlocked resets the row to the clean "unlocked"
        // baseline: no current version, no in-flight marker, no failure state.
        let updated = record_device_unlocked(&mut conn, mac, CredentialRotationType::LockdownIkm)
            .await
            .unwrap();
        assert!(updated, "an existing row must report as updated");

        let status = device_rotation_status(
            &mut conn,
            CredentialRotationType::LockdownIkm,
            "02:00:00:00:00:0d".parse().unwrap(),
        )
        .await
        .unwrap()
        .expect("the row still exists after unlock");
        assert_eq!(status.current_version, None);
        assert_eq!(status.rotating_to_version, None);
        assert!(!status.converged, "a NULL current_version is not converged");
        assert_eq!(status.rotate_attempts, 0);
        assert!(!status.quarantined);
        assert!(status.quarantined_until.is_none());
        assert!(status.rotate_last_error_redacted.is_none());

        // Idempotent while the row exists; false only when no row is present.
        let again = record_device_unlocked(&mut conn, mac, CredentialRotationType::LockdownIkm)
            .await
            .unwrap();
        assert!(
            again,
            "a re-observed unlock still reports the row as present"
        );
        let missing: MacAddress = "02:00:00:00:00:ff".parse().unwrap();
        let absent =
            record_device_unlocked(&mut conn, missing, CredentialRotationType::LockdownIkm)
                .await
                .unwrap();
        assert!(!absent, "a missing row reports as not updated");
    }

    #[crate::sqlx_test]
    async fn corrected_target_supersedes_pre_dispatch_rejection(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:0c".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        publish_nvos_target(&mut conn, 1).await;

        let Applied(attempt) =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap()
        else {
            panic!("the first attempt should be staged");
        };

        assert_eq!(
            record_device_rotation_failed(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                attempt,
                "backend did not accept password rotation",
            )
            .await
            .unwrap(),
            Applied(())
        );

        let blocked =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap();

        assert_eq!(
            blocked,
            NotApplied(RotationStartNotEligible),
            "the failed target must remain blocked"
        );

        assert!(
            matches!(
                set_next_target_version(
                    &mut conn,
                    CredentialRotationType::Nvos,
                    1,
                    serde_json::json!({}),
                )
                .await
                .unwrap(),
                Applied(_)
            ),
            "operator should publish a later target"
        );

        let next_attempt =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 2)
                .await
                .unwrap();

        let Applied(next_attempt) = next_attempt else {
            panic!("a corrected target should replace a rejection");
        };

        let stale_release = record_device_rotation_failed(
            &mut conn,
            mac,
            CredentialRotationType::Nvos,
            1,
            attempt,
            "late failure from the previous attempt",
        )
        .await
        .unwrap();

        assert_eq!(stale_release, NotApplied(RotationAttemptNotEligible));

        let state = device_rotation_operation_state(&mut *conn, CredentialRotationType::Nvos, mac)
            .await
            .unwrap()
            .expect("operation state should exist");

        assert_eq!(state.current_version, None);
        assert_eq!(state.rotating_to_version, Some(2));
        assert_eq!(state.rotate_job_id, None);
        assert_eq!(state.rotate_attempts, next_attempt);

        assert_eq!(state.rotate_last_error_redacted, None);
    }

    #[crate::sqlx_test]
    async fn retry_retains_original_target_after_later_publication(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:1a".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        publish_nvos_target(&mut conn, 1).await;

        let Applied(first_attempt) =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap()
        else {
            panic!("the first attempt should be staged");
        };

        assert_eq!(
            record_device_rotation_submitted(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                first_attempt,
                "first-job",
            )
            .await
            .unwrap(),
            Applied(())
        );

        assert!(
            matches!(
                set_next_target_version(
                    &mut conn,
                    CredentialRotationType::Nvos,
                    1,
                    serde_json::json!({}),
                )
                .await
                .unwrap(),
                Applied(_)
            ),
            "operator should publish a later target"
        );

        let retry = record_device_rotation_retry_started(
            &mut conn,
            mac,
            CredentialRotationType::Nvos,
            1,
            first_attempt,
            Some("first-job"),
        )
        .await
        .unwrap();

        let retry_attempt = first_attempt + 1;
        assert_eq!(retry, Applied(retry_attempt));

        // Responses from the first attempt must leave the retry's state alone.
        assert_eq!(
            record_device_rotation_submitted(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                first_attempt,
                "first-job",
            )
            .await
            .unwrap(),
            NotApplied(RotationAttemptNotEligible)
        );
        assert_eq!(
            record_device_rotation_retry_started(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                first_attempt,
                Some("first-job"),
            )
            .await
            .unwrap(),
            NotApplied(RotationAttemptNotEligible)
        );
        assert_eq!(
            record_device_rotation_succeeded(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                first_attempt,
                "first-job",
            )
            .await
            .unwrap(),
            NotApplied(RotationAttemptNotEligible)
        );

        let state = device_rotation_operation_state(&mut *conn, CredentialRotationType::Nvos, mac)
            .await
            .unwrap()
            .expect("operation state should exist");

        assert_eq!(state.current_version, None);
        assert_eq!(state.rotating_to_version, Some(1));
        assert_eq!(state.rotate_job_id, None);
        assert_eq!(state.rotate_attempts, retry_attempt);

        assert_eq!(
            record_device_rotation_submitted(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                retry_attempt,
                "retry-job",
            )
            .await
            .unwrap(),
            Applied(())
        );

        assert_eq!(
            record_device_rotation_succeeded(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                retry_attempt,
                "retry-job",
            )
            .await
            .unwrap(),
            Applied(())
        );

        let converged =
            device_rotation_operation_state(&mut *conn, CredentialRotationType::Nvos, mac)
                .await
                .unwrap()
                .expect("operation state should exist");

        assert_eq!(converged.current_version, Some(1));
        assert_eq!(converged.rotating_to_version, None);
    }

    #[crate::sqlx_test]
    async fn active_quarantine_blocks_then_allows_initial_work_claim(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:0d".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        publish_nvos_target(&mut conn, 1).await;

        sqlx::query(
            "INSERT INTO device_credential_rotation \
             (device_mac, credential_type, current_version, rotate_quarantined_until) \
             VALUES ($1, 'nvos', 0, now() + interval '1 hour')",
        )
        .bind(mac)
        .execute(&mut *conn)
        .await
        .unwrap();

        let quarantine_active: bool = sqlx::query_scalar(
            "SELECT rotate_quarantined_until > now() \
             FROM device_credential_rotation \
             WHERE device_mac = $1 AND credential_type = 'nvos'",
        )
        .bind(mac)
        .fetch_one(&mut *conn)
        .await
        .unwrap();

        assert!(quarantine_active);

        let blocked_attempt =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap();

        assert_eq!(
            blocked_attempt,
            NotApplied(RotationStartNotEligible),
            "active quarantine must still block retry"
        );

        sqlx::query(
            "UPDATE device_credential_rotation \
             SET rotate_quarantined_until = now() - interval '1 second' \
             WHERE device_mac = $1 AND credential_type = 'nvos'",
        )
        .bind(mac)
        .execute(&mut *conn)
        .await
        .unwrap();

        let next_attempt =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap();

        assert_eq!(
            next_attempt,
            Applied(1),
            "expired quarantine must permit work claim"
        );

        let quarantine_cleared: bool = sqlx::query_scalar(
            "SELECT rotate_quarantined_until IS NULL \
             FROM device_credential_rotation \
             WHERE device_mac = $1 AND credential_type = 'nvos'",
        )
        .bind(mac)
        .fetch_one(&mut *conn)
        .await
        .unwrap();

        assert!(
            quarantine_cleared,
            "successful claim must clear expired quarantine"
        );
    }

    #[crate::sqlx_test]
    async fn accepted_job_cannot_be_marked_as_pre_dispatch_failure(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:12".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        publish_nvos_target(&mut conn, 1).await;

        let Applied(attempt) =
            record_device_rotation_started(&mut conn, mac, CredentialRotationType::Nvos, 1)
                .await
                .unwrap()
        else {
            panic!("the first attempt should be staged");
        };

        assert_eq!(
            record_device_rotation_submitted(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                attempt,
                "job-1",
            )
            .await
            .unwrap(),
            Applied(())
        );

        assert_eq!(
            record_device_rotation_failed(
                &mut conn,
                mac,
                CredentialRotationType::Nvos,
                1,
                attempt,
                "backend reported failure",
            )
            .await
            .unwrap(),
            NotApplied(RotationAttemptNotEligible)
        );

        let promoted = record_device_rotation_succeeded(
            &mut conn,
            mac,
            CredentialRotationType::Nvos,
            1,
            attempt,
            "job-1",
        )
        .await
        .unwrap();

        assert_eq!(promoted, Applied(()));

        let state = device_rotation_operation_state(&mut *conn, CredentialRotationType::Nvos, mac)
            .await
            .unwrap()
            .expect("failed operation state should remain");

        assert_eq!(state.current_version, Some(1));
        assert_eq!(state.rotating_to_version, None);
        assert_eq!(state.rotate_job_id, None);
        assert_eq!(state.rotate_last_error_redacted, None);
    }

    #[crate::sqlx_test]
    async fn delete_removes_only_the_targeted_row_and_is_idempotent(pool: PgPool) {
        let mac: MacAddress = "02:00:00:00:00:01".parse().unwrap();
        let mut conn = pool.acquire().await.unwrap();

        record_device_converged(&mut conn, mac, CredentialRotationType::Bmc)
            .await
            .unwrap();

        record_device_converged(&mut conn, mac, CredentialRotationType::HostUefi)
            .await
            .unwrap();

        // Deleting one credential type leaves the device's other markers intact.
        delete_device_converged(&mut conn, mac, CredentialRotationType::Bmc)
            .await
            .unwrap();
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:01", "bmc").await,
            None
        );
        assert_eq!(
            version_of(&mut conn, "02:00:00:00:00:01", "host_uefi").await,
            Some(0),
            "deleting bmc must not touch the host_uefi marker"
        );

        // Deleting a row that no longer exists is a no-op, not an error.
        delete_device_converged(&mut conn, mac, CredentialRotationType::Bmc)
            .await
            .unwrap();
    }

    #[crate::sqlx_test]
    async fn set_next_target_version_advances_and_detects_races(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        // bmc is seeded at target 0 by the backfill. Advancing from the current
        // target succeeds and returns the new version.
        let Applied(staged) = set_next_target_version(
            &mut conn,
            CredentialRotationType::Bmc,
            0,
            serde_json::json!({"reason": "first"}),
        )
        .await
        .unwrap() else {
            panic!("advancing from the current target must succeed");
        };
        assert_eq!(staged.target_version, 1);

        // Supersede: advancing from the new current (1) goes to 2 and re-stamps
        // started_at.
        let Applied(staged_again) = set_next_target_version(
            &mut conn,
            CredentialRotationType::Bmc,
            1,
            serde_json::json!({"reason": "second"}),
        )
        .await
        .unwrap() else {
            panic!("advancing from the new current target must succeed");
        };
        assert_eq!(staged_again.target_version, 2);
        assert!(staged_again.started_at >= staged.started_at);

        // A stale expected_current (0) no longer matches the row -- a concurrent
        // rotation already advanced it -- so the CAS makes no change and reports
        // the race via `NotApplied`.
        let stale = set_next_target_version(
            &mut conn,
            CredentialRotationType::Bmc,
            0,
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(
            stale,
            NotApplied(RotationTargetNotCurrent),
            "a stale expected version must not advance the target"
        );
        assert_eq!(
            current_target_version(&mut conn, CredentialRotationType::Bmc)
                .await
                .unwrap(),
            Some(2),
            "the failed CAS must leave the target unchanged"
        );
    }

    #[crate::sqlx_test]
    async fn initial_target_publication_is_compare_and_set_on_absence(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        assert_eq!(
            current_target_version(&mut conn, CredentialRotationType::Nvos)
                .await
                .unwrap(),
            None
        );

        let Applied(initialized) = set_initial_target_version(
            &mut conn,
            CredentialRotationType::Nvos,
            serde_json::json!({"reason": "verified secret exists"}),
        )
        .await
        .unwrap() else {
            panic!("first publisher should initialize the target");
        };

        assert_eq!(initialized.target_version, 0);

        let raced = set_initial_target_version(
            &mut conn,
            CredentialRotationType::Nvos,
            serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(raced, NotApplied(RotationTargetNotCurrent));

        assert_eq!(
            current_target_version(&mut conn, CredentialRotationType::Nvos)
                .await
                .unwrap(),
            Some(0)
        );
    }

    #[crate::sqlx_test]
    async fn rotation_status_counts_converged_pending_and_quarantined(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        // Advance host_uefi to target 1 so devices can sit on either side of it.
        assert!(matches!(
            set_next_target_version(
                &mut conn,
                CredentialRotationType::HostUefi,
                0,
                serde_json::json!({}),
            )
            .await
            .unwrap(),
            Applied(_)
        ));

        // converged: current_version >= target (1).
        insert_device(&mut conn, "02:00:00:00:00:01", "host_uefi", Some(1)).await;
        // pending: behind the target.
        insert_device(&mut conn, "02:00:00:00:00:02", "host_uefi", Some(0)).await;
        // pending: not yet established (NULL current_version).
        insert_device(&mut conn, "02:00:00:00:00:03", "host_uefi", None).await;

        // quarantined: behind the target but with a future backoff window, so it
        // counted as quarantined rather than pending.
        sqlx::query(
            "INSERT INTO device_credential_rotation \
                 (device_mac, credential_type, current_version, rotate_quarantined_until) \
             VALUES ('02:00:00:00:00:04'::macaddr, 'host_uefi', 0, now() + interval '1 hour')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        // A different credential type must not leak into the host_uefi counts.
        insert_device(&mut conn, "02:00:00:00:00:05", "bmc", Some(0)).await;

        let status = rotation_status(&mut conn, CredentialRotationType::HostUefi)
            .await
            .unwrap();
        assert_eq!(status.target_version, 1);
        assert_eq!(status.converged, 1);
        assert_eq!(status.pending, 2);
        assert_eq!(status.quarantined, 1);
        assert_eq!(
            status.quarantined_device_macs,
            vec!["02:00:00:00:00:04".to_string()]
        );

        // A type with no devices reports zero everywhere but still surfaces the
        // site-wide target/started_at (the LEFT JOIN's synthetic row is ignored).
        let empty = rotation_status(&mut conn, CredentialRotationType::DpuUefi)
            .await
            .unwrap();
        assert_eq!(empty.target_version, 0);
        assert_eq!(empty.converged, 0);
        assert_eq!(empty.pending, 0);
        assert_eq!(empty.quarantined, 0);
        assert!(empty.quarantined_device_macs.is_empty());

        // NVOS remains uninitialized until its first target secret is stored
        // and verified, so status must not fabricate a target.
        let nvos = rotation_status(&mut conn, CredentialRotationType::Nvos).await;

        assert!(matches!(
            nvos,
            Err(crate::DatabaseError::MissingSitewideRotationTarget(
                CredentialRotationType::Nvos
            ))
        ));
    }

    #[crate::sqlx_test]
    async fn nvos_status_counts_live_switches_without_rows_as_pending(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        publish_nvos_target(&mut conn, 1).await;
        insert_switch(&mut conn, "nvos-sw-1", "02:00:00:00:40:01", false).await;
        insert_credentialless_switch(&mut conn, "nvos-sw-2", "02:00:00:00:40:02", false).await;
        insert_switch(&mut conn, "nvos-sw-deleted", "02:00:00:00:40:03", true).await;

        sqlx::query(
            "INSERT INTO switches (id, name, config) \
             VALUES ('nvos-sw-no-bmc', 'nvos-sw-no-bmc', '{}'::jsonb)",
        )
        .execute(&mut *conn)
        .await
        .unwrap();

        insert_device(&mut conn, "02:00:00:00:40:01", "nvos", Some(1)).await;
        insert_device(&mut conn, "02:00:00:00:40:03", "nvos", Some(0)).await;

        let status = rotation_status(&mut conn, CredentialRotationType::Nvos)
            .await
            .unwrap();

        assert_eq!(status.target_version, 1);
        assert_eq!(status.converged, 1);
        assert_eq!(status.pending, 2);
        assert_eq!(status.quarantined, 0);

        insert_device(&mut conn, "02:00:00:00:40:02", "nvos", Some(1)).await;

        let complete = rotation_status(&mut conn, CredentialRotationType::Nvos)
            .await
            .unwrap();

        assert_eq!(complete.converged, 2);
        assert_eq!(complete.pending, 1);
        assert_eq!(complete.quarantined, 0);
    }

    #[crate::sqlx_test]
    async fn nvos_device_status_reports_live_switch_without_row_as_pending(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let live_mac: MacAddress = "02:00:00:00:50:01".parse().unwrap();
        let deleted_mac: MacAddress = "02:00:00:00:50:02".parse().unwrap();
        let unknown_mac: MacAddress = "02:00:00:00:50:ff".parse().unwrap();
        let credentialless_mac: MacAddress = "02:00:00:00:50:10".parse().unwrap();

        insert_switch(&mut conn, "nvos-device-live", "02:00:00:00:50:01", false).await;
        insert_switch(&mut conn, "nvos-device-deleted", "02:00:00:00:50:02", true).await;

        insert_credentialless_switch(
            &mut conn,
            "nvos-device-credentialless",
            "02:00:00:00:50:10",
            false,
        )
        .await;

        let before_publish =
            device_rotation_status(&mut conn, CredentialRotationType::Nvos, live_mac).await;

        let credentialless_before_publish =
            device_rotation_status(&mut conn, CredentialRotationType::Nvos, credentialless_mac)
                .await;

        assert!(matches!(
            before_publish,
            Err(crate::DatabaseError::MissingSitewideRotationTarget(
                CredentialRotationType::Nvos
            ))
        ));

        assert!(matches!(
            credentialless_before_publish,
            Err(crate::DatabaseError::MissingSitewideRotationTarget(
                CredentialRotationType::Nvos
            ))
        ));

        assert!(
            device_rotation_status(&mut conn, CredentialRotationType::Nvos, unknown_mac)
                .await
                .unwrap()
                .is_none(),
            "an unknown switch must remain NotFound before target publication"
        );

        publish_nvos_target(&mut conn, 1).await;

        let pending = device_rotation_status(&mut conn, CredentialRotationType::Nvos, live_mac)
            .await
            .unwrap()
            .expect("live switch should report pending");

        assert_eq!(pending.current_version, None);
        assert_eq!(pending.rotating_to_version, None);
        assert!(!pending.converged);
        assert_eq!(pending.rotate_attempts, 0);

        let credentialless_pending =
            device_rotation_status(&mut conn, CredentialRotationType::Nvos, credentialless_mac)
                .await
                .unwrap()
                .expect("live credentialless switch should remain visible as pending");

        assert!(!credentialless_pending.converged);
        assert_eq!(credentialless_pending.rotate_attempts, 0);

        assert!(
            device_rotation_status(&mut conn, CredentialRotationType::Nvos, deleted_mac)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[crate::sqlx_test]
    async fn device_rotation_status_reports_one_device(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();

        // Advance host_uefi to target 1 so devices can sit on either side of it.
        assert!(matches!(
            set_next_target_version(
                &mut conn,
                CredentialRotationType::HostUefi,
                0,
                serde_json::json!({}),
            )
            .await
            .unwrap(),
            Applied(_)
        ));

        // Converged: current_version >= target (1).
        insert_device(&mut conn, "02:00:00:00:00:01", "host_uefi", Some(1)).await;
        let converged = device_rotation_status(
            &mut conn,
            CredentialRotationType::HostUefi,
            "02:00:00:00:00:01".parse().unwrap(),
        )
        .await
        .unwrap()
        .expect("a recorded device must have a status");
        assert_eq!(converged.target_version, 1);
        assert_eq!(converged.current_version, Some(1));
        assert!(converged.converged);
        assert!(!converged.quarantined);
        assert_eq!(converged.device_mac, "02:00:00:00:00:01");

        // Pending: behind the target, no backoff window.
        insert_device(&mut conn, "02:00:00:00:00:02", "host_uefi", Some(0)).await;
        let pending = device_rotation_status(
            &mut conn,
            CredentialRotationType::HostUefi,
            "02:00:00:00:00:02".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!pending.converged);
        assert!(!pending.quarantined);

        // Not yet established: NULL current_version is not converged.
        insert_device(&mut conn, "02:00:00:00:00:03", "host_uefi", None).await;
        let unestablished = device_rotation_status(
            &mut conn,
            CredentialRotationType::HostUefi,
            "02:00:00:00:00:03".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(unestablished.current_version, None);
        assert!(!unestablished.converged);

        // Quarantined: behind the target with a future backoff window.
        sqlx::query(
            "INSERT INTO device_credential_rotation \
                 (device_mac, credential_type, current_version, rotate_quarantined_until, \
                  rotate_attempts, rotate_last_error_redacted) \
             VALUES ('02:00:00:00:00:04'::macaddr, 'host_uefi', 0, now() + interval '1 hour', \
                     3, 'redacted boom')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let quarantined = device_rotation_status(
            &mut conn,
            CredentialRotationType::HostUefi,
            "02:00:00:00:00:04".parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!quarantined.converged);
        assert!(quarantined.quarantined);
        assert!(quarantined.quarantined_until.is_some());
        assert_eq!(quarantined.rotate_attempts, 3);
        assert_eq!(
            quarantined.rotate_last_error_redacted.as_deref(),
            Some("redacted boom")
        );

        // An unknown MAC has no row, so the per-device status is None (the
        // handler maps that to NotFound rather than guessing a state).
        let missing = device_rotation_status(
            &mut conn,
            CredentialRotationType::HostUefi,
            "02:00:00:00:00:ff".parse().unwrap(),
        )
        .await
        .unwrap();
        assert!(missing.is_none(), "an unknown MAC must yield no status");
    }
}
