// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"bytes"
	"fmt"
	"io"
	"math"
	"net/http"
	"os"
	"reflect"
	"runtime"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"

	"github.com/rs/zerolog/log"
	"google.golang.org/protobuf/types/known/timestamppb"
)

const (
	// SiteStatus path is status
	SiteStatus = "/status"
	// LivenessStatus path is healthz
	LivenessStatus = "/healthz"
	// ReadinessStatus path is readyz
	ReadinessStatus = "/readyz"
	// DefaultStatusPort is the port the Site Agent serves its status page and probes on
	// when ESA_PORT is unset.
	DefaultStatusPort = "8080"
	// ParamName in URI
	ParamName = "name"
)

// CompStatus Component Status is used in prometheus metrics
type CompStatus uint64

const (
	// CompUnhealthy component is unhealthy
	CompUnhealthy CompStatus = iota
	// CompHealthy component is Healthy
	CompHealthy
	// CompNotKnown component state is Not Known
	CompNotKnown
)

func (e CompStatus) String() string {
	switch e {
	case CompUnhealthy:
		return "Unhealthy"
	case CompHealthy:
		return "Healthy"
	default:
		return "NotKnown"
	}
}

const (
	// DBResDataKey - DB Resource Data key name
	DBResDataKey = "value"
	// NICoApiPageSize page sizing to use with paginated nico APIs
	NICoApiPageSize = 100
)

// GetFunctionName - Get Function Name
func GetFunctionName(temp interface{}) string {
	strs := strings.Split((runtime.FuncForPC(reflect.ValueOf(temp).Pointer()).Name()), ".")
	return strs[len(strs)-1]
}

func httpIsRetryable(response *http.Response) bool {
	if response.StatusCode >= 500 && response.StatusCode < 600 {
		return true
	}
	return false
}

// RetryWithExponentialBackoff : Interval 0.5s, 1s, 2s, 4s, 8s, 16s
func RetryWithExponentialBackoff(client *http.Client, req *http.Request,
	reqBody []byte) (*http.Response, error) {
	delayMs := 250
	backoff := float64(2)
	maxAttempts := 5
	for i := 0; i <= maxAttempts; i++ {
		resp, err := client.Do(req)
		if err != nil {
			log.Error().Msgf("client connection failed: %v", err.Error())
			return nil, err
		}
		if resp.StatusCode != http.StatusOK {
			badStatus := fmt.Errorf("bad status code: %v", resp.StatusCode)
			log.Error().Msg(badStatus.Error())
			if !httpIsRetryable(resp) {
				return nil, badStatus
			}
			req.Body = io.NopCloser(bytes.NewReader(reqBody))
			sleepTime := float64(delayMs) * math.Pow(backoff, float64(i+1))
			log.Info().Msgf("sleeping for %v", sleepTime)
			time.Sleep(time.Duration(sleepTime) * time.Millisecond)
		} else {
			return resp, nil
		}
	}
	return nil, fmt.Errorf("connection down")
}

// ConvertTimestampToVersion - Use Timestamp as resource version
func ConvertTimestampToVersion(ts *timestamppb.Timestamp) (uint64, error) {
	if err := ts.CheckValid(); err != nil {
		return 0, err
	}
	stdTime := ts.AsTime()
	return uint64(stdTime.UnixMicro()), nil
}

// StatusPort returns the port the Site Agent serves its status page and probes on.
func StatusPort() string {
	port := os.Getenv("ESA_PORT")
	if port == "" {
		return DefaultStatusPort
	}
	return port
}

func UpdateState(Elektra *elektratypes.Elektra) {
	if (CompStatus(Elektra.Managers.CoreGrpc.State.HealthStatus.Load()) == CompHealthy) &&
		(CompStatus(Elektra.Managers.Workflow.State.HealthStatus.Load()) == CompHealthy) {
		Elektra.HealthStatus.Store(uint64(CompHealthy))
	} else {
		Elektra.HealthStatus.Store(uint64(CompUnhealthy))
	}
}
