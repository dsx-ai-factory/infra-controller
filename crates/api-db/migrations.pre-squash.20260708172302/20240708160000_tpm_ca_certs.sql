-- tpm ca cert stores all CA certificates for all TPMs for a given site
-- it is expected that this table is populated at Sites creation
-- TPM EK certificates are validated against these CA certs
CREATE TABLE tpm_ca_certs(
    id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    not_valid_before TIMESTAMPTZ NOT NULL,
    not_valid_after TIMESTAMPTZ NOT NULL,
    ca_cert_der BYTEA NOT NULL,
    -- the reason for storing cert subject as DER is to 
    -- avoid problems with different formatting of this field
    -- by various parsers
    cert_subject BYTEA NOT NULL,

    CONSTRAINT tpm_ca_certs_unique_ca_cert_der UNIQUE(ca_cert_der)
);

-- this table stores the status of ek certs
CREATE TABLE ek_cert_verification_status(
    -- an sha256 of ek cert (this is not always the same as fingerprint!)
    ek_sha256 BYTEA NOT NULL,
    serial_num text NOT NULL,
    -- is there a CA file in tpm_ca_certs that signed this EK cert?
    signing_ca_found BOOLEAN DEFAULT FALSE NOT NULL,
    ca_id INT,
    -- this maps to cert_subject field from tpm_ca_certs
    issuer BYTEA NOT NULL,
    -- this is to store Authority Information Access X.509 Extension (1.3.6.1.5.5.7.1.1)
    issuer_access_info text,
    -- machine id that this EK cert came from
    machine_id text NOT NULL,

    PRIMARY KEY(ek_sha256),
    FOREIGN KEY(ca_id) REFERENCES tpm_ca_certs(id)
);
