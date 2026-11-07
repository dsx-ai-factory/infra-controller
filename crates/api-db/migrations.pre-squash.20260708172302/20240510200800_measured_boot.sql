--- mock_machines
---
--- This contains a mock "machines" table for the purpose
--- of simulating flows. There's not much in here, just a
--- machine ID, vendor, and product. State is derived from
--- the latest journal entry for the machine.

CREATE TABLE mock_machines (
    machine_id text PRIMARY KEY NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp()
);

--- mock_machines_attrs
---
--- This contains machine attributes. Think of it as
--- mocking out discovery info for the purpose of the
--- MVP/PoC/demo work. Ultimately the functions calls
--- written to "get_attr_info" here will stay the same,
--- but the underlying logic will be to pull discovery
--- data instead.
CREATE TABLE mock_machines_attrs (
    attribute_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    machine_id text REFERENCES mock_machines NOT NULL,
    key text NOT NULL,
    value text NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    CONSTRAINT unique_machine_attr UNIQUE(machine_id, key)
);

--- measurement_system_profiles
---
--- This table stores high-level info about a given profile,
--- which is currently the ID, a more friendly/readable name,
--- and when it was created.
---
--- A constraint exists such that the name must be unique.
---
--- The measurement_system_profiles_attrs table references the
--- parent profile ID.
CREATE TABLE measurement_system_profiles (
    profile_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    name text NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    CONSTRAINT unique_profile_name UNIQUE(name)
);

--- measurement_system_profiles_attrs
---
--- This table describes attributes that make up the profile,
--- which is currently just a hardware vendor and product, i.e.
--- {vendor=dell, product=poweredge_r750}.
---
--- A CONSTRAINT exists such that the key (e.g. `vendor`) MUST
--- be unique for a given `profile_id`.
CREATE TABLE measurement_system_profiles_attrs (
    attribute_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    profile_id uuid REFERENCES measurement_system_profiles NOT NULL,
    key text NOT NULL,
    value text NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    CONSTRAINT unique_profile_attr UNIQUE(profile_id, key)
);

--- measurement_bundle_state
---
--- This is used to set the state of a given measurement bundle,
--- providing the opportunity for an operator to "approve" the
--- bundle, allowing machines presenting matching values to
--- pass attestation (and go into a Ready state). Once approved,
--- it becomes Active.
---
--- Obsolete bundles will continue to work, but are used as
--- a feedback mechanism that a machine is on a deprecated set
--- of measurements, and will need to re-run through attestation
--- soon. Once a bundle becomes Retired (either via an operator
--- running a command, or a TTL being hit), all machines matching
--- that bundles values will not pass attestation. A Revoked
--- bundle is similar to Retired, but different in that a Revoked
--- bundle cannot be set back to an Active state.
CREATE TYPE measurement_bundle_state AS enum (
    'pending',
    'active',
    'obsolete',
    'retired',
    'revoked'
);

--- measurement_machine_state
---
--- This is used to store the state of the machine when a journal
--- entry is created from attestation. It allows an operator to see
--- the history of the results of journal entries.
CREATE TYPE measurement_machine_state AS enum (
    'discovered',
    'pendingbundle',
    'measured',
    'measuringfailed'
);

--- measurement_bundles
---
--- These are also known as the "golden measurements", where either
--- an operator creates a "golden" bundle, or an operator "promotes"
--- (manually or automatically) a measurement bundle from a machine
--- report to become a bundle. This table contains the `profile_id`
--- its associated with (i.e. the hardware profile), and a `state`
--- enum to track the state of this bundle (i.e. if a machine report
--- matches this bundle or not, and, if it does, it is an active
--- bundle).
---
--- A constraint exists such that the name must be unique.
---
CREATE TABLE measurement_bundles (
    bundle_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    name text NOT NULL,
    profile_id uuid REFERENCES measurement_system_profiles NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    state measurement_bundle_state DEFAULT 'pending' NOT NULL,
    CONSTRAINT unique_bundle_name UNIQUE(name)
);

--- measurement_bundles_values
---
--- These are the values associated with the measurement bundle,
--- as in, the values from all of the PCR banks that we care
--- about. Currently we're looking at 0-6, but this table allows
--- for as many as we would like.
---
--- A CONSTRAINT exists such that only a single pcr_register value
--- can be populated for a given bundle.
CREATE TABLE measurement_bundles_values (
    value_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    bundle_id uuid REFERENCES measurement_bundles,
    pcr_register smallint NOT NULL,
    sha256 varchar(64) NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    CONSTRAINT unique_bundle_value UNIQUE(bundle_id, pcr_register)
);

--- measurement_reports
---
--- Every time a machine reports in measurements, the record of
--- that report is kept in this table.
CREATE TABLE measurement_reports (
    report_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    machine_id text REFERENCES mock_machines NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp()
);

--- measurement_reports_values
---
--- This contains a record of the actual measurement values
--- that were reported with a given machine report entry,
--- allowing an operator to cross-check values for a failing
--- machine, and audit the values across all machines. One
--- thought being, if many machines are failing, but all have
--- the same values in common, that maybe there's something
--- significant going on (like the need for a new bundle).
---
--- This is effectively the same as a measurements_bundle_values
--- entry, except related to a `journal_id` instead of a
--- `bundle_id`, and the potential for ALL values, where a
--- bundle may be a subset of all report values.
CREATE TABLE measurement_reports_values (
    value_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    report_id uuid REFERENCES measurement_reports NOT NULL,
    pcr_register smallint NOT NULL,
    sha256 varchar(64) NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    CONSTRAINT unique_report_value UNIQUE(report_id, pcr_register)
);

--- measurement_journal
---
--- This tracks the history (including the current state) of a
--- machine in respect to its latest report, the bundle and/or
--- profile it does (or doesn't match), and the resulting state
--- for that machine.
---
--- As bundles are managed, computations are done to determine
--- if state needs to change for matching reports, and, if they
--- do, those changes get inserted into the journal.
---
--- If you need to know the current state of a machine, the
--- journal is where you look.
CREATE TABLE measurement_journal (
    journal_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    machine_id text NOT NULL,
    report_id uuid REFERENCES measurement_reports,
    profile_id uuid REFERENCES measurement_system_profiles,
    bundle_id uuid REFERENCES measurement_bundles,
    state measurement_machine_state DEFAULT 'discovered' NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp()
);

--- measurement_approved_type
---
--- This is the type of approval for a machine or profile.
---
--- Instances here can be configured as `oneshot` or `persist`,
--- where `oneshot` will remove the machine/profile from the table
--- immediately after its bundle was accepted, and `persist`
--- means the service will continue to allow any and all
--- further updates to be accepted as [potentially] additional
--- golden measurement bundles.
CREATE TYPE measurement_approved_type AS enum (
    'oneshot',
    'persist'
);

--- measurement_approved_machines
---
--- This is a table of allowed machine IDs whose measurements
--- will automatically be "accepted" as golden measurement
--- bundles. In other words, their journal entries will be
--- automatically approved. This is a special case in which
--- the values will be taken to initalize a bundle BEFORE
--- the journal is written, allowing the journal entry to
--- automatically "match" a bundle from the start.
---
--- A CONSTRAINT exists such that the `machine_id` MUST be unique.
CREATE TABLE measurement_approved_machines (
    approval_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    machine_id text NOT NULL,
    approval_type measurement_approved_type DEFAULT 'oneshot' NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    pcr_registers text,
    comments text,
    CONSTRAINT unique_machine_id UNIQUE(machine_id)
);

--- measurement_approved_profiles
---
--- This is a table of allowed profile IDs whose measurements
--- will automatically be "accepted" as golden measurement
--- bundles. In other words, their journal entries will be
--- automatically approved. This is a special case in which
--- the values will be taken to initalize a bundle BEFORE
--- the journal is written, allowing the journal entry to
--- automatically "match" a bundle from the start.
---
--- A CONSTRAINT exists such that the `profile_id` MUST be unique.
CREATE TABLE measurement_approved_profiles (
    approval_id uuid PRIMARY KEY DEFAULT gen_random_uuid (),
    profile_id uuid references measurement_system_profiles NOT NULL,
    approval_type measurement_approved_type DEFAULT 'oneshot' NOT NULL,
    ts timestamp with time zone DEFAULT clock_timestamp(),
    pcr_registers text,
    comments text,
    CONSTRAINT unique_profile_id UNIQUE(profile_id)
);
