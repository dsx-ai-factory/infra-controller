// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"bufio"
	"bytes"
	"fmt"
	"io/ioutil"
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
	// VPCStatus path is status-vpc
	VPCStatus = "/status-vpc"
	// SubnetStatus path is status-subnet
	SubnetStatus = "/status-subnet"
	// InstanceStatus path is status-instance
	InstanceStatus = "/status-instance"
	// MachineStatus path is status-machine
	MachineStatus = "/status-machine"
	// DatastoreStatus path is status-datastore
	DatastoreStatus = "/status-datastore"
	// InfiniBandPartitionStatus path is status-infinibandpartition"
	InfiniBandPartitionStatus = "/status-infinibandpartition"
	// SSHKeyGroupStatus path is status-sshkeygroup"
	SSHKeyGroupStatus = "/status-sshkeygroup"
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
			req.Body = ioutil.NopCloser(bytes.NewReader(reqBody))
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

// GetSAStatus - Get Status from elektra agent
func GetSAStatus(path string) {
	addr := os.Getenv("ESA_PORT")
	resp, err := http.Get("http://localhost:" + addr + path)
	if err != nil {
		fmt.Println(err.Error())
		return
	}
	defer resp.Body.Close()
	scanner := bufio.NewScanner(resp.Body)
	for scanner.Scan() {
		fmt.Println(scanner.Text())
	}
	if err = scanner.Err(); err != nil {
		fmt.Println(err.Error())
	}
}

// SiteHealth derives aggregate health from Core, Temporal, and enabled Flow state.
// No aggregate is cached, so concurrent updates cannot leave a stale stored result.
func SiteHealth(Elektra *elektratypes.Elektra) CompStatus {
	coreHealthy := CompStatus(Elektra.Managers.CoreGrpc.State.HealthStatus.Load()) == CompHealthy
	temporalHealthy := CompStatus(Elektra.Managers.Workflow.State.HealthStatus.Load()) == CompHealthy
	flowHealthy := !Elektra.Conf.FlowGrpc.Enabled ||
		CompStatus(Elektra.Managers.FlowGrpc.State.HealthStatus.Load()) == CompHealthy
	if coreHealthy && temporalHealthy && flowHealthy {
		return CompHealthy
	}
	return CompUnhealthy
}
