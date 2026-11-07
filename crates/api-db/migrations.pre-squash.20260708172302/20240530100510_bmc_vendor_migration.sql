--- Migrate exploration report Vendor column from free text - the Vendor field of Redfish service root - to our bmc_vendor::BMCVendor enum

UPDATE explored_endpoints SET exploration_report = jsonb_set(exploration_report, '{Vendor}', '"Hpe"', false) WHERE exploration_report->>'Vendor' = 'HPE';
UPDATE explored_endpoints SET exploration_report = jsonb_set(exploration_report, '{Vendor}', '"Nvidia"', false) WHERE exploration_report->>'Vendor' = 'AMI';

