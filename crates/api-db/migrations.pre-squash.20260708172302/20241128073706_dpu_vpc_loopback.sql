-- Add migration script here
CREATE TABLE vpc_dpu_loopbacks (
    dpu_id VARCHAR NOT NULL,
    vpc_id uuid NOT NULL REFERENCES vpcs(id),
    loopback_ip inet UNIQUE,
    
    UNIQUE (dpu_id, vpc_id),
    CONSTRAINT fk_dpu_storage_ips_mid FOREIGN KEY (dpu_id) REFERENCES machines(id) ON DELETE CASCADE,
    CONSTRAINT dpu_id_is_dpu CHECK (dpu_id LIKE 'fm100d%')
);

UPDATE vpcs SET network_virtualization_type='fnn' WHERE network_virtualization_type='fnn_l3' OR network_virtualization_type='fnn_classic';
