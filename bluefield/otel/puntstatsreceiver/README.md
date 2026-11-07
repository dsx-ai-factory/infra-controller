The punt stats receiver periodically reads a file containing punt stats and
parses it to generate metrics. The file can be inside a named container.

Example:

```
receivers:
  ...
  punt_stats:
    file_path: /cumulus/nl2docad/run/stats/punt
    container_name: doca-hbn
    scrape_interval: 5m
```

The punt stats receiver generates metrics like the following:

### Punt Stats

```
punt_stats_bytes_total{component="punt_stats",dropped="false",host_name="10-217-170-242.local.nico",protocol="dhcp"} 862206
punt_stats_bytes_total{component="punt_stats",dropped="true",host_name="10-217-170-242.local.nico",protocol="dhcp"} 0
punt_stats_packets_total{component="punt_stats",dropped="false",host_name="10-217-170-242.local.nico",protocol="dhcp"} 2686
punt_stats_packets_total{component="punt_stats",dropped="true",host_name="10-217-170-242.local.nico",protocol="dhcp"} 0
```
