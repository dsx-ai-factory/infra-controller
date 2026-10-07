-- Add migration script here
ALTER TABLE machines 
    ADD COLUMN failure_details JSONB NOT NULL DEFAULT ('{"cause": "noerror", "source": "noerror", "failed_at": "2023-07-31T11:26:18.261228950+00:00"}') 
;
