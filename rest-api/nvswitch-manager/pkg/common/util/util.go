// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"

	log "github.com/sirupsen/logrus"
)

// PrintPrettyResponse prints HTTP response status, headers, and attempts to pretty-print JSON bodies.
func PrintPrettyResponse(resp *http.Response) {
	// Print status
	fmt.Printf("Status: %s\n", resp.Status)

	// Print headers
	fmt.Println("Headers:")
	for key, value := range resp.Header {
		fmt.Printf("  %s: %s\n", key, value)
	}

	// Print body
	fmt.Println("Body:")
	bodyBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		log.Fatalf("failed to read response body: %v\n", err)
	}
	defer resp.Body.Close()

	var prettyJSON bytes.Buffer
	if err := json.Indent(&prettyJSON, bodyBytes, "", "  "); err != nil {
		// If the body is not JSON, just print it as a string
		fmt.Printf("Body is not JSON: %s\n", string(bodyBytes))
		return
	}

	fmt.Println(prettyJSON.String())
}
