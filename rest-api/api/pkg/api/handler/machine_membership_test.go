// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/require"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// A malformed request and an unresolvable machine must never outrank the
// membership gate. Nil handler dependencies also prove this path needs no DB
// or site client. A Provider role in another org must not grant access.
func checkMachineMembershipGate(t *testing.T, handle echo.HandlerFunc) {
	t.Helper()
	req := httptest.NewRequest(http.MethodPatch, "/?pageNumber=invalid&includeMetadata=invalid", strings.NewReader("{"))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	c := echo.New().NewContext(req, rec)
	c.SetParamNames("orgName", "id")
	c.SetParamValues("target-org", "unknown-machine")
	c.Set("user", &cdbm.User{OrgData: cdbm.OrgData{
		"other-org": {Name: "other-org", Roles: []string{authz.ProviderAdminRole}},
	}})

	require.NoError(t, handle(c))
	require.Equal(t, http.StatusForbidden, rec.Code)
	var response cutil.APIError
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
	require.Equal(t, "Failed to validate membership for org: target-org", response.Message)
	require.Nil(t, response.Data)
}
