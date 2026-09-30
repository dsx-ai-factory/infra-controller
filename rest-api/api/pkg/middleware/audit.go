// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"encoding/json"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/labstack/echo/v4/middleware"
	"github.com/rs/zerolog/log"
)

var auditSkipMethods = map[string]bool{
	echo.GET:     true,
	echo.HEAD:    true,
	echo.CONNECT: true,
	echo.TRACE:   true,
	echo.OPTIONS: true,
}

func AuditLog(dbSession *cdb.Session) echo.MiddlewareFunc {
	aeDAO := cdbm.NewAuditEntryDAO(dbSession)
	return middleware.RequestLoggerWithConfig(middleware.RequestLoggerConfig{
		Skipper: func(c echo.Context) bool {
			// check if method should be skipped
			return auditSkipMethods[c.Request().Method]
		},
		LogURIPath:  true,
		LogMethod:   true,
		LogRemoteIP: true,
		LogStatus:   true,
		LogError:    true,
		LogLatency:  true,
		LogValuesFunc: func(c echo.Context, v middleware.RequestLoggerValues) error {
			createInput := cdbm.AuditEntryCreateInput{
				Endpoint:    v.URIPath,
				QueryParams: c.QueryParams(),
				Method:      v.Method,
				StatusCode:  v.Status,
				ClientIP:    v.RemoteIP,
				OrgName:     c.Param("orgName"),
				Timestamp:   v.StartTime,
				Duration:    v.Latency,
				APIVersion:  metadata.Version,
			}
			// get user
			if dbUser, ok := c.Get("user").(*cdbm.User); ok && dbUser != nil {
				createInput.UserID = &dbUser.ID
			}
			if auditEntry, err := aeDAO.Create(c.Request().Context(), nil, createInput); err != nil {
				log.Error().Err(err).Msg("Failed to create audit entry")
			} else if auditEntry != nil {
				c.Set("auditEntryID", auditEntry.ID)
			}
			return nil
		},
	})
}

// obfuscateFields contains lowercase field names that should be obfuscated when recording request bodies.
var obfuscateFields = map[string]struct{}{
	"ipxescript":         {},
	"userdata":           {},
	"publickey":          {},
	"defaultbmcusername": {},
	"defaultbmcpassword": {},
	"authenticationdata": {},
	"authtoken":          {},
	"clientsecret":       {},
	"imageauthtoken":     {},
	"password":           {},
	"nvospassword":       {},
}

const (
	auditObfuscatedValue          = "*******************"
	auditBodyValueField           = "value"
	auditBodyJSONParseFailedField = "jsonParseFailed"
)

func obfuscateRequestBody(body interface{}) {
	switch body := body.(type) {
	case map[string]interface{}:
		for key, value := range body {
			normalizedKey := strings.ToLower(key)
			_, isSensitive := obfuscateFields[normalizedKey]
			if isSensitive {
				body[key] = auditObfuscatedValue
				continue
			}
			obfuscateRequestBody(value)
		}
	case []interface{}:
		for _, value := range body {
			obfuscateRequestBody(value)
		}
	}
}

func prepareAuditRequestBody(reqBody []byte) (map[string]interface{}, error) {
	var body interface{}
	err := json.Unmarshal(reqBody, &body)
	if err != nil {
		return map[string]interface{}{
			auditBodyJSONParseFailedField: true,
		}, err
	}

	obfuscateRequestBody(body)
	bodyMap, ok := body.(map[string]interface{})
	if ok {
		return bodyMap, nil
	}

	return map[string]interface{}{
		auditBodyValueField: body,
	}, nil
}

type ResponseError struct {
	Source  string `json:"source"`
	Message string `json:"message"`
}

func AuditBody(dbSession *cdb.Session) echo.MiddlewareFunc {
	aeDAO := cdbm.NewAuditEntryDAO(dbSession)
	return middleware.BodyDumpWithConfig(middleware.BodyDumpConfig{
		Skipper: func(c echo.Context) bool {
			// check if method should be skipped
			return auditSkipMethods[c.Request().Method]
		},
		Handler: func(c echo.Context, reqBody []byte, resBody []byte) {
			auditEntryID, ok := c.Get("auditEntryID").(uuid.UUID)
			if !ok {
				return
			}
			updateInput := cdbm.AuditEntryUpdateInput{
				ID: auditEntryID,
			}
			// save status message
			if c.Response().Status >= 400 {
				responseError := ResponseError{}
				if err := json.Unmarshal(resBody, &responseError); err != nil {
					log.Error().Err(err).Msgf("failed to unmarshall error response %s for audit entry %s", string(resBody), auditEntryID)
				} else {
					updateInput.StatusMessage = cutil.GetPtr(responseError.Message)
				}
			}
			// save request body
			if len(reqBody) > 0 {
				bodyMap, err := prepareAuditRequestBody(reqBody)
				if err != nil {
					log.Error().Err(err).Msgf("failed to unmarshall body for audit entry %s", auditEntryID)
				}
				updateInput.Body = bodyMap
			}
			// update
			if _, err := aeDAO.Update(c.Request().Context(), nil, updateInput); err != nil {
				log.Error().Err(err).Msgf("failed to update audit entry %s", auditEntryID)
			}
		},
	})
}
