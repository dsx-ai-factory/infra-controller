// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"
)

const (
	validationCommonErrorField = "__all__"
)

// APILoginRequest represents the request body for the login endpoint, handles both authentication initiation and token exchange
type APILoginRequest struct {
	Email        *string `json:"email"`
	RedirectURI  *string `json:"redirectUri"`
	ClientID     *string `json:"clientId"`
	ClientSecret *string `json:"clientSecret"`
}

func (lr *APILoginRequest) Validate() error {
	err := validation.ValidateStruct(lr,
		validation.Field(&lr.Email, validation.When(lr.ClientID == nil, validation.Required.Error("email must be specified if clientId is not specified"))),
		validation.Field(&lr.RedirectURI,
			validationIs.URL.Error("redirectUri must be a valid URL"),
			validation.When(lr.Email != nil, validation.Required.Error("redirectUri must be specified if email is specified")),
			validation.When(lr.ClientID != nil, validation.Nil.Error("redirectUri cannot be specified if clientId is specified"))),
		validation.Field(&lr.ClientID, validation.When(lr.Email == nil, validation.Required.Error("clientId must be specified if email is not specified"))),
		validation.Field(&lr.ClientSecret, validation.When(lr.ClientID != nil, validation.Required.Error("clientSecret must be specified if clientId is specified"))),
	)

	if lr.Email != nil && lr.ClientID != nil {
		return validation.Errors{
			validationCommonErrorField: errors.New("email and clientId cannot be specified together"),
		}
	}

	return err
}

func (lr *APILoginRequest) IsClientCredentials() bool {
	return lr.ClientID != nil && lr.ClientSecret != nil
}

// LogoutRequest represents the request body for logout
type APILogoutRequest struct {
	RefreshToken string `json:"refreshToken"`
}

func (lor *APILogoutRequest) Validate() error {
	return validation.ValidateStruct(lor,
		validation.Field(&lor.RefreshToken, validation.Required.Error("refreshToken is required")),
	)
}

// RefreshTokenRequest represents the request body for token refresh
type APIRefreshTokenRequest struct {
	RefreshToken string `json:"refreshToken"`
}

func (rtr *APIRefreshTokenRequest) Validate() error {
	return validation.ValidateStruct(rtr,
		validation.Field(&rtr.RefreshToken, validation.Required.Error("refreshToken is required")),
	)
}

// CallbackRequest represents the request body for the callback endpoint from BFF
type APICallbackRequest struct {
	Code        string `json:"code"`
	RedirectURI string `json:"redirectUri"`
	State       string `json:"state"`
}

func (cr *APICallbackRequest) Validate() error {
	return validation.ValidateStruct(cr,
		validation.Field(&cr.Code, validation.Required.Error("code is required")),
		validation.Field(&cr.RedirectURI, validation.Required.Error("redirectUri is required"), validationIs.URL.Error("redirectUri must be a valid URL")),
		validation.Field(&cr.State, validation.Required.Error("state is required")),
	)
}

// APILoginResponse represents the response for initiating the authentication flow
type APILoginResponse struct {
	AuthURL   string `json:"authURL"`
	State     string `json:"state"`
	IDP       string `json:"idp"`
	RealmName string `json:"realmName"`
}

// APITokenResponse represents the response returned from Callback handler and RefreshToken handler
type APITokenResponse struct {
	AccessToken  string `json:"accessToken"`
	RefreshToken string `json:"refreshToken"`
	ExpiresIn    int    `json:"expiresIn"`
	TokenType    string `json:"tokenType"`
}
