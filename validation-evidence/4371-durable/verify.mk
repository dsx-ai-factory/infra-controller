.PHONY: ensure-postgres test-4371-durable-handler
ensure-postgres:
	@echo 'Owned disposable PostgreSQL 127.0.0.1:31435; Docker disabled'
test-4371-durable-handler: ensure-postgres
	go test -p 1 -count=1 -run '^TestCreateDomainHandler_Handle$$/(lost_cancellation_reply_cannot_allow_late_Ready_completion|Core_rejection_preserves_Error_reservation)$$' -v ./api/pkg/api/handler
.PHONY: test-4371-durable-activity
test-4371-durable-activity: ensure-postgres
	go test -p 1 -count=1 -run '^TestReservedDomainDurableRejection_SQLStateOrders$$' -v ./workflow/pkg/activity/domain
