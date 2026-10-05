// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package user

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"testing"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	"github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/util"
	"github.com/dsx-ai-factory/infra-controller/rest-api/workflow/internal/config"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/uptrace/bun/extra/bundebug"
)

func testManageUserInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// Reset the tables needed for ManageUser tests.
func testManageUserUser(t *testing.T, dbSession *cdb.Session, auxid, starfleetid *string) *cdbm.User {
	// Create User table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)

	uDAO := cdbm.NewUserDAO(dbSession)

	u, err := uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
		AuxiliaryID: auxid,
		StarfleetID: starfleetid,
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData:     cdbm.OrgData{},
	})
	assert.Nil(t, err)

	return u
}

func TestManageUser_GetUserDataFromNgc(t *testing.T) {
	type fields struct {
		dbSession *cdb.Session
		cfg       *config.Config
	}
	type args struct {
		ctx               context.Context
		userID            uuid.UUID
		encryptedNgcToken []byte
	}

	dbSession := testManageUserInitDB(t)
	defer dbSession.DB.Close()

	user := testManageUserUser(t, dbSession, cutil.GetPtr(uuid.NewString()), cutil.GetPtr(uuid.NewString()))

	ngcToken := "test67890"
	encryptedNgcToken := cutil.EncryptData([]byte(ngcToken), *user.StarfleetID)

	ngcUser := NgcUser{
		Email: *user.Email,
		Name:  "John Doe",
		Roles: []NgcOrgRole{
			{
				Org: NgcOrg{
					ID:          123,
					Name:        "Test Org",
					DisplayName: "Test Org Display Name",
					OrgType:     "test-org-type",
					Description: "Test Org Description",
				},
				OrgRoles: []string{"test-org-role"},
			},
		},
	}

	ngcResp := NgcUserResponse{
		RequestStatus: NgcRequestStatus{
			StatusCode: NgcRequestStatusSuccess,
			RequestID:  "test-request-id",
		},
		User: ngcUser,
	}

	ngcRespBytes, err := json.Marshal(ngcResp)
	assert.Nil(t, err)

	// Generate a test server so we can capture and inspect the request
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		res.WriteHeader(http.StatusOK)
		res.Write(ngcRespBytes)
	}))

	defer testServer.Close()

	errServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		res.WriteHeader(http.StatusServiceUnavailable)
	}))

	defer errServer.Close()

	cfg := config.NewConfig()

	tests := []struct {
		name    string
		fields  fields
		args    args
		server  *httptest.Server
		want    *NgcUser
		wantErr bool
	}{
		{
			name: "test get user data from ngc activity success",
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			server: testServer,
			args: args{
				ctx:               context.Background(),
				userID:            user.ID,
				encryptedNgcToken: encryptedNgcToken,
			},
			want: &ngcUser,
		},
		{
			name: "test get user data from ngc activity failure, service unavailable",
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			server: errServer,
			args: args{
				ctx:               context.Background(),
				userID:            user.ID,
				encryptedNgcToken: encryptedNgcToken,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.fields.cfg.SetNgcAPIBaseURL(tt.server.URL)

			mu := ManageUser{
				dbSession: tt.fields.dbSession,
				cfg:       tt.fields.cfg,
			}

			got, err := mu.GetUserDataFromNgc(tt.args.ctx, tt.args.userID, tt.args.encryptedNgcToken)
			if tt.wantErr {
				assert.NotNil(t, err)
				return
			}

			assert.Equal(t, *tt.want, *got)
		})
	}
}

func TestManageUser_UpdateUserInDB(t *testing.T) {
	type fields struct {
		dbSession *cdb.Session
		cfg       *config.Config
	}
	type args struct {
		ctx     context.Context
		userID  uuid.UUID
		ngcUser *NgcUser
	}

	dbSession := testManageUserInitDB(t)
	defer dbSession.DB.Close()

	user := testManageUserUser(t, dbSession, cutil.GetPtr(uuid.NewString()), cutil.GetPtr(uuid.NewString()))

	fmt.Printf("user: %+v\n", user)

	ngcUser := NgcUser{
		Email: "johnd@test.com",
		Name:  "John Doe",
		Roles: []NgcOrgRole{
			{
				Org: NgcOrg{
					ID:          123,
					Name:        "Test Org",
					DisplayName: "Test Org Display Name",
					OrgType:     "test-org-type",
					Description: "Test Org Description",
				},
				OrgRoles: []string{"test-org-role"},
			},
		},
	}

	cfg := config.NewConfig()

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update user in DB activity",
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx:     context.Background(),
				userID:  user.ID,
				ngcUser: &ngcUser,
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mu := ManageUser{
				dbSession: tt.fields.dbSession,
				cfg:       cfg,
			}
			if err := mu.UpdateUserInDB(tt.args.ctx, tt.args.userID, tt.args.ngcUser); (err != nil) != tt.wantErr {
				t.Errorf("ManageUser.UpdateUserInDB() error = %v, wantErr %v", err, tt.wantErr)
			}

			// Check if the user was updated in the DB
			uDAO := cdbm.NewUserDAO(dbSession)
			uu, err := uDAO.Get(context.Background(), nil, tt.args.userID, nil)
			assert.Nil(t, err)

			fmt.Printf("updated user: %+v\n", uu)

			assert.Equal(t, tt.args.ngcUser.Email, *uu.Email)

			ngcRole := tt.args.ngcUser.Roles[0]

			userNgcOrg, err := uu.OrgData.GetOrgByName(ngcRole.Org.Name)
			assert.Nil(t, err)

			assert.Equal(t, ngcRole.Org.Name, userNgcOrg.Name)
			assert.Equal(t, ngcRole.OrgRoles, userNgcOrg.Roles)
		})
	}
}

func TestNewManageUser(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		cfg       *config.Config
	}

	dbSession := &cdb.Session{}
	cfg := config.NewConfig()

	tests := []struct {
		name string
		args args
		want ManageUser
	}{
		{
			name: "test new ManageUser instantiation",
			args: args{
				dbSession: dbSession,
				cfg:       cfg,
			},
			want: ManageUser{
				dbSession: dbSession,
				cfg:       cfg,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageUser(tt.args.dbSession, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageUser() = %v, want %v", got, tt.want)
			}

		})
	}
}

func TestManageUser_CreateOrUpdateUserInDBWithAuxiliaryID(t *testing.T) {
	type fields struct {
		dbSession *cdb.Session
		cfg       *config.Config
	}
	type args struct {
		ctx     context.Context
		ngcUser *NgcUser
	}

	dbSession := testManageUserInitDB(t)
	defer dbSession.DB.Close()

	// Reset User table for clean state
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)

	cfg := config.NewConfig()

	tests := []struct {
		name    string
		setup   func(t *testing.T) // Function to set up test data
		fields  fields
		args    args
		wantErr bool
		errMsg  string             // Expected error message substring
		verify  func(t *testing.T) // Function to verify results
	}{
		{
			name: "no existing user - creates new user",
			setup: func(t *testing.T) {
				// Clean slate - no setup needed
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "newuser@test.com",
					ClientID:    "client123",
					StarfleetID: "starfleet123",
					Name:        "New User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{},
					paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "newuser@test.com", *users[0].Email)
				assert.Equal(t, "client123", *users[0].AuxiliaryID)
				assert.Equal(t, "starfleet123", *users[0].StarfleetID)
			},
		},
		{
			name: "user exists by starfleet ID only - updates with auxiliary ID",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					StarfleetID: cutil.GetPtr("existing_starfleet"),
					Email:       cutil.GetPtr("existing@test.com"),
					FirstName:   cutil.GetPtr("Existing"),
					LastName:    cutil.GetPtr("User"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "existing@test.com",
					ClientID:    "new_auxiliary_id",
					StarfleetID: "existing_starfleet",
					Name:        "Existing User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					StarfleetIDs: []string{"existing_starfleet"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "new_auxiliary_id", *users[0].AuxiliaryID)
			},
		},
		{
			name: "user exists by auxiliary ID only - updates with starfleet ID",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					AuxiliaryID: cutil.GetPtr("existing_auxiliary"),
					Email:       cutil.GetPtr("existing2@test.com"),
					FirstName:   cutil.GetPtr("Existing2"),
					LastName:    cutil.GetPtr("User2"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "existing2@test.com",
					ClientID:    "existing_auxiliary",
					StarfleetID: "new_starfleet_id",
					Name:        "Existing2 User2",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					AuxiliaryIDs: []string{"existing_auxiliary"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "new_starfleet_id", *users[0].StarfleetID)
			},
		},
		{
			name: "user exists by both IDs and they match - updates user",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					AuxiliaryID: cutil.GetPtr("both_auxiliary"),
					StarfleetID: cutil.GetPtr("both_starfleet"),
					Email:       cutil.GetPtr("both@test.com"),
					FirstName:   cutil.GetPtr("Both"),
					LastName:    cutil.GetPtr("User"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "both_updated@test.com",
					ClientID:    "both_auxiliary",
					StarfleetID: "both_starfleet",
					Name:        "Both Updated User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					StarfleetIDs: []string{"both_starfleet"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "both_updated@test.com", *users[0].Email)
				assert.Equal(t, "Both", *users[0].FirstName)
			},
		},
		{
			name: "users exist by both IDs but are different - returns error",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				// Create user with starfleet ID
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					StarfleetID: cutil.GetPtr("conflict_starfleet"),
					Email:       cutil.GetPtr("starfleet@test.com"),
					FirstName:   cutil.GetPtr("Starfleet"),
					LastName:    cutil.GetPtr("User"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)

				// Create different user with auxiliary ID
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					AuxiliaryID: cutil.GetPtr("conflict_auxiliary"),
					Email:       cutil.GetPtr("auxiliary@test.com"),
					FirstName:   cutil.GetPtr("Auxiliary"),
					LastName:    cutil.GetPtr("User"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "conflict@test.com",
					ClientID:    "conflict_auxiliary",
					StarfleetID: "conflict_starfleet",
					Name:        "Conflict User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: true,
			errMsg:  "different users found",
			verify: func(t *testing.T) {
				// Verify no users were modified
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{},
					paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 2, len(users)) // Still 2 separate users
			},
		},
		{
			name: "empty starfleet ID - returns error",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "empty_starfleet@test.com",
					ClientID:    "valid_client_id",
					StarfleetID: "",
					Name:        "Empty StarfleetID User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: true,
			errMsg:  "StarfleetID is required and cannot be empty",
		},
		{
			name: "empty client ID - returns error",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "empty_client@test.com",
					ClientID:    "",
					StarfleetID: "valid_starfleet_id",
					Name:        "Empty ClientID User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: true,
			errMsg:  "ClientID is required and cannot be empty",
		},
		{
			name: "both IDs empty - returns error for StarfleetID first",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "both_empty@test.com",
					ClientID:    "",
					StarfleetID: "",
					Name:        "Both Empty User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: true,
			errMsg:  "StarfleetID is required and cannot be empty",
		},
		{
			name: "test unique constraint violation - creates then updates on retry",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				// Pre-create a user with StarfleetID to trigger unique constraint violation later
				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					StarfleetID: cutil.GetPtr("constraint_starfleet"),
					Email:       cutil.GetPtr("constraint@test.com"),
					FirstName:   cutil.GetPtr("Constraint"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "constraint_updated@test.com",
					ClientID:    "constraint_auxiliary",
					StarfleetID: "constraint_starfleet",
					Name:        "Constraint User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					StarfleetIDs: []string{"constraint_starfleet"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "constraint_updated@test.com", *users[0].Email)
				assert.Equal(t, "constraint_auxiliary", *users[0].AuxiliaryID)
			},
		},
		{
			name: "multiple users found by auxiliary ID - returns error",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				// Create first user with auxiliary ID
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					AuxiliaryID: cutil.GetPtr("duplicate_auxiliary"),
					Email:       cutil.GetPtr("auxdup1@test.com"),
					FirstName:   cutil.GetPtr("AuxDup1"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "multiple_aux@test.com",
					ClientID:    "duplicate_auxiliary",
					StarfleetID: "starfleet_multi_aux",
					Name:        "Multiple Auxiliary User",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false, // Should update the existing user
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					AuxiliaryIDs: []string{"duplicate_auxiliary"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "starfleet_multi_aux", *users[0].StarfleetID)
			},
		},
		{
			name: "test len(starfleetIDUsers) > 0 condition - single user found by starfleet ID",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					StarfleetID: cutil.GetPtr("single_starfleet"),
					Email:       cutil.GetPtr("single_starfleet@test.com"),
					FirstName:   cutil.GetPtr("Single"),
					LastName:    cutil.GetPtr("StarfleetUser"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "single_starfleet_updated@test.com",
					ClientID:    "new_auxiliary_for_single",
					StarfleetID: "single_starfleet",
					Name:        "Single StarfleetUser Updated",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					StarfleetIDs: []string{"single_starfleet"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "single_starfleet_updated@test.com", *users[0].Email)
				assert.Equal(t, "new_auxiliary_for_single", *users[0].AuxiliaryID)
			},
		},
		{
			name: "test len(auxiliaryIDUsers) > 0 condition - single user found by auxiliary ID",
			setup: func(t *testing.T) {
				err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
				assert.Nil(t, err)

				uDAO := cdbm.NewUserDAO(dbSession)
				_, err = uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
					AuxiliaryID: cutil.GetPtr("single_auxiliary"),
					Email:       cutil.GetPtr("single_auxiliary@test.com"),
					FirstName:   cutil.GetPtr("Single"),
					LastName:    cutil.GetPtr("AuxiliaryUser"),
					OrgData:     cdbm.OrgData{},
				})
				assert.Nil(t, err)
			},
			fields: fields{
				dbSession: dbSession,
				cfg:       cfg,
			},
			args: args{
				ctx: context.Background(),
				ngcUser: &NgcUser{
					Email:       "single_auxiliary_updated@test.com",
					ClientID:    "single_auxiliary",
					StarfleetID: "new_starfleet_for_single",
					Name:        "Single AuxiliaryUser Updated",
					Roles:       []NgcOrgRole{},
				},
			},
			wantErr: false,
			verify: func(t *testing.T) {
				uDAO := cdbm.NewUserDAO(dbSession)
				users, _, err := uDAO.GetAll(context.Background(), nil, cdbm.UserFilterInput{
					AuxiliaryIDs: []string{"single_auxiliary"},
				}, paginator.PageInput{Limit: cutil.GetPtr(100)}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(users))
				assert.Equal(t, "single_auxiliary_updated@test.com", *users[0].Email)
				assert.Equal(t, "new_starfleet_for_single", *users[0].StarfleetID)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Setup test data
			tt.setup(t)

			mu := ManageUser{
				dbSession: tt.fields.dbSession,
				cfg:       tt.fields.cfg,
			}

			err := mu.CreateOrUpdateUserInDBWithAuxiliaryID(tt.args.ctx, tt.args.ngcUser)

			if tt.wantErr {
				assert.NotNil(t, err)
				if tt.errMsg != "" {
					assert.Contains(t, err.Error(), tt.errMsg)
				}
			} else {
				if err != nil {
					t.Errorf("ManageUser.CreateOrUpdateUserInDBWithAuxiliaryID() error = %v, wantErr %v", err, tt.wantErr)
				}
			}

			// Run verification if provided
			if tt.verify != nil {
				tt.verify(t)
			}
		})
	}
}
