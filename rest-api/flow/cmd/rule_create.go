// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"

	"github.com/spf13/cobra"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var ruleCreateCmd = &cobra.Command{
	Use:   "create",
	Short: "Create operation rule(s)",
	Long: `Create one or more operation rules.

Single rule mode (create one rule):
  flow rule create \
    --name "My Rule" \
    --description "..." \
    --operation-type power_control \
    --operation power_on \
    --rule-file steps.json \
    --is-default

Batch mode (create multiple rules from YAML):
  flow rule create --from-yaml examples/operation-rules-example.yaml

Batch mode with dry-run (validate without creating):
  flow rule create --from-yaml examples/operation-rules-example.yaml --dry-run

Batch mode with overwrite (replace existing rules):
  flow rule create --from-yaml examples/operation-rules-example.yaml --overwrite

The --dry-run flag validates rules without creating them (preview mode).
The --overwrite flag will replace existing rules with the same operation_type and operation.`,
	RunE: runRuleCreate,
}

var (
	createName        string
	createDescription string
	createOpType      string
	createOperation   string
	createRuleFile    string
	createIsDefault   bool
	createFromYAML    string
	createOverwrite   bool
	createDryRun      bool
)

func init() {
	ruleCmd.AddCommand(ruleCreateCmd)

	// Single rule mode flags
	ruleCreateCmd.Flags().StringVar(&createName, "name", "", "Rule name (required for single mode)")
	ruleCreateCmd.Flags().StringVar(&createDescription, "description", "", "Rule description")
	ruleCreateCmd.Flags().StringVar(&createOpType, "operation-type", "", "Operation type: power_control or firmware_control (required for single mode)")
	ruleCreateCmd.Flags().StringVar(&createOperation, "operation", "", "Operation name: power_on, power_off, upgrade, etc. (required for single mode)")
	ruleCreateCmd.Flags().StringVar(&createRuleFile, "rule-file", "", "Path to JSON file containing rule definition steps (required for single mode)")
	ruleCreateCmd.Flags().BoolVar(&createIsDefault, "is-default", false, "Set as default rule for this operation")

	// Batch mode flags
	ruleCreateCmd.Flags().StringVar(&createFromYAML, "from-yaml", "", "Path to YAML file with complete rules (batch mode)")
	ruleCreateCmd.Flags().BoolVar(&createOverwrite, "overwrite", false, "Overwrite existing rules (only valid with --from-yaml)")
	ruleCreateCmd.Flags().BoolVar(&createDryRun, "dry-run", false, "Validate rules without creating them (only valid with --from-yaml)")

	// Make flags mutually exclusive
	ruleCreateCmd.MarkFlagsMutuallyExclusive("name", "from-yaml")
	ruleCreateCmd.MarkFlagsMutuallyExclusive("rule-file", "from-yaml")
	ruleCreateCmd.MarkFlagsMutuallyExclusive("operation-type", "from-yaml")
	ruleCreateCmd.MarkFlagsMutuallyExclusive("operation", "from-yaml")
	ruleCreateCmd.MarkFlagsMutuallyExclusive("is-default", "from-yaml")
}

// runRuleCreate is the RunE handler for ruleCreateCmd. It dispatches to
// createRulesFromYAML (batch mode) or createSingleRule (single mode).
func runRuleCreate(cmd *cobra.Command, args []string) error {
	if createFromYAML != "" {
		return createRulesFromYAML()
	}
	return createSingleRule()
}

// createSingleRule creates one operation rule using the individual flag values
// (--name, --operation-type, --operation, --rule-file).
func createSingleRule() error {
	// Validate required flags for single mode
	if createName == "" {
		return fmt.Errorf("--name is required for single rule mode")
	}
	if createOpType == "" {
		return fmt.Errorf("--operation-type is required for single rule mode")
	}
	if createOperation == "" {
		return fmt.Errorf("--operation is required for single rule mode")
	}
	if createRuleFile == "" {
		return fmt.Errorf("--rule-file is required for single rule mode")
	}

	// Read rule definition from file
	ruleDefBytes, err := os.ReadFile(createRuleFile)
	if err != nil {
		return fmt.Errorf("failed to read rule file: %w", err)
	}

	// Validate JSON
	var ruleDefJSON map[string]interface{}
	if err := json.Unmarshal(ruleDefBytes, &ruleDefJSON); err != nil {
		return fmt.Errorf("invalid JSON in rule file: %w", err)
	}

	// Convert operation type
	var opType types.OperationType
	switch createOpType {
	case "power_control":
		opType = types.OperationTypePowerControl
	case "firmware_control":
		opType = types.OperationTypeFirmwareControl
	default:
		return fmt.Errorf("invalid operation type: %s (must be power_control or firmware_control)", createOpType)
	}

	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	ruleID, err := flowClient.CreateOperationRule(
		context.Background(),
		createName,
		createDescription,
		opType,
		createOperation,
		string(ruleDefBytes),
		createIsDefault,
	)
	if err != nil {
		return fmt.Errorf("failed to create rule: %w", err)
	}

	fmt.Printf("Successfully created rule\n")
	fmt.Printf("ID:   %s\n", ruleID.String())
	fmt.Printf("Name: %s\n", createName)

	return nil
}

// createRulesFromYAML loads rules from the YAML file specified by --from-yaml
// and creates each rule via the gRPC client. Supports --dry-run and --overwrite.
func createRulesFromYAML() error {
	// Validate flags
	if createOverwrite && createFromYAML == "" {
		return fmt.Errorf("--overwrite can only be used with --from-yaml")
	}
	if createDryRun && createFromYAML == "" {
		return fmt.Errorf("--dry-run can only be used with --from-yaml")
	}
	if createDryRun && createOverwrite {
		return fmt.Errorf("--dry-run and --overwrite cannot be used together")
	}

	// Load YAML using YAMLRuleLoader
	fmt.Printf("Loading rules from: %s\n\n", createFromYAML)
	loader, err := operationrules.NewYAMLRuleLoader(createFromYAML)
	if err != nil {
		return fmt.Errorf("failed to create loader: %w", err)
	}

	rules, err := loader.Load()
	if err != nil {
		return fmt.Errorf("❌ Validation failed: %w", err)
	}

	// Dry-run mode: validate and show what would be created
	if createDryRun {
		return showDryRunOutput(rules)
	}

	// Create client
	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	ctx := context.Background()
	created := 0
	skipped := 0
	overwritten := 0

	// For each rule, create via API
	for opType, opRules := range rules {
		for operation, rule := range opRules {
			// Check if rule already exists by listing and filtering
			existing, err := findExistingRule(ctx, flowClient, opType, operation)
			if err != nil {
				return fmt.Errorf("failed to check existing rule %s (%s/%s): %w", rule.Name, opType, operation, err)
			}

			if existing != nil {
				if !createOverwrite {
					fmt.Printf("⏭️  Skipped: %s (%s/%s) - already exists\n",
						rule.Name, opType, operation)
					skipped++
					continue
				}

				// Delete existing rule before creating new one
				err := flowClient.DeleteOperationRule(ctx, existing.ID)
				if err != nil {
					return fmt.Errorf("failed to delete existing rule %s: %w", rule.Name, err)
				}
				fmt.Printf("🔄 Overwriting: %s (%s/%s)\n",
					rule.Name, opType, operation)
				overwritten++
			}

			// Create rule
			ruleDefJSON, err := json.Marshal(rule.RuleDefinition)
			if err != nil {
				return fmt.Errorf("failed to marshal rule definition for %s: %w", rule.Name, err)
			}

			_, err = flowClient.CreateOperationRule(
				ctx,
				rule.Name,
				rule.Description,
				taskTypeToOperationType(opType),
				operation,
				string(ruleDefJSON),
				rule.IsDefault,
			)
			if err != nil {
				return fmt.Errorf("failed to create rule %s: %w", rule.Name, err)
			}

			fmt.Printf("✅ Created: %s (%s/%s)\n", rule.Name, opType, operation)
			created++
		}
	}

	// Print summary
	fmt.Println()
	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
	if created > 0 {
		fmt.Printf("✅ Created: %d rules\n", created)
	}
	if overwritten > 0 {
		fmt.Printf("🔄 Overwritten: %d rules\n", overwritten)
	}
	if skipped > 0 {
		fmt.Printf("⏭️  Skipped: %d rules (already exist)\n", skipped)
	}

	return nil
}

// showDryRunOutput prints a preview of the rules that would be created,
// without making any API calls.
func showDryRunOutput(rules map[taskcommon.TaskType]map[string]*operationrules.OperationRule) error {
	fmt.Println("✅ Validation successful!")
	fmt.Println()
	fmt.Println("Would create the following rules:")
	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
	fmt.Println()

	totalRules := 0
	for opType, opRules := range rules {
		fmt.Printf("%s:\n", opType)
		for operation, rule := range opRules {
			totalRules++
			fmt.Printf("  • %s: %s", operation, rule.Name)
			if rule.Description != "" {
				fmt.Printf(" - %s", rule.Description)
			}
			fmt.Printf(" (%d stages", len(rule.RuleDefinition.Steps))
			if rule.IsDefault {
				fmt.Printf(", default")
			}
			fmt.Println(")")
		}
		fmt.Println()
	}

	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
	fmt.Printf("Would create: %d rules\n", totalRules)
	fmt.Println()
	fmt.Println("To actually create these rules, run without --dry-run:")
	fmt.Printf("  flow rule create --from-yaml %s\n", createFromYAML)

	return nil
}

// findExistingRule searches the server's rule list for a rule matching the
// given operation type and operation code. Returns (nil, nil) if no matching
// rule is found, or (nil, err) if the underlying list call fails.
func findExistingRule(ctx context.Context, flowClient *client.Client, opType taskcommon.TaskType, operation string) (*types.OperationRule, error) {
	rules, _, err := flowClient.ListOperationRules(ctx, nil, nil, nil, nil)
	if err != nil {
		return nil, err
	}

	typesOpType := taskTypeToOperationType(opType)
	for _, rule := range rules {
		if rule.OperationType == typesOpType && rule.OperationCode == operation {
			return rule, nil
		}
	}

	return nil, nil
}

// taskTypeToOperationType converts a taskcommon.TaskType to the corresponding
// types.OperationType used in the Flow API.
func taskTypeToOperationType(taskType taskcommon.TaskType) types.OperationType {
	switch taskType {
	case taskcommon.TaskTypePowerControl:
		return types.OperationTypePowerControl
	case taskcommon.TaskTypeFirmwareControl:
		return types.OperationTypeFirmwareControl
	default:
		return types.OperationTypeUnknown
	}
}
