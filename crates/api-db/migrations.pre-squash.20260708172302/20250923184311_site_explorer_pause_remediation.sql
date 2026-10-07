-- Add pause_remediation flag to explored_endpoints table
-- This flag prevents site explorer from taking remediation actions on redfish errors
ALTER TABLE explored_endpoints
  ADD COLUMN pause_remediation bool NOT NULL DEFAULT false;
