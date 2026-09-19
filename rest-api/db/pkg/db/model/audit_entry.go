// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"net/http"
	"net/url"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

const (
	AuditEntryOrderByDefault = "timestamp"
)

var (
	// AuditEntryOrderByFields is a list of valid order by fields for the AuditEntry model
	AuditEntryOrderByFields = []string{"timestamp"}
)

type AuditEntry struct {
	bun.BaseModel `bun:"table:audit_entry,alias:ae"`

	ID            uuid.UUID              `bun:"id,type:uuid,pk"`
	Endpoint      string                 `bun:"endpoint,notnull"`
	QueryParams   url.Values             `bun:"query_params,type:jsonb"`
	Method        string                 `bun:"method,notnull"`                       // POST, PUT, DELETE, PATCH
	Body          map[string]interface{} `bun:"body,type:jsonb,notnull,default:'{}'"` // in JSON
	StatusCode    int                    `bun:"status_code,notnull"`
	StatusMessage string                 `bun:"status_message,notnull"`
	ClientIP      string                 `bun:"client_ip,notnull"`
	UserID        *uuid.UUID             `bun:"user_id,type:uuid"`
	OrgName       string                 `bun:"org_name,notnull"`
	ExtraData     map[string]interface{} `bun:"extra_data,type:jsonb,notnull,default:'{}'"` // in JSON
	Timestamp     time.Time              `bun:"timestamp,nullzero,notnull,default:current_timestamp"`
	Duration      time.Duration          `bun:"duration,nullzero,notnull"`
	APIVersion    string                 `bun:"api_version,notnull"`
}

type AuditEntryCreateInput struct {
	Endpoint      string
	QueryParams   url.Values
	Method        string
	Body          map[string]interface{}
	StatusCode    int
	StatusMessage *string
	ClientIP      string
	UserID        *uuid.UUID
	OrgName       string
	ExtraData     map[string]interface{}
	Timestamp     time.Time
	Duration      time.Duration
	APIVersion    string
}

type AuditEntryUpdateInput struct {
	ID            uuid.UUID
	StatusMessage *string
	Body          map[string]interface{}
	ExtraData     map[string]interface{}
}

type AuditEntryFilterInput struct {
	OrgName    *string
	FailedOnly *bool // returns only entries with status_code >= 400
}

type AuditEntryDAO interface {
	Create(ctx context.Context, tx *db.Tx, entry AuditEntryCreateInput) (*AuditEntry, error)
	Update(ctx context.Context, tx *db.Tx, input AuditEntryUpdateInput) (*AuditEntry, error)
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID) (*AuditEntry, error)
	GetAll(ctx context.Context, tx *db.Tx, filter AuditEntryFilterInput, page paginator.PageInput) ([]AuditEntry, int, error)
}

// AuditEntrySQLDAO is the SQL data access object for AuditEntry
type AuditEntrySQLDAO struct {
	dbSession *db.Session
	AuditEntryDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates an AuditEntry from the given parameters
func (aed AuditEntrySQLDAO) Create(ctx context.Context, tx *db.Tx, input AuditEntryCreateInput) (_ *AuditEntry, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, daoSpan := aed.tracerSpan.CreateChildInCurrentContext(ctx, "AuditEntryDAO.Create")
	if daoSpan != nil {
		defer func() {
			daoSpan.EndWith(retErr)
		}()
		aed.tracerSpan.SetAttribute(daoSpan, "endpoint", input.Endpoint)
	}

	entry := &AuditEntry{
		ID:          uuid.New(),
		Endpoint:    input.Endpoint,
		QueryParams: input.QueryParams,
		Method:      input.Method,
		StatusCode:  input.StatusCode,
		ClientIP:    input.ClientIP,
		UserID:      input.UserID,
		OrgName:     input.OrgName,
		Timestamp:   input.Timestamp.UTC().Round(time.Microsecond),
		Duration:    input.Duration,
		APIVersion:  input.APIVersion,
	}
	if input.Body != nil {
		entry.Body = input.Body
	}
	if input.StatusMessage != nil {
		entry.StatusMessage = *input.StatusMessage
	}
	if input.ExtraData != nil {
		entry.ExtraData = input.ExtraData
	}
	_, err := db.GetIDB(tx, aed.dbSession).NewInsert().Model(entry).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return aed.GetByID(ctx, tx, entry.ID)
}

func (aed AuditEntrySQLDAO) Update(ctx context.Context, tx *db.Tx, input AuditEntryUpdateInput) (_ *AuditEntry, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, daoSpan := aed.tracerSpan.CreateChildInCurrentContext(ctx, "AuditEntryDAO.Update")
	if daoSpan != nil {
		defer func() {
			daoSpan.EndWith(retErr)
		}()
		aed.tracerSpan.SetAttribute(daoSpan, "id", input.ID.String())
	}

	var updatedFields []string

	entry := &AuditEntry{
		ID: input.ID,
	}

	if input.StatusMessage != nil {
		entry.StatusMessage = *input.StatusMessage
		updatedFields = append(updatedFields, "status_message")
		aed.tracerSpan.SetAttribute(daoSpan, "status_message", *input.StatusMessage)
	}

	if input.Body != nil {
		entry.Body = input.Body
		updatedFields = append(updatedFields, "body")
		aed.tracerSpan.SetAttribute(daoSpan, "body", input.Body)
	}

	if input.ExtraData != nil {
		entry.ExtraData = input.ExtraData
		updatedFields = append(updatedFields, "extra_data")
		aed.tracerSpan.SetAttribute(daoSpan, "extra_data", input.ExtraData)
	}

	if len(updatedFields) > 0 {
		_, err := db.GetIDB(tx, aed.dbSession).NewUpdate().Model(entry).Column(updatedFields...).Where("id = ?", entry.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}
	return aed.GetByID(ctx, tx, entry.ID)
}

func (aed AuditEntrySQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID) (_ *AuditEntry, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, daoSpan := aed.tracerSpan.CreateChildInCurrentContext(ctx, "AuditEntryDAO.GetByID")
	if daoSpan != nil {
		defer func() {
			daoSpan.EndWith(retErr)
		}()
		aed.tracerSpan.SetAttribute(daoSpan, "id", id.String())
	}

	entry := &AuditEntry{}

	query := db.GetIDB(tx, aed.dbSession).NewSelect().Model(entry).Where("ae.id = ?", id)

	if err := query.Scan(ctx); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return entry, nil
}

func (aed AuditEntrySQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter AuditEntryFilterInput, page paginator.PageInput) (_ []AuditEntry, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, daoSpan := aed.tracerSpan.CreateChildInCurrentContext(ctx, "AuditEntryDAO.GetAll")
	if daoSpan != nil {
		defer func() {
			daoSpan.EndWith(retErr)
		}()
	}

	var entries []AuditEntry

	query := db.GetIDB(tx, aed.dbSession).NewSelect().Model(&entries)

	if filter.OrgName != nil {
		query = query.Where("ae.org_name = ?", *filter.OrgName)
		aed.tracerSpan.SetAttribute(daoSpan, "org_name", *filter.OrgName)
	}
	if filter.FailedOnly != nil {
		if *filter.FailedOnly {
			query = query.Where("ae.status_code >= ?", http.StatusBadRequest)
			aed.tracerSpan.SetAttribute(daoSpan, "failedOnly", *filter.FailedOnly)
		}
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy == nil {
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(AuditEntryOrderByDefault))
	} else {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		if page.OrderBy.Field != AuditEntryOrderByDefault {
			multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(AuditEntryOrderByDefault))
		}
	}

	dbPaginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, AuditEntryOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = dbPaginator.Query.Limit(dbPaginator.Limit).Offset(dbPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return entries, dbPaginator.Total, nil
}

func NewAuditEntryDAO(dbSession *db.Session) AuditEntryDAO {
	return &AuditEntrySQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
