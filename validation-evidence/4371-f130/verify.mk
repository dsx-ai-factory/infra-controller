.PHONY: ensure-postgres test-4371-domain-handlers
ensure-postgres:
	@echo 'Owned disposable PostgreSQL loopback 31435; Docker disabled'
test-4371-domain-handlers: ensure-postgres
	go test -p 1 -count=1 -run '^Test(Create|Delete|GetAll|Get)DomainHandler_Handle$$' -v ./api/pkg/api/handler
.PHONY: test-4371-terminal
test-4371-terminal: ensure-postgres
	go test -p 1 -count=1 -run '^TestDomainCreateTerminalCoreReply_RetainsOneReservedIdentity$$' -v ./api/pkg/api/handler
.PHONY: test-4371-fairness
test-4371-fairness: ensure-postgres
	go test -p 1 -count=1 -run '^TestDomainRecovery_OneSlowLeaseDoesNotStarveNextDueRow$$' -v ./db/pkg/db/model
