use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct LatencyRow {
    pub provider: Option<String>,
    pub total: f64,
    pub ttft: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Distribution {
    pub samples: usize,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStats {
    pub samples: usize,
    pub total_p50: Option<f64>,
    pub total_p95: Option<f64>,
    pub ttft_p50: Option<f64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    pub samples: usize,
    pub ttft: Distribution,
    pub total: Distribution,
    pub by_provider: BTreeMap<String, ProviderStats>,
}

pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((p / 100.0 * sorted.len() as f64).ceil() as isize - 1)
        .clamp(0, sorted.len() as isize - 1) as usize;
    Some(sorted[index])
}
fn distribution(values: &mut [f64]) -> Distribution {
    values.sort_by(f64::total_cmp);
    Distribution {
        samples: values.len(),
        p50: percentile(values, 50.0),
        p95: percentile(values, 95.0),
    }
}
pub fn get_latency_stats(
    db: &rusqlite::Connection,
    since: Option<&str>,
    limit: i64,
) -> rusqlite::Result<LatencyStats> {
    let sql = if since.is_some() {
        "SELECT data FROM requestDetails WHERE timestamp >= ?1 ORDER BY timestamp DESC LIMIT ?2"
    } else {
        "SELECT data FROM requestDetails ORDER BY timestamp DESC LIMIT ?1"
    };
    let mut statement = db.prepare(sql)?;
    let mut query = if let Some(since) = since {
        statement.query(rusqlite::params![since, limit])?
    } else {
        statement.query(rusqlite::params![limit])?
    };
    let mut rows = Vec::new();
    while let Some(row) = query.next()? {
        let data: String = row.get(0)?;
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
            let number = |v: &serde_json::Value| {
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            };
            rows.push(LatencyRow {
                provider: value["provider"].as_str().map(str::to_owned),
                total: number(&value["latency"]["total"]).unwrap_or(0.0),
                ttft: number(&value["latency"]["ttft"]),
            });
        }
    }
    Ok(compute_latency_stats(&rows))
}

pub fn compute_latency_stats(rows: &[LatencyRow]) -> LatencyStats {
    let mut totals = Vec::new();
    let mut ttfts = Vec::new();
    let mut providers: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for row in rows {
        if !row.total.is_finite() || row.total <= 0.0 {
            continue;
        }
        totals.push(row.total);
        let provider = row
            .provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        let entry = providers.entry(provider.to_owned()).or_default();
        entry.0.push(row.total);
        if let Some(ttft) = row.ttft.filter(|v| v.is_finite() && *v > 0.0) {
            ttfts.push(ttft);
            entry.1.push(ttft);
        }
    }
    let by_provider = providers
        .into_iter()
        .map(|(name, (mut totals, mut ttfts))| {
            let total = distribution(&mut totals);
            let ttft = distribution(&mut ttfts);
            (
                name,
                ProviderStats {
                    samples: total.samples,
                    total_p50: total.p50,
                    total_p95: total.p95,
                    ttft_p50: ttft.p50,
                },
            )
        })
        .collect();
    LatencyStats {
        samples: totals.len(),
        ttft: distribution(&mut ttfts),
        total: distribution(&mut totals),
        by_provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn database_filters_limits_and_skips_invalid_json() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE requestDetails (timestamp TEXT, data TEXT);")
            .unwrap();
        for (timestamp, data) in [
            ("2026-01-01", r#"{"provider":"a","latency":{"total":100}}"#),
            ("2026-01-02", "invalid"),
            (
                "2026-01-03",
                r#"{"provider":"b","latency":{"total":"200","ttft":20}}"#,
            ),
        ] {
            db.execute(
                "INSERT INTO requestDetails VALUES (?1, ?2)",
                rusqlite::params![timestamp, data],
            )
            .unwrap();
        }
        let stats = get_latency_stats(&db, Some("2026-01-02"), 10).unwrap();
        assert_eq!(stats.samples, 1);
        assert_eq!(stats.by_provider["b"].total_p50, Some(200.0));
        assert_eq!(get_latency_stats(&db, None, 1).unwrap().samples, 1);
        assert_eq!(get_latency_stats(&db, None, 2).unwrap().samples, 1);
    }
    #[test]
    fn nearest_rank_and_aggregation() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[10., 20., 30., 40.], 50.0), Some(20.));
        assert_eq!(percentile(&[10., 20., 30., 40.], 95.0), Some(40.));
        let rows = [
            ("a", 100., 10.),
            ("a", 200., 20.),
            ("b", 300., 0.),
            ("b", 400., 40.),
            ("b", -5., 5.),
        ]
        .map(|(p, total, ttft)| LatencyRow {
            provider: Some(p.into()),
            total,
            ttft: Some(ttft),
        });
        let stats = compute_latency_stats(&rows);
        assert_eq!(stats.samples, 4);
        assert_eq!(stats.total.p50, Some(200.));
        assert_eq!(stats.total.p95, Some(400.));
        assert_eq!(stats.ttft.samples, 3);
        assert_eq!(stats.ttft.p50, Some(20.));
        assert_eq!(stats.by_provider["a"].total_p95, Some(200.));
        assert_eq!(stats.by_provider["b"].ttft_p50, Some(40.));
        assert_eq!(compute_latency_stats(&[]).total.p50, None);
    }
}
