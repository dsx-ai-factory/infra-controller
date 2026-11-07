-- Add migration script here

-- this identifies consecutive same reports for a machine id and removes journals, report values, and, finally, reports themselves
-- except for the very last one

WITH flattened_table AS (    
-- Create a signature for each report based on its PCR data
    SELECT 
	r.machine_id,
        r.report_id,
        r.ts,
        string_agg(v.pcr_register::text || ':' || v.sha_any::text, '|' ORDER BY v.pcr_register) AS report_signature
    FROM measurement_reports r
    JOIN measurement_reports_values v ON r.report_id = v.report_id
    GROUP BY r.machine_id, r.ts, r.report_id
    ORDER BY r.machine_id, r.ts
),
duplicate_identification AS (
    SELECT
        machine_id,
        report_id,
        report_signature,
        ts,
        -- Check if current value equals next value
        CASE
            WHEN report_signature = LEAD(report_signature) OVER (PARTITION BY machine_id ORDER BY ts)
            THEN 1
            ELSE 0
        END as has_next_duplicate
    FROM flattened_table
),
duplicate_reports AS (
	SELECT
	    machine_id,
	    report_id,
	    report_signature,
	    ts
	FROM duplicate_identification
	WHERE has_next_duplicate = 1
),
deleted_journals AS (
    DELETE FROM measurement_journal
    WHERE report_id IN (SELECT report_id FROM duplicate_reports)
    RETURNING report_id
),
deleted_values AS (
    DELETE FROM measurement_reports_values
    WHERE report_id IN (SELECT report_id FROM duplicate_reports)
    RETURNING report_id
)
DELETE FROM measurement_reports
WHERE report_id IN (SELECT report_id FROM duplicate_reports);