// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import "encoding/json"

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

// APILabels is a response label map that serializes nil as an empty JSON object.
// Request and database models retain their own nil semantics.
// Response fields using this type must not use omitempty.
type APILabels map[string]string

// MarshalJSON encodes labels as an object, including when the map is nil.
func (labels APILabels) MarshalJSON() ([]byte, error) {
	if labels == nil {
		return []byte("{}"), nil
	}
	return json.Marshal(map[string]string(labels))
}

// APIList is a response collection that serializes nil as an empty JSON array.
// Response fields using this type must not use omitempty.
type APIList[T any] []T

// MarshalJSON encodes items as an array, including when the slice is nil.
func (items APIList[T]) MarshalJSON() ([]byte, error) {
	if items == nil {
		return []byte("[]"), nil
	}
	return json.Marshal([]T(items))
}
