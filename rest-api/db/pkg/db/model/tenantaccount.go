// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// TenantAccountStatusPending status is pending
	TenantAccountStatusPending = "Pending"
	// TenantAccountStatusInvited status is invited
	TenantAccountStatusInvited = "Invited"
	// TenantAccountStatusReady status is ready
	TenantAccountStatusReady = "Ready"
	// TenantAccountStatusError status is error
	TenantAccountStatusError = "Error"
	// TenantAccountRelationName is the relation name for the TenantAccount model
	TenantAccountRelationName = "TenantAccount"
	TenantContactRelationName = "TenantContact"

	// names of order by fields
	tenantAccountOrderByNumber                    = "account_number"
	tenantAccountOrderByStatus                    = "status"
	tenantAccountOrderByCreated                   = "created"
	tenantAccountOrderByUpdated                   = "updated"
	tenantAccountOrderByTenantOrgNameExt          = "tenant_org_name"
	tenantAccountOrderByTenantOrgNameInt          = "tenant.org"
	tenantAccountOrderByTenantOrgDisplayNameExt   = "tenant_org_display_name"
	tenantAccountOrderByTenantOrgDisplayNameInt   = "tenant.org_display_name"
	tenantAccountOrderByTenantContactEmailExt     = "tenant_contact_email"
	tenantAccountOrderByTenantContactEmailInt     = "tenant_contact.email"
	tenantAccountOrderByTenantContactFullNameExt  = "tenant_contact_full_name"
	tenantAccountOrderByTenantContactFirstNameInt = "tenant_contact.first_name"
	tenantAccountOrderByTenantContactLastNameInt  = "tenant_contact.last_name"
	// TenantAccountOrderByDefault default field to be used for ordering when none specified
	TenantAccountOrderByDefault = tenantAccountOrderByCreated
)

var (
	// TenantAccountOrderByFields is the external list of fields that can be used for sorting
	TenantAccountOrderByFields = []string{
		tenantAccountOrderByNumber,
		tenantAccountOrderByStatus,
		tenantAccountOrderByCreated,
		tenantAccountOrderByUpdated,
		tenantAccountOrderByTenantOrgNameExt,
		tenantAccountOrderByTenantOrgDisplayNameExt,
		tenantAccountOrderByTenantContactEmailExt,
		tenantAccountOrderByTenantContactFullNameExt,
	}
	// internal list of fields that can be used for ordering
	tenantAccountOrderByFieldsInt = []string{
		tenantAccountOrderByNumber,
		tenantAccountOrderByStatus,
		tenantAccountOrderByCreated,
		tenantAccountOrderByUpdated,
		tenantAccountOrderByTenantOrgNameInt,
		tenantAccountOrderByTenantOrgDisplayNameInt,
		tenantAccountOrderByTenantContactEmailInt,
		tenantAccountOrderByTenantContactFirstNameInt,
		tenantAccountOrderByTenantContactLastNameInt,
	}
	// mapping of sort fields and required relation (for those that need it)
	tenantAccountOrderByFieldToRelation = map[string]string{
		tenantAccountOrderByTenantOrgNameExt:         TenantRelationName,
		tenantAccountOrderByTenantOrgDisplayNameExt:  TenantRelationName,
		tenantAccountOrderByTenantContactEmailExt:    TenantContactRelationName,
		tenantAccountOrderByTenantContactFullNameExt: TenantContactRelationName,
	}
	// mapping of external sort by field to internal
	tenantAccountOrderByFieldExtToInt = map[string]string{
		tenantAccountOrderByTenantOrgNameExt:        tenantAccountOrderByTenantOrgNameInt,
		tenantAccountOrderByTenantOrgDisplayNameExt: tenantAccountOrderByTenantOrgDisplayNameInt,
		tenantAccountOrderByTenantContactEmailExt:   tenantAccountOrderByTenantContactEmailInt,
	}
	// TenantAccountRelatedEntities is a list of valid relation by fields for the TenantAccount model
	TenantAccountRelatedEntities = map[string]bool{
		InfrastructureProviderRelationName: true,
		TenantRelationName:                 true,
		TenantContactRelationName:          true,
	}
	// TenantAccountStatusList is a list of valid status for the TenantAccount model
	TenantAccountStatusList = []string{
		TenantAccountStatusPending,
		TenantAccountStatusInvited,
		TenantAccountStatusReady,
		TenantAccountStatusError,
	}

	// TenantAccountStatusMap is a list of valid status for the TenantAccount model
	TenantAccountStatusMap = map[string]bool{
		TenantAccountStatusPending: true,
		TenantAccountStatusInvited: true,
		TenantAccountStatusReady:   true,
		TenantAccountStatusError:   true,
	}
)

// TenantAccountConfig holds provider-scoped configuration for a TenantAccount.
// Fields must stay flat so jsonb partial merge works.
type TenantAccountConfig struct {
	TargetedInstanceCreation bool `json:"targetedInstanceCreation"`
}

// TenantAccount represents a tenant account - the relationship between a Tenant and an Infrastructure Provider
type TenantAccount struct {
	bun.BaseModel `bun:"table:tenant_account,alias:ta"`

	ID                        uuid.UUID               `bun:"type:uuid,pk"`
	AccountNumber             string                  `bun:"account_number,unique,notnull"`
	TenantID                  *uuid.UUID              `bun:"tenant_id,type:uuid"`
	Tenant                    *Tenant                 `bun:"rel:belongs-to,join:tenant_id=id"`
	TenantOrg                 string                  `bun:"tenant_org,notnull"`
	InfrastructureProviderID  uuid.UUID               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider    *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	InfrastructureProviderOrg string                  `bun:"infrastructure_provider_org,notnull"`
	SubscriptionID            *string                 `bun:"subscription_id"`
	SubscriptionTier          *string                 `bun:"subscription_tier"`
	TenantContactID           *uuid.UUID              `bun:"tenant_contact_id,type:uuid"`
	TenantContact             *User                   `bun:"rel:belongs-to,join:tenant_contact_id=id"`
	Status                    string                  `bun:"status,notnull"`
	Config                    TenantAccountConfig     `bun:"config,type:jsonb,notnull,default:'{}'::jsonb"`
	Created                   time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                   time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                   *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy                 uuid.UUID               `bun:"type:uuid,notnull"`
}

// TenantAcccountCreateInput parameters for Create method
type TenantAccountCreateInput struct {
	AccountNumber             string
	TenantID                  *uuid.UUID
	TenantOrg                 string
	InfrastructureProviderID  uuid.UUID
	InfrastructureProviderOrg string
	SubscriptionID            *string
	SubscriptionTier          *string
	Status                    string
	Config                    *TenantAccountConfig
	CreatedBy                 uuid.UUID
}

// TenantAccountUpdateInput parameters for Update method
type TenantAccountUpdateInput struct {
	TenantAccountID  uuid.UUID
	TenantID         *uuid.UUID
	SubscriptionID   *string
	SubscriptionTier *string
	TenantContactID  *uuid.UUID
	Status           *string
	Config           *TenantAccountConfig
}

// TenantAccountFilterInput filtering options for GetAll and GetCount method, including SearchQuery for filtering by account or tenant org
type TenantAccountFilterInput struct {
	InfrastructureProviderID *uuid.UUID
	Statuses                 []string
	TenantIDs                []uuid.UUID
	TenantOrgs               []string
	TenantAccountIDs         []uuid.UUID
	SearchQuery              *string
}

var _ bun.BeforeAppendModelHook = (*TenantAccount)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ta *TenantAccount) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ta.Created = db.GetCurTime()
		ta.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ta.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Site)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ta *TenantAccount) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id") ON DELETE CASCADE`).
		ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id") ON DELETE CASCADE`).
		ForeignKey(`("tenant_contact_id") REFERENCES "user" ("id") ON DELETE SET NULL`)
	return nil
}

// TenantAccountDAO is an interface for interacting with the TenantAccount model
type TenantAccountDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*TenantAccount, error)
	//
	GetByAccountNumber(ctx context.Context, tx *db.Tx, accountNumber string, includeRelations []string) (*TenantAccount, error)
	//
	GetCountByStatus(ctx context.Context, tx *db.Tx, infrastructureProviderID *uuid.UUID, tenantID *uuid.UUID) (map[string]int, error)
	//
	GetCount(ctx context.Context, tx *db.Tx, filter TenantAccountFilterInput) (int, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter TenantAccountFilterInput, page paginator.PageInput, includeRelations []string) ([]TenantAccount, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input TenantAccountCreateInput) (*TenantAccount, error)
	//
	Update(ctx context.Context, tx *db.Tx, input TenantAccountUpdateInput) (*TenantAccount, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// TenantAccountSQLDAO is an implementation of the TenantAccountDAO interface
type TenantAccountSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a TenantAccount by ID
func (tasd TenantAccountSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *TenantAccount, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.GetByID")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()

		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "id", id.String())
	}

	ta := &TenantAccount{}

	query := db.GetIDB(tx, tasd.dbSession).NewSelect().Model(ta).Where("ta.id = ?", id)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return ta, nil
}

// GetCountByStatus returns count of TenantAccounts for given status
func (tasd TenantAccountSQLDAO) GetCountByStatus(ctx context.Context, tx *db.Tx, infrastructureProviderID *uuid.UUID, tenantID *uuid.UUID) (_ map[string]int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.GetCountByStatus")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
	}

	ta := &TenantAccount{}
	var statusQueryResults []map[string]interface{}

	query := db.GetIDB(tx, tasd.dbSession).NewSelect().Model(ta)
	if infrastructureProviderID != nil {
		query = query.Where("ta.infrastructure_provider_id = ?", *infrastructureProviderID)
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "infrastructure_provider_id", infrastructureProviderID.String())
	}
	if tenantID != nil {
		query = query.Where("ta.tenant_id = ?", *tenantID)
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "tenant_id", tenantID.String())
	}

	err := query.Column("ta.status").ColumnExpr("COUNT(*) AS total_count").GroupExpr("ta.status").Scan(ctx, &statusQueryResults)
	if err != nil {
		return nil, err
	}

	// creare results map by holding key as status value with total count
	results := map[string]int{
		"total":                    0,
		TenantAccountStatusPending: 0,
		TenantAccountStatusInvited: 0,
		TenantAccountStatusReady:   0,
		TenantAccountStatusError:   0,
	}
	if len(statusQueryResults) > 0 {
		for _, statusMap := range statusQueryResults {
			results[statusMap["status"].(string)] = int(statusMap["total_count"].(int64))
			results["total"] = results["total"] + int(statusMap["total_count"].(int64))
		}
	}
	return results, nil
}

// GetByAccountNumber returns a TenantAccount by account number
func (tasd TenantAccountSQLDAO) GetByAccountNumber(ctx context.Context, tx *db.Tx, accountNumber string, includeRelations []string) (_ *TenantAccount, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.GetByAccountNumber")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
	}

	ta := &TenantAccount{}

	query := db.GetIDB(tx, tasd.dbSession).NewSelect().Model(ta).Where("account_number = ?", accountNumber)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		return nil, err
	}

	return ta, nil
}

func (tasd TenantAccountSQLDAO) setQueryWithFilter(filter TenantAccountFilterInput, query *bun.SelectQuery, tnaDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.TenantIDs != nil {
		if len(filter.TenantIDs) == 1 {
			query = query.Where("ta.tenant_id = ?", filter.TenantIDs[0])
		} else {
			query = query.Where("ta.tenant_id IN (?)", bun.In(filter.TenantIDs))
		}
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "tenant_id", filter.TenantIDs)
	}

	if filter.TenantOrgs != nil {
		if len(filter.TenantOrgs) == 1 {
			query = query.Where("ta.tenant_org = ?", filter.TenantOrgs[0])
		} else {
			query = query.Where("ta.tenant_org IN (?)", bun.In(filter.TenantOrgs))
		}

		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "tenant_org", filter.TenantOrgs)
	}

	if filter.InfrastructureProviderID != nil {
		query = query.Where("ta.infrastructure_provider_id = ?", *filter.InfrastructureProviderID)
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "infrastructure_provider_id", filter.InfrastructureProviderID.String())
	}

	if filter.Statuses != nil {
		if len(filter.Statuses) == 1 {
			query = query.Where("ta.status = ?", filter.Statuses[0])
		} else {
			query = query.Where("ta.status IN (?)", bun.In(filter.Statuses))
		}
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "status", filter.Statuses)
	}

	if filter.TenantAccountIDs != nil {
		if len(filter.TenantAccountIDs) == 1 {
			query = query.Where("ta.id = ?", filter.TenantAccountIDs[0])
		} else {
			query = query.Where("ta.id IN (?)", bun.In(filter.TenantAccountIDs))
		}
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "id", filter.TenantAccountIDs)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', ta.account_number || ' ' || ta.tenant_org) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("ta.account_number ILIKE ?", "%"+searchQuery+"%").
				WhereOr("ta.tenant_org ILIKE ?", "%"+searchQuery+"%").
				WhereOr("EXISTS (SELECT 1 FROM tenant WHERE tenant.id = ta.tenant_id AND tenant.deleted IS NULL AND tenant.org_display_name ILIKE ?)", "%"+searchQuery+"%")
		})
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "search_query", searchQuery)
	}

	return query, nil
}

// GetCount returns the count of TenantAccounts that match the parameters
func (tasd TenantAccountSQLDAO) GetCount(ctx context.Context, tx *db.Tx, filter TenantAccountFilterInput) (_ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tenantAccountDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.GetCount")
	if tenantAccountDAOSpan != nil {
		defer func() {
			tenantAccountDAOSpan.EndWith(retErr)
		}()
	}

	query := db.GetIDB(tx, tasd.dbSession).NewSelect().Model((*TenantAccount)(nil))
	query, err := tasd.setQueryWithFilter(filter, query, tenantAccountDAOSpan)
	if err != nil {
		return 0, err
	}

	return query.Count(ctx)
}

// GetAll returns a list of TenantAccounts filtering by tenantID, tenantOrg, infrastructureProviderID, offset, limit and orderBy
// if orderBy is nil, then records are ordered by column specified in TenantAccountOrderByDefault in ascending order
func (tasd TenantAccountSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter TenantAccountFilterInput, page paginator.PageInput, includeRelations []string) (_ []TenantAccount, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.GetAll")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
	}

	tas := []TenantAccount{}

	query := db.GetIDB(tx, tasd.dbSession).NewSelect().Model(&tas)

	query, err := tasd.setQueryWithFilter(filter, query, tnaDAOSpan)
	if err != nil {
		return tas, 0, err
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(TenantAccountOrderByDefault)
	}

	// validate order by
	if relationName := tenantAccountOrderByFieldToRelation[page.OrderBy.Field]; relationName != "" {
		if !db.IsStrInSlice(relationName, includeRelations) {
			// add relation, so that we can sort on joined data
			includeRelations = append(includeRelations, relationName)
		}
	}
	// convert to internal
	if internalName := tenantAccountOrderByFieldExtToInt[page.OrderBy.Field]; internalName != "" {
		page.OrderBy.Field = internalName
	}

	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy.Field == tenantAccountOrderByTenantContactFullNameExt {
		// sort by first name, last name
		multiOrderBy = append(multiOrderBy, &paginator.OrderBy{Field: tenantAccountOrderByTenantContactFirstNameInt, Order: page.OrderBy.Order})
		multiOrderBy = append(multiOrderBy, &paginator.OrderBy{Field: tenantAccountOrderByTenantContactLastNameInt, Order: page.OrderBy.Order})
	} else {
		// Add as is
		multiOrderBy = append(multiOrderBy, page.OrderBy)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	newPaginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, tenantAccountOrderByFieldsInt)
	if err != nil {
		return nil, 0, err
	}

	err = newPaginator.Query.Limit(newPaginator.Limit).Offset(newPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return tas, newPaginator.Total, nil
}

// Create creates a new TenantAccount from the given parameters
func (tasd TenantAccountSQLDAO) Create(ctx context.Context, tx *db.Tx, input TenantAccountCreateInput) (_ *TenantAccount, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.Create")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
	}

	ta := &TenantAccount{
		ID:                        uuid.New(),
		AccountNumber:             input.AccountNumber,
		TenantID:                  input.TenantID,
		TenantOrg:                 input.TenantOrg,
		InfrastructureProviderID:  input.InfrastructureProviderID,
		InfrastructureProviderOrg: input.InfrastructureProviderOrg,
		SubscriptionID:            input.SubscriptionID,
		SubscriptionTier:          input.SubscriptionTier,
		Status:                    input.Status,
		CreatedBy:                 input.CreatedBy,
	}
	if input.Config != nil {
		ta.Config = *input.Config
	}

	_, err := db.GetIDB(tx, tasd.dbSession).NewInsert().Model(ta).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nta, err := tasd.GetByID(ctx, tx, ta.ID, nil)
	if err != nil {
		return nil, err
	}

	return nta, nil
}

// Update updates an existing TenantAccount from the given parameters
func (tasd TenantAccountSQLDAO) Update(ctx context.Context, tx *db.Tx, input TenantAccountUpdateInput) (_ *TenantAccount, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.Update")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "id", input.TenantAccountID.String())
	}

	ta := &TenantAccount{
		ID: input.TenantAccountID,
	}

	updatedFields := []string{}

	if input.TenantID != nil {
		ta.TenantID = input.TenantID
		updatedFields = append(updatedFields, "tenant_id")
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "tenant_id", input.TenantID.String())
	}

	if input.SubscriptionID != nil {
		ta.SubscriptionID = input.SubscriptionID
		updatedFields = append(updatedFields, "subscription_id")
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "subscription_id", *input.SubscriptionID)
	}

	if input.SubscriptionTier != nil {
		ta.SubscriptionTier = input.SubscriptionTier
		updatedFields = append(updatedFields, "subscription_tier")
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "subscription_tier", *input.SubscriptionTier)
	}

	if input.TenantContactID != nil {
		ta.TenantContactID = input.TenantContactID
		updatedFields = append(updatedFields, "tenant_contact_id")
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "tenant_contact_id", input.TenantContactID.String())
	}

	if input.Status != nil {
		ta.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "status", *input.Status)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, tasd.dbSession).NewUpdate().Model(ta).Column(updatedFields...).Where("id = ?", input.TenantAccountID.String()).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	if input.Config != nil {
		_, err := db.GetIDB(tx, tasd.dbSession).NewUpdate().
			Model(ta).
			Set("config = config || ?::jsonb, updated = current_timestamp", input.Config).
			Where("id = ?", input.TenantAccountID.String()).
			Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	uta, err := tasd.GetByID(ctx, tx, input.TenantAccountID, nil)
	if err != nil {
		return nil, err
	}

	return uta, nil
}

// Delete deletes a TenantAccount by ID
func (tasd TenantAccountSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnaDAOSpan := tasd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantAccountDAO.DeleteByID")
	if tnaDAOSpan != nil {
		defer func() {
			tnaDAOSpan.EndWith(retErr)
		}()
		tasd.tracerSpan.SetAttribute(tnaDAOSpan, "id", id.String())
	}

	ta := &TenantAccount{
		ID: id,
	}

	_, err := db.GetIDB(tx, tasd.dbSession).NewDelete().Model(ta).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewTenantAccountDAO creates a new TenantAccountDAO
func NewTenantAccountDAO(dbSession *db.Session) TenantAccountDAO {
	return &TenantAccountSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
