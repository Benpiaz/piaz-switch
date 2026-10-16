//! The Usage pane's report: what the tools used and would have cost over a
//! range, built from the ledger's entries and the price table. Pure: no
//! Tauri, no clock, no files.
//!
//! Only numbers, model names and tool names go in here; the report goes to
//! the webview as JSON.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Timelike};
use pitstop_core::ledger::{Entry, Tokens};
use pitstop_core::pricing::{Price, Prices};
use pitstop_core::ProviderId;
use serde::Serialize;

/// The ranges the pane offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    Quarter,
}

impl Range {
    pub fn parse(id: &str) -> Option<Range> {
        match id {
            "24h" => Some(Range::Day),
            "7d" => Some(Range::Week),
            "30d" => Some(Range::Month),
            "90d" => Some(Range::Quarter),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Range::Day => "24h",
            Range::Week => "7d",
            Range::Month => "30d",
            Range::Quarter => "90d",
        }
    }
}

/// The columns of the chart: where each starts, and where the last ends,
/// in epoch seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buckets {
    pub hourly: bool,
    pub starts: Vec<i64>,
    pub end: i64,
}

impl Buckets {
    fn index(&self, at: i64) -> Option<usize> {
        if at >= self.end || self.starts.first().is_none_or(|first| at < *first) {
            return None;
        }
        Some(self.starts.partition_point(|start| *start <= at) - 1)
    }
}

/// The columns for `range` as of `now`, in `now`'s time zone: the past 24
/// hours by the hour, otherwise whole days up to the end of today.
pub fn buckets<Tz: TimeZone>(range: Range, now: &DateTime<Tz>) -> Buckets {
    let days = match range {
        Range::Day => {
            let this_hour = now.timestamp() - i64::from(now.minute() * 60 + now.second());
            return Buckets {
                hourly: true,
                starts: (0..24).map(|h| this_hour - (23 - h) * 3_600).collect(),
                end: this_hour + 3_600,
            };
        }
        Range::Week => 7,
        Range::Month => 30,
        Range::Quarter => 90,
    };
    let today = now.date_naive();
    // A day starts at its first local midnight; where a clock change skips
    // midnight, at the moment it would have been.
    let midnight = |date: chrono::NaiveDate| {
        let local = date.and_time(NaiveTime::MIN);
        now.timezone()
            .from_local_datetime(&local)
            .earliest()
            .map(|t| t.timestamp())
            .unwrap_or_else(|| local.and_utc().timestamp())
    };
    Buckets {
        hourly: false,
        starts: (0..days)
            .map(|d| midnight(today - Duration::days(days - 1 - d)))
            .collect(),
        end: midnight(today + Duration::days(1)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub range: &'static str,
    /// `hour` or `day`.
    pub bucket: &'static str,
    pub starts: Vec<i64>,
    pub end: i64,
    /// Everything, then each tool alone.
    pub views: Vec<View>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    /// `all`, `claude-code` or `codex`.
    pub id: &'static str,
    /// Dollars, priced tokens only.
    pub cost: f64,
    /// Tokens of models without a price.
    pub unpriced_tokens: u64,
    pub sessions: usize,
    pub totals: Totals,
    pub cache_savings: f64,
    pub tools: Vec<ToolRow>,
    pub series: Vec<Series>,
    pub models: Vec<ModelRow>,
    /// Columns that used tokens, newest first.
    pub days: Vec<DayRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub processed: u64,
    pub cache_read: u64,
    pub input: u64,
    pub cache_write: u64,
    pub output: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRow {
    pub id: &'static str,
    pub name: &'static str,
    pub sessions: usize,
    pub cost: f64,
    pub tokens: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

/// One tool's line on the chart, a value per column.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub tool: &'static str,
    pub cost: Vec<f64>,
    pub tokens: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub tool: &'static str,
    pub model: String,
    /// None when the model has no price.
    pub cost: Option<f64>,
    pub tokens: u64,
    pub cost_share: Option<f64>,
    pub token_share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayRow {
    pub start: i64,
    pub cost: f64,
    pub tokens: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

pub fn report<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    prices: &Prices,
    range: Range,
    buckets: &Buckets,
) -> UsageReport {
    let columns = buckets.starts.len();
    let mut all = Tally::new(columns);
    let mut by_tool: BTreeMap<ProviderId, Tally> = BTreeMap::new();
    let mut priced: HashMap<Arc<str>, Option<&Price>> = HashMap::new();
    for entry in entries {
        let Some(column) = buckets.index(entry.at) else {
            continue;
        };
        let price = *priced
            .entry(entry.model.clone())
            .or_insert_with(|| prices.get(&entry.model));
        all.add(entry, column, price);
        by_tool
            .entry(entry.tool)
            .or_insert_with(|| Tally::new(columns))
            .add(entry, column, price);
    }

    let mut views = vec![all.view("all", buckets)];
    for tool in ProviderId::ALL {
        let tally = by_tool.remove(&tool).unwrap_or_else(|| Tally::new(columns));
        views.push(tally.view(tool.as_str(), buckets));
    }
    UsageReport {
        range: range.id(),
        bucket: if buckets.hourly { "hour" } else { "day" },
        starts: buckets.starts.clone(),
        end: buckets.end,
        views,
    }
}

/// Running sums for one view.
struct Tally {
    columns: usize,
    cost: f64,
    unpriced_tokens: u64,
    tokens: Tokens,
    savings: f64,
    tools: BTreeMap<ProviderId, ToolTally>,
    /// (tool, model) → cost (none when unpriced), tokens.
    models: BTreeMap<(ProviderId, Arc<str>), (Option<f64>, u64)>,
    /// Per column: cost, tokens.
    days: Vec<(f64, u64)>,
}

struct ToolTally {
    sessions: HashSet<Arc<str>>,
    cost: f64,
    tokens: u64,
    cost_by_column: Vec<f64>,
    tokens_by_column: Vec<u64>,
}

fn share<T: Into<f64>>(part: T, whole: T) -> f64 {
    let whole = whole.into();
    if whole > 0.0 {
        part.into() / whole
    } else {
        0.0
    }
}

impl Tally {
    fn new(columns: usize) -> Self {
        Tally {
            columns,
            cost: 0.0,
            unpriced_tokens: 0,
            tokens: Tokens::default(),
            savings: 0.0,
            tools: BTreeMap::new(),
            models: BTreeMap::new(),
            days: vec![(0.0, 0); columns],
        }
    }

    fn add(&mut self, entry: &Entry, column: usize, price: Option<&Price>) {
        let processed = entry.tokens.processed();
        let cost = price.map(|p| p.cost(&entry.tokens));
        let spent = cost.unwrap_or(0.0);
        self.cost += spent;
        if cost.is_none() {
            self.unpriced_tokens += processed;
        }
        self.savings += price.map_or(0.0, |p| p.savings(&entry.tokens));
        self.tokens.add(&entry.tokens);

        let columns = self.columns;
        let tool = self.tools.entry(entry.tool).or_insert_with(|| ToolTally {
            sessions: HashSet::new(),
            cost: 0.0,
            tokens: 0,
            cost_by_column: vec![0.0; columns],
            tokens_by_column: vec![0; columns],
        });
        tool.sessions.insert(entry.session.clone());
        tool.cost += spent;
        tool.tokens += processed;
        tool.cost_by_column[column] += spent;
        tool.tokens_by_column[column] += processed;

        let model = self
            .models
            .entry((entry.tool, entry.model.clone()))
            .or_insert((price.map(|_| 0.0), 0));
        if let (Some(sum), Some(cost)) = (&mut model.0, cost) {
            *sum += cost;
        }
        model.1 += processed;

        self.days[column].0 += spent;
        self.days[column].1 += processed;
    }

    fn view(self, id: &'static str, buckets: &Buckets) -> View {
        let processed = self.tokens.processed();
        let tools = self
            .tools
            .iter()
            .map(|(tool, t)| ToolRow {
                id: tool.as_str(),
                name: tool.display_name(),
                sessions: t.sessions.len(),
                cost: t.cost,
                tokens: t.tokens,
                cost_share: share(t.cost, self.cost),
                token_share: share(t.tokens as f64, processed as f64),
            })
            .collect();
        let series = self
            .tools
            .iter()
            .map(|(tool, t)| Series {
                tool: tool.as_str(),
                cost: t.cost_by_column.clone(),
                tokens: t.tokens_by_column.clone(),
            })
            .collect();
        let mut models: Vec<ModelRow> = self
            .models
            .into_iter()
            .filter(|(_, (_, tokens))| *tokens > 0)
            .map(|((tool, model), (cost, tokens))| ModelRow {
                tool: tool.as_str(),
                model: model.to_string(),
                cost,
                tokens,
                cost_share: cost.map(|c| share(c, self.cost)),
                token_share: share(tokens as f64, processed as f64),
            })
            .collect();
        // Dearest first, unpriced last; ties by tokens, then by name.
        models.sort_by(|a, b| {
            b.cost
                .is_some()
                .cmp(&a.cost.is_some())
                .then(b.cost.unwrap_or(0.0).total_cmp(&a.cost.unwrap_or(0.0)))
                .then(b.tokens.cmp(&a.tokens))
                .then(a.model.cmp(&b.model))
        });
        let days = self
            .days
            .iter()
            .zip(&buckets.starts)
            .filter(|((_, tokens), _)| *tokens > 0)
            .rev()
            .map(|(&(cost, tokens), &start)| DayRow {
                start,
                cost,
                tokens,
                cost_share: share(cost, self.cost),
                token_share: share(tokens as f64, processed as f64),
            })
            .collect();
        View {
            id,
            cost: self.cost,
            unpriced_tokens: self.unpriced_tokens,
            sessions: self.tools.values().map(|t| t.sessions.len()).sum(),
            totals: Totals {
                processed,
                cache_read: self.tokens.cache_read,
                input: self.tokens.input,
                cache_write: self.tokens.cache_write(),
                output: self.tokens.output,
            },
            cache_savings: self.savings,
            tools,
            series,
            models,
            days,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use pitstop_core::pricing::Table;

    fn vietnam() -> FixedOffset {
        FixedOffset::east_opt(7 * 3_600).unwrap()
    }

    /// 2026-10-15 04:56 in Vietnam.
    fn now() -> DateTime<FixedOffset> {
        vietnam().timestamp_opt(1_792_014_960, 0).unwrap()
    }

    #[test]
    fn thirty_days_are_whole_local_days_ending_with_today() {
        let b = buckets(Range::Month, &now());
        assert!(!b.hourly);
        assert_eq!(b.starts.len(), 30);
        assert_eq!(b.starts[0], 1_789_491_600, "Sep 16 00:00 +07");
        assert_eq!(b.starts[1] - b.starts[0], 86_400);
        assert_eq!(b.end, 1_792_083_600, "Oct 16 00:00 +07");
    }

    #[test]
    fn the_past_24_hours_are_hours_ending_with_this_one() {
        let b = buckets(Range::Day, &now());
        assert!(b.hourly);
        assert_eq!(b.starts.len(), 24);
        assert_eq!(b.starts[0], 1_791_928_800, "Oct 14 05:00 +07");
        assert_eq!(b.end, 1_792_015_200, "Oct 15 05:00 +07");
    }

    fn price(input: f64, output: f64, write: Option<f64>, read: f64) -> Price {
        Price {
            input,
            output,
            cache_write: write,
            cache_write_1h: None,
            cache_read: Some(read),
            long_input: None,
            long_output: None,
            long_cache_write: None,
            long_cache_read: None,
        }
    }

    fn prices() -> Prices {
        let mut table = Table::default();
        table.models.insert(
            "claude-opus-5-5".into(),
            price(4e-6, 2e-5, Some(5e-6), 2e-7),
        );
        table
            .models
            .insert("gpt-5.6-sol".into(), price(4e-6, 2e-5, None, 4e-7));
        Prices::new(table, None)
    }

    fn entry(tool: ProviderId, session: &str, model: &str, at: i64, tokens: Tokens) -> Entry {
        Entry {
            at,
            tool,
            session: Arc::from(session),
            model: Arc::from(model),
            tokens,
        }
    }

    fn tokens(input: u64, cache_read: u64, output: u64) -> Tokens {
        Tokens {
            input,
            cache_read,
            output,
            ..Tokens::default()
        }
    }

    /// A week of both tools: opus on Oct 14, an unpriced model and Codex on
    /// Oct 15, a session that only failed on Oct 13, and opus on Oct 1,
    /// before the week.
    fn week() -> UsageReport {
        use ProviderId::{ClaudeCode, Codex};
        let entries = [
            entry(
                ClaudeCode,
                "a",
                "claude-opus-5-5",
                1_791_946_800,
                tokens(1_000, 10_000, 500),
            ),
            entry(
                ClaudeCode,
                "a",
                "deepseek-v4-1-flash",
                1_792_000_800,
                tokens(0, 2_000, 100),
            ),
            entry(
                Codex,
                "x",
                "gpt-5.6-sol",
                1_792_004_400,
                tokens(200, 800, 50),
            ),
            entry(
                ClaudeCode,
                "b",
                "<synthetic>",
                1_791_867_600,
                Tokens::default(),
            ),
            entry(
                ClaudeCode,
                "c",
                "claude-opus-5-5",
                1_790_830_800,
                tokens(9, 9, 9),
            ),
        ];
        report(
            &entries,
            &prices(),
            Range::Week,
            &buckets(Range::Week, &now()),
        )
    }

    fn view<'r>(report: &'r UsageReport, id: &str) -> &'r View {
        report.views.iter().find(|v| v.id == id).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_whole_view_adds_up_cost_tokens_sessions_and_savings_in_range() {
        let report = week();
        let all = view(&report, "all");
        // opus 0.004 + 0.002 + 0.010; codex 0.0008 + 0.00032 + 0.001
        assert!(close(all.cost, 0.01812), "{}", all.cost);
        assert_eq!(all.unpriced_tokens, 2_100);
        assert_eq!(all.sessions, 3, "a, b and x; c ran before the week");
        assert_eq!(
            all.totals,
            Totals {
                processed: 14_650,
                cache_read: 12_800,
                input: 1_200,
                cache_write: 0,
                output: 650,
            }
        );
        // 10_000 × 3.8e-6 + 800 × 3.6e-6
        assert!(close(all.cache_savings, 0.04088), "{}", all.cache_savings);
    }

    #[test]
    fn each_tool_has_its_share_of_cost_and_tokens() {
        let report = week();
        let tools = &view(&report, "all").tools;
        let ids: Vec<&str> = tools.iter().map(|t| t.id).collect();
        assert_eq!(ids, ["claude-code", "codex"]);
        assert_eq!(tools[0].sessions, 2);
        assert_eq!(tools[0].tokens, 13_600);
        assert!(close(tools[0].cost, 0.016));
        assert!(
            close(tools[0].cost_share, 0.883_002_207_5),
            "{}",
            tools[0].cost_share
        );
        assert!(
            close(tools[0].token_share, 0.928_327_645_0),
            "{}",
            tools[0].token_share
        );
        assert_eq!(tools[1].name, "Codex");
        assert_eq!(tools[1].sessions, 1);
    }

    #[test]
    fn models_are_listed_per_tool_with_unpriced_ones_last_and_empty_ones_left_out() {
        let report = week();
        let models: Vec<(&str, &str, Option<f64>, u64)> = view(&report, "all")
            .models
            .iter()
            .map(|m| {
                (
                    m.tool,
                    m.model.as_str(),
                    m.cost.map(|c| (c * 1e5).round() / 1e5),
                    m.tokens,
                )
            })
            .collect();
        assert_eq!(
            models,
            [
                ("claude-code", "claude-opus-5-5", Some(0.016), 11_500),
                ("codex", "gpt-5.6-sol", Some(0.00212), 1_050),
                ("claude-code", "deepseek-v4-1-flash", None, 2_100),
            ]
        );
        let unpriced = &view(&report, "all").models[2];
        assert_eq!(unpriced.cost_share, None);
    }

    #[test]
    fn days_that_used_tokens_are_listed_newest_first_and_charted_per_tool() {
        let report = week();
        let all = view(&report, "all");
        let days: Vec<(i64, u64)> = all.days.iter().map(|d| (d.start, d.tokens)).collect();
        assert_eq!(
            days,
            [(1_791_997_200, 3_150), (1_791_910_800, 11_500)],
            "Oct 15 and Oct 14; Oct 13 used nothing"
        );
        let claude = all.series.iter().find(|s| s.tool == "claude-code").unwrap();
        assert_eq!(claude.tokens, [0, 0, 0, 0, 0, 11_500, 2_100]);
        let codex = all.series.iter().find(|s| s.tool == "codex").unwrap();
        assert_eq!(codex.tokens, [0, 0, 0, 0, 0, 0, 1_050]);
    }

    #[test]
    fn models_are_ordered_by_what_they_cost_not_by_their_tokens() {
        use ProviderId::ClaudeCode;
        let entries = [
            // 10_000 reads × 4e-7 = $0.004
            entry(
                ClaudeCode,
                "a",
                "gpt-5.6-sol",
                1_791_946_800,
                tokens(0, 10_000, 0),
            ),
            // 1_000 output × 2e-5 = $0.02
            entry(
                ClaudeCode,
                "a",
                "claude-opus-5-5",
                1_791_946_800,
                tokens(0, 0, 1_000),
            ),
        ];
        let report = report(
            &entries,
            &prices(),
            Range::Week,
            &buckets(Range::Week, &now()),
        );
        let models: Vec<&str> = view(&report, "all")
            .models
            .iter()
            .map(|m| m.model.as_str())
            .collect();
        assert_eq!(models, ["claude-opus-5-5", "gpt-5.6-sol"]);
    }

    #[test]
    fn requests_stamped_after_the_range_ends_are_left_out() {
        let entries = [entry(
            ProviderId::Codex,
            "late",
            "gpt-5.6-sol",
            1_792_087_200, // Oct 16 01:00 +07, past the end of today
            tokens(1, 1, 1),
        )];
        let report = report(
            &entries,
            &prices(),
            Range::Week,
            &buckets(Range::Week, &now()),
        );
        assert_eq!(view(&report, "all").sessions, 0);
        assert_eq!(view(&report, "all").totals.processed, 0);
    }

    #[test]
    fn nothing_said_in_a_session_reaches_the_window() {
        use pitstop_core::ledger::{Ledger, Roots};
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("C--secret-project")).unwrap();
        std::fs::write(
            projects.join("C--secret-project").join("s-1.jsonl"),
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"SECRET-PROMPT"},"cwd":"C:\\secret-project","sessionId":"s-1","timestamp":"2026-10-14T03:00:00Z"}"#,
                "\n",
                r#"{"type":"assistant","cwd":"C:\\secret-project","sessionId":"s-1","requestId":"r","timestamp":"2026-10-14T03:00:01Z","message":{"id":"m","model":"claude-opus-5-5","content":[{"type":"text","text":"SECRET-ANSWER"}],"usage":{"input_tokens":3,"output_tokens":4}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let mut ledger = Ledger::default();
        ledger.scan(
            &Roots {
                claude: vec![projects],
                codex: Vec::new(),
            },
            std::time::SystemTime::now(),
        );
        let report = report(
            ledger.entries(),
            &prices(),
            Range::Week,
            &buckets(Range::Week, &now()),
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            json.contains("claude-opus-5-5"),
            "the request is in the report: {json}"
        );
        for secret in ["SECRET", "secret-project"] {
            assert!(!json.contains(secret), "{secret} leaked into {json}");
        }
    }

    #[test]
    fn a_tool_view_holds_that_tool_alone() {
        let report = week();
        let codex = view(&report, "codex");
        assert!(close(codex.cost, 0.00212));
        assert_eq!(codex.sessions, 1);
        assert_eq!(codex.totals.processed, 1_050);
        assert_eq!(codex.tools.len(), 1);
        assert_eq!(codex.models.len(), 1);
        assert_eq!(codex.series.len(), 1);
        assert!(close(codex.tools[0].cost_share, 1.0));
    }
}
