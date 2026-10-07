-- This fix is being inserted before the previous commit that added
-- a new status field to VPCs.  We need to safely make sure there are no null
-- VNI values.
-- We can exploit the fact that VNIs are unsigned, but the column in postgres is signed.
-- We set the VNI for any VPCs with a null VNI to a negative number with a subselect
-- that will keep the values unique by assigning a row number.
-- Later, we'll clean it up by setting a new status field to '{}' wherever we see a negative VNI.
UPDATE vpcs v SET vni = v_sub.vni*-1 FROM (select id, ROW_NUMBER() OVER (ORDER BY id) AS vni FROM vpcs WHERE vni IS null) AS v_sub WHERE v.id=v_sub.id;