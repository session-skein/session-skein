use rusqlite::Connection;
use serde::Serialize;

use crate::Registry;
use crate::Result;

/// Default maximum covered age after which a source is labeled stale.
pub const DEFAULT_STALE_AFTER_SECONDS: i64 = 24 * 60 * 60;

/// Stable interpretation of one source's durable observation coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Empty,
    Missing,
    Fresh,
    Stale,
}

/// Read-only freshness projection for one durable source family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFreshness {
    pub state: FreshnessState,
    pub items: usize,
    pub missing: usize,
    pub latest_observed_at: Option<i64>,
    pub latest_age_seconds: Option<i64>,
    pub oldest_observed_at: Option<i64>,
    pub max_age_seconds: Option<i64>,
}

/// Read-only catalog freshness, derived entirely from existing timestamps.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFreshness {
    pub state: FreshnessState,
    pub checked_at: i64,
    pub stale_after_seconds: i64,
    pub may_be_stale: bool,
    pub git: SourceFreshness,
    pub documents: SourceFreshness,
    pub sessions: SourceFreshness,
    pub context: SourceFreshness,
}

impl Registry {
    /// Summarize existing observations without touching repositories or source adapters.
    pub fn catalog_freshness(
        &self,
        now: i64,
        stale_after_seconds: i64,
    ) -> Result<CatalogFreshness> {
        let threshold = stale_after_seconds.max(0);
        let project_count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
        let git = source_with_expected(
            &self.connection,
            "SELECT COUNT(*), MIN(refreshed_at), MAX(refreshed_at) FROM project_metadata",
            project_count,
            now,
            threshold,
        )?;
        let documents = source_with_expected(
            &self.connection,
            "SELECT COUNT(*), MIN(refreshed_at), MAX(refreshed_at) FROM project_documents",
            project_count,
            now,
            threshold,
        )?;
        let sessions = source(
            &self.connection,
            "SELECT COUNT(*), MIN(last_seen_at), MAX(last_seen_at) FROM sessions",
            now,
            threshold,
        )?;
        let context = source(
            &self.connection,
            "SELECT COUNT(*), MIN(refreshed_at), MAX(refreshed_at) FROM context_documents",
            now,
            threshold,
        )?;
        let state = overall_state([&git, &documents, &sessions, &context]);
        Ok(CatalogFreshness {
            state,
            checked_at: now,
            stale_after_seconds: threshold,
            may_be_stale: matches!(state, FreshnessState::Missing | FreshnessState::Stale),
            git,
            documents,
            sessions,
            context,
        })
    }
}

fn source(
    connection: &Connection,
    sql: &str,
    now: i64,
    stale_after_seconds: i64,
) -> Result<SourceFreshness> {
    source_with_expected(connection, sql, 0, now, stale_after_seconds)
}

fn source_with_expected(
    connection: &Connection,
    sql: &str,
    expected: i64,
    now: i64,
    stale_after_seconds: i64,
) -> Result<SourceFreshness> {
    let (count, oldest, latest): (i64, Option<i64>, Option<i64>) =
        connection.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let missing = expected.saturating_sub(count);
    let latest_age = latest.map(|value| now.saturating_sub(value).max(0));
    let max_age = oldest.map(|value| now.saturating_sub(value).max(0));
    let state = if expected > 0 && missing > 0 {
        FreshnessState::Missing
    } else if count == 0 {
        FreshnessState::Empty
    } else if max_age.is_some_and(|value| value > stale_after_seconds) {
        FreshnessState::Stale
    } else {
        FreshnessState::Fresh
    };
    Ok(SourceFreshness {
        state,
        items: usize::try_from(count).unwrap_or(0),
        missing: usize::try_from(missing).unwrap_or(0),
        latest_observed_at: latest,
        latest_age_seconds: latest_age,
        oldest_observed_at: oldest,
        max_age_seconds: max_age,
    })
}

fn overall_state<'a>(sources: impl IntoIterator<Item = &'a SourceFreshness>) -> FreshnessState {
    let states = sources
        .into_iter()
        .map(|source| source.state)
        .collect::<Vec<_>>();
    if states.contains(&FreshnessState::Missing) {
        FreshnessState::Missing
    } else if states.contains(&FreshnessState::Stale) {
        FreshnessState::Stale
    } else if states.iter().all(|state| *state == FreshnessState::Empty) {
        FreshnessState::Empty
    } else {
        FreshnessState::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkeinPaths;

    #[test]
    fn stale_boundary_is_strict_and_missing_projects_fail_closed() -> Result<()> {
        let temp = tempfile::tempdir().expect("temporary state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        let empty = registry.catalog_freshness(100, 10)?;
        assert_eq!(empty.state, FreshnessState::Empty);

        let project = temp.path().join("project");
        std::fs::create_dir(&project).expect("project directory");
        registry.add_project(&project, None)?;
        let missing = registry.catalog_freshness(100, 10)?;
        assert_eq!(missing.state, FreshnessState::Missing);

        registry.connection.execute(
            "INSERT INTO project_metadata (project_id, vcs_kind, fingerprint, refreshed_at)
             VALUES (1, 'none', 'synthetic', 90)",
            [],
        )?;
        registry.connection.execute(
            "INSERT INTO project_documents
             (project_id, fingerprint, refreshed_at, title, body, source_paths, indexed_bytes)
             VALUES (1, 'synthetic', 90, 'Synthetic', '', '[]', 0)",
            [],
        )?;
        assert_eq!(
            registry.catalog_freshness(100, 10)?.state,
            FreshnessState::Fresh
        );
        assert_eq!(
            registry.catalog_freshness(101, 10)?.state,
            FreshnessState::Stale
        );
        Ok(())
    }

    #[test]
    fn newest_row_cannot_mask_an_older_covered_observation() -> Result<()> {
        let temp = tempfile::tempdir().expect("temporary state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute(
            "INSERT INTO context_documents
             (source_kind, source_path, fingerprint, refreshed_at, title, body, imported_bytes)
             VALUES ('codex_memory', 'old.md', 'old', 80, 'Old', '', 0),
                    ('codex_memory', 'new.md', 'new', 100, 'New', '', 0)",
            [],
        )?;
        let report = registry.catalog_freshness(100, 10)?;
        assert_eq!(report.context.state, FreshnessState::Stale);
        assert_eq!(report.context.latest_observed_at, Some(100));
        assert_eq!(report.context.latest_age_seconds, Some(0));
        assert_eq!(report.context.oldest_observed_at, Some(80));
        assert_eq!(report.context.max_age_seconds, Some(20));
        assert!(report.may_be_stale);
        Ok(())
    }
}
