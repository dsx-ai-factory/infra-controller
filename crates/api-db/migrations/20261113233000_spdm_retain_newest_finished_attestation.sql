-- Retention kept each device's newest attempt, not its newest finished result.
-- A device being attested again has an unfinished newest attempt, so once ten
-- newer jobs completed without covering it, the finished failure behind the
-- retry became deletable. Readers of the newest finished result then see the
-- device as never finished, and the machine's attestation alerts clear with no
-- pass, waiver, or profile change behind it.
CREATE OR REPLACE FUNCTION spdm_attestation_jobs_keep_limit() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- Serialize cleanup across device completions for the same machine.
    PERFORM pg_advisory_xact_lock(
        hashtextextended(NEW.machine_id, TG_RELID::bigint)
    );

    WITH completed_jobs AS (
        SELECT
            job_id,
            MAX(started_at) AS started_at,
            BOOL_OR(evidence_collection_locked) AS evidence_collection_locked
        FROM spdm_machine_devices_attestation
        WHERE machine_id = NEW.machine_id
        GROUP BY job_id
        HAVING BOOL_AND(completed_at IS NOT NULL)
    ),
    recent_jobs AS (
        SELECT job_id
        FROM completed_jobs
        ORDER BY started_at DESC, job_id DESC
        LIMIT 10
    ),
    -- Use the same ordering as the machine-wide recovery status query.
    latest_device_jobs AS (
        SELECT DISTINCT ON (device_id) job_id
        FROM spdm_machine_devices_attestation
        WHERE machine_id = NEW.machine_id
        ORDER BY device_id, started_at DESC, job_id DESC
    ),
    -- The result a reader of the newest finished attestation still sees for a
    -- device whose newest attempt has not finished.
    latest_finished_device_jobs AS (
        SELECT DISTINCT ON (device_id) job_id
        FROM spdm_machine_devices_attestation
        WHERE machine_id = NEW.machine_id AND completed_at IS NOT NULL
        ORDER BY device_id, started_at DESC, job_id DESC
    )
    DELETE FROM spdm_machine_devices_attestation AS attestation
    USING completed_jobs AS job
    WHERE attestation.machine_id = NEW.machine_id
      AND attestation.job_id = job.job_id
      -- The controller still needs this job's rows to persist the completing
      -- iteration. A later completion can remove it if it remains eligible.
      AND job.job_id <> NEW.job_id
      AND NOT job.evidence_collection_locked
      AND NOT EXISTS (
          SELECT 1 FROM recent_jobs WHERE job_id = job.job_id
      )
      AND NOT EXISTS (
          SELECT 1 FROM latest_device_jobs WHERE job_id = job.job_id
      )
      AND NOT EXISTS (
          SELECT 1 FROM latest_finished_device_jobs WHERE job_id = job.job_id
      );

    RETURN NULL;
END;
$$;
