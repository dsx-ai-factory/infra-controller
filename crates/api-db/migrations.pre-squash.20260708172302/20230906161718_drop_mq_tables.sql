--- Drop unused message queue elements

-- CASCADE also drops the mq_new_t type
DROP FUNCTION IF EXISTS
	mq_poll,
	mq_uuid_exists,
	mq_latest_message,
	mq_keep_alive,
	mq_insert,
	mq_delete,
	mq_commit,
	mq_clear_all,
	mq_clear,
	mq_checkpoint,
	mq_active_channels
	CASCADE;

DROP TABLE mq_payloads;
DROP TABLE mq_msgs;
DROP TABLE bg_status;

