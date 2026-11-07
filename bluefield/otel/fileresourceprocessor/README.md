The fileresourceprocessor reads a name=value pair from a file specified in the
configuration on a regular interval until it succeeds, then applies that
name=value as a resource attribute to all processed traces, metrics, and logs.

If it fails to read the file, it continues without error and simply doesn't add
any attribute. After it succeeds, it stops polling the file and continues to
apply the name=value from that point on.

Example:
```
  fileresource:
    file_path: /run/otelcol-contrib/machine-id
    poll_interval: 5s
```
