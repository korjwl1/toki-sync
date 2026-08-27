# Custom dashboards

Build a client against toki-sync's authenticated virtual-query endpoint. The
server does not expose `/api/v1/query`, `/api/v1/query_range`, a PromQL proxy,
or a VictoriaMetrics configuration. The only usage-query route currently
implemented is `GET /api/v1/toki/query`.

## Architecture

```text
Browser or backend
    |  Authorization: Bearer <access token>
    v
toki-sync /api/v1/toki/query
    |
    +-- Fjall or ClickHouse EventStore
```

For a browser-only UI, obtain tokens from `POST /login` and keep the refresh
token out of logs and URLs. A custom backend is safer when you need to combine
data sources or keep credentials out of frontend storage. It can forward the
user's access token to toki-sync; do not mint wider queries on behalf of a user.

## Authentication

```http
POST /login
Content-Type: application/json

{"username":"alice","password":"..."}
```

Use the returned access token:

```http
Authorization: Bearer <access_token>
```

Rotate an expired token pair with `POST /token/refresh`. See [API.md](API.md)
for the exact request/response contracts.

## Querying

An instant aggregate omits `step`:

```http
GET /api/v1/toki/query?query=sum%20by%20(model)(usage)&start=1787788800&end=1787875200&scope=self
```

A chart supplies `step`:

```http
GET /api/v1/toki/query?query=sum%20by%20(model)(usage)&start=20260801&end=20260827&step=1d&tz=Asia%2FSeoul&scope=self
```

Supported query building blocks are:

- metrics: `usage` (or legacy `toki_tokens_total`), `cost`, and `events`;
- optional `sum(...)` and `increase(...)` wrappers;
- a single equality filter, `provider="..."`;
- one grouping dimension: `model`, `project`, or `device_id` (`type` may be
  present alongside it but token kinds are emitted as fields);
- bare `windows`, with `scope=self` only.

The range selector inside a query is syntax-checked, but the HTTP `start`,
`end`, and `step` parameters control the actual scan and buckets. Unsupported
PromQL functions, arbitrary labels, regex matchers, `offset`, `sessions`, and
`projects` return `400`.

## Scope

| Scope | Data | Requirement |
|---|---|---|
| `self` | Authenticated user's events | Always available |
| `team:<team_id>` | Current members of one team | Non-admin must be a member and server maximum must allow `team` or `all` |
| `all` | All users | Non-admin requires server maximum `all` |

Admins bypass the configured maximum, but the requested scope still narrows the
result. An admin requesting `self` receives only their own data.

## Response handling

Aggregates use toki's provider-grouped JSON shape:

```json
{
  "providers": {
    "claude_code": [
      {
        "period": "2026-08-01T00:00:00|claude-opus-4-6",
        "usage_per_models": [
          {
            "model": "claude-opus-4-6",
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 50,
            "total_tokens": 170,
            "events": 2,
            "cost_usd": 0.001
          }
        ]
      }
    ]
  }
}
```

Do not assume the field named `model` always contains a model: for project or
device grouping it contains that selected group key for local-client
compatibility. Codex entries use `cached_input_tokens` and
`reasoning_output_tokens` instead of the Claude cache field names.

## Limits you must surface

- Time-series queries are limited to 2,000 time buckets.
- The server reads at most 200,000 input events per request; there is no cursor.
- Bare raw `events` can return top-level `"truncated": true`.
- Aggregates currently do not propagate the 200,000-input truncation flag, so a
  wide aggregate may look complete when it is not. Use bounded time windows.
- Aggregation reports `"truncated": true` if it exceeds 50,000 distinct
  `(bucket, group)` combinations.
- ClickHouse raw scans may return `502` at the adapter's 10 MiB buffered response
  limit before the event ceiling. Retry with a narrower range.
- RFC 3339 and dashed dates are not accepted remotely. Use epoch seconds,
  13-digit epoch milliseconds, `YYYYMMDD`, or `YYYYMMDDhhmmss`.
- The server range is `[start, end)`, except a date-only end is promoted to the
  next local midnight so that date is included.

Treat an absent `truncated` field as inconclusive for wide aggregates, not proof
that every matching event was counted.

## Security notes

- Only token counts and metadata are stored; prompts and responses are not.
- Never call the EventStore database directly from an untrusted frontend.
- Do not expose toki-sync's plain ports to the Internet. Terminate TLS for both
  HTTP 9091 and TCP 9090 in a trusted reverse proxy.
- The built-in `/admin` page is an administration console, not a usage chart UI.

For full parameters and response shapes, see [API.md](API.md).
