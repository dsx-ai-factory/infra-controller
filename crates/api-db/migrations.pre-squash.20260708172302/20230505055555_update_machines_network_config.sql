-- This JSON field has to exactly match the fields of the struct it is decoded into,
--// no missing fields, no extra fields. Make it blank to start, and ensure the struct
--// has only `Option` fields.
UPDATE machines SET network_config = '{}';
