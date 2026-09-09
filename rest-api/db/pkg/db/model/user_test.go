// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestUserSQLDAO_Get(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Reset User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ngcOrg := Org{
		ID:          123,
		Name:        "test-org",
		DisplayName: "Test Org",
		OrgType:     "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *User
		wantErr            bool
		wantErrVal         error
		verifyChildSpanner bool
	}{
		{
			name: "retrieve a User by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  user.ID,
			},
			want:               user,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "error retrieving a User by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				id:  uuid.New(),
			},
			want:       nil,
			wantErr:    true,
			wantErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			usd := UserSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := usd.Get(tt.args.ctx, nil, tt.args.id, nil)
			if (err != nil) != tt.wantErr {
				t.Errorf("UserSQLDAO.Get() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if tt.wantErr {
				assert.Equal(t, tt.wantErrVal, err)
				return
			}
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, *tt.want.StarfleetID, *got.StarfleetID)
			assert.Equal(t, *tt.want.Email, *got.Email)

			rNgcOrg, _ := tt.want.OrgData.GetOrgByName(ngcOrg.Name)
			assert.Equal(t, (tt.want.OrgData)[ngcOrg.Name], *rNgcOrg)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUserSQLDAO_GetAll(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx    context.Context
		filter UserFilterInput
	}

	type want struct {
		users []User
		total int
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ngcOrg := Org{
		ID:          123,
		Name:        "test-org",
		DisplayName: "Test Org",
		OrgType:     "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	user1 := User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(&user1).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	user2 := User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jsmith@test.com"),
		FirstName:   cutil.GetPtr("Jimmy"),
		LastName:    cutil.GetPtr("Smith"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(&user2).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	user3 := User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.NewString()),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}

	_, err = dbSession.DB.NewInsert().Model(&user3).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name   string
		fields fields
		args   args
		want   want
	}{
		{
			name: "filter by single ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: UserFilterInput{
					UserIDs: []uuid.UUID{user1.ID},
				},
			},
			want: want{
				users: []User{user1},
				total: 1,
			},
		},
		{
			name: "filter by multiple IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: UserFilterInput{
					UserIDs: []uuid.UUID{user1.ID, user2.ID},
				},
			},
			want: want{
				users: []User{user1, user2},
				total: 2,
			},
		},
		{
			name: "filter by unknown ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				filter: UserFilterInput{
					UserIDs: []uuid.UUID{uuid.New()},
				},
			},
			want: want{
				users: nil,
				total: 0,
			},
		},
		{
			name: "filter by auxiliary ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: UserFilterInput{
					AuxiliaryIDs: []string{*user3.AuxiliaryID},
				},
			},
			want: want{
				users: []User{user3},
				total: 1,
			},
		},
		{
			name: "filter by starfleet ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: UserFilterInput{
					StarfleetIDs: []string{*user3.StarfleetID},
				},
			},
			want: want{
				users: []User{user3},
				total: 1,
			},
		},
		{
			name: "filter by auxiliary and starfleet ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				filter: UserFilterInput{
					AuxiliaryIDs: []string{*user3.AuxiliaryID},
					StarfleetIDs: []string{*user3.StarfleetID},
				},
			},
			want: want{
				users: []User{user3},
				total: 1,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			usd := UserSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, total, err := usd.GetAll(tt.args.ctx, nil, tt.args.filter, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
			assert.NoError(t, err)
			assert.Equal(t, tt.want.total, total)
			assert.Equal(t, len(tt.want.users), len(got))
			for i, user := range got {
				assert.Equal(t, tt.want.users[i].ID, user.ID)
			}
		})
	}
}

func TestUserSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input UserCreateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	testOrgData := OrgData{
		"test-org": Org{
			ID:          1,
			Name:        "test-org",
			DisplayName: "Test Organization",
			OrgType:     "standard",
			Roles:       []string{"user", "admin"},
			Teams:       []Team{},
		},
	}

	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData:     testOrgData,
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *User
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create a User from params",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: UserCreateInput{
					StarfleetID: user.StarfleetID,
					Email:       user.Email,
					FirstName:   user.FirstName,
					LastName:    user.LastName,
					OrgData:     testOrgData,
				},
			},
			want:               user,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			usd := UserSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := usd.Create(tt.args.ctx, nil, tt.args.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("UserSQLDAO.Create() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if *got.StarfleetID != *tt.want.StarfleetID {
				t.Errorf("StarfleetID = %v, want %v", *got.StarfleetID, *tt.want.StarfleetID)
			}

			if *got.Email != *tt.want.Email {
				t.Errorf("Email = %v, want %v", got.Email, tt.want.Email)
			}

			if *got.FirstName != *tt.want.FirstName {
				t.Errorf("FirstName = %v, want %v", *got.FirstName, *tt.want.FirstName)
			}

			if *got.LastName != *tt.want.LastName {
				t.Errorf("LastName = %v, want %v", got.LastName, tt.want.LastName)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUserSQLDAO_Update(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input UserUpdateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	// Create user
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	ngcOrg := Org{
		ID:      123,
		Name:    "test-org",
		OrgType: "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	// Updated user
	updatedUser := &User{
		ID:          user.ID,
		StarfleetID: user.StarfleetID,
		Email:       cutil.GetPtr("jdoe@test2.com"),
		FirstName:   cutil.GetPtr("John2"),
		LastName:    cutil.GetPtr("Doe2"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *User
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update a User from params",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: UserUpdateInput{
					UserID:    user.ID,
					Email:     updatedUser.Email,
					FirstName: updatedUser.FirstName,
					LastName:  updatedUser.LastName,
					OrgData:   updatedUser.OrgData,
				},
			},
			want:               updatedUser,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			usd := UserSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := usd.Update(tt.args.ctx, nil, tt.args.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("UserSQLDAO.Update() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if *got.StarfleetID != *tt.want.StarfleetID {
				t.Errorf("StarfleetID = %v, want %v", *got.StarfleetID, *tt.want.StarfleetID)
			}

			if *got.Email != *tt.want.Email {
				t.Errorf("Email = %v, want %v", *got.Email, *tt.want.Email)
			}

			if *got.FirstName != *tt.want.FirstName {
				t.Errorf("FirstName = %v, want %v", *got.FirstName, *tt.want.FirstName)
			}

			if *got.LastName != *tt.want.LastName {
				t.Errorf("LastName = %v, want %v", *got.LastName, *tt.want.LastName)
			}

			if got.Updated.String() == user.Updated.String() {
				t.Errorf("got.Updated = %v, want different value", got.Updated)
			}

			retNgcOrg, err := got.OrgData.GetOrgByName(ngcOrg.Name)
			assert.NoError(t, err)

			assert.Equal(t, ngcOrg.ID, retNgcOrg.ID)
			assert.Equal(t, ngcOrg.Name, retNgcOrg.Name)
			assert.Equal(t, ngcOrg.OrgType, retNgcOrg.OrgType)
			assert.Equal(t, len(ngcOrg.Roles), len(retNgcOrg.Roles))
			assert.Equal(t, len(ngcOrg.Teams), len(retNgcOrg.Teams))

			assert.Equal(t, ngcOrg.Teams[0].ID, retNgcOrg.Teams[0].ID)
			assert.Equal(t, ngcOrg.Teams[0].Name, retNgcOrg.Teams[0].Name)
			assert.Equal(t, ngcOrg.Teams[0].TeamType, retNgcOrg.Teams[0].TeamType)
			assert.Equal(t, len(ngcOrg.Teams[0].Roles), len(retNgcOrg.Teams[0].Roles))

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUser_GetOrgByName(t *testing.T) {
	type fields struct {
		ID          uuid.UUID
		StarfleetID string
		Email       *string
		FirstName   *string
		LastName    *string
		OrgData     OrgData
		Created     time.Time
		Updated     time.Time
	}
	type args struct {
		ngcOrgName string
	}

	ngcOrg := Org{
		ID:      123,
		Name:    "test-org",
		OrgType: "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: OrgData{
			"test-org": ngcOrg,
		},
		Created: db.GetCurTime(),
		Updated: db.GetCurTime(),
	}

	tests := []struct {
		name    string
		fields  fields
		args    args
		want    *Org
		wantErr bool
	}{
		{
			name: "get ngc org",
			fields: fields{
				ID:          user.ID,
				StarfleetID: *user.StarfleetID,
				Email:       user.Email,
				FirstName:   user.FirstName,
				LastName:    user.LastName,
				OrgData:     user.OrgData,
				Created:     user.Created,
				Updated:     user.Updated,
			},
			args: args{
				ngcOrgName: ngcOrg.Name,
			},
			want:    &ngcOrg,
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			u := &User{
				ID:          tt.fields.ID,
				StarfleetID: &tt.fields.StarfleetID,
				Email:       tt.fields.Email,
				FirstName:   tt.fields.FirstName,
				LastName:    tt.fields.LastName,
				OrgData:     tt.fields.OrgData,
				Created:     tt.fields.Created,
				Updated:     tt.fields.Updated,
			}
			got, err := u.OrgData.GetOrgByName(tt.args.ngcOrgName)
			if (err != nil) != tt.wantErr {
				t.Errorf("User.OrgData.GetOrgByName() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !reflect.DeepEqual(*got, *tt.want) {
				t.Errorf("User.OrgData.GetOrgByName() = got %v, want %v", *got, *tt.want)
			}
		})
	}
}

func TestUserSQLDAO_GetOrCreate(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Reset User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ngcOrg := Org{
		ID:          123,
		Name:        "test-org",
		DisplayName: "Test Org",
		OrgType:     "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	user1 := User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(&user1).Exec(context.Background())
	require.NoError(t, err)

	user2 := User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jdoe@test2.com"),
		FirstName:   cutil.GetPtr("John2"),
		LastName:    cutil.GetPtr("Doe2"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(&user2).Exec(context.Background())
	require.NoError(t, err)

	user3 := User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr(uuid.NewString()),
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("jboth@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Both"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err = dbSession.DB.NewInsert().Model(&user3).Exec(context.Background())
	require.NoError(t, err)

	type fields struct {
		dbSession  *db.Session
		tracerSpan *stracer.TracerSpan
	}
	type args struct {
		ctx   context.Context
		tx    *db.Tx
		input UserGetOrCreateInput
	}

	tests := []struct {
		name      string
		fields    fields
		args      args
		want      *User
		wantIsNew bool
		wantErr   bool
	}{
		{
			name: "get existing user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					StarfleetID: user1.StarfleetID,
				},
			},
			want:      &user1,
			wantIsNew: false,
			wantErr:   false,
		},
		{
			name: "get new user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr("new-user-aux-id"),
				},
			},
			want: &User{
				AuxiliaryID: cutil.GetPtr("new-user-aux-id"),
				StarfleetID: nil,
				Email:       nil,
				FirstName:   nil,
				LastName:    nil,
				OrgData:     OrgData{},
			},
			wantIsNew: true,
			wantErr:   false,
		},
		{
			name: "create new user when both IDs provided but no exact match exists",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr("non-existent-aux-id"),
					StarfleetID: cutil.GetPtr("non-existent-starfleet-id"),
				},
			},
			want: &User{
				AuxiliaryID: cutil.GetPtr("non-existent-aux-id"),
				StarfleetID: cutil.GetPtr("non-existent-starfleet-id"),
				Email:       nil,
				FirstName:   nil,
				LastName:    nil,
				OrgData:     OrgData{},
			},
			wantIsNew: true,
			wantErr:   false,
		},
		{
			name: "get existing user when both IDs match same user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: user3.AuxiliaryID, // Both IDs match user3
					StarfleetID: user3.StarfleetID, // user3 has both IDs
				},
			},
			want:      &user3,
			wantIsNew: false,
			wantErr:   false,
		},
		{
			name: "error when AuxiliaryID is empty string",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr(""), // Empty string
					StarfleetID: cutil.GetPtr("new-starfleet-id-empty-aux"),
				},
			},
			want:      nil,
			wantIsNew: false,
			wantErr:   true,
		},
		{
			name: "error when StarfleetID is empty string",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr("new-aux-id-empty-starfleet"),
					StarfleetID: cutil.GetPtr(""), // Empty string
				},
			},
			want:      nil,
			wantIsNew: false,
			wantErr:   true,
		},
		{
			name: "error when both AuxiliaryID and StarfleetID are empty strings",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr(""), // Empty string
					StarfleetID: cutil.GetPtr(""), // Empty string
				},
			},
			want:      nil,
			wantIsNew: false,
			wantErr:   true,
		},
		{
			name: "create new user when only AuxiliaryID is empty string (only AuxiliaryID provided)",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				input: UserGetOrCreateInput{
					AuxiliaryID: cutil.GetPtr(""), // Empty string should be converted to nil
					StarfleetID: nil,
				},
			},
			want:      nil,
			wantIsNew: false,
			wantErr:   true, // Should error because AuxiliaryID becomes nil and no StarfleetID
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			usd := UserSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: tt.fields.tracerSpan,
			}
			got, gotIsNew, err := usd.GetOrCreate(tt.args.ctx, tt.args.tx, tt.args.input)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want.AuxiliaryID, got.AuxiliaryID)
			assert.Equal(t, tt.want.StarfleetID, got.StarfleetID)
			assert.Equal(t, tt.want.Email, got.Email)
			assert.Equal(t, tt.want.FirstName, got.FirstName)
			assert.Equal(t, tt.want.LastName, got.LastName)
			assert.Equal(t, tt.want.OrgData, got.OrgData)
			assert.Equal(t, tt.wantIsNew, gotIsNew)
		})
	}
}
