# Admin detail pages

Resolve the primary record before rendering HTML or JSON. Return HTTP 404 for
a valid ID with no matching record. Check how the lookup represents absence
and propagate other errors. Preserve retained history and valid records with
empty child collections.

For each new admin detail page, request the page with a well-formed ID that
doesn’t exist and check that it returns HTTP 404. If the page also has a
`.json` URL, check that URL too. If the application handles deleted records
differently from IDs that never existed, also test what happens after deleting
a real record. See the [detail route tests](src/tests/component_detail.rs).
