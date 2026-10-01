// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package user

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"net/http"
	"net/url"
	"sort"
	"strings"

	"github.com/jackc/pgerrcode"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/pkg/errors"
	"github.com/rs/zerolog"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
)

const (
	// NgcRequestStatusSuccess is the code present in successful request status
	NgcRequestStatusSuccess = "SUCCESS"
)

// NgcUser captures NGC user data
type NgcUser struct {
	Email       string       `json:"email"`
	ClientID    string       `json:"clientId"`
	StarfleetID string       `json:"starfleetId"`
	Name        string       `json:"name"`
	Roles       []NgcOrgRole `json:"roles"`
}

// NgcOrg captures attributes for NGC organizations
type NgcOrg struct {
	ID          int    `json:"id"`
	Name        string `json:"name"`
	DisplayName string `json:"displayName"`
	OrgType     string `json:"type"`
	Description string `json:"description"`
}

// NgcTeam captures attributes for NGC teams
type NgcTeam struct {
	ID        int    `json:"id"`
	Name      string `json:"name"`
	TeamType  string `json:"type"`
	IsDeleted bool   `json:"isDeleted"`
}

// NgcOrgRole captures attributes for organization roles user has within NGC
type NgcOrgRole struct {
	Org       NgcOrg   `json:"org"`
	Team      *NgcTeam `json:"team"`
	OrgRoles  []string `json:"orgRoles"`
	TeamRoles []string `json:"teamRoles"`
}

// NgcRequestStatus captures status of the request
type NgcRequestStatus struct {
	StatusCode string `json:"statusCode"`
	RequestID  string `json:"requestId"`
}

// NgcUserResponse captures response data returned by NGC API user endpoint
type NgcUserResponse struct {
	RequestStatus NgcRequestStatus `json:"requestStatus"`
	User          NgcUser          `json:"user"`
}

// ManageUser is an activity wrapper for updating user that allows injecting DB access
type ManageUser struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// GetUserDataFromNgc is a Temporal activity that verifies that a connection to a K8s cluster API
// can be established
func (mu ManageUser) GetUserDataFromNgc(ctx context.Context, userID uuid.UUID, encryptedNgcToken []byte) (*NgcUser, error) {
	logger := log.With().Str("Activity", "GetUserDataFromNgc").Str("User ID", userID.String()).Logger()

	logger.Info().Msg("starting activity")

	// Get user
	uDAO := cdbm.NewUserDAO(mu.dbSession)
	dbUser, err := uDAO.Get(ctx, nil, userID, nil)
	if err != nil {
		log.Error().Err(err).Msg("failed to refresh user data from NGC, could not retrieve user from DB using ID")
		return nil, err
	}

	// Decrypt token using Starfleet ID
	ngcToken := string(cloudutils.DecryptData(encryptedNgcToken, *dbUser.StarfleetID))

	apiBaseURL := mu.cfg.GetNgcAPIBaseURL()

	return getUserDataFromNGC(apiBaseURL, ngcToken, logger)
}

// getUserDataFromNGC retrieves user data from NGC user API endpoint
func getUserDataFromNGC(baseURL string, ngcToken string, logger zerolog.Logger) (*NgcUser, error) {
	reqURL, _ := url.Parse(fmt.Sprintf("%v/v2/users/me", baseURL))
	req := &http.Request{
		Method: "GET",
		URL:    reqURL,
		Header: map[string][]string{
			"Authorization":   {fmt.Sprintf("Bearer %v", ngcToken)},
			"Accept-Encoding": {"identity"},
		},
	}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		logger.Error().Err(err).Str("Request URL", fmt.Sprintf("%v", reqURL)).
			Msg("http error retrieving user data from NGC user API endpoint")
		return nil, err
	}

	if resp.StatusCode != http.StatusOK {
		logger.Error().Str("Request URL", fmt.Sprintf("%v", reqURL)).
			Str("Response Status", resp.Status).Msg("invalid response code returned by NGC API")
		return nil, fmt.Errorf("invalid response code: %v returned by NGC API", resp.StatusCode)
	}

	respData, err := ioutil.ReadAll(resp.Body)
	defer resp.Body.Close()

	if err != nil {
		logger.Error().Err(err).Msg("failed to read NGC user API response")
		return nil, err
	}

	var ngcUserResponse NgcUserResponse
	err = json.Unmarshal(respData, &ngcUserResponse)
	if err != nil {
		logger.Error().Err(err).Str("Response Data", fmt.Sprintf("%+v", respData)).
			Msg("failed to parse user data from NGC API response")
		return nil, err
	}

	if ngcUserResponse.RequestStatus.StatusCode != NgcRequestStatusSuccess {
		logger.Error().Err(err).Str("Request Status Code", ngcUserResponse.RequestStatus.StatusCode).
			Msg("invalid request status returned by NGC API")
		return nil, err
	}

	logger.Info().Msg("successfully completed activity")
	return &ngcUserResponse.User, nil
}

// UpdateUserInDB updates user data in DB from NGC user data
func (mu ManageUser) UpdateUserInDB(ctx context.Context, userID uuid.UUID, ngcUser *NgcUser) error {
	logger := log.With().Str("Activity", "UpdateUserInDB").Str("User ID", userID.String()).Logger()

	logger.Info().Msg("starting activity")

	nameComps := strings.SplitN(ngcUser.Name, " ", 2)
	firstName := &nameComps[0]
	var lastName *string
	if len(nameComps) == 2 {
		lastName = &nameComps[1]
	}

	email := strings.ToLower(ngcUser.Email)

	// Org information, org roles, team information and roles are saved as JSON
	orgData := GetOrgData(ngcUser)

	userDAO := cdbm.NewUserDAO(mu.dbSession)

	_, err := userDAO.Update(ctx, nil, cdbm.UserUpdateInput{
		UserID:    userID,
		Email:     &email,
		FirstName: firstName,
		LastName:  lastName,
		OrgData:   orgData,
	})
	if err != nil {
		log.Error().Err(err).Msg("failed to update user in DB from NGC user API response")
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// GetUserDataFromNgcWithAuxiliaryID is a Temporal activity that verifies that a connection to a K8s cluster API
func (mu ManageUser) GetUserDataFromNgcWithAuxiliaryID(ctx context.Context, auxiliaryID string, encryptedNgcToken []byte) (*NgcUser, error) {
	logger := log.With().Str("Activity", "GetUserDataFromNgcWithAuxiliaryID").Str("User AuxiliaryID", auxiliaryID).Logger()

	logger.Info().Msg("starting activity")

	tcfg, _ := mu.cfg.GetTemporalConfig()

	ngcToken := string(cloudutils.DecryptData(encryptedNgcToken, tcfg.EncryptionKey))

	apiBaseURL := mu.cfg.GetNgcAPIBaseURL()

	return getUserDataFromNGC(apiBaseURL, ngcToken, logger)
}

// CreateOrUpdateUserInDBWithAuxiliaryID updates user data in DB from NGC user data
func (mu ManageUser) CreateOrUpdateUserInDBWithAuxiliaryID(ctx context.Context, ngcUser *NgcUser) error {
	logger := log.With().Str("Activity", "CreateOrUpdateUserInDBWithAuxiliaryID").Str("User Email", ngcUser.Email).Logger()

	logger.Info().Msg("starting activity")

	// Validate that both StarfleetID and ClientID are present
	if ngcUser.StarfleetID == "" {
		return errors.Wrap(cdb.ErrInvalidParams, "StarfleetID is required and cannot be empty")
	}
	if ngcUser.ClientID == "" {
		return errors.Wrap(cdb.ErrInvalidParams, "ClientID is required and cannot be empty")
	}

	nameComps := strings.SplitN(ngcUser.Name, " ", 2)
	firstName := &nameComps[0]
	var lastName *string
	if len(nameComps) == 2 {
		lastName = &nameComps[1]
	}

	email := strings.ToLower(ngcUser.Email)

	// Org information, org roles, team information and roles are saved as JSON
	orgData := GetOrgData(ngcUser)

	userDAO := cdbm.NewUserDAO(mu.dbSession)

	var starfleetIDUsers []cdbm.User
	var auxiliaryIDUsers []cdbm.User

	// Search by StarfleetID
	starfleetIDFilter := cdbm.UserFilterInput{
		StarfleetIDs: []string{ngcUser.StarfleetID},
	}
	var err error
	starfleetIDUsers, _, err = userDAO.GetAll(ctx, nil, starfleetIDFilter, paginator.PageInput{Limit: cloudutils.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		log.Error().Err(err).Msg("failed to get users by StarfleetID from DB")
		return err
	}

	// Search by AuxiliaryID
	auxiliaryIdFilter := cdbm.UserFilterInput{
		AuxiliaryIDs: []string{ngcUser.ClientID},
	}
	auxiliaryIDUsers, _, err = userDAO.GetAll(ctx, nil, auxiliaryIdFilter, paginator.PageInput{Limit: cloudutils.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		log.Error().Err(err).Msg("failed to get users by AuxiliaryID from DB")
		return err
	}

	// Compare and validate results
	var existingUser *cdbm.User

	if len(starfleetIDUsers) > 0 && len(auxiliaryIDUsers) > 0 {
		// Both searches found users - verify they are the same user
		if len(starfleetIDUsers) > 1 || len(auxiliaryIDUsers) > 1 {
			return errors.Wrap(cdb.ErrInvalidParams, fmt.Sprintf("multiple users found for StarfleetID %s or AuxiliaryID %s", ngcUser.StarfleetID, ngcUser.ClientID))
		}

		if starfleetIDUsers[0].ID != auxiliaryIDUsers[0].ID {
			return errors.Wrap(cdb.ErrInvalidParams, fmt.Sprintf("different users found: StarfleetID %s belongs to user %s, AuxiliaryID %s belongs to user %s",
				ngcUser.StarfleetID, starfleetIDUsers[0].ID.String(), ngcUser.ClientID, auxiliaryIDUsers[0].ID.String()))
		}

		existingUser = &starfleetIDUsers[0]
	} else if len(starfleetIDUsers) > 0 {
		if len(starfleetIDUsers) > 1 {
			return errors.Wrap(cdb.ErrInvalidParams, fmt.Sprintf("multiple users found for StarfleetID %s", ngcUser.StarfleetID))
		}
		existingUser = &starfleetIDUsers[0]
	} else if len(auxiliaryIDUsers) > 0 {
		if len(auxiliaryIDUsers) > 1 {
			return errors.Wrap(cdb.ErrInvalidParams, fmt.Sprintf("multiple users found for AuxiliaryID %s", ngcUser.ClientID))
		}
		existingUser = &auxiliaryIDUsers[0]
	}

	if existingUser != nil {
		// User found, update with latest data including AuxiliaryID and OrgData
		_, err := userDAO.Update(ctx, nil, cdbm.UserUpdateInput{
			UserID:      existingUser.ID,
			AuxiliaryID: &ngcUser.ClientID,
			StarfleetID: &ngcUser.StarfleetID,
			Email:       &email,
			FirstName:   firstName,
			LastName:    lastName,
			OrgData:     orgData,
		})
		if err != nil {
			log.Error().Err(err).Msg("failed to update existing user in DB")
			return err
		}
	} else {
		createInput := cdbm.UserCreateInput{
			AuxiliaryID: &ngcUser.ClientID,
			StarfleetID: &ngcUser.StarfleetID,
			Email:       &email,
			FirstName:   firstName,
			LastName:    lastName,
			OrgData:     orgData,
		}

		_, err := userDAO.Create(ctx, nil, createInput)
		if err != nil {
			var pErr *pgconn.PgError
			isUniqueViolation := errors.As(err, &pErr) && pErr.Code == pgerrcode.UniqueViolation
			if !isUniqueViolation {
				log.Error().Err(err).Msg("failed to create new user in DB")
				return err
			}

			retryFilter := cdbm.UserFilterInput{}
			// We check above that ngcUser.StarfleetID is not empty
			retryFilter.StarfleetIDs = []string{ngcUser.StarfleetID}

			users, uCount, err := userDAO.GetAll(ctx, nil, retryFilter, paginator.PageInput{Limit: cloudutils.GetPtr(paginator.TotalLimit)}, nil)
			if err != nil {
				return err
			}
			if uCount == 1 {
				_, err := userDAO.Update(ctx, nil, cdbm.UserUpdateInput{
					UserID:      users[0].ID,
					AuxiliaryID: &ngcUser.ClientID,
					StarfleetID: &ngcUser.StarfleetID,
					Email:       &email,
					FirstName:   firstName,
					LastName:    lastName,
					OrgData:     orgData,
				})
				if err != nil {
					log.Error().Err(err).Msg("failed to update User after constraint violation")
					return err
				}
			} else {
				return fmt.Errorf("unable to create User after constraint violation")
			}
		}
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// NewManageUser returns a new user management activity wrapper
func NewManageUser(dbSession *cdb.Session, cfg *config.Config) ManageUser {
	return ManageUser{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// GetOrgData returns the NGC org data from the user
func GetOrgData(ngcUser *NgcUser) cdbm.OrgData {
	orgData := cdbm.OrgData{}

	if ngcUser.Roles != nil {
		for _, role := range ngcUser.Roles {
			var ngcOrg cdbm.Org
			var found bool

			ngcOrg, found = orgData[role.Org.Name]

			if !found {
				roles := append([]string{}, role.OrgRoles...)
				sort.Strings(roles)

				ngcOrg = cdbm.Org{
					ID:          role.Org.ID,
					Name:        role.Org.Name,
					DisplayName: role.Org.DisplayName,
					OrgType:     role.Org.OrgType,
					Roles:       roles,
					Teams:       []cdbm.Team{},
				}
				orgData[role.Org.Name] = ngcOrg
			}

			if role.Team != nil && !role.Team.IsDeleted {
				roles := append([]string{}, role.TeamRoles...)
				sort.Strings(roles)

				ngcOrg.Teams = append(ngcOrg.Teams, cdbm.Team{
					ID:       role.Team.ID,
					Name:     role.Team.Name,
					TeamType: role.Team.TeamType,
					Roles:    roles,
				})

				// Update the org in the map with the new team
				orgData[role.Org.Name] = ngcOrg
			}
		}
	}

	return orgData
}
