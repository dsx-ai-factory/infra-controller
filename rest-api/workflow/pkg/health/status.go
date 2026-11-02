// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package health

import (
	"encoding/json"
	"net/http"

	"github.com/rs/zerolog/log"
)

// Check captures the API response for workflow service health check
type Check struct {
	IsHealthy bool    `json:"is_healthy"`
	Error     *string `json:"error"`
}

// StatusHandler is an API handler to return health status of the workflow service
func StatusHandler(w http.ResponseWriter, r *http.Request) {
	check := Check{
		IsHealthy: true,
	}
	bytes, err := json.Marshal(check)
	if err != nil {
		log.Error().Err(err).Msg("error converting health check object into JSON")
		http.Error(w, "failed to construct health check response", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_, err = w.Write(bytes)
	if err != nil {
		log.Error().Err(err).Msg("failed to return health check response")
	}
}
