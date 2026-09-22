// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package identifier

import (
	"errors"

	"github.com/google/uuid"
)

type Identifier struct {
	ID         uuid.UUID `json:"id"`
	Name       string    `json:"name"`
	ExternalID string    `json:"external_id"`
}

func New(id uuid.UUID, name string) *Identifier {
	return &Identifier{
		ID:   id,
		Name: name,
	}
}

func (id *Identifier) Validate() error {
	if id == nil {
		return errors.New("identifier is not specfied")
	}

	if id.Name == "" {
		return errors.New("identifier name is not specfied")
	}

	if id.ID == uuid.Nil {
		return errors.New("identifier id is not specfied")
	}

	return nil
}

func (id *Identifier) ValidateAtLeastOne() bool {
	return id != nil && (id.ID != uuid.Nil || id.Name != "" || id.ExternalID != "")
}
