//! Read-only statistics over recent dated audit logs. No cache or database.
use crate::{audit, config, usage::Tokens};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::{collections::HashMap, path::Path};

#[derive(Default)]
struct Request {
    calls: u64,
    responses: u64,
    unknown: bool,
    input: u128,
    output: u128,
    completed: Option<(DateTime<Utc>, Option<u64>)>,
}
#[derive(Debug, Default)]
struct Totals {
    approvals: u64,
    input: u128,
    output: u128,
    duration_ms: u128,
    unknown_tokens: bool,
    unknown_time: bool,
}
struct Collector {
    now: DateTime<Utc>,
    mode: Option<config::Mode>,
    requests: HashMap<(String, String), Request>,
}
impl Collector {
    fn event(&mut self, event: Value) {
        let Some(mode) = event["mode"].as_str() else {
            return;
        };
        if self.mode.is_some_and(|m| m.as_str() != mode) {
            return;
        }
        let Some(id) = event["id"].as_str() else {
            return;
        };
        let Some(timestamp) = event["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            return;
        };
        if timestamp > self.now {
            return;
        }
        let Some(kind) = event["event"].as_str() else {
            return;
        };
        if !matches!(
            kind,
            "agy_request"
                | "agentapi_request"
                | "agy_response"
                | "agentapi_response"
                | "agy_error"
                | "agentapi_error"
                | "hook_result"
        ) {
            return;
        }
        let request = self.requests.entry((mode.into(), id.into())).or_default();
        let data = &event["data"];
        match kind {
            "agy_request" | "agentapi_request" => request.calls += 1,
            "agy_response" | "agentapi_response" => {
                request.responses += 1;
                match serde_json::from_value::<Tokens>(data["usage_delta"].clone()) {
                    Ok(tokens) if data["exit_code"] == 0 => {
                        request.input += u128::from(tokens.input_tokens);
                        request.output += u128::from(tokens.output_tokens);
                    }
                    _ => request.unknown = true,
                }
            }
            "agy_error" | "agentapi_error" => request.unknown = true,
            "hook_result" => {
                if data["stage"] == "reviewer"
                    && matches!(
                        data["output"]["decision"].as_str(),
                        Some("allow" | "deny" | "ask" | "force_ask")
                    )
                {
                    request.completed = Some((timestamp, data["duration_ms"].as_u64()));
                }
            }
            _ => unreachable!(),
        }
    }
    fn totals(self) -> [Totals; 3] {
        let mut totals = std::array::from_fn(|_| Totals::default());
        for request in self.requests.values() {
            let Some((completed, duration)) = request.completed else {
                continue;
            };
            for (days, total) in [1, 7, 30].into_iter().zip(&mut totals) {
                if completed < self.now - Duration::days(days) {
                    continue;
                }
                total.approvals += 1;
                total.input += request.input;
                total.output += request.output;
                total.unknown_tokens |=
                    request.unknown || request.calls == 0 || request.calls != request.responses;
                match duration {
                    Some(ms) => total.duration_ms += u128::from(ms),
                    None => total.unknown_time = true,
                }
            }
        }
        totals
    }
}
fn collect(dir: &Path, now: DateTime<Utc>, mode: Option<config::Mode>) -> Result<[Totals; 3]> {
    let first = (now - Duration::days(31)).date_naive();
    let last = now.date_naive();
    let paths = audit::paths(dir)?
        .into_iter()
        .filter(|(date, _)| *date >= first && *date <= last)
        .map(|(_, path)| path);
    let mut collector = Collector {
        now,
        mode,
        requests: HashMap::new(),
    };
    audit::scan(paths, |event| collector.event(event))?;
    Ok(collector.totals())
}
fn number(value: u128) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}
fn time(ms: u128) -> String {
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let seconds = (ms + 500) / 1000;
    if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}
fn table(totals: &[Totals; 3]) -> String {
    let mut rows = vec![
        vec![
            "Period",
            "Approvals",
            "Input Tokens",
            "Output Tokens",
            "Total Time",
            "Avg Input",
            "Avg Output",
            "Avg Time",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    ];
    for (period, total) in ["Last 24 hours", "Last 7 days", "Last 30 days"]
        .into_iter()
        .zip(totals)
    {
        let tokens = |value| {
            if total.unknown_tokens {
                "N/A".into()
            } else {
                number(value)
            }
        };
        let average = |value| {
            if total.approvals == 0 {
                "—".into()
            } else if total.unknown_tokens {
                "N/A".into()
            } else {
                number((value + u128::from(total.approvals) / 2) / u128::from(total.approvals))
            }
        };
        rows.push(vec![
            period.into(),
            number(total.approvals.into()),
            tokens(total.input),
            tokens(total.output),
            if total.unknown_time {
                "N/A".into()
            } else {
                time(total.duration_ms)
            },
            average(total.input),
            average(total.output),
            if total.approvals == 0 {
                "—".into()
            } else if total.unknown_time {
                "N/A".into()
            } else {
                format!(
                    "{:.1}s",
                    total.duration_ms as f64 / total.approvals as f64 / 1000.0
                )
            },
        ]);
    }
    let widths: Vec<usize> = (0..8)
        .map(|i| rows.iter().map(|row| row[i].chars().count()).max().unwrap())
        .collect();
    let border = |left, middle: &str, right| {
        format!(
            "{left}{}{right}\n",
            widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join(middle)
        )
    };
    let mut output = border("┌", "┬", "┐");
    for (index, row) in rows.iter().enumerate() {
        output.push('│');
        for (column, (cell, width)) in row.iter().zip(&widths).enumerate() {
            let padding = " ".repeat(width - cell.chars().count());
            if index == 0 || column == 0 {
                output.push_str(&format!(" {cell}{padding} │"));
            } else {
                output.push_str(&format!(" {padding}{cell} │"));
            }
        }
        output.push('\n');
        if index == 0 {
            output.push_str(&border("├", "┼", "┤"));
        }
    }
    output.push_str(&border("└", "┴", "┘"));
    output
}
pub fn print(mode: Option<config::Mode>) -> Result<()> {
    let now = Utc::now();
    print!("{}", table(&collect(&config::log_dir(), now, mode)?));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{fs, io::Write};

    fn now() -> DateTime<Utc> {
        "2026-09-18T00:00:10Z".parse().unwrap()
    }
    fn event(id: &str, mode: &str, at: DateTime<Utc>, kind: &str, data: Value) -> Value {
        json!({"id":id,"mode":mode,"timestamp":at.to_rfc3339(),"event":kind,"data":data})
    }
    fn call(c: &mut Collector, id: &str, mode: &str, at: DateTime<Utc>, input: u64, output: u64) {
        c.event(event(id, mode, at, "agy_request", json!({})));
        c.event(event(
            id,
            mode,
            at,
            "agy_response",
            json!({"exit_code":0,"usage_delta":{"input_tokens":input,"output_tokens":output}}),
        ));
    }
    fn finish(c: &mut Collector, id: &str, mode: &str, at: DateTime<Utc>, stage: &str, ms: u64) {
        c.event(event(
            id,
            mode,
            at,
            "hook_result",
            json!({"stage":stage,"duration_ms":ms,"output":{"decision":"deny"}}),
        ));
    }
    fn collector() -> Collector {
        Collector {
            now: now(),
            mode: None,
            requests: HashMap::new(),
        }
    }

    #[test]
    fn exact_rolling_boundaries_initialization_retries_and_exclusions() {
        let mut c = collector();
        let cases = [
            ("today", now(), 10, 2, 1000),
            ("day", now() - Duration::days(1), 20, 4, 2000),
            (
                "outside_day",
                now() - Duration::days(1) - Duration::milliseconds(1),
                30,
                6,
                3000,
            ),
            ("week", now() - Duration::days(7), 40, 8, 4000),
            (
                "outside_week",
                now() - Duration::days(7) - Duration::milliseconds(1),
                50,
                10,
                5000,
            ),
            ("month", now() - Duration::days(30), 60, 12, 6000),
            (
                "outside_month",
                now() - Duration::days(30) - Duration::milliseconds(1),
                70,
                14,
                7000,
            ),
            ("future", now() + Duration::milliseconds(1), 80, 16, 8000),
        ];
        for (id, at, input, output, ms) in cases {
            call(&mut c, id, "cli", at - Duration::seconds(1), input, output);
            finish(&mut c, id, "cli", at, "reviewer", ms);
        }
        // Extra initialization and retry calls belong to the same approval.
        call(&mut c, "today", "cli", now() - Duration::seconds(2), 3, 1);
        call(&mut c, "today", "cli", now() - Duration::seconds(1), 7, 2);
        for stage in [
            "whitelist",
            "blacklist",
            "circuit_breaker",
            "reviewer_error",
            "state_error",
        ] {
            call(&mut c, stage, "cli", now(), 10000, 10000);
            finish(&mut c, stage, "cli", now(), stage, 10000);
        }
        let totals = c.totals();
        for (actual, expected) in
            totals
                .iter()
                .zip([(2, 40, 9, 3000), (4, 110, 23, 10000), (6, 220, 45, 21000)])
        {
            assert_eq!(
                (
                    actual.approvals,
                    actual.input,
                    actual.output,
                    actual.duration_ms
                ),
                expected
            );
            assert!(!actual.unknown_tokens);
        }
        let output = table(&totals);
        assert!(output.contains("1.5s"));
        assert!(output.contains("Last 30 days"));
        assert!(!output.contains("365"));
        assert_eq!(output.lines().count(), 7);
        assert!(
            output
                .lines()
                .all(|l| l.chars().count() == output.lines().next().unwrap().chars().count())
        );
    }
    #[test]
    fn unknown_usage_and_failed_retry_do_not_become_zero() {
        let mut c = collector();
        call(&mut c, "good", "cli", now(), 100, 10);
        finish(&mut c, "good", "cli", now(), "reviewer", 1000);
        c.event(event("retry", "cli", now(), "agy_request", json!({})));
        c.event(event("retry", "cli", now(), "agy_error", json!({})));
        call(&mut c, "retry", "cli", now(), 50, 5);
        finish(&mut c, "retry", "cli", now(), "reviewer", 3000);
        let totals = c.totals();
        assert_eq!(totals[0].approvals, 2);
        assert!(totals[0].unknown_tokens);
        assert!(table(&totals).contains("N/A"));
        assert!(table(&totals).contains("2.0s"));
    }
    #[test]
    fn filters_modes_and_handles_missing_sidecar_usage() {
        let mut c = collector();
        c.mode = Some(config::Mode::Cli);
        call(&mut c, "id", "cli", now(), 100, 10);
        finish(&mut c, "id", "cli", now(), "reviewer", 1000);
        finish(&mut c, "id", "sidecar", now(), "reviewer", 2000);
        assert_eq!(c.totals()[0].approvals, 1);
        let mut c = collector();
        finish(&mut c, "id", "sidecar", now(), "reviewer", 2000);
        assert!(c.totals()[0].unknown_tokens);
    }
    #[test]
    fn reads_dated_files_across_midnight_ignores_legacy_and_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let before = now() - Duration::seconds(20);
        let request = event("a", "cli", before, "agy_request", json!({}));
        let response = event(
            "a",
            "cli",
            before,
            "agy_response",
            json!({"exit_code":0,"usage_delta":{"input_tokens":1200,"output_tokens":100}}),
        );
        let result = event(
            "a",
            "cli",
            now(),
            "hook_result",
            json!({"stage":"reviewer","duration_ms":20000,"output":{"decision":"allow"}}),
        );
        fs::write(
            audit::daily_path(dir.path(), before.date_naive()),
            format!("{request}\n{response}\n"),
        )
        .unwrap();
        let today = audit::daily_path(dir.path(), now().date_naive());
        fs::write(&today, format!("invalid\n{result}\n{result}")).unwrap();
        fs::write(dir.path().join("approvals.jsonl"), format!("{result}\n")).unwrap();
        fs::write(
            audit::daily_path(dir.path(), (now() - Duration::days(40)).date_naive()),
            format!("{result}\n"),
        )
        .unwrap();
        let totals = collect(dir.path(), now(), None).unwrap();
        assert_eq!(
            (
                totals[0].approvals,
                totals[0].input,
                totals[0].output,
                totals[0].duration_ms
            ),
            (1, 1200, 100, 20000)
        );
        assert!(!totals[0].unknown_tokens);
        // Complete duplicate result must not double count the approval.
        writeln!(fs::OpenOptions::new().append(true).open(today).unwrap()).unwrap();
        assert_eq!(collect(dir.path(), now(), None).unwrap()[0].approvals, 1);
    }
    #[test]
    fn incomplete_response_and_missing_duration_remain_unknown() {
        let mut c = collector();
        c.event(event("partial", "cli", now(), "agy_request", json!({})));
        c.event(event(
            "partial",
            "cli",
            now(),
            "agy_response",
            json!({"exit_code":0,"usage_delta":{"input_tokens":100}}),
        ));
        c.event(event(
            "partial",
            "cli",
            now(),
            "hook_result",
            json!({"stage":"reviewer","output":{"decision":"allow"}}),
        ));
        let totals = c.totals();
        assert_eq!(totals[0].approvals, 1);
        assert!(totals[0].unknown_tokens);
        assert!(totals[0].unknown_time);
        let mut c = collector();
        c.event(event("unfinished", "cli", now(), "agy_request", json!({})));
        assert_eq!(c.totals()[0].approvals, 0);
    }

    #[test]
    fn empty_history_and_large_counters() {
        let dir = tempfile::tempdir().unwrap();
        let totals = collect(dir.path(), now(), None).unwrap();
        assert!(table(&totals).contains('—'));
        assert!(!table(&totals).contains("N/A"));
        let mut c = collector();
        for id in ["a", "b"] {
            call(&mut c, id, "cli", now(), u64::MAX, u64::MAX);
            finish(&mut c, id, "cli", now(), "reviewer", u64::MAX);
        }
        assert_eq!(c.totals()[0].input, 2 * u128::from(u64::MAX));
        assert_eq!(number(1234567), "1,234,567");
        assert_eq!(time(3_661_000), "1h 01m 01s");
    }
}
