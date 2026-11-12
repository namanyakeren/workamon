use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection};
use serde::Serialize;

use crate::util;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
    pub path: PathBuf,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemSample {
    pub ts: i64,
    pub cpu_pct: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub temp_c: Option<f64>,
    pub containers_running: u32,
    pub containers_total: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContainerSample {
    pub ts: i64,
    pub container_id: String,
    pub name: String,
    pub cpu_pct: f64,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogRow {
    pub id: i64,
    pub ts: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DbStats {
    pub size_bytes: u64,
    pub max_bytes: u64,
    pub system_rows: i64,
    pub container_rows: i64,
    pub log_rows: i64,
    pub oldest_ts: Option<i64>,
    pub newest_ts: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TrimReport {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub deleted_system: usize,
    pub deleted_containers: usize,
    pub deleted_logs: usize,
    pub vacuumed: bool,
}

impl Db {
    pub fn open(path: &Path, max_bytes: u64) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("gagal membuka database {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-2000;
             PRAGMA wal_autocheckpoint=256;
             PRAGMA foreign_keys=ON;",
        )?;

        let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        if auto_vacuum != 2 {
            conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                cpu_pct REAL NOT NULL,
                mem_used INTEGER NOT NULL,
                mem_total INTEGER NOT NULL,
                swap_used INTEGER NOT NULL,
                swap_total INTEGER NOT NULL,
                disk_used INTEGER NOT NULL,
                disk_total INTEGER NOT NULL,
                load1 REAL NOT NULL,
                load5 REAL NOT NULL,
                load15 REAL NOT NULL,
                temp_c REAL,
                containers_running INTEGER NOT NULL,
                containers_total INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics(ts);

            CREATE TABLE IF NOT EXISTS container_metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                container_id TEXT NOT NULL,
                name TEXT NOT NULL,
                cpu_pct REAL NOT NULL,
                mem_used INTEGER NOT NULL,
                mem_limit INTEGER NOT NULL,
                net_rx INTEGER NOT NULL,
                net_tx INTEGER NOT NULL,
                state TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cm_ts ON container_metrics(ts);
            CREATE INDEX IF NOT EXISTS idx_cm_container_ts ON container_metrics(container_id, ts);

            CREATE TABLE IF NOT EXISTS app_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                level TEXT NOT NULL,
                target TEXT NOT NULL,
                message TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_logs_ts ON app_logs(ts);
            CREATE INDEX IF NOT EXISTS idx_logs_level_ts ON app_logs(level, ts);",
        )?;

        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            path: path.to_path_buf(),
            max_bytes,
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn insert_system(&self, sample: &SystemSample) -> Result<()> {
        self.conn().execute(
            "INSERT INTO metrics
             (ts, cpu_pct, mem_used, mem_total, swap_used, swap_total, disk_used, disk_total,
              load1, load5, load15, temp_c, containers_running, containers_total)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                sample.ts,
                sample.cpu_pct,
                sample.mem_used as i64,
                sample.mem_total as i64,
                sample.swap_used as i64,
                sample.swap_total as i64,
                sample.disk_used as i64,
                sample.disk_total as i64,
                sample.load1,
                sample.load5,
                sample.load15,
                sample.temp_c,
                sample.containers_running as i64,
                sample.containers_total as i64,
            ],
        )?;
        Ok(())
    }

    pub fn insert_container(&self, sample: &ContainerSample) -> Result<()> {
        self.conn().execute(
            "INSERT INTO container_metrics
             (ts, container_id, name, cpu_pct, mem_used, mem_limit, net_rx, net_tx, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                sample.ts,
                sample.container_id,
                sample.name,
                sample.cpu_pct,
                sample.mem_used as i64,
                sample.mem_limit as i64,
                sample.net_rx as i64,
                sample.net_tx as i64,
                sample.state,
            ],
        )?;
        Ok(())
    }

    pub fn insert_log(&self, ts: i64, level: &str, target: &str, message: &str) -> Result<()> {
        let message = util::truncate_chars(message, 4_000);
        self.conn().execute(
            "INSERT INTO app_logs (ts, level, target, message) VALUES (?1, ?2, ?3, ?4)",
            params![ts, level, target, message],
        )?;
        Ok(())
    }

    pub fn recent_system(&self, limit: usize) -> Result<Vec<SystemSample>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT ts, cpu_pct, mem_used, mem_total, swap_used, swap_total, disk_used,
                    disk_total, load1, load5, load15, temp_c, containers_running, containers_total
             FROM metrics ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt
            .query_map([limit as i64], row_to_system)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    pub fn system_bucketed(&self, since: i64, bucket_secs: i64) -> Result<Vec<SystemSample>> {
        let bucket = bucket_secs.max(1);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT MIN(ts) AS ts,
                    COALESCE(AVG(cpu_pct), 0),
                    CAST(COALESCE(AVG(mem_used), 0) AS INTEGER),
                    COALESCE(MAX(mem_total), 0),
                    CAST(COALESCE(AVG(swap_used), 0) AS INTEGER),
                    COALESCE(MAX(swap_total), 0),
                    CAST(COALESCE(AVG(disk_used), 0) AS INTEGER),
                    COALESCE(MAX(disk_total), 0),
                    COALESCE(AVG(load1), 0),
                    COALESCE(AVG(load5), 0),
                    COALESCE(AVG(load15), 0),
                    AVG(temp_c),
                    COALESCE(MAX(containers_running), 0),
                    COALESCE(MAX(containers_total), 0)
             FROM metrics
             WHERE ts >= ?1
             GROUP BY CAST(ts / ?2 AS INTEGER)
             ORDER BY CAST(ts / ?2 AS INTEGER) ASC",
        )?;
        let rows = stmt
            .query_map(params![since, bucket], row_to_system)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn container_history(
        &self,
        container_id: &str,
        since: i64,
        bucket_secs: i64,
    ) -> Result<Vec<ContainerSample>> {
        let bucket = bucket_secs.max(1);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT MIN(ts),
                    COALESCE(AVG(cpu_pct), 0),
                    CAST(COALESCE(AVG(mem_used), 0) AS INTEGER),
                    COALESCE(MAX(mem_limit), 0),
                    CAST(COALESCE(AVG(net_rx), 0) AS INTEGER),
                    CAST(COALESCE(AVG(net_tx), 0) AS INTEGER),
                    COALESCE(MAX(name), ''),
                    COALESCE(MAX(state), '')
             FROM container_metrics
             WHERE container_id = ?1 AND ts >= ?2
             GROUP BY CAST(ts / ?3 AS INTEGER)
             ORDER BY CAST(ts / ?3 AS INTEGER) ASC",
        )?;
        let rows = stmt
            .query_map(params![container_id, since, bucket], |row| {
                Ok(ContainerSample {
                    ts: row.get(0)?,
                    container_id: container_id.to_string(),
                    cpu_pct: row.get(1)?,
                    mem_used: row.get::<_, i64>(2)? as u64,
                    mem_limit: row.get::<_, i64>(3)? as u64,
                    net_rx: row.get::<_, i64>(4)? as u64,
                    net_tx: row.get::<_, i64>(5)? as u64,
                    name: row.get(6)?,
                    state: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn logs(
        &self,
        level: Option<&str>,
        query: Option<&str>,
        page: usize,
        per_page: usize,
    ) -> Result<(Vec<LogRow>, i64)> {
        let per_page = per_page.clamp(10, 500);
        let page = page.max(1);
        let offset = (page - 1) * per_page;

        let mut condition = String::from(" WHERE 1=1");
        let mut args: Vec<SqlValue> = Vec::new();

        if let Some(level) = level.filter(|v| !v.is_empty() && *v != "ALL") {
            condition.push_str(" AND level = ?");
            args.push(SqlValue::Text(level.to_string()));
        }
        if let Some(query) = query.filter(|v| !v.trim().is_empty()) {
            let pattern = like_pattern(query.trim());
            condition.push_str(" AND (message LIKE ? ESCAPE '\\' OR target LIKE ? ESCAPE '\\')");
            args.push(SqlValue::Text(pattern.clone()));
            args.push(SqlValue::Text(pattern));
        }

        let conn = self.conn();
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM app_logs{condition}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        let mut list_args = args;
        list_args.push(SqlValue::Integer(per_page as i64));
        list_args.push(SqlValue::Integer(offset as i64));
        let mut stmt = conn.prepare(&format!(
            "SELECT id, ts, level, target, message FROM app_logs{condition}
             ORDER BY id DESC LIMIT ? OFFSET ?"
        ))?;
        let rows = stmt
            .query_map(params_from_iter(list_args.iter()), |row| {
                Ok(LogRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    level: row.get(2)?,
                    target: row.get(3)?,
                    message: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rows, total))
    }

    pub fn stats(&self) -> Result<DbStats> {
        let conn = self.conn();
        let system_rows: i64 = conn.query_row("SELECT COUNT(*) FROM metrics", [], |row| row.get(0))?;
        let container_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM container_metrics", [], |row| row.get(0))?;
        let log_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM app_logs", [], |row| row.get(0))?;
        let oldest_ts: Option<i64> = conn.query_row(
            "SELECT MIN(ts) FROM (
                SELECT ts FROM metrics
                UNION ALL SELECT ts FROM container_metrics
                UNION ALL SELECT ts FROM app_logs
             )",
            [],
            |row| row.get(0),
        )?;
        let newest_ts: Option<i64> = conn.query_row(
            "SELECT MAX(ts) FROM (
                SELECT ts FROM metrics
                UNION ALL SELECT ts FROM container_metrics
                UNION ALL SELECT ts FROM app_logs
             )",
            [],
            |row| row.get(0),
        )?;
        drop(conn);
        Ok(DbStats {
            size_bytes: self.size_on_disk(),
            max_bytes: self.max_bytes,
            system_rows,
            container_rows,
            log_rows,
            oldest_ts,
            newest_ts,
        })
    }

    pub fn cleanup_logs_before(&self, ts: i64) -> Result<usize> {
        Ok(self
            .conn()
            .execute("DELETE FROM app_logs WHERE ts < ?1", [ts])?)
    }

    pub fn clear_logs(&self) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM app_logs", [])?)
    }

    pub fn clear_metrics(&self) -> Result<usize> {
        let conn = self.conn();
        let system = conn.execute("DELETE FROM metrics", [])?;
        let containers = conn.execute("DELETE FROM container_metrics", [])?;
        Ok(system + containers)
    }

    pub fn vacuum(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Menjaga ukuran file database (termasuk WAL) di bawah batas. Data tertua
    /// dihapus bertahap sampai ukuran kembali aman.
    pub fn trim_to_limit(&self) -> Result<TrimReport> {
        let before = self.size_on_disk();
        let mut report = TrimReport {
            before_bytes: before,
            after_bytes: before,
            ..Default::default()
        };
        if before <= self.max_bytes {
            return Ok(report);
        }

        let conn = self.conn();
        for _ in 0..200 {
            let deleted_system = conn.execute(
                "DELETE FROM metrics WHERE id IN (SELECT id FROM metrics ORDER BY id LIMIT 100)",
                [],
            )?;
            let deleted_containers = conn.execute(
                "DELETE FROM container_metrics
                 WHERE id IN (SELECT id FROM container_metrics ORDER BY id LIMIT 100)",
                [],
            )?;
            let deleted_logs = conn.execute(
                "DELETE FROM app_logs WHERE id IN (SELECT id FROM app_logs ORDER BY id LIMIT 100)",
                [],
            )?;

            report.deleted_system += deleted_system;
            report.deleted_containers += deleted_containers;
            report.deleted_logs += deleted_logs;

            if deleted_system + deleted_containers + deleted_logs == 0 {
                break;
            }

            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            let _ = conn.execute_batch("PRAGMA incremental_vacuum(1000);");
            if self.size_on_disk() <= self.max_bytes {
                break;
            }
        }

        if self.size_on_disk() > self.max_bytes {
            conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
            report.vacuumed = true;
        }
        report.after_bytes = self.size_on_disk();
        Ok(report)
    }

    pub fn size_on_disk(&self) -> u64 {
        file_size(&self.path)
            + file_size(&with_suffix(&self.path, "-wal"))
            + file_size(&with_suffix(&self.path, "-shm"))
    }
}

fn row_to_system(row: &rusqlite::Row<'_>) -> rusqlite::Result<SystemSample> {
    Ok(SystemSample {
        ts: row.get(0)?,
        cpu_pct: row.get(1)?,
        mem_used: row.get::<_, i64>(2)? as u64,
        mem_total: row.get::<_, i64>(3)? as u64,
        swap_used: row.get::<_, i64>(4)? as u64,
        swap_total: row.get::<_, i64>(5)? as u64,
        disk_used: row.get::<_, i64>(6)? as u64,
        disk_total: row.get::<_, i64>(7)? as u64,
        load1: row.get(8)?,
        load5: row.get(9)?,
        load15: row.get(10)?,
        temp_c: row.get(11)?,
        containers_running: row.get::<_, i64>(12)? as u32,
        containers_total: row.get::<_, i64>(13)? as u32,
    })
}

fn like_pattern(query: &str) -> String {
    let mut pattern = String::from("%");
    for ch in query.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "workamon-test-{}-{nanos}.db",
            std::process::id()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(with_suffix(path, "-wal"));
        let _ = std::fs::remove_file(with_suffix(path, "-shm"));
    }

    #[test]
    fn trims_oldest_rows_until_size_limit() {
        let path = temp_path();
        let max_bytes = 256 * 1024;
        let db = Db::open(&path, max_bytes).expect("database harus terbuka");
        let base = util::now() - 10_000;

        for index in 0..2_000i64 {
            let sample = SystemSample {
                ts: base + index,
                cpu_pct: (index % 100) as f64,
                mem_used: 512 * 1024 * 1024,
                mem_total: 1024 * 1024 * 1024,
                swap_used: 0,
                swap_total: 0,
                disk_used: 8 * 1024 * 1024 * 1024,
                disk_total: 16 * 1024 * 1024 * 1024,
                load1: 0.5,
                load5: 0.4,
                load15: 0.3,
                temp_c: Some(48.0),
                containers_running: 3,
                containers_total: 5,
            };
            db.insert_system(&sample).expect("sampel harus tersimpan");
        }

        assert!(db.size_on_disk() > max_bytes, "data uji harus melewati batas");
        let report = db.trim_to_limit().expect("pemangkasan harus berhasil");
        assert!(report.deleted_system > 0, "harus ada baris lama yang dihapus");
        assert!(
            db.size_on_disk() <= max_bytes,
            "ukuran {} masih di atas batas {}",
            db.size_on_disk(),
            max_bytes
        );

        let remaining = db.recent_system(1).expect("query harus berhasil");
        assert_eq!(remaining.len(), 1);
        assert!(
            remaining[0].ts > base,
            "baris tertua seharusnya sudah dihapus"
        );

        drop(db);
        cleanup(&path);
    }
}
