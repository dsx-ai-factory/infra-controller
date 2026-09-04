// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

const (
	// DeletionRequestAcceptedMessage is returned when an asynchronous delete has
	// been accepted for processing.
	DeletionRequestAcceptedMessage = "Deletion request was accepted"
)

// APIDeletionAcceptedResponse is the JSON body for accepted async DELETE requests.
type APIMessageResponse struct {
	Message string `json:"message"`
}

// APILabelGetAllRequest contains the optional scope for a label list query.
type APILabelGetAllRequest struct {
	SiteID string `query:"siteId"`
}

// NewAPIDeletionAcceptedResponse returns the JSON body for accepted async deletes.
func NewAPIDeletionAcceptedResponse() APIMessageResponse {
	return APIMessageResponse{
		Message: DeletionRequestAcceptedMessage,
	}
}
