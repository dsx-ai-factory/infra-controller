// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"errors"
	"fmt"
	"net/http"
	"reflect"
	"regexp"
	"strings"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

const (
	// MaxUserDataBytes caps `userData` on Instance and Operating System
	// create/update requests. A Site publishes Instance inventory in pages of
	// InventoryCloudPageSize (25), each budgeted at maxPublishPayloadBytes
	// (1945 KiB) to stay under the 2 MiB blob Temporal rejects. 32 KiB holds a
	// full page's user data to 800 KiB, leaving the rest for the other Instance
	// fields, and keeps one record far enough under the budget that the publish
	// ladder never has to floor at a single item to fit it. It also sits
	// mid-range for infra providers: AWS 16 KB, Hetzner/Alibaba 32 KiB,
	// Azure/OpenStack ~48 KiB, DO/IBM 64 KiB.
	MaxUserDataBytes = 32 * 1024
)

var (
	LeadingWhitespaceRegexp  = regexp.MustCompile("^\\s+.*")
	TrailingWhitespaceRegexp = regexp.MustCompile(".*\\s+$")
	NotAllWhitespaceRegexp   = regexp.MustCompile("[^\\s]+")
	ShaHashRegex             = regexp.MustCompile("^[A-Fa-f0-9]+$")
	Sha256LowercaseHexRegex  = regexp.MustCompile("^[a-f0-9]{64}$")
	DiskImagePathRegex       = regexp.MustCompile(`^(smallest|/dev/(nvme[0-9]+n[0-9]+|sd[a-z]+|disk/by-id/[^/[:space:]]+))$`)
	diskImagePartitionRegex  = regexp.MustCompile(`^/dev/disk/by-id/.*-part[0-9]+$`)
	errInvalidDiskImagePath  = errors.New("not a valid disk path")

	ValidationErrorNameHasLeadingWhitespace  = errors.New("name field has leading whitespace")
	ValidationErrorNameHasTrailingWhitespace = errors.New("name field has trailing whitespace")
	ValidationErrorNameFieldIsEmpty          = errors.New("name field is empty")
	ValidationErrorOnlyWhitespace            = errors.New("field consists only of whitespace")

	// Label restrictions
	LabelKeyMaxLength   = 255
	LabelValueMaxLength = 255
	LabelCountMax       = 10

	// Label validation error messages
	ErrValidationLabelKeyEmpty    = errors.New("one or more labels do not have a key specified")
	ErrValidationLabelKeyLength   = fmt.Errorf("label key must contain at least 1 character and a maximum of %v characters", LabelKeyMaxLength)
	ErrValidationLabelValueLength = fmt.Errorf("label value cannot exceed a maximum of %v characters", LabelValueMaxLength)
	ErrValidationLabelCount       = fmt.Errorf("up to %v key/value pairs can be specified in labels", LabelCountMax)
	ErrValidationLabelNUL         = errors.New("label keys and values must not contain NUL characters")

	// ErrValidationEffectiveUserDataLength reports the merged user data
	// breaching MaxUserDataBytes after Operating System defaults are
	// inherited and the phone-home block is inserted.
	ErrValidationEffectiveUserDataLength = fmt.Errorf("effective `userData` exceeds %d KiB after applying Operating System defaults and phone-home configuration", MaxUserDataBytes/1024)
)

// ValidateDiskImagePath validates whole-disk targets accepted by the image
// provisioning flow. Partition aliases in /dev/disk/by-id use a trailing
// -part<number> suffix and must not be accepted as whole disks.
func ValidateDiskImagePath(value interface{}) error {
	value, isNil := validation.Indirect(value)
	if isNil {
		return nil
	}

	path, ok := value.(string)
	if !ok || !DiskImagePathRegex.MatchString(path) || diskImagePartitionRegex.MatchString(path) {
		return errInvalidDiskImagePath
	}

	return nil
}

// ValidateEffectiveUserData checks the byte length of the user data a request
// sends to the Site. Request-field validation runs before Operating System
// defaults are inherited and before phone-home insertion enlarges the YAML,
// so every path that finalizes user data has to re-check the result.
func ValidateEffectiveUserData(userData *string) error {
	if userData == nil || len(*userData) <= MaxUserDataBytes {
		return nil
	}
	return validation.Errors{
		"userData": ErrValidationEffectiveUserDataLength,
	}
}

// ValidateLabels validates optional API label maps (count, keys, values).
// Signature matches ozzo's `validation.RuleFunc` so it can be used
// directly inside a `validation.By(...)` call from a struct's `Validate`.
// Returns nil when labels is nil; ignores values that aren't a
// `map[string]string`.
func ValidateLabels(value interface{}) error {
	if value == nil {
		return nil
	}
	labels, ok := value.(map[string]string)
	if !ok {
		return nil
	}
	if labels == nil {
		return nil
	}
	if len(labels) > LabelCountMax {
		return validation.Errors{
			"labels": ErrValidationLabelCount,
		}
	}

	keyErrMsg := ErrValidationLabelKeyLength.Error()
	valueErrMsg := ErrValidationLabelValueLength.Error()

	for key, value := range labels {
		if key == "" {
			return validation.Errors{
				"labels": ErrValidationLabelKeyEmpty,
			}
		}
		if strings.ContainsRune(key, '\x00') || strings.ContainsRune(value, '\x00') {
			return validation.Errors{
				"labels": ErrValidationLabelNUL,
			}
		}

		err := validation.Validate(key,
			validation.Match(NotAllWhitespaceRegexp).Error("label key consists only of whitespace"),
			validation.Length(1, LabelKeyMaxLength).Error(keyErrMsg),
		)
		if err != nil {
			return validation.Errors{
				"labels": err,
			}
		}

		err = validation.Validate(value,
			validation.When(value != "",
				validation.Length(0, LabelValueMaxLength).Error(valueErrMsg),
			),
		)
		if err != nil {
			return validation.Errors{
				"labels": err,
			}
		}
	}

	return nil
}

// util.GetUUIDPtrToStrPtr is a utility function to return string pointer from uuid pointer
func GetUUIDPtrToStrPtr(id *uuid.UUID) *string {
	if id == nil {
		return nil
	}
	s := id.String()
	return &s
}

// ValidateNested is a utility function to validate nested struct
func ValidateNested(target interface{}, fieldRules ...*validation.FieldRules) *validation.FieldRules {
	if target == nil {
		return nil
	}

	return validation.Field(target, validation.By(func(value interface{}) error {
		valueV := reflect.Indirect(reflect.ValueOf(value))
		if valueV.CanAddr() {
			addr := valueV.Addr().Interface()
			return validation.ValidateStruct(addr, fieldRules...)
		}
		return validation.ValidateStruct(target, fieldRules...)
	}))
}

// ValidateNameCharacters is a utility function to lexically validate the name field
// Currently checks for leading or trailing whitespaces
// NOTE: Can only be used in conjunction with validation.Required or with validation.When(name != nil, validation.By(util.ValidateNameCharacters))
func ValidateNameCharacters(value interface{}) error {
	s, ok := value.(string)
	var name string
	if !ok {
		// check for string pointer
		sPtr, ok := value.(*string)
		if !ok {
			return errors.New("value in name field must be a string type")
		}
		if sPtr == nil {
			return errors.New("name field cannot be nil")
		}
		name = *sPtr
	} else {
		name = s
	}
	if LeadingWhitespaceRegexp.Match([]byte(name)) {
		return ValidationErrorNameHasLeadingWhitespace
	}
	if TrailingWhitespaceRegexp.Match([]byte(name)) {
		return ValidationErrorNameHasTrailingWhitespace
	}
	return nil
}

// IsNilOrEmptyStrPtr is a utility function to check if the string pointer is nil or the underlying value is empty
func IsNilOrEmptyStrPtr(s *string) bool {
	return s == nil || *s == ""
}

// IsEmptyStrPtr is a utility function to check if the underlying value of a string pointer is empty
func IsEmptyStrPtr(s *string) bool {
	return s != nil && *s == ""
}

// ValidateSitePowerManagement checks whether a site accepts a power-management
// value. Omission preserves the current value, and an explicit empty string
// clears it, so both remain valid when DPS power management is disabled.
func ValidateSitePowerManagement(siteConfig *cdbm.SiteConfig, value *string) *cutil.APIError {
	if value == nil || *value == "" {
		return nil
	}
	if siteConfig == nil || !siteConfig.DPSPowerManagement {
		return cutil.NewAPIError(http.StatusPreconditionFailed, "Site does not have DPS power management enabled", nil)
	}
	return nil
}

// ValidateStrTime is a utility function to validate a string as a time.Time
func ValidateStrPtrTime(value interface{}) error {
	s, ok := value.(*string)
	if !ok {
		return errors.New("value must be a string pointer")
	}

	if s == nil {
		return nil
	}

	_, err := time.Parse(time.RFC3339, *s)
	if err != nil {
		return fmt.Errorf("value is not a valid RFC3339 time")
	}
	return nil
}
