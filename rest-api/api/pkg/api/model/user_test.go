// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPIUserFromDBUser(t *testing.T) {
	u := cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test123"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		Created:     time.Now(),
		Updated:     time.Now(),
	}

	tests := []struct {
		name          string
		email         *string
		wantEmail     *string
		wantJSONEmail string
	}{
		{
			name:          "valid email is preserved",
			email:         cutil.GetPtr("jdoe@test.com"),
			wantEmail:     cutil.GetPtr("jdoe@test.com"),
			wantJSONEmail: `"jdoe@test.com"`,
		},
		{
			name:          "blank email becomes null",
			email:         cutil.GetPtr(""),
			wantJSONEmail: "null",
		},
		{
			name:          "nil email remains null",
			wantJSONEmail: "null",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dbUser := u
			dbUser.Email = tt.email
			got := NewAPIUserFromDBUser(dbUser)
			assert.Equal(t, &APIUser{
				ID:        u.ID.String(),
				FirstName: u.FirstName,
				LastName:  u.LastName,
				Email:     tt.wantEmail,
				Created:   u.Created,
				Updated:   u.Updated,
			}, got)

			data, err := json.Marshal(got)
			require.NoError(t, err)
			var encoded map[string]json.RawMessage
			err = json.Unmarshal(data, &encoded)
			require.NoError(t, err)
			assert.Equal(t, tt.wantJSONEmail, string(encoded["email"]))
		})
	}
}
