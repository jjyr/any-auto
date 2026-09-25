//! Repeatable, isolated evaluations of fixture evidence using production decisions.
use crate::{
    audit,
    backend::{Backend, jev},
    config,
    reviewer::{Assessment, ReviewInput},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, VecDeque},
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Args)]
pub struct Options {
    /// JSONL fixture file. Repeat this flag to combine suites.
    #[arg(long, required = true)]
    pub suite: Vec<PathBuf>,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub repeat: u32,
    /// Maximum simultaneous trials; each trial has an isolated reviewer session.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub concurrency: u32,
    /// Deadline in seconds for each trial, independent of provider.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(1..))]
    pub deadline: u64,
    /// Full Jev questions JSON, replacing the effective configured rubric.
    #[arg(long)]
    pub questions: Option<PathBuf>,
    /// New JSON report file; existing files are never overwritten.
    #[arg(long)]
    pub output: PathBuf,
    /// Previous report for the identical suite and repeat count.
    #[arg(long)]
    pub compare: Option<PathBuf>,
    /// Maximum backend calls, including initialization and retry allowance.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=10000))]
    pub max_calls: u32,
    /// Retry transient failures per evaluation; no retries by default.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=2))]
    pub retries: u32,
    /// Print a machine-readable JSON summary instead of the terminal table.
    #[arg(long)]
    pub json: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    decision: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    #[serde(default)]
    tags: Vec<String>,
    input: Value,
    expected: Expected,
    reason: String,
}
#[derive(Deserialize, Serialize)]
struct Trial {
    case_id: String,
    repetition: u32,
    duration_ms: u128,
    assessment: Option<Assessment>,
    error: Option<String>,
    matched: Option<bool>,
    events: Vec<Value>,
}
#[derive(Deserialize, Serialize)]
struct Report {
    schema_version: u32,
    build_version: String,
    started_at: String,
    completed: bool,
    #[serde(default)]
    elapsed_ms: Option<u128>,
    suite_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    questions_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    questions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_hash: Option<String>,
    provider: String,
    decision_policy_version: String,
    base_rubric_version: String,
    requested_model: Option<String>,
    probability_threshold: f64,
    repeat: u32,
    #[serde(default = "default_concurrency")]
    concurrency: u32,
    retries: u32,
    max_calls: u32,
    cases: Vec<Case>,
    trials: Vec<Trial>,
    summary: Value,
    comparison: Value,
}
fn default_concurrency() -> u32 {
    1
}
fn hash(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("Cannot read {}", path.display()))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "File exceeds {limit} bytes: {}",
        path.display()
    );
    Ok(bytes)
}
fn validate_questions(value: &Value) -> Result<()> {
    let defaults = crate::prompts::jev_questions(&config::JevInstructions::default());
    ensure!(
        value.as_object().is_some_and(|v| v.len() == 3),
        "Questions must contain risk, authorization and policy"
    );
    for name in ["risk", "authorization", "policy"] {
        let q = &value[name];
        ensure!(
            q.as_object().is_some_and(|v| v.len() == 3) && q["type"] == "choice",
            "Invalid {name} question"
        );
        let valid_text = |v: &Value| {
            v.as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.len() <= 4096)
        };
        ensure!(
            valid_text(&q["instructions"]),
            "Invalid {name} instructions"
        );
        let criteria = q["criteria"]
            .as_object()
            .context("Question requires criteria")?;
        ensure!(
            criteria
                .keys()
                .eq(defaults[name]["criteria"].as_object().unwrap().keys())
                && criteria.values().all(valid_text),
            "Question options must match the production schema: {name}"
        );
    }
    Ok(())
}
fn load_cases(paths: &[PathBuf]) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    let mut ids = BTreeSet::new();
    for path in paths {
        let bytes = read_bounded(path, 4 * 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes).context("Suite must be UTF-8")?;
        for (line, text) in text
            .lines()
            .enumerate()
            .filter(|(_, s)| !s.trim().is_empty())
        {
            let case: Case = serde_json::from_str(text)
                .map_err(|_| anyhow::anyhow!("Invalid case at {}:{}", path.display(), line + 1))?;
            ensure!(
                !case.id.is_empty()
                    && case.id.len() <= 128
                    && case
                        .id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "Case ID must be 1-128 ASCII letters, digits, dots, dashes or underscores"
            );
            ensure!(
                ids.insert(case.id.clone()),
                "Duplicate case ID: {}",
                case.id
            );
            ensure!(
                !case.reason.trim().is_empty(),
                "Case requires a reason: {}",
                case.id
            );
            ensure!(
                matches!(case.expected.decision.as_str(), "allow" | "deny"),
                "Expected decision must be allow or deny"
            );
            ensure!(
                case.input.is_object()
                    && case.input["toolCall"]["name"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                    && case.input["toolCall"]["args"].is_object(),
                "Case requires a canonical toolCall: {}",
                case.id
            );
            cases.push(case);
            ensure!(cases.len() <= 10000, "Too many cases");
        }
    }
    ensure!(!cases.is_empty(), "Suite is empty");
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cases)
}
fn matches(case: &Case, assessment: &Assessment) -> bool {
    assessment.outcome.as_str() == case.expected.decision
}

fn usage_summary(runs: &[&Trial]) -> Value {
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut input_samples = 0;
    let mut output_samples = 0;
    let mut usage_samples = 0;
    let mut http_attempts = 0;
    for run in runs {
        http_attempts += run
            .events
            .iter()
            .filter(|e| e["event"] == "backend_request")
            .count();
        // Prefer per-attempt usage, including retries. Older reports use backend_response.
        let event_name = if run.events.iter().any(|e| e["event"] == "jev_attempt_usage") {
            "jev_attempt_usage"
        } else if run.events.iter().any(|e| e["event"] == "backend_usage") {
            "backend_usage"
        } else {
            "backend_response"
        };
        for event in run.events.iter().filter(|e| e["event"] == event_name) {
            let usage = &event["data"]["usage_delta"];
            let input = usage["input_tokens"].as_u64();
            let output = usage["output_tokens"].as_u64();
            if let Some(n) = input {
                input_tokens = input_tokens.saturating_add(n);
                input_samples += 1;
            }
            if let Some(n) = output {
                output_tokens = output_tokens.saturating_add(n);
                output_samples += 1;
            }
            if input.is_some() && output.is_some() {
                usage_samples += 1;
            }
        }
    }
    let expected_samples = http_attempts.max(runs.len());
    json!({"input_tokens":input_tokens,"output_tokens":output_tokens,
        "total_tokens":input_tokens.saturating_add(output_tokens),
        "input_usage_samples":input_samples,"output_usage_samples":output_samples,
        "usage_samples":usage_samples,"usage_partial":usage_samples < expected_samples,
        "input_usage_partial":input_samples < expected_samples,"output_usage_partial":output_samples < expected_samples,
        "backend_calls":http_attempts,"http_attempts":http_attempts,"duration_ms":runs.iter().map(|r| r.duration_ms).sum::<u128>()})
}

fn summarize(cases: &[Case], trials: &[Trial]) -> Value {
    let mut false_allows = 0;
    let mut false_denials = 0;
    let mut expected_allow = 0;
    let mut expected_deny = 0;
    let mut unstable = Vec::new();
    let mut per_case = Vec::new();
    for case in cases {
        let runs: Vec<_> = trials.iter().filter(|t| t.case_id == case.id).collect();
        let mut decisions = BTreeSet::new();
        for run in &runs {
            if let Some(a) = &run.assessment {
                decisions.insert(a.outcome);
                if case.expected.decision == "allow" {
                    expected_allow += 1;
                    false_denials += usize::from(a.outcome == crate::policy::Outcome::Deny);
                } else {
                    expected_deny += 1;
                    false_allows += usize::from(a.outcome == crate::policy::Outcome::Allow);
                }
            }
        }
        if decisions.len() > 1 {
            unstable.push(case.id.clone());
        }
        let mut row = usage_summary(&runs);
        row.as_object_mut().unwrap().extend(
            json!({"id":case.id,"attempts":runs.len(),
            "matched":runs.iter().filter(|t| t.matched == Some(true)).count(),
            "errors":runs.iter().filter(|t| t.error.is_some()).count(),"decisions":decisions})
            .as_object()
            .unwrap()
            .clone(),
        );
        per_case.push(row);
    }
    let rate = |n: usize, d: usize| {
        if d == 0 {
            Value::Null
        } else {
            json!(n as f64 / d as f64)
        }
    };
    let mut summary = json!({"attempts":trials.len(),"errors":trials.iter().filter(|t| t.error.is_some()).count(),
        "matched":trials.iter().filter(|t| t.matched == Some(true)).count(),
        "false_allows":false_allows,"false_denials":false_denials,
        "expected_allow_completed":expected_allow,"expected_deny_completed":expected_deny,
        "false_allow_rate":rate(false_allows,expected_deny),"false_denial_rate":rate(false_denials,expected_allow),
        "unstable_cases":unstable,"cases":per_case,
        "scenario_count":cases.len()});
    summary.as_object_mut().unwrap().extend(
        usage_summary(&trials.iter().collect::<Vec<_>>())
            .as_object()
            .unwrap()
            .clone(),
    );
    summary
}
fn comparison(current: &Report, baseline: &Report) -> Value {
    let mut improved = Vec::new();
    let mut regressed = Vec::new();
    let mut inconclusive = Vec::new();
    for case in &current.cases {
        let score = |r: &Report| {
            let runs: Vec<_> = r.trials.iter().filter(|t| t.case_id == case.id).collect();
            if runs.len() != r.repeat as usize || runs.iter().any(|t| t.error.is_some()) {
                None
            } else {
                Some(runs.iter().filter(|t| t.matched == Some(true)).count())
            }
        };
        match (score(current), score(baseline)) {
            (Some(now), Some(before)) if now > before => improved.push(case.id.clone()),
            (Some(now), Some(before)) if now < before => regressed.push(case.id.clone()),
            (Some(_), Some(_)) => {}
            _ => inconclusive.push(case.id.clone()),
        }
    }
    let before = usage_summary(&baseline.trials.iter().collect::<Vec<_>>());
    let now = usage_summary(&current.trials.iter().collect::<Vec<_>>());
    let token_delta = if before["usage_partial"] == false && now["usage_partial"] == false {
        Some(
            now["total_tokens"].as_u64().unwrap() as i128
                - before["total_tokens"].as_u64().unwrap() as i128,
        )
    } else {
        None
    };
    let elapsed_delta = current
        .elapsed_ms
        .zip(baseline.elapsed_ms)
        .map(|(now, before)| now as i128 - before as i128);
    json!({"baseline_questions_hash":baseline.questions_hash,"baseline_prompt_hash":baseline.prompt_hash,"baseline_model":baseline.requested_model,
        "baseline_threshold":baseline.probability_threshold,"baseline_decision_policy_version":baseline.decision_policy_version,"improved":improved,"regressed":regressed,"inconclusive":inconclusive, "token_delta":token_delta,"elapsed_ms_delta":elapsed_delta})
}
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut result = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}
fn token_text(usage: &Value, kind: &str) -> String {
    let (samples, partial) = match kind {
        "input" => (
            usage["input_usage_samples"].as_u64().unwrap_or(0),
            usage["input_usage_partial"] == true,
        ),
        "output" => (
            usage["output_usage_samples"].as_u64().unwrap_or(0),
            usage["output_usage_partial"] == true,
        ),
        _ => (
            usage["input_usage_samples"].as_u64().unwrap_or(0)
                + usage["output_usage_samples"].as_u64().unwrap_or(0),
            usage["usage_partial"] == true,
        ),
    };
    if samples == 0 && partial {
        return "unknown (partial)".into();
    }
    format!(
        "{}{}",
        grouped(usage[format!("{kind}_tokens")].as_u64().unwrap_or(0)),
        if partial { " (partial)" } else { "" }
    )
}
fn seconds(ms: u128) -> String {
    format!("{:.2} s", ms as f64 / 1000.0)
}
fn human_summary(report: &Report, output: &Path) -> String {
    use std::fmt::Write as _;
    let s = &report.summary;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "\nReviewer evaluation{}",
        if report.completed {
            ""
        } else {
            " (incomplete)"
        }
    );
    let _ = writeln!(
        text,
        "Provider: {} · Model: {} · Threshold (when probabilities exist): {}",
        report.provider,
        audit::safe_text(report.requested_model.as_deref().unwrap_or("default")),
        report.probability_threshold
    );
    let _ = writeln!(
        text,
        "Scenarios: {} · Repeat: {} · Concurrency: {} · Evaluations: {} / {}",
        report.cases.len(),
        report.repeat,
        report.concurrency,
        report.trials.len(),
        report.cases.len() * report.repeat as usize
    );
    let matched = s["matched"].as_u64().unwrap_or(0);
    let attempted = report.trials.len();
    let percent = if attempted == 0 {
        "n/a".into()
    } else {
        format!("{:.1}%", matched as f64 * 100.0 / attempted as f64)
    };
    let _ = writeln!(
        text,
        "\nResults\n  Matched expectations   {matched} / {attempted} ({percent})\n  False allows           {}\n  False denials          {}\n  Service errors         {}\n  Unstable scenarios     {}",
        s["false_allows"],
        s["false_denials"],
        s["errors"],
        s["unstable_cases"].as_array().map_or(0, Vec::len)
    );
    let _ = writeln!(
        text,
        "\nUsage & time\n  Tokens                 {} (input {} · output {})\n  Total elapsed          {}\n  Report                 {}",
        token_text(s, "total"),
        token_text(s, "input"),
        token_text(s, "output"),
        report
            .elapsed_ms
            .map(seconds)
            .unwrap_or_else(|| "unknown".into()),
        audit::safe_text(&output.display().to_string())
    );
    let mut rows = vec![vec![
        "Scenario".to_owned(),
        "Match".into(),
        "Errors".into(),
        "Tokens".into(),
        "Input".into(),
        "Output".into(),
        "Time".into(),
    ]];
    for case in s["cases"].as_array().into_iter().flatten() {
        rows.push(vec![
            case["id"].as_str().unwrap_or("").into(),
            format!("{}/{}", case["matched"], case["attempts"]),
            case["errors"].to_string(),
            token_text(case, "total"),
            token_text(case, "input"),
            token_text(case, "output"),
            seconds(case["duration_ms"].as_u64().unwrap_or(0) as u128),
        ]);
    }
    let widths: Vec<_> = (0..7)
        .map(|i| rows.iter().map(|row| row[i].len()).max().unwrap_or(0))
        .collect();
    text.push('\n');
    for row in rows {
        for (i, value) in row.iter().enumerate() {
            if i == 0 {
                let _ = write!(text, "{value:<width$}", width = widths[i]);
            } else {
                let _ = write!(text, "  {value:>width$}", width = widths[i]);
            }
        }
        text.push('\n');
    }
    if s["usage_partial"] == true {
        text.push_str(
            "\npartial: only reported usage is counted; missing usage is unknown, not zero.\n",
        );
    }
    if report.comparison.is_object() {
        let c = &report.comparison;
        text.push_str("\nCompared with baseline\n");
        for (key, label) in [
            ("improved", "Improved"),
            ("regressed", "Regressed"),
            ("inconclusive", "Inconclusive"),
        ] {
            let ids: Vec<_> = c[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(audit::safe_text)
                .collect();
            let _ = writeln!(
                text,
                "  {label:<13} {}{}",
                ids.len(),
                if ids.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", ids.join(", "))
                }
            );
        }
        let delta = c["token_delta"]
            .as_i64()
            .map(|n| format!("{n:+}"))
            .unwrap_or_else(|| "unknown (partial usage)".into());
        let elapsed = c["elapsed_ms_delta"]
            .as_i64()
            .map(|n| format!("{:+.2} s", n as f64 / 1000.0))
            .unwrap_or_else(|| "unknown (baseline has no elapsed time)".into());
        let _ = writeln!(text, "  Token change  {delta}\n  Time change   {elapsed}");
    }
    text
}

fn checkpoint(file: &mut File, report: &Report) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report)?;
    file.rewind()?;
    file.write_all(&bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.flush()?;
    Ok(())
}
struct TrialOptions {
    retries: u32,
    deadline: u64,
}

async fn run_trial(
    case: Case,
    repetition: u32,
    config: config::ReviewerConfig,
    questions: Value,
    options: TrialOptions,
    trial_dir: PathBuf,
    mut cancelled: tokio::sync::watch::Receiver<bool>,
) -> Trial {
    let TrialOptions { retries, deadline } = options;
    let start = Instant::now();
    let input = ReviewInput::from_fixture(&case.input, config.approver.context_budget_bytes);
    let (result, events) = audit::capture(async {
        let mut backend: Box<dyn Backend> = if config.approver.provider == config::Provider::Jev {
            Box::new(jev::JevBackend::for_evaluation(config.clone(), questions, retries)?)
        } else {
            crate::backend::for_config(&config, trial_dir.join("workspace"),
                trial_dir.join("reviewer_session.json"), "evaluation".into(), None, false)?
        };
        tokio::select! {
            biased;
            _ = cancelled.changed() => Err(anyhow::anyhow!("Evaluation interrupted")),
            result = tokio::time::timeout(std::time::Duration::from_secs(deadline), backend.review(&input)) =>
                result.context("Evaluation deadline exceeded").and_then(|result| result),
        }
    }).await;
    let (assessment, error, matched) = match result {
        Ok(review) => {
            let a = crate::policy::decide(review, &input, config.approver.probability_threshold);
            let matched = matches(&case, &a);
            (Some(a), None, Some(matched))
        }
        Err(error) => (None, Some(error.to_string()), None),
    };
    Trial {
        case_id: case.id,
        repetition,
        duration_ms: start.elapsed().as_millis(),
        assessment,
        error,
        matched,
        events,
    }
}

pub async fn run(options: Options) -> Result<()> {
    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    let cases = load_cases(&options.suite)?;
    let config = config::reviewer_config()?;
    let provider = config.approver.provider;
    ensure!(
        matches!(
            provider,
            config::Provider::Jev
                | config::Provider::Pi
                | config::Provider::Cli
                | config::Provider::Openai
        ),
        "reviewer-eval supports Jev, Pi, agy CLI and OpenAI providers"
    );
    ensure!(
        provider == config::Provider::Jev || (options.questions.is_none() && options.retries == 0),
        "--questions and --retries are Jev-only; conversational backends use prompt/ANY_AUTO_PROMPT"
    );
    let calls_per_trial = if provider == config::Provider::Cli {
        2
    } else {
        1
    };
    let upper_bound = (cases.len() as u64)
        * u64::from(options.repeat)
        * (u64::from(options.retries) + 1)
        * calls_per_trial;
    ensure!(
        upper_bound <= u64::from(options.max_calls),
        "Evaluation could make {upper_bound} backend calls, exceeding --max-calls {}",
        options.max_calls
    );
    let questions = if provider == config::Provider::Jev {
        let questions = match &options.questions {
            Some(path) => serde_json::from_slice(&read_bounded(path, 64 * 1024)?)
                .context("Invalid questions JSON")?,
            None => crate::prompts::jev_questions(&config.approver.instructions),
        };
        validate_questions(&questions)?;
        Some(questions)
    } else {
        None
    };
    let prompt = match provider {
        config::Provider::Jev => None,
        config::Provider::Cli => Some(crate::prompts::agy_session_prompt(&config.prompt)),
        _ => Some(config.prompt.clone()),
    };
    let suite_hash = hash(&cases)?;
    let baseline: Option<Report> = options
        .compare
        .as_ref()
        .map(|path| -> Result<Report> {
            let report: Report = serde_json::from_slice(&read_bounded(path, 64 * 1024 * 1024)?)
                .context("Invalid baseline report")?;
            ensure!(
                report.schema_version == 1
                    && report.completed
                    && report.suite_hash == hash(&report.cases)?
                    && report.suite_hash == suite_hash
                    && report.repeat == options.repeat,
                "Baseline must be a completed report for the identical suite and repeat count"
            );
            Ok(report)
        })
        .transpose()?;
    if provider == config::Provider::Jev {
        ensure!(
            !config.approver.api_key.trim().is_empty(),
            "Jev API key is empty"
        );
        jev::JevBackend::for_evaluation(
            config.clone(),
            questions.clone().unwrap(),
            options.retries,
        )?;
    }
    // Native reviewer state and workspaces are isolated from production and each other.
    let evaluation_dir = tempfile::Builder::new()
        .prefix("any-auto-eval-")
        .tempdir()?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&options.output)
        .with_context(|| format!("Cannot create new report {}", options.output.display()))?;
    let mut report = Report {
        schema_version: 1,
        build_version: env!("CARGO_PKG_VERSION").into(),
        started_at,
        completed: false,
        elapsed_ms: None,
        suite_hash,
        questions_hash: questions.as_ref().map(hash).transpose()?,
        questions,
        prompt_hash: prompt.as_ref().map(hash).transpose()?,
        prompt,
        provider: provider.as_str().into(),
        decision_policy_version: crate::policy::DECISION_POLICY_VERSION.into(),
        base_rubric_version: crate::policy::RUBRIC_VERSION.into(),
        requested_model: config.approver.model.clone(),
        probability_threshold: config.approver.probability_threshold,
        repeat: options.repeat,
        concurrency: options.concurrency,
        retries: options.retries,
        max_calls: options.max_calls,
        cases,
        trials: Vec::new(),
        summary: Value::Null,
        comparison: Value::Null,
    };
    checkpoint(&mut output, &report)?;
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    let (cancel_tx, _) = tokio::sync::watch::channel(false);
    let mut cancelled = false;
    let mut pending: VecDeque<_> = report
        .cases
        .iter()
        .flat_map(|case| (1..=options.repeat).map(move |repetition| (case.clone(), repetition)))
        .collect();
    let mut workers = tokio::task::JoinSet::new();
    let mut next_trial = 0;
    loop {
        while !cancelled && workers.len() < options.concurrency as usize {
            let Some((case, repetition)) = pending.pop_front() else {
                break;
            };
            let trial_dir = evaluation_dir.path().join(format!("trial-{next_trial}"));
            next_trial += 1;
            workers.spawn(run_trial(
                case,
                repetition,
                config.clone(),
                report.questions.clone().unwrap_or(Value::Null),
                TrialOptions {
                    retries: options.retries,
                    deadline: options.deadline,
                },
                trial_dir,
                cancel_tx.subscribe(),
            ));
        }
        if workers.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            _ = &mut interrupted, if !cancelled => {
                cancelled = true;
                let _ = cancel_tx.send(true);
            }
            result = workers.join_next() => {
                let trial = result.expect("active workers").context("Evaluation worker failed")?;
                eprintln!("{} {}/{}: {}", trial.case_id, trial.repetition, options.repeat,
                    if trial.error.is_some() { "ERROR" } else if trial.matched == Some(true) { "PASS" } else { "FAIL" });
                report.trials.push(trial);
                report.trials.sort_by(|a,b| (&a.case_id,a.repetition).cmp(&(&b.case_id,b.repetition)));
                checkpoint(&mut output, &report)?;
            }
        }
    }
    report.completed =
        !cancelled && report.trials.len() == report.cases.len() * options.repeat as usize;
    report.elapsed_ms = Some(started.elapsed().as_millis());
    report.summary = summarize(&report.cases, &report.trials);
    report.summary["elapsed_ms"] = json!(report.elapsed_ms);
    if let Some(baseline) = &baseline {
        report.comparison = comparison(&report, baseline);
    }
    checkpoint(&mut output, &report)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"report":options.output,
            "completed":report.completed,"summary":report.summary,"comparison":report.comparison}))?
        );
    } else {
        print!("{}", human_summary(&report, &options.output));
    }
    ensure!(
        report.completed,
        "Evaluation interrupted; completed trials saved"
    );
    ensure!(
        report.trials.iter().all(|t| t.error.is_none()),
        "Evaluation had service errors; see report"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn deadline_argument_defaults_and_validation() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            options: super::Options,
        }
        let base = ["eval", "--suite", "suite.jsonl", "--output", "report.json"];
        assert_eq!(Cli::try_parse_from(base).unwrap().options.deadline, 45);
        assert_eq!(
            Cli::try_parse_from(base.into_iter().chain(["--deadline", "90"]))
                .unwrap()
                .options
                .deadline,
            90
        );
        assert!(Cli::try_parse_from(base.into_iter().chain(["--deadline", "0"])).is_err());
    }

    use super::*;
    #[test]
    fn usage_preserves_partial_components_and_supports_old_reports() {
        let trial = |events: Vec<Value>| Trial {
            case_id: "a".into(),
            repetition: 1,
            duration_ms: 1200,
            assessment: None,
            error: None,
            matched: None,
            events,
        };
        let old = trial(vec![
            json!({"event":"backend_request"}),
            json!({"event":"backend_response","data":{"usage_delta":{"input_tokens":1000,"output_tokens":20}}}),
        ]);
        let partial = trial(vec![
            json!({"event":"backend_request"}),
            json!({"event":"jev_attempt_usage","data":{"usage_delta":{"input_tokens":7,"output_tokens":null}}}),
        ]);
        let failed_response = trial(vec![
            json!({"event":"backend_request"}),
            json!({"event":"backend_usage","data":{"usage_delta":{"input_tokens":10,"output_tokens":5}}}),
            json!({"event":"backend_error","data":{"error":"OpenAI response incomplete or failed"}}),
        ]);
        let failed_usage = usage_summary(&[&failed_response]);
        assert_eq!(failed_usage["total_tokens"], 15);
        assert_eq!(failed_usage["usage_partial"], false);
        let usage = usage_summary(&[&old, &partial]);
        assert_eq!(usage["total_tokens"], 1027);
        assert_eq!(usage["duration_ms"], 2400);
        assert_eq!(token_text(&usage, "total"), "1,027 (partial)");
        assert_eq!(token_text(&usage, "input"), "1,007");
        assert_eq!(token_text(&usage, "output"), "20 (partial)");
        let missing = usage_summary(&[&partial]);
        assert_eq!(token_text(&missing, "output"), "unknown (partial)");
        let unavailable = trial(vec![json!({"event":"backend_request"})]);
        assert_eq!(
            token_text(&usage_summary(&[&unavailable]), "total"),
            "unknown (partial)"
        );
    }
    #[test]
    fn maintained_suites_and_rubric_validate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/suites");
        let paths = vec![root.join("scenarios.jsonl")];
        assert_eq!(load_cases(&paths).unwrap().len(), 45);
        let mut questions = crate::prompts::jev_questions(&config::JevInstructions::default());
        validate_questions(&questions).unwrap();
        questions["risk"]["criteria"]["surprise"] = json!("Invalid option");
        assert!(validate_questions(&questions).is_err());
    }
}
