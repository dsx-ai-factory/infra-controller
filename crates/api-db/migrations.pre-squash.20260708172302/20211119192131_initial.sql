-- Modeled after https://raphael.medaer.me/2019/06/12/pgfsm.html
CREATE EXTENSION pgcrypto;
CREATE TYPE machine_state AS ENUM (
	'init',
	'new',
	'adopted',
	'tested',
	'ready',
	'reset',
	'assigned',
	'broken',
	'decommissioned',
	'error',
	'unknown'
);
CREATE TYPE machine_action AS ENUM (
	'discover',
	'adopt',
	'test',
	'commission',
	'assign',
	'fail',
	'decommission',
	'recommission',
	'unassign',
	'release',
	'cleanup'
);

CREATE TYPE instance_type_capabilities as ENUM (
	'default'
);

CREATE TYPE vpc_resource_action AS ENUM (
  'initialize',
  'submit',
  'accept',
  'wait',
  'fail',
  'recommission',
  'vpcsuccess'
);

CREATE TABLE instance_types (
	id uuid DEFAULT gen_random_uuid() NOT NULL,
	short_name VARCHAR(32) NOT NULL,
	description TEXT NOT NULL,
	capabilities instance_type_capabilities NOT NULL DEFAULT 'default',
	active BOOLEAN NOT NULL DEFAULT 't',
	created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),

	PRIMARY KEY (id)
);


-- a leaf is a hbn endpoint configured by forge-vpc
CREATE TABLE vpc_resource_leafs(
    -- uuid is used for the 'name' of the leaf CRD
    id uuid DEFAULT gen_random_uuid() NOT NULL,

    loopback_ip_address inet,

    PRIMARY KEY (id)
);


CREATE table vpc_resource_leaf_events(
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  vpc_leaf_id uuid NOT NULL,
  action vpc_resource_action NOT NULL,
  timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  FOREIGN KEY (vpc_leaf_id) REFERENCES vpc_resource_leafs(id)
);

CREATE TABLE machines (
	id uuid DEFAULT gen_random_uuid() NOT NULL,

	-- if !null == at the moment this is a hbn endpoint (dpu), but could be expanded to other network api endpoints
	-- null == x86
	vpc_leaf_id uuid NULL,

	supported_instance_type uuid NULL,

	created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	deployed TIMESTAMPTZ NULL,

	PRIMARY KEY (id),
	FOREIGN KEY (supported_instance_type) REFERENCES instance_types(id),
	FOREIGN KEY (vpc_leaf_id) REFERENCES vpc_resource_leafs(id)
);

CREATE TABLE instances (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    machine_id uuid NOT NULL,

    requested TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished TIMESTAMPTZ NULL,

    user_data text,
    custom_ipxe text NOT NULL DEFAULT 'need a proper string',
    ssh_keys text[],
    managed_resource_id uuid DEFAULT gen_random_uuid() NOT NULL,
	use_custom_pxe_on_boot bool NOT NULL DEFAULT false,

    PRIMARY KEY (id),
    FOREIGN KEY (machine_id) REFERENCES machines(id),
	CONSTRAINT instances_unique_machine_id UNIQUE(machine_id)
);

CREATE TABLE machine_topologies (
    machine_id uuid NOT NULL,
    topology jsonb NOT NULL,

    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (machine_id),
    FOREIGN KEY (machine_id) REFERENCES machines(id)
);

CREATE TABLE machine_events (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	machine_id uuid NOT NULL,
	action machine_action NOT NULL,
	timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	FOREIGN KEY (machine_id) REFERENCES machines(id)
);

CREATE FUNCTION update_machine_updated_trigger() RETURNS TRIGGER
LANGUAGE plpgsql as $$
BEGIN
	NEW.updated := NOW();
	RETURN NEW;
END
$$;

CREATE TRIGGER machine_last_updated BEFORE UPDATE ON machines FOR EACH ROW EXECUTE PROCEDURE update_machine_updated_trigger();

CREATE TABLE domains(
	id uuid DEFAULT gen_random_uuid() NOT NULL,
	name VARCHAR NOT NULL UNIQUE,

	created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	deleted TIMESTAMPTZ,

	PRIMARY KEY(id),

	CONSTRAINT domain_name_lower_case CHECK (((name)::TEXT = LOWER((name)::TEXT))),
	CONSTRAINT valid_domain_name_regex CHECK ( name ~ '^(?!.*?_.*?)(?!(?:[\w]+?\.)?\-[\w\.\-]*?)(?![\w]+?\-\.(?:[\w\.\-]+?))(?=[\w])(?=[\w\.\-]*?\.+[\w\.\-]*?)(?![\w\.\-]{254})(?!(?:\.?[\w\-\.]*?[\w\-]{64,}\.)+?)[\w\.\-]+?(?<![\w\-\.]*?\.[\d]+?)(?<=[\w\-]{2,})(?<![\w\-]{25})$')
);

DROP TABLE IF EXISTS vpcs;
CREATE TABLE vpcs(
	id uuid DEFAULT gen_random_uuid() NOT NULL,
	name VARCHAR NOT NULL UNIQUE,
	organization_id VARCHAR,

	version VARCHAR(64) NOT NULL,
	created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	deleted TIMESTAMPTZ,

	PRIMARY KEY(id)
);

-- different types of network_segments
--
-- type    |  admin	| vni	  |=> managed by vpc |
--------------------------------------------------
-- underlay | false	| NULL	  |=> no
-- overlay	| false	| VNI ID  |=> yes
---admin	| true	| NULL	  |=> yes


CREATE TABLE network_segments(
	id uuid DEFAULT gen_random_uuid() NOT NULL,
	name VARCHAR NOT NULL UNIQUE,
	subdomain_id uuid NULL,
	vpc_id uuid NULL,

	mtu INTEGER NOT NULL DEFAULT 1500 CHECK(mtu >= 576 AND mtu <= 9000),

	version VARCHAR(64) NOT NULL,
	created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	deleted TIMESTAMPTZ,

	-- An admin network is not a vxlan but still needs to be sent to froge-vpc
	-- Forge-vpc has a reserved ResourceGroup name used for 'admin' network
	admin_network BOOLEAN NOT NULL default 'f',

	-- if an overlay, will be used to disambiguate overlapping IP space in future
	vni_id INT NULL,

	PRIMARY KEY(id),
	FOREIGN KEY(subdomain_id) REFERENCES domains(id),
	FOREIGN KEY(vpc_id) REFERENCES vpcs(id)
);

-- only one admin network permitted per Site (control plane)
CREATE UNIQUE INDEX idx_one_admin_network ON network_segments (admin_network) WHERE admin_network = true;

CREATE TABLE network_prefixes(
	id uuid DEFAULT gen_random_uuid() NOT NULL,
	segment_id uuid NOT NULL,

	prefix cidr NOT NULL,
	gateway inet,

	num_reserved INTEGER NOT NULL DEFAULT 0,

	PRIMARY KEY(id),
	FOREIGN KEY(segment_id) REFERENCES network_segments(id),

	-- Gateway addresses for IPv6 networks are provided by RAs and not DHCP.
	-- Gateway addresses for IPv4 networks are optional (may be a private network)
	CONSTRAINT no_gateway_on_ipv6 CHECK ((family(prefix) = 6 AND gateway IS NULL) OR family(prefix) = 4),

	-- Make sure the gateway is actually on the network
	CONSTRAINT gateway_within_network CHECK (gateway << prefix),

	EXCLUDE USING gist (prefix inet_ops WITH &&)
);

-- Make sure there''s at most one IPv4 prefix or one IPv6 prefix on a network segment
CREATE UNIQUE INDEX network_prefix_family ON network_prefixes (family(prefix), segment_id);

-- network_prefixes / network_segments are ResourceGroups in forge-vpc
CREATE TABLE network_prefix_events(
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  network_prefix_id uuid NOT NULL,
  action vpc_resource_action NOT NULL,
  timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  FOREIGN KEY (network_prefix_id) REFERENCES network_prefixes(id)
);

CREATE TABLE machine_interfaces(
	id uuid DEFAULT gen_random_uuid() NOT NULL,

    -- if machine is a dpu, this creates the relationship for the interface
	attached_dpu_machine_id uuid REFERENCES machines(id) NULL,

	machine_id uuid,
	segment_id uuid NOT NULL,

	mac_address macaddr NOT NULL,

	domain_id uuid,
	primary_interface bool NOT NULL,
	hostname VARCHAR(63) NOT NULL,

	PRIMARY KEY(id),
	FOREIGN KEY(machine_id) REFERENCES machines(id),
	FOREIGN KEY(segment_id) REFERENCES network_segments(id),
	FOREIGN KEY(domain_id) REFERENCES domains(id),

	UNIQUE (segment_id, mac_address),

	CONSTRAINT fqdn_must_be_unique UNIQUE (domain_id, hostname),
	CONSTRAINT one_primary_interface_per_machine UNIQUE (machine_id, primary_interface)
);

CREATE TABLE machine_interface_addresses(
	id uuid DEFAULT gen_random_uuid() NOT NULL,

	interface_id uuid NOT NULL,
	address inet NOT NULL,

	PRIMARY KEY(id),
	FOREIGN KEY(interface_id) REFERENCES machine_interfaces(id),

	UNIQUE (interface_id, address)
);

DROP VIEW IF EXISTS dpu_machines;
CREATE OR REPLACE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
    machines.vpc_leaf_id as vpc_leaf_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL
);

CREATE TABLE tags(
	id uuid DEFAULT gen_random_uuid() NOT NULL,
    slug VARCHAR(50) NOT NULL,
    name VARCHAR(50) NOT NULL,
    
    PRIMARY KEY(id),
    UNIQUE(slug)
);

CREATE TABLE tags_machine(
    tag_id uuid,
    target_id uuid,
    UNIQUE(tag_id, target_id),
    CONSTRAINT fk_tags_machine_slug FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE,
    CONSTRAINT fk_tags_machine FOREIGN KEY(target_id) REFERENCES machines(id) ON DELETE CASCADE
);

CREATE TABLE tags_networksegment(
    tag_id uuid,
    target_id uuid,
    UNIQUE(tag_id, target_id),
    CONSTRAINT fk_tags_machine_slug FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE,
    CONSTRAINT fk_tags_ns FOREIGN KEY(target_id) REFERENCES network_segments(id) ON DELETE CASCADE
);

-- Make sure there''s at most one IPv4 address or one IPv6 address on an interface, i guess?
CREATE UNIQUE INDEX unique_address_family_on_interface ON machine_interface_addresses (family(address), interface_id);

DROP VIEW IF EXISTS machine_dhcp_records;
CREATE OR REPLACE VIEW machine_dhcp_records AS (
	SELECT
	machines.id as machine_id,
	machine_interfaces.id as machine_interface_id,
	network_segments.id as segment_id,
	network_segments.subdomain_id as subdomain_id,
	CONCAT(machine_interfaces.hostname,'.', domains.name) as fqdn,
	machine_interfaces.mac_address as mac_address,
	machine_interface_addresses.address as address,
	network_segments.mtu as mtu,
	network_prefixes.prefix as prefix,
	network_prefixes.gateway as gateway
	FROM machine_interfaces
	LEFT JOIN machines ON machine_interfaces.machine_id=machines.id
	INNER JOIN network_segments ON network_segments.id=machine_interfaces.segment_id
	INNER JOIN network_prefixes ON network_prefixes.segment_id=network_segments.id
	INNER JOIN machine_interface_addresses ON machine_interface_addresses.interface_id=machine_interfaces.id
	INNER JOIN domains on domains.id = machine_interfaces.domain_id
	WHERE address << prefix
);

CREATE TABLE dhcp_entries
(
    machine_interface_id uuid NOT NULL,
    vendor_string VARCHAR NOT NULL,

    PRIMARY KEY(machine_interface_id, vendor_string),
    FOREIGN KEY(machine_interface_id) REFERENCES machine_interfaces(id)
);

DROP VIEW IF EXISTS dns_records;
CREATE OR REPLACE VIEW dns_records AS (
  SELECT
  CONCAT(CONCAT(hostname,'.', name), '.') as q_name, address as resource_record
  from machine_interfaces
  INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id = interface_id
  INNER JOIN domains on domains.id = machine_interfaces.domain_id AND primary_interface=true
);


CREATE TABLE instance_subnets(
  id uuid DEFAULT gen_random_uuid() NOT NULL,

  machine_interface_id uuid NOT NULL,

  network_segment_id uuid NOT NULL,

  -- an instance _can_ have more than one subnet assigned
  instance_id uuid NOT NULL REFERENCES instances(id),

  -- if null = PF, !null = vfX where X is vfnum
  vfid int NULL,

  PRIMARY KEY(id),
  FOREIGN KEY(machine_interface_id) REFERENCES machine_interfaces(id),
  FOREIGN KEY(network_segment_id) REFERENCES network_segments(id),

  -- If vfid is not null, skip constraint. Otherwise check that its between 0 and 15
  CONSTRAINT valid_vfid check (case when vfid IS NOT NULL THEN vfid <= 15 and vfid >= 0 END)

  -- NEED constraint on preventing the deletion of an instance_subnet
  -- if vfid = null(pf) without first deleting the related instance.
  -- deleting an instance_subnet where vfid != null should be permitted as
  -- !null = vf
  -- when that VF is deleted we need to tell forge-vpc to re-map that VF device to a blackhole network
);

-- only one null vfid (pf) per (instance_id, machine_interface_id)
CREATE UNIQUE INDEX idx_one_null_vfid ON instance_subnets (vfid, instance_id, machine_interface_id) WHERE vfid = NULL;

-- instance subnets and instance_subnet_addresses = ManagedResource in forge-fpc
CREATE TABLE IF NOT EXISTS instance_subnet_events(
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  instance_subnet_id uuid NOT NULL,
  action vpc_resource_action NOT NULL,
  timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  FOREIGN KEY (instance_subnet_id) REFERENCES instance_subnets(id)
);

CREATE TABLE instance_subnet_addresses(
  id uuid DEFAULT gen_random_uuid() NOT NULL,

  instance_subnet_id uuid NOT NULL,
  address inet NOT NULL,

  UNIQUE(instance_subnet_id, address),

  PRIMARY KEY(id),
  FOREIGN KEY (instance_subnet_id) REFERENCES instance_subnets(id)

  -- ADD constraint to verify address belongs in the instance_subnet
  -- e.g. we do not allow address 191.168.1.5/24 into instance_subnet 192.168.250.0/24

);

DROP VIEW IF EXISTS instance_dhcp_records;
CREATE OR REPLACE VIEW instance_dhcp_records AS (
   SELECT
   machines.id as machine_id,
   machine_interfaces.id as machine_interface_id,
   network_segments.id as segment_id,
   network_segments.subdomain_id as subdomain_id,
   CONCAT(machine_interfaces.hostname,'.', domains.name) as fqdn,
   machine_interfaces.mac_address as mac_address,
   instance_subnet_addresses.address as address,
   network_segments.mtu as mtu,
   network_prefixes.prefix as prefix,
   instance_subnets.vfid as vfid,
   network_prefixes.gateway as gateway
   FROM machine_interfaces
   LEFT JOIN machines ON machine_interfaces.machine_id=machines.id
   INNER JOIN domains on domains.id = machine_interfaces.domain_id
   INNER JOIN instances ON instances.machine_id = machines.id
   INNER JOIN instance_subnets ON instance_subnets.instance_id = instances.id
   INNER JOIN network_segments ON network_segments.id=instance_subnets.network_segment_id
   INNER JOIN network_prefixes ON network_prefixes.segment_id=network_segments.id
   INNER JOIN instance_subnet_addresses ON instance_subnet_addresses.instance_subnet_id = instance_subnets.id
   WHERE address << prefix
);

CREATE TYPE user_roles AS ENUM (
	'user',
	'administrator',
	'operator',
	'noaccess'
);

CREATE table ssh_public_keys (
    username VARCHAR NOT NULL UNIQUE,
    role user_roles NOT NULL,
    pubkeys VARCHAR ARRAY
);

CREATE TYPE console_type AS ENUM (
	'ipmi',
	'redfish'
);

CREATE table machine_console_metadata (
    machine_id uuid NOT NULL,
    username VARCHAR NOT NULL,
    role user_roles NOT NULL,
    password VARCHAR(16) NOT NULL,
    bmctype console_type NOT NULL DEFAULT 'ipmi',

    UNIQUE (machine_id, username, role),
	FOREIGN KEY(machine_id) REFERENCES machines(id)
);

CREATE TABLE machine_state_controller_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);
