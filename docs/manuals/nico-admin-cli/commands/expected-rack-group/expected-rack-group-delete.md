# `nico-admin-cli expected-rack-group delete`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **delete***

## NAME

nico-admin-cli-expected-rack-group-delete - Delete an expected rack
group

## SYNOPSIS

```text
nico-admin-cli expected-rack-group delete [-h|--help]
<RACK_GROUP_ID>
```

## DESCRIPTION

Delete an expected rack group

Deletion fails while NVLink domain declarations reference the group. A
pending domain deletion continues to block group deletion.

## OPTIONS

`-h, --help`

Print help (see a summary with '-h')

`<RACK_GROUP_ID>`

External group ID to delete

## Examples

```sh
nico-admin-cli expected-rack-group delete nvl5-gp1-jhb01
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
