-- Add migration script here

CREATE OR REPLACE FUNCTION measured_boot_reports_keep_limit(keep_rows integer)
RETURNS INTEGER AS $$
DECLARE
  deleted_total INTEGER;
BEGIN
    -- within each machine_id group, assign a sequential number to each report
    WITH report_rows_numbered AS (
    SELECT
        machine_id,
        report_id,
        ROW_NUMBER() OVER (PARTITION BY machine_id ORDER BY ts DESC) as row_idx,
        ts
    FROM
        measurement_reports r
    ORDER BY machine_id
    ),
    -- identify all rows where their sequence number is over keep_rows
    excess_rows AS (
    SELECT
        report_id,
        row_idx,
        machine_id
    FROM
        report_rows_numbered
    WHERE row_idx > keep_rows
    ORDER BY machine_id
    ),
    -- delete journals, report values, reports
    deleted_journals AS (
        DELETE FROM measurement_journal
        WHERE report_id IN (SELECT report_id FROM excess_rows)
        RETURNING report_id
    ),
    deleted_values AS (
        DELETE FROM measurement_reports_values
        WHERE report_id IN (SELECT report_id FROM excess_rows)
        RETURNING report_id
    ),
    deleted_reports AS (
        DELETE FROM measurement_reports
        WHERE report_id IN (SELECT report_id FROM excess_rows) RETURNING *
    )
    SELECT count(*) INTO deleted_total from deleted_reports;
    RETURN deleted_total;
END;
$$ LANGUAGE plpgsql;