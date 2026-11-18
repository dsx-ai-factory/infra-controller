// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"
)

const (
	deprecationPreTemplate  = `"'%s' is being deprecated%s. Please take action prior to the specified date"`
	deprecationPostTemplate = `"'%s' has been deprecated%s. Please take action immediately"`

	// DeprecationTypeAttribute denotes a deprecation of an API model attribute
	DeprecationTypeAttribute = "Attribute"
	// DeprecationTypeQueryParam denotes a deprecation of an API query parameter
	DeprecationTypeQueryParam = "QueryParam"
	// DeprecationTypeEndpoint denotes a deprecation of an API endpoint
	DeprecationTypeEndpoint = "Endpoint"
)

// DeprecatdEntity denotes an entity that is being deprecated
type DeprecatedEntity struct {
	OldValue     string
	NewValue     *string
	Type         string
	TakeActionBy time.Time
}

// APIDeprecation captures API representation of a deprecation message
type APIDeprecation struct {
	// Field denotes the field that is deprecated (optional)
	Attribute *string `json:"attribute,omitempty"`
	// Field denotes the field that is deprecated (optional)
	QueryParam *string `json:"queryParam,omitempty"`
	// Endpoint denotes the endpoint that is deprecated (optional)
	Endpoint *string `json:"endpoint,omitempty"`
	// ReplacedBy denotes the field that replaces the deprecated field (optional)
	ReplacedBy *string `json:"replacedBy,omitempty"`
	// TakeActionBy indicates the ISO datetime string for when the deprecated field will no longer be accepted or available in the API
	TakeActionBy time.Time `json:"takeActionBy"`
	// Notice describes the deprecated field
	Notice string `json:"notice"`
}

// NewAPIDeprecation creates an API deprecation object from parameters
func NewAPIDeprecation(de DeprecatedEntity) APIDeprecation {
	apiDeprecation := APIDeprecation{
		TakeActionBy: de.TakeActionBy,
	}

	switch de.Type {
	case DeprecationTypeAttribute:
		apiDeprecation.Attribute = &de.OldValue
	case DeprecationTypeQueryParam:
		apiDeprecation.QueryParam = &de.OldValue
	case DeprecationTypeEndpoint:
		apiDeprecation.Endpoint = &de.OldValue
	}

	if de.NewValue != nil {
		apiDeprecation.ReplacedBy = de.NewValue
	}

	noticeReplacedBy := ""
	if de.NewValue != nil {
		noticeReplacedBy = fmt.Sprintf(" in favor of '%s'", *de.NewValue)
	}

	if de.TakeActionBy.After(time.Now()) {
		apiDeprecation.Notice = fmt.Sprintf(deprecationPreTemplate, de.OldValue, noticeReplacedBy)
	} else {
		apiDeprecation.Notice = fmt.Sprintf(deprecationPostTemplate, de.OldValue, noticeReplacedBy)
	}

	return apiDeprecation
}
