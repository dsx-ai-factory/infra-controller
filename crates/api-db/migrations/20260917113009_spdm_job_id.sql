ALTER TABLE spdm_machine_devices_attestation ADD COLUMN job_id uuid;
ALTER TABLE spdm_machine_devices_attestation ADD COLUMN evidence_collection_locked BOOLEAN NOT NULL DEFAULT FALSE;

-- Existing attestations have no recorded trigger origin.
ALTER TABLE spdm_machine_devices_attestation ADD COLUMN triggered_by_mh_controller BOOLEAN NOT NULL DEFAULT FALSE;

-- Each machine held one logical job before this migration, so backfill generate job ids
UPDATE spdm_machine_devices_attestation AS a
SET job_id = m.job_id
FROM (
    SELECT machine_id, gen_random_uuid() AS job_id
    FROM spdm_machine_devices_attestation
    GROUP BY machine_id
) AS m
WHERE a.machine_id = m.machine_id;

ALTER TABLE spdm_machine_devices_attestation DROP CONSTRAINT spdm_machine_devices_attestation_pkey;
ALTER TABLE spdm_machine_devices_attestation ADD PRIMARY KEY (machine_id, device_id, job_id);

CREATE UNIQUE INDEX spdm_device_evidence_collection_lock
ON spdm_machine_devices_attestation (machine_id, device_id)
WHERE evidence_collection_locked;

-- Keep the newest ten completed jobs per machine, plus jobs containing a
-- device's latest attestation. Delete only whole jobs that hold no claims.
CREATE FUNCTION spdm_attestation_jobs_keep_limit() RETURNS trigger
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
      );

    RETURN NULL;
END;
$$;

-- Cancellation requests and intermediate state updates do not grow completed history.
CREATE TRIGGER t_spdm_attestation_jobs_keep_limit
    AFTER UPDATE OF completed_at ON spdm_machine_devices_attestation
    FOR EACH ROW
    WHEN (OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL)
    EXECUTE FUNCTION spdm_attestation_jobs_keep_limit();

-- Restart legacy SPDM polling states that have no job ID to resume. I.e. we "shift" attestation to the previous state.
UPDATE machines
SET controller_state = jsonb_set(
    controller_state, '{machine_state,spdm_measuring_state}', '"triggermeasurements"', false
)
WHERE controller_state->>'state' = 'hostinit'
  AND controller_state #>> '{machine_state,state}' = 'spdmmeasuring'
  AND controller_state #> '{machine_state,spdm_measuring_state}' = '"pollresult"'::jsonb;

UPDATE machines
SET controller_state = jsonb_set(
    controller_state, '{spdm_measuring_state}', '"triggermeasurements"', false
)
WHERE controller_state->>'state' = 'preassignedmeasuring'
  AND controller_state->'spdm_measuring_state' = '"pollresult"'::jsonb;

UPDATE machines
SET controller_state = jsonb_set(
    controller_state, '{attestation_mode,spdmattestation,spdm_measuring_state}',
    '"triggermeasurements"', false
)
WHERE controller_state->>'state' = 'postassignedmeasuring'
  AND controller_state #> '{attestation_mode,spdmattestation,spdm_measuring_state}' = '"pollresult"'::jsonb;
