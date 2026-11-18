-- Remember whether force deletion must wait for Admin, including after an interrupted request.
ALTER TABLE machines ADD COLUMN force_delete_requires_admin_ack BOOLEAN;
