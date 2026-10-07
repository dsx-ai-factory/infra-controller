-- Clear out any of the negative VNIs
-- If the VNI is negative, it's because the original VNI
-- of the VPC was NULL when the new status field was added.
UPDATE vpcs SET status='{}', vni=NULL WHERE (status->>'vni')::integer < 0;