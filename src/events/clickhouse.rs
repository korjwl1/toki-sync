use anyhow::{Context, Result};
use super::{EventStore, ServerEvent, UserFilter};

/// ClickHouse-backed event store.
///
/// Uses ReplacingMergeTree(ts_ms) ORDER BY (device_id, msg_id) for automatic dedup.
/// Queries use FINAL keyword to get deduplicated results at read time.
///
/// ClickHouse merges happen asynchronously in the background, but FINAL forces
/// dedup at query time regardless of merge state.
pub struct ClickHouseEventStore {
    url: String,
    client: ureq::Agent,
    /// Serializes window read-merge-write cycles PER USER (see
    /// upsert_windows) — one user's slow FINAL scan or INSERT must not queue
    /// every other user's window sync behind a single global mutex.
    /// DEPLOYMENT LIMIT: process-local — the documented toki-sync topology is
    /// a single instance. Running replicas against one ClickHouse would need
    /// distributed coordination (or an append+aggregate table design).
    windows_locks: tokio::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

/// Shared by table creation and the one-time `updated_at` migration, which
/// derives the v2 table from it by name substitution — they cannot drift.
const WINDOWS_DDL: &str = "
            CREATE TABLE IF NOT EXISTS toki_windows (
                user_id String,
                provider String,
                limit_id String,
                account String,
                window_kind UInt8,
                window_end_ms Int64,
                raw_resets_at_ms Int64,
                window_minutes UInt32,
                peak_pct_x100 UInt16,
                last_pct_x100 UInt16,
                observed_ts_ms Int64,
                first_seen_ms Int64,
                finalized UInt8,
                maxed_out UInt8,
                limit_reached_kind UInt8,
                time_to_100_ms Int64,
                active_ms UInt64,
                last_sample_gap_ms Int64,
                sampled_active_fraction UInt16,
                n_samples UInt32,
                plan String,
                updated_at UInt64
            ) ENGINE = ReplacingMergeTree(updated_at)
            ORDER BY (user_id, provider, limit_id, account, window_kind, window_end_ms)
";

impl ClickHouseEventStore {
    pub fn new(url: &str) -> Result<Self> {
        let client = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let store = ClickHouseEventStore {
            url: url.trim_end_matches('/').to_string(),
            client,
            windows_locks: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        };
        // Create table on startup (idempotent)
        store.recover_interrupted_migration()?;
        store.create_table()?;
        Ok(store)
    }

    fn create_table(&self) -> Result<()> {
        let ddl = "
            CREATE TABLE IF NOT EXISTS toki_events (
                device_id String,
                user_id String,
                msg_id String,
                ts_ms Int64,
                provider String,
                model String,
                project String,
                input_tokens UInt64,
                output_tokens UInt64,
                cache_creation_input_tokens UInt64,
                cache_read_input_tokens UInt64,
                usage_total UInt64
            ) ENGINE = ReplacingMergeTree(ts_ms)
            ORDER BY (device_id, provider, msg_id)
        ";
        self.execute(ddl).context("create toki_events table")?;

        // Version note: ReplacingMergeTree keeps the row with the highest
        // version. observed_ts_ms would TIE whenever a merge changes a row
        // without advancing it (a late device contributing a higher peak),
        // leaving the winner unspecified and the update possibly lost — so the
        // version is `updated_at`, a monotonic write clock.
        // Rate-limit windows: account-level, no device_id in the key (see
        // events/mod.rs). ReplacingMergeTree keeps the row with the highest
        // observed_ts_ms per key; upsert_windows does an app-level field-wise
        // merge first and inserts the merged row (which carries max observed),
        // so the survivor is always the merged state. A concurrent upsert can
        // transiently lose one side's contribution — clients resend their full
        // recent set, so the merge converges on the next cycle.
        self.execute(WINDOWS_DDL).context("create toki_windows table")?;
        self.migrate_windows_table()?;
        Ok(())
    }

    /// Recover from a crash between the two RENAMEs of the fallback migration
    /// path. Must run BEFORE `CREATE TABLE IF NOT EXISTS`: that statement would
    /// otherwise mint an empty new-schema table, the migration check would see
    /// its `updated_at` column and return happy, and the real history would sit
    /// stranded in `toki_windows_old` forever — unrecoverable by client resends
    /// for any device that has since gone offline.
    fn recover_interrupted_migration(&self) -> Result<()> {
        let live = self.execute(
            "SELECT count() FROM system.tables WHERE database = currentDatabase() \
             AND name = 'toki_windows'",
        )?;
        if live.trim() != "0" {
            return Ok(());
        }
        for staged in ["toki_windows_v2", "toki_windows_old"] {
            let exists = self.execute(&format!(
                "SELECT count() FROM system.tables WHERE database = currentDatabase() \
                 AND name = '{staged}'"
            ))?;
            if exists.trim() == "0" {
                continue;
            }
            tracing::warn!("recovering interrupted toki_windows migration from {staged}");
            self.execute(&format!("RENAME TABLE {staged} TO toki_windows"))
                .with_context(|| format!("recover toki_windows from {staged}"))?;
            return Ok(());
        }
        Ok(())
    }

    /// `CREATE TABLE IF NOT EXISTS` is a no-op against a table created by an
    /// earlier build, so a deployment that ran before `updated_at` existed
    /// would keep the old 21-column / `ReplacingMergeTree(observed_ts_ms)`
    /// shape and fail EVERY window INSERT ("no such column: updated_at")
    /// forever — invisibly, since startup still succeeds.
    ///
    /// ClickHouse cannot change a ReplacingMergeTree version column in place,
    /// so this rebuilds: new table, copy with `observed_ts_ms AS updated_at`,
    /// swap, drop. Idempotent — a table that already has the column returns
    /// immediately, which is the only path a fresh install ever takes.
    fn migrate_windows_table(&self) -> Result<()> {
        let has_col = self.execute(
            "SELECT count() FROM system.columns WHERE database = currentDatabase() \
             AND table = 'toki_windows' AND name = 'updated_at'",
        )?;
        if has_col.trim() != "0" {
            return Ok(());
        }
        tracing::warn!("toki_windows predates updated_at; rebuilding table (one-time migration)");
        self.execute("DROP TABLE IF EXISTS toki_windows_v2")
            .context("drop stale migration table")?;
        let v2_ddl = WINDOWS_DDL.replace("toki_windows", "toki_windows_v2");
        self.execute(&v2_ddl).context("create toki_windows_v2")?;
        // Old rows have no write clock; observed_ts_ms is the closest
        // monotonic stand-in and is what the old engine versioned on.
        self.execute(
            "INSERT INTO toki_windows_v2 SELECT *, toUInt64(observed_ts_ms) AS updated_at \
             FROM toki_windows",
        )
        .context("copy toki_windows rows")?;
        // EXCHANGE requires the Atomic database engine (ClickHouse 20.7+).
        // On Ordinary it errors, so fall back to the two-step RENAME — briefly
        // table-less, but the alternative is a server that cannot migrate at
        // all. A crash before this point simply re-runs the whole migration on
        // restart (the detection query still sees no `updated_at`), so the
        // sequence is idempotent.
        if self.execute("EXCHANGE TABLES toki_windows AND toki_windows_v2").is_err() {
            tracing::warn!("EXCHANGE TABLES unavailable; falling back to RENAME");
            self.execute("RENAME TABLE toki_windows TO toki_windows_old")
                .context("rename old toki_windows")?;
            self.execute("RENAME TABLE toki_windows_v2 TO toki_windows")
                .context("rename new toki_windows")?;
            self.execute("DROP TABLE IF EXISTS toki_windows_old")
                .context("drop old toki_windows")?;
            tracing::info!("toki_windows migration complete (RENAME path)");
            return Ok(());
        }
        self.execute("DROP TABLE IF EXISTS toki_windows_v2")
            .context("drop old toki_windows")?;
        tracing::info!("toki_windows migration complete");
        Ok(())
    }

    /// Reverse ClickHouse TSV escaping (\\, \t, \n, \r). Without it a stored
    /// string containing a backslash never compares equal to its re-read
    /// form, and the skip-identical check re-inserts that row every cycle.
    fn tsv_unescape(s: &str) -> String {
        if !s.contains('\\') {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }

    fn window_from_row(cols: &[&str]) -> Option<(String, toki_sync_protocol::WireWindow)> {
        // TSV column order matches the SELECT in query_user_windows.
        if cols.len() < 21 { return None; }
        Some((
            Self::tsv_unescape(cols[1]),
            toki_sync_protocol::WireWindow {
                window_kind: cols[4].parse().ok()?,
                limit_id: Self::tsv_unescape(cols[2]),
                account: Self::tsv_unescape(cols[3]),
                window_end_ms: cols[5].parse().ok()?,
                raw_resets_at_ms: cols[6].parse().ok()?,
                window_minutes: cols[7].parse().ok()?,
                peak_pct_x100: cols[8].parse().ok()?,
                last_pct_x100: cols[9].parse().ok()?,
                observed_ts_ms: cols[10].parse().ok()?,
                first_seen_ms: cols[11].parse().ok()?,
                finalized: cols[12] == "1",
                maxed_out: cols[13] == "1",
                limit_reached_kind: cols[14].parse().ok()?,
                time_to_100_ms: cols[15].parse().ok()?,
                active_ms: cols[16].parse().ok()?,
                last_sample_gap_ms: cols[17].parse().ok()?,
                sampled_active_fraction: cols[18].parse().ok()?,
                n_samples: cols[19].parse().ok()?,
                plan: Self::tsv_unescape(cols[20]),
            },
        ))
    }

    const WINDOW_COLS: &'static str =
        "user_id, provider, limit_id, account, window_kind, window_end_ms, raw_resets_at_ms, \
         window_minutes, peak_pct_x100, last_pct_x100, observed_ts_ms, first_seen_ms, finalized, \
         maxed_out, limit_reached_kind, time_to_100_ms, active_ms, last_sample_gap_ms, \
         sampled_active_fraction, n_samples, plan";

    fn execute(&self, query: &str) -> Result<String> {
        let resp = self.client.post(&self.url)
            .set("Content-Type", "text/plain")
            .send_string(query)
            .map_err(|e| anyhow::anyhow!("ClickHouse query failed: {e}"))?;
        let body = resp.into_string().context("read ClickHouse response")?;
        Ok(body)
    }

    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\")
         .replace('\'', "\\'")
         .replace('\0', "")
         .replace('\n', "\\n")
         .replace('\r', "\\r")
         .replace('\t', "\\t")
    }
}

impl ClickHouseEventStore {
    /// Window query with an optional provider predicate (index-aligned with
    /// the table's ORDER BY).
    async fn query_user_windows_filtered(
        &self,
        user_id: &str,
        provider: Option<&str>,
        since_ms: i64,
    ) -> Result<Vec<(String, toki_sync_protocol::WireWindow)>> {
        let provider_clause = provider
            .map(|p| format!(" AND provider = '{}'", Self::escape(p)))
            .unwrap_or_default();
        let sql = format!(
            "SELECT {} FROM toki_windows FINAL WHERE user_id = '{}'{} AND window_end_ms >= {} FORMAT TSV",
            Self::WINDOW_COLS,
            Self::escape(user_id),
            provider_clause,
            since_ms,
        );
        let client = self.client.clone();
        let url = self.url.clone();
        let body = tokio::task::spawn_blocking(move || -> Result<String> {
            let resp = client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse windows SELECT failed: {e}"))?;
            resp.into_string().context("read ClickHouse response")
        })
        .await
        .context("spawn_blocking panicked")??;

        let mut out = Vec::new();
        for line in body.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            if let Some(pair) = Self::window_from_row(&cols) {
                out.push(pair);
            }
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl EventStore for ClickHouseEventStore {
    async fn upsert_events(&self, events: &[ServerEvent]) -> Result<()> {
        if events.is_empty() { return Ok(()); }

        // Build INSERT with VALUES — ClickHouse ReplacingMergeTree handles dedup
        let mut sql = String::from(
            "INSERT INTO toki_events (device_id, user_id, msg_id, ts_ms, provider, model, project, \
             input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, usage_total) VALUES "
        );

        for (i, e) in events.iter().enumerate() {
            if i > 0 { sql.push(','); }
            sql.push_str(&format!(
                "('{}','{}','{}',{},'{}','{}','{}',{},{},{},{},{})",
                Self::escape(&e.device_id),
                Self::escape(&e.user_id),
                Self::escape(&e.msg_id),
                e.ts_ms,
                Self::escape(&e.provider),
                Self::escape(&e.model),
                Self::escape(&e.project),
                e.input_tokens,
                e.output_tokens,
                e.cache_creation_input_tokens,
                e.cache_read_input_tokens,
                e.usage_total,
            ));
        }

        let client_url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            client.post(&client_url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse INSERT failed: {e}"))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn query_events(
        &self,
        since_ms: i64,
        until_ms: i64,
        filter: UserFilter,
    ) -> Result<Vec<ServerEvent>> {
        let user_clause = match &filter {
            UserFilter::Single(uid) => format!("AND user_id = '{}'", Self::escape(uid)),
            UserFilter::Multiple(uids) => {
                let list: Vec<String> = uids.iter().map(|u| format!("'{}'", Self::escape(u))).collect();
                format!("AND user_id IN ({})", list.join(","))
            }
            UserFilter::All => String::new(),
        };

        let sql = format!(
            "SELECT device_id, user_id, msg_id, ts_ms, provider, model, project, \
             input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, usage_total \
             FROM toki_events FINAL \
             WHERE ts_ms >= {since_ms} AND ts_ms < {until_ms} {user_clause} \
             ORDER BY ts_ms \
             FORMAT JSONEachRow"
        );

        let url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            let resp = client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse SELECT failed: {e}"))?;
            let body = resp.into_string().context("read ClickHouse response")?;

            let mut events = Vec::new();
            for line in body.lines() {
                if line.is_empty() { continue; }
                let e: ServerEvent = serde_json::from_str(line)
                    .with_context(|| format!("parse ClickHouse row: {}", &line[..line.len().min(100)]))?;
                events.push(e);
            }
            Ok(events)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn cleanup_old_dedup(&self, _device_id: &str, _cutoff_ms: i64) -> Result<()> {
        // No-op: ClickHouse handles dedup via ReplacingMergeTree, no idx_msg to clean up.
        Ok(())
    }

    async fn delete_device_events(&self, device_id: &str) -> Result<()> {
        // Validate device_id format (UUID) to prevent SQL injection
        if !device_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            anyhow::bail!("invalid device_id format: {}", device_id);
        }
        let sql = format!(
            "ALTER TABLE toki_events DELETE WHERE device_id = '{}'",
            Self::escape(device_id)
        );

        let url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse DELETE failed: {e}"))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn upsert_windows(
        &self,
        user_id: &str,
        provider: &str,
        items: &[toki_sync_protocol::WireWindow],
    ) -> Result<usize> {
        if items.is_empty() { return Ok(0); }

        // Serialize read-merge-write per user: two devices of one user
        // upserting concurrently could otherwise transiently lose one side's
        // contribution (fjall has its windows lock; this is the CH
        // equivalent, keyed so users never queue behind each other).
        let user_lock = {
            let mut locks = self.windows_locks.lock().await;
            // Prune stale entries opportunistically (bounded growth).
            if locks.len() > 10_000 {
                locks.retain(|_, l| std::sync::Arc::strong_count(l) > 1);
            }
            locks
                .entry(user_id.to_string())
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = user_lock.lock().await;

        // Read the current merged state — bounded to the incoming anchors'
        // range so the FINAL (merge-on-read) scan never walks the user's full
        // multi-year history on every 5-minute upload.
        let since = items.iter().map(|w| w.window_end_ms).min().unwrap_or(0);
        // Provider-scoped: the premerge only needs THIS provider's rows, and
        // the table's ORDER BY leads with (user_id, provider) so the
        // predicate is index-aligned. Reading every provider made the cost
        // P²n per change round instead of Pn.
        let existing = self
            .query_user_windows_filtered(user_id, Some(provider), since)
            .await?;
        let mut by_key: std::collections::HashMap<(u8, &str, &str, i64), &toki_sync_protocol::WireWindow> =
            std::collections::HashMap::with_capacity(existing.len());
        for (p, e) in &existing {
            if p == provider {
                by_key.insert((e.window_kind, e.limit_id.as_str(), e.account.as_str(), e.window_end_ms), e);
            }
        }
        let mut merged: Vec<toki_sync_protocol::WireWindow> = Vec::with_capacity(items.len());
        for w in items {
            match by_key.get(&(w.window_kind, w.limit_id.as_str(), w.account.as_str(), w.window_end_ms)) {
                Some(prev) => {
                    let mut base = (*prev).clone();
                    super::merge_wire_windows(&mut base, w);
                    // Skip rows whose merged state equals what is already
                    // stored: with cursorless 60-day resends, re-inserting
                    // unchanged rows multiplies ReplacingMergeTree versions
                    // by orders of magnitude between background merges.
                    if &&base == prev {
                        continue;
                    }
                    merged.push(base);
                }
                None => merged.push(w.clone()),
            }
        }
        if merged.is_empty() { return Ok(0); }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut sql = format!("INSERT INTO toki_windows ({}, updated_at) VALUES ", Self::WINDOW_COLS);
        for (i, w) in merged.iter().enumerate() {
            if i > 0 { sql.push(','); }
            sql.push_str(&format!(
                "('{}','{}','{}','{}',{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},'{}',{})",
                Self::escape(user_id),
                Self::escape(provider),
                Self::escape(&w.limit_id),
                Self::escape(&w.account),
                w.window_kind,
                w.window_end_ms,
                w.raw_resets_at_ms,
                w.window_minutes,
                w.peak_pct_x100,
                w.last_pct_x100,
                w.observed_ts_ms,
                w.first_seen_ms,
                w.finalized as u8,
                w.maxed_out as u8,
                w.limit_reached_kind,
                w.time_to_100_ms,
                w.active_ms,
                w.last_sample_gap_ms,
                w.sampled_active_fraction,
                w.n_samples,
                Self::escape(&w.plan),
                now_ms,
            ));
        }

        let client = self.client.clone();
        let url = self.url.clone();
        tokio::task::spawn_blocking(move || {
            client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse windows INSERT failed: {e}"))?;
            Ok(0)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn query_user_windows(
        &self,
        user_id: &str,
        since_ms: i64,
    ) -> Result<Vec<(String, toki_sync_protocol::WireWindow)>> {
        self.query_user_windows_filtered(user_id, None, since_ms).await
    }

    async fn cleanup_old_windows(&self, cutoff_ms: i64) -> Result<usize> {
        // ALTER ... DELETE is a part-rewriting mutation — with 730-day
        // retention it would be a queued no-op every 6 hours for the table's
        // first two years. Only mutate when matching rows actually exist.
        // No FINAL on the gate: as a GATE the plain count is exactly as
        // correct (zero iff no physical row matches iff the mutation has
        // nothing to do), and FINAL would force a merging read of all six
        // ORDER BY columns plus the version across every part — every 6 hours,
        // for two years, to compute a number that is always 0. The returned
        // figure can over-report un-merged duplicates on the rare tick where
        // the gate actually fires; that is a log line, not a decision.
        let count_sql = format!(
            "SELECT count() FROM toki_windows WHERE window_end_ms < {} FORMAT TSV",
            cutoff_ms,
        );
        let delete_sql = format!(
            "ALTER TABLE toki_windows DELETE WHERE window_end_ms < {}",
            cutoff_ms,
        );
        let client = self.client.clone();
        let url = self.url.clone();
        tokio::task::spawn_blocking(move || {
            let body = client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&count_sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse windows count failed: {e}"))?
                .into_string()
                .unwrap_or_default();
            let stale: usize = body.trim().parse().unwrap_or(0);
            if stale == 0 {
                return Ok(0);
            }
            client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&delete_sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse windows cleanup failed: {e}"))?;
            Ok(stale)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn delete_user_windows(&self, user_id: &str) -> Result<()> {
        let sql = format!(
            "ALTER TABLE toki_windows DELETE WHERE user_id = '{}'",
            Self::escape(user_id),
        );
        let client = self.client.clone();
        let url = self.url.clone();
        tokio::task::spawn_blocking(move || {
            client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse windows DELETE failed: {e}"))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }
}
