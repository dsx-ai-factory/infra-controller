# Pagination Helper

The `pagination.go` file is a hand-written helper that deserializes the JSON-encoded
`X-Pagination` response header returned by list endpoints.

The API returns pagination metadata as a JSON string inside the `X-Pagination` header
rather than in the response body. The OpenAPI generator treats response headers as plain
strings and does not generate typed deserialization code for them, so this helper bridges
that gap.
