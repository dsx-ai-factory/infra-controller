// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/spf13/cobra"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/client"
)

var ruleSetDefaultCmd = &cobra.Command{
	Use:   "set-default",
	Short: "Set a rule as the default for its operation",
	Long: `Set a rule as the default for its operation.

This will automatically unset any existing default rule for the same
operation type and operation combination.

Only one rule can be the default for each (operation_type, operation) pair.

Example:
  flow rule set-default --id abc123-def4-5678-90ab-cdef12345678`,
	RunE: runRuleSetDefault,
}

var (
	setDefaultRuleID string
)

func init() {
	ruleCmd.AddCommand(ruleSetDefaultCmd)

	ruleSetDefaultCmd.Flags().StringVar(&setDefaultRuleID, "id", "", "Rule ID (required)")

	ruleSetDefaultCmd.MarkFlagRequired("id")
}

// runRuleSetDefault is the RunE handler for ruleSetDefaultCmd. It parses the
// rule ID from the --id flag and calls SetRuleAsDefault via the client.
func runRuleSetDefault(cmd *cobra.Command, args []string) error {
	ruleID, err := uuid.Parse(setDefaultRuleID)
	if err != nil {
		return fmt.Errorf("invalid rule ID: %w", err)
	}

	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	err = flowClient.SetRuleAsDefault(context.Background(), ruleID)
	if err != nil {
		return fmt.Errorf("failed to set rule as default: %w", err)
	}

	fmt.Printf("Successfully set rule as default\n")
	fmt.Printf("Rule ID: %s\n", setDefaultRuleID)

	return nil
}
