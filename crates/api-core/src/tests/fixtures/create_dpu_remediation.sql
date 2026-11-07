INSERT INTO dpu_remediations (script, retries, script_author) VALUES ('echo \"Hello world\"', 0, 'mbasnight');
INSERT INTO dpu_remediations (script, retries, script_author, script_reviewed_by) VALUES ('echo \"Hello world\"', 0, 'wminckler', 'ront');
INSERT INTO dpu_remediations (script, retries, script_author, script_reviewed_by, enabled) VALUES ('echo \"Hello world\"', 0, 'aforgue', 'hanyux', true);
INSERT INTO dpu_remediations (script, retries, script_author, script_reviewed_by, enabled) VALUES ('echo \"Hello world\"', 2, 'ianderson', 'abvarshney', true);
INSERT INTO dpu_remediations (metadata_name, metadata_description, metadata_labels, script, retries, script_author) VALUES ('MY_COOL_NAME', 'MY_COOL_DESCRIPTION', '{"MY_COOL_LABEL":"my_cool_label"}', 'echo \"Hello world\"', 0, 'mnoori');
