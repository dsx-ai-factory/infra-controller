# Operation Rules Versioning

Rule definitions use the `version` field to identify their persisted JSON
format. `v1` is the only supported version. See the
[Operation Rules guide](../../../docs/operations/flow/operation-rules.md)
for the schema and executable examples.

## Read and write behavior

The implementation is in
[`internal/task/operationrules/rules.go`](../internal/task/operationrules/rules.go).

| Path | Omitted or empty version | Explicit version |
|---|---|---|
| `MarshalRuleDefinition` | Writes `v1` | Accepts only `v1`; rejects other versions before serialization. |
| `UnmarshalRuleDefinition` | Reads as `v1` for compatibility with unversioned records | Dispatches `v1` to its decoder; rejects other versions. |

Malformed JSON returns an error. Version decoding does not replace rule
validation: action names, component types, stages, and retry settings are
validated separately. Unsupported versions are not silently converted or
stored for a future reader.

## Adding a version

A new version requires coordinated reader, writer, and validation changes:

1. Define the new representation and its conversion from supported persisted versions.
2. Add a version-specific decoder and retain readers needed by existing records.
3. Update the writer's supported version and validation together.
4. Test legacy records, round trips, malformed input, and unsupported versions.

Define upgrade and rollback compatibility before changing stored records.
Changing `CurrentRuleDefinitionVersion` alone does not migrate existing data
or add a decoder. No `v2` format or automatic `v1`-to-`v2` migration is implemented.
