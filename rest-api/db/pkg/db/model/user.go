// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/google/uuid"
	"github.com/jackc/pgerrcode"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/pkg/errors"

	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// UserRelationName is the relation name for the User model
	UserRelationName = "User"

	// UserOrderByDefault default field to be used for ordering when none specified
	UserOrderByDefault = "created"
)

var (
	// UserOrderByFields is a list of valid order by fields for the User model
	UserOrderByFields = []string{"created"}
)

// Org captures details for organizations
type Org struct {
	ID          int        `json:"id"`
	Name        string     `json:"name"`
	DisplayName string     `json:"displayName"`
	OrgType     string     `json:"orgType"`
	Roles       []string   `json:"roles"`
	Teams       []Team     `json:"teams"`
	Updated     *time.Time `json:"updated,omitempty"`
}

// Equal compares two Org structs, ignoring order in slices
func (o Org) Equal(other Org) bool {
	if o.ID != other.ID || o.Name != other.Name || o.DisplayName != other.DisplayName || o.OrgType != other.OrgType {
		return false
	}
	if !db.CompareStringSlicesIgnoreOrder(o.Roles, other.Roles) {
		return false
	}
	if len(o.Teams) != len(other.Teams) {
		return false
	}
	// Compare teams by value, ignoring order
	used := make([]bool, len(other.Teams))
	for _, to := range o.Teams {
		found := false
		for j, tOther := range other.Teams {
			if used[j] {
				continue
			}
			if to.Equal(tOther) {
				used[j] = true
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

// Team captures details for NGC teams
type Team struct {
	ID       int      `json:"id"`
	Name     string   `json:"name"`
	TeamType string   `json:"teamType"`
	Roles    []string `json:"roles"`
}

// Equal compares two Team structs, ignoring order in slices
func (t Team) Equal(other Team) bool {
	return t.ID == other.ID && t.Name == other.Name && t.TeamType == other.TeamType && db.CompareStringSlicesIgnoreOrder(t.Roles, other.Roles)
}

// OrgData is a map of org names to Org structs for the user
type OrgData map[string]Org

// Equal compares two OrgData pointers, ignoring order in slices
func (od OrgData) Equal(other OrgData) bool {
	if od == nil && other == nil {
		return true
	}
	if od == nil || other == nil {
		return false
	}
	if len(od) != len(other) {
		return false
	}
	for k, vO := range od {
		vOther, ok := (other)[k]
		if !ok {
			return false
		}
		if !vO.Equal(vOther) {
			return false
		}
	}
	return true
}

// GetOrgByName retrieves details for a given org name (case-insensitive)
func (od OrgData) GetOrgByName(name string) (*Org, error) {
	if od == nil {
		return nil, db.ErrDoesNotExist
	}

	if org, ok := od[name]; ok {
		return &org, nil
	}

	// If exact match fails, try case-insensitive search
	nameLower := strings.ToLower(name)
	for orgKey, org := range od {
		if strings.ToLower(orgKey) == nameLower {
			return &org, nil
		}
	}

	return nil, db.ErrDoesNotExist
}

// UserFilterInput input parameters for GetAll method
type UserFilterInput struct {
	UserIDs      []uuid.UUID
	AuxiliaryIDs []string
	StarfleetIDs []string
}

// UserCreateInput input parameters for Create method
type UserCreateInput struct {
	AuxiliaryID *string
	StarfleetID *string
	Email       *string
	FirstName   *string
	LastName    *string
	OrgData     OrgData
}

// UserUpdateInput input parameters for Update method
type UserUpdateInput struct {
	UserID          uuid.UUID
	AuxiliaryID     *string
	StarfleetID     *string
	Email           *string
	FirstName       *string
	LastName        *string
	OrgData         OrgData
	PreserveUpdated bool
}

// UserGetOrCreateInput input parameters for GetOrCreate method
type UserGetOrCreateInput struct {
	AuxiliaryID *string
	StarfleetID *string
}

// User represents entries in the user table
type User struct {
	bun.BaseModel `bun:"table:user,alias:u"`
	ID            uuid.UUID `bun:"type:uuid,pk"`
	AuxiliaryID   *string   `bun:"auxiliary_id,unique"`
	StarfleetID   *string   `bun:"starfleet_id,unique"`
	Email         *string   `bun:"email"`
	FirstName     *string   `bun:"first_name"`
	LastName      *string   `bun:"last_name"`
	NgcOrgData    OrgData   `bun:"ngc_org_data,json_use_number"`
	OrgData       OrgData   `bun:"org_data,type:jsonb,notnull,default:'{}'::jsonb"`
	Created       time.Time `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated       time.Time `bun:"updated,nullzero,notnull,default:current_timestamp"`
}

var _ bun.BeforeAppendModelHook = (*User)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (u *User) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		// Ensure OrgData is never nil to avoid JSON null in DB
		if u.OrgData == nil {
			u.OrgData = OrgData{}
		}
		u.Created = db.GetCurTime()
		u.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		u.Updated = db.GetCurTime()
	}
	return nil
}

// UserDAO is the interface for the User model
type UserDAO interface {
	//
	Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*User, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter UserFilterInput, page paginator.PageInput, includeRelations []string) ([]User, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input UserCreateInput) (*User, error)
	//
	Update(ctx context.Context, tx *db.Tx, input UserUpdateInput) (*User, error)
	//
	GetOrCreate(ctx context.Context, tx *db.Tx, input UserGetOrCreateInput) (*User, bool, error)
}

// UserSQLDAO is the SQL implementation of UserDAO
type UserSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// Get returns a user by ID
func (usd UserSQLDAO) Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*User, error) {
	// Create a child span and set the attributes for current request
	ctx, userDAOSpan := usd.tracerSpan.CreateChildInCurrentContext(ctx, "UserDAO.GetByID")
	if userDAOSpan != nil {
		defer userDAOSpan.End()
	}

	u := &User{}
	query := db.GetIDB(tx, usd.dbSession).NewSelect().Model(u)

	usd.tracerSpan.SetAttribute(userDAOSpan, "id", id.String())
	query = query.Where("u.id = ?", id)

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
	return u, nil
}

func (usd UserSQLDAO) setQueryWithFilter(filter UserFilterInput, query *bun.SelectQuery, userDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.UserIDs != nil {
		query = query.Where("u.id IN (?)", bun.In(filter.UserIDs))

		if userDAOSpan != nil {
			usd.tracerSpan.SetAttribute(userDAOSpan, "user_ids", filter.UserIDs)
		}
	}

	if filter.AuxiliaryIDs != nil {
		query = query.Where("u.auxiliary_id IN (?)", bun.In(filter.AuxiliaryIDs))
		if userDAOSpan != nil {
			usd.tracerSpan.SetAttribute(userDAOSpan, "auxiliary_ids", filter.AuxiliaryIDs)
		}
	}

	if filter.StarfleetIDs != nil {
		query = query.Where("u.starfleet_id IN (?)", bun.In(filter.StarfleetIDs))
		if userDAOSpan != nil {
			usd.tracerSpan.SetAttribute(userDAOSpan, "starfleet_ids", filter.StarfleetIDs)
		}
	}

	return query, nil
}

// GetAll returns all Users for given params
// if orderBy is nil, then records are ordered by column specified in UserOrderByDefault in ascending order
func (usd UserSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter UserFilterInput, page paginator.PageInput, includeRelations []string) ([]User, int, error) {
	// Create a child span and set the attributes for current request
	ctx, daoSpan := usd.tracerSpan.CreateChildInCurrentContext(ctx, "UserDAO.GetAll")
	if daoSpan != nil {
		defer daoSpan.End()
	}

	var users []User

	query := db.GetIDB(tx, usd.dbSession).NewSelect().Model(&users)
	query, err := usd.setQueryWithFilter(filter, query, daoSpan)
	if err != nil {
		return users, 0, err
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy == nil {
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(UserOrderByDefault))
	} else {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		if page.OrderBy.Field != UserOrderByDefault {
			multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(UserOrderByDefault))
		}
	}

	paginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, UserOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return users, paginator.Total, nil
}

// Create creates a new user from the given input
func (usd UserSQLDAO) Create(ctx context.Context, tx *db.Tx, input UserCreateInput) (*User, error) {
	// Check and reject empty string IDs
	if input.AuxiliaryID != nil && strings.TrimSpace(*input.AuxiliaryID) == "" {
		return nil, errors.Wrap(db.ErrInvalidValue, "AuxiliaryID cannot be empty or whitespace-only string")
	}

	if input.StarfleetID != nil && strings.TrimSpace(*input.StarfleetID) == "" {
		return nil, errors.Wrap(db.ErrInvalidValue, "StarfleetID cannot be empty or whitespace-only string")
	}

	// Create a child span and set the attributes for current request
	ctx, userDAOSpan := usd.tracerSpan.CreateChildInCurrentContext(ctx, "UserDAO.Create")
	if userDAOSpan != nil {
		defer userDAOSpan.End()

		if input.StarfleetID != nil {
			usd.tracerSpan.SetAttribute(userDAOSpan, "starfleet_id", *input.StarfleetID)
		}

		if input.AuxiliaryID != nil {
			usd.tracerSpan.SetAttribute(userDAOSpan, "auxiliary_id", *input.AuxiliaryID)
		}
	}

	if input.StarfleetID == nil && input.AuxiliaryID == nil {
		return nil, db.ErrInvalidParams
	}

	u := &User{
		ID:          uuid.New(),
		StarfleetID: input.StarfleetID,
		AuxiliaryID: input.AuxiliaryID,
		Email:       input.Email,
		FirstName:   input.FirstName,
		LastName:    input.LastName,
		OrgData:     input.OrgData,
	}

	_, err := db.GetIDB(tx, usd.dbSession).NewInsert().Model(u).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nu, err := usd.Get(ctx, tx, u.ID, nil)
	if err != nil {
		return nil, err
	}

	return nu, nil
}

// Update updates a user from the given input
func (usd UserSQLDAO) Update(ctx context.Context, tx *db.Tx, input UserUpdateInput) (*User, error) {
	// Check and reject empty string IDs
	if input.AuxiliaryID != nil && strings.TrimSpace(*input.AuxiliaryID) == "" {
		return nil, errors.Wrap(db.ErrInvalidValue, "AuxiliaryID cannot be empty or whitespace-only string")
	}

	if input.StarfleetID != nil && strings.TrimSpace(*input.StarfleetID) == "" {
		return nil, errors.Wrap(db.ErrInvalidValue, "StarfleetID cannot be empty or whitespace-only string")
	}

	// Create a child span and set the attributes for current request
	ctx, userDAOSpan := usd.tracerSpan.CreateChildInCurrentContext(ctx, "UserDAO.Update")
	if userDAOSpan != nil {
		defer userDAOSpan.End()

		usd.tracerSpan.SetAttribute(userDAOSpan, "user_id", input.UserID.String())
	}

	u := &User{}

	updatedFields := []string{}

	if input.AuxiliaryID != nil {
		u.AuxiliaryID = input.AuxiliaryID
		updatedFields = append(updatedFields, "auxiliary_id")
	}

	if input.StarfleetID != nil {
		u.StarfleetID = input.StarfleetID
		updatedFields = append(updatedFields, "starfleet_id")
	}

	if input.Email != nil {
		u.Email = input.Email
		updatedFields = append(updatedFields, "email")
	}

	if input.FirstName != nil {
		u.FirstName = input.FirstName
		updatedFields = append(updatedFields, "first_name")
	}

	if input.LastName != nil {
		u.LastName = input.LastName
		updatedFields = append(updatedFields, "last_name")
	}

	if input.OrgData != nil {
		u.OrgData = input.OrgData
		updatedFields = append(updatedFields, "org_data")
	}

	if len(updatedFields) > 0 {
		if !input.PreserveUpdated {
			updatedFields = append(updatedFields, "updated")
		}

		_, err := db.GetIDB(tx, usd.dbSession).NewUpdate().Model(u).Column(updatedFields...).Where("id = ?", input.UserID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	un, err := usd.Get(ctx, tx, input.UserID, nil)
	if err != nil {
		return nil, err
	}

	return un, nil
}

// GetOrCreate returns a user by AuxiliaryID and/or StarfleetID, or creates a new one if it doesn't exist
// The database unique constraints prevent race conditions during concurrent user creation.
// Returns db.ErrInvalidParams if neither ID is provided
func (usd UserSQLDAO) GetOrCreate(ctx context.Context, tx *db.Tx, input UserGetOrCreateInput) (*User, bool, error) {
	// Check and reject empty string IDs
	if input.AuxiliaryID != nil && strings.TrimSpace(*input.AuxiliaryID) == "" {
		return nil, false, errors.Wrap(db.ErrInvalidValue, "AuxiliaryID cannot be empty or whitespace-only string")
	}

	if input.StarfleetID != nil && strings.TrimSpace(*input.StarfleetID) == "" {
		return nil, false, errors.Wrap(db.ErrInvalidValue, "StarfleetID cannot be empty or whitespace-only string")
	}

	// Validate that at least one type of ID is provided (after empty string conversion)
	hasAuxiliaryID := input.AuxiliaryID != nil
	hasStarfleetID := input.StarfleetID != nil

	if !hasAuxiliaryID && !hasStarfleetID {
		return nil, false, errors.Wrap(db.ErrInvalidParams, "at least one of StarfleetID or AuxiliaryID must be provided")
	}

	// Create a child span and set the attributes for current request
	ctx, userDAOSpan := usd.tracerSpan.CreateChildInCurrentContext(ctx, "UserDAO.GetOrCreate")
	if userDAOSpan != nil {
		defer userDAOSpan.End()

		if hasAuxiliaryID {
			usd.tracerSpan.SetAttribute(userDAOSpan, "auxiliary_id", *input.AuxiliaryID)
		}
		if hasStarfleetID {
			usd.tracerSpan.SetAttribute(userDAOSpan, "starfleet_id", *input.StarfleetID)
		}
	}

	// Search for existing user using OR conditions with direct SQL query
	// This avoids GetAll which would use AND logic and allows us to match either ID
	var users []User
	query := db.GetIDB(tx, usd.dbSession).NewSelect().Model(&users).Limit(2) // Limit to 2 to detect multiple matches

	// Build OR conditions based on provided IDs
	if hasAuxiliaryID && hasStarfleetID {
		query = query.Where("(u.auxiliary_id = ? AND u.starfleet_id = ?)", *input.AuxiliaryID, *input.StarfleetID)
	} else if hasAuxiliaryID {
		query = query.Where("u.auxiliary_id = ?", *input.AuxiliaryID)
	} else if hasStarfleetID {
		query = query.Where("u.starfleet_id = ?", *input.StarfleetID)
	}

	err := query.Scan(ctx)
	if err != nil {
		return nil, false, err
	}

	if len(users) > 1 {
		return nil, false, errors.Wrap(db.ErrInvalidParams, "multiple users found for the given ID(s)")
	}

	if len(users) == 1 {
		// Found existing user
		return &users[0], false, nil
	}

	// No existing user found, try to create new one
	// Use constraint violation handling for race conditions
	createInput := UserCreateInput{
		AuxiliaryID: input.AuxiliaryID,
		StarfleetID: input.StarfleetID,
	}
	newUser, err := usd.Create(ctx, tx, createInput)
	if err == nil {
		return newUser, true, nil
	}

	// Check if it's a PostgreSQL unique violation error
	var pErr *pgconn.PgError
	isUniqueViolation := errors.As(err, &pErr) && pErr.Code == pgerrcode.UniqueViolation
	if !isUniqueViolation {
		return nil, false, err
	}

	// Race condition occurred - try to find the user again
	err = query.Scan(ctx)
	if err != nil {
		return nil, false, err
	}

	if len(users) > 1 {
		return nil, false, errors.Wrap(db.ErrInvalidParams, "multiple users found for the given ID(s) after constraint violation")
	} else if len(users) == 1 {
		return &users[0], false, nil
	} else {
		return nil, false, errors.Wrap(db.ErrInvalidParams, "unable to create user after constraint violation")
	}
}

// NewUserDAO creates a new UserDAO
func NewUserDAO(dbSession *db.Session) UserDAO {
	return &UserSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
