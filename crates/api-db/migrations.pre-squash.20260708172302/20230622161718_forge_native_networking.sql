-- etv: EThernet Virtualizer. L2VNI
-- fnn: Forge Native Networking. L3VNI
CREATE TYPE network_virtualization_type_t AS ENUM ('etv', 'fnn');

ALTER TABLE vpcs
	ADD column network_virtualization_type network_virtualization_type_t NOT NULL DEFAULT ('etv'),
	ADD column vni integer NULL UNIQUE;

