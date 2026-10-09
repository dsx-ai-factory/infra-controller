# localhost certs

This is a CA cert (`ca.crt`, `ca.key`), tls server cert (`localhost.crt`, `localhost.key`), and
client cert (`client.crt`, `client.key`), which work together for localhost as the common name.
There's nothing nico-specific about this, it's just a set of certs that work if you put the
ca.crt in your trust store.

The server certificate covers the DNS names `localhost` and `host.docker.internal`, plus the IP
addresses `127.0.0.1`, `::1`, `192.168.65.254` (Docker Desktop), and `192.168.5.2` (Colima).

To regenerate only the server certificate after it expires or its SANs change, run the following
from this directory. This keeps the existing CA, private keys, and client certificate:

```bash
touch localhost.key
./gen-certs.sh
```

If they expire, use `rm -f *.crt *.key && ./gen-certs.sh` to regenerate them.
