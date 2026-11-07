-- Add instance_types
CREATE TABLE instance_types (
    id                   character varying(64) NOT NULL DEFAULT 'INVALID_INSTANCE_TYPE',
    name                 character varying NOT NULL,
    version              character varying(64) NOT NULL DEFAULT 'V1-T1666644937952267'::character varying,
    labels               jsonb NOT NULL DEFAULT '{}'::jsonb,
    desired_capabilities jsonb NOT NULL DEFAULT '[]'::jsonb,
    description          character varying NOT NULL DEFAULT '',    
    created              timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted              timestamp with time zone
);
ALTER TABLE ONLY instance_types ADD CONSTRAINT instance_types_pkey PRIMARY KEY (id);
