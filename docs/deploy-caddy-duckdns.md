# Scenario A: DuckDNS with public TLS

This scenario is **not turnkey in the current repository revision**. DuckDNS
can provide a hostname, but the bundled Caddyfile explicitly uses Caddy's
internal CA:

```caddyfile
{$TOKI_DOMAIN:localhost} {
    tls {$TLS_MODE:internal}
    reverse_proxy toki-sync-server:9091
}
```

`TLS_MODE` is not set by `docker-compose.yml`, so the default `internal` is
always selected. `DUCKDNS_TOKEN` is passed into the container but neither the
Caddy image nor the Caddyfile includes/uses a DuckDNS DNS provider. Following
the previous version of this guide therefore produced an untrusted certificate,
not a Let's Encrypt certificate.

## Safe choices

- Recommended: use [an existing/public reverse proxy](deploy-reverse-proxy.md)
  that already obtains a trusted certificate and can proxy both HTTPS :443 and
  TLS TCP :9090.
- For a trusted LAN/home lab, use the documented
  [internal-CA mode](deploy-self-signed.md) with toki's `--insecure` option.
- If you maintain a custom Caddy build/config, remove the forced internal CA,
  arrange a supported ACME challenge and certificate for both listeners, and
  validate it independently. That customization is outside the tested contents
  of this repository.

## DuckDNS hostname only

You may still point a DuckDNS name at the server. For a dynamic address, update
it separately, for example:

```bash
*/5 * * * * curl -s "https://www.duckdns.org/update?domains=myserver&token=YOUR_TOKEN&ip=" > /dev/null
```

This updates DNS only; it does not configure toki-sync or issue a certificate.

## Published image or source build

Use the published server image or build the release-pinned source:

```bash
docker compose pull toki-sync-server
# Alternatively: docker compose build toki-sync-server
docker compose up -d
```

If your custom TLS proxy is the bundled `caddy` service, build that service as
needed (`docker compose build caddy`). Do not describe
that result as DuckDNS/Let's Encrypt unless the certificate chain has actually
been verified.

The administration console is at `/admin`; there is no `/dashboard` route.
Remote usage queries use:

```bash
toki query --remote 'sum by (model)(toki_tokens_total)'
```
