// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIUser is a data structure to capture information about user at the API layer
type APIUser struct {
	// ID is the unique UUID v4 identifier of the user in NICo Cloud
	ID string `json:"id"`
	// FirstName denotes the first name of the user
	FirstName *string `json:"firstName"`
	// LastName denotes the surname of the user
	LastName *string `json:"lastName"`
	// Email contains the email used by the user to register with NGC
	Email *string `json:"email"`
	// Created indicates the ISO datetime string for when the user was created in NICo
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the user was last updated in NICo
	Updated time.Time `json:"updated"`
}

// NewAPIUserFromDBUser creates and returns a new APIUser object
func NewAPIUserFromDBUser(dbUser cdbm.User) *APIUser {
	apiUser := &APIUser{
		ID:        dbUser.ID.String(),
		FirstName: dbUser.FirstName,
		LastName:  dbUser.LastName,
		Email:     dbUser.Email,
		Created:   dbUser.Created,
		Updated:   dbUser.Updated,
	}

	return apiUser
}
