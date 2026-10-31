//! Best-effort usage, read the way the tools themselves read it:
//! - Claude Code: `GET https://api.anthropic.com/api/oauth/usage`, as `/usage` does;
//! - Codex: `GET https://chatgpt.com/backend-api/wham/usage` with the account's
//!   `ChatGPT-Account-ID`, as the Codex CLI sends it;
//! - Cursor: `POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage`,
//!   as the editor's own usage panel does, then `…/GetPlanInfo` for the plan
//!   as its site names it;
//! - Antigravity: not asked at all. The quota is the one Antigravity itself
//!   last read and kept with the login (see `provider::antigravity::quota`).
//!
//! Pitstop never refreshes a live login. Refreshing rotates the refresh
//! token, and the live login is the tool's to renew — two holders renewing
//! one chain would break it for both. A parked Claude Code login has one
//! holder, the vault, so Pitstop renews it the way Claude Code would once its
//! access token lapses (see `Engine::renew_parked`), and its usage can still
//! be read. Any other parked account whose access token has lapsed shows as
//! expired until it is switched to and the tool renews it.

use std::time::Duration;

use serde_json::Value;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub const CURSOR_USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
pub const CURSOR_PLAN_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo";
/// Where Claude Code renews its login, and the client it signs in as.
pub const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// How long before its access token lapses a parked login is renewed, so a
/// read every few minutes never finds it lapsed.
pub const RENEW_AHEAD: i64 = 5 * 60;

/// One rate-limit window, e.g. the five-hour session or the weekly limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub label: String,
    pub percent: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
    /// How long the window runs, when known, so its pace can be judged.
    pub length_secs: Option<i64>,
    /// A per-model limit nobody has used yet: kept so a total over several
    /// accounts counts it as 0%, left out wherever one account's limits
    /// are listed.
    pub quiet: bool,
    /// A limit whose period has not begun: nothing used and no clock
    /// running. Its period starts with the first use, so it has no reset yet.
    pub idle: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub windows: Vec<Window>,
    /// The plan as the service names it now, where the answer says (Cursor):
    /// fresher than the one recorded when the login was parked.
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsageReport {
    Ok(Usage),
    /// The parked access token has lapsed; switching to the account lets
    /// Claude Code renew it.
    Expired,
    /// Anthropic refused the token (revoked or signed out elsewhere), or the
    /// login holds nothing that renews any more.
    Unauthorized,
    Failed(String),
}

/// What a read the service turned away for coming too often came to.
pub const RATE_LIMITED: &str = "rate limited, try again later";

/// How a read that never got an answer (offline, a stalled connection)
/// starts.
pub const NETWORK: &str = "network: ";

/// How long one try at a read may take. Slow links take seconds just to
/// shake hands, so this is generous.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The pause before the one retry of a read that got no answer.
const RETRY_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum FetchError {
    Unauthorized,
    Other(String),
}

/// The network calls, as a seam so the engine is testable offline.
pub trait UsageApi: Sync {
    /// Anthropic's answer for a Claude Code access token.
    fn claude(&self, access_token: &str) -> Result<Value, FetchError>;
    /// OpenAI's answer for a Codex access token and its ChatGPT account.
    fn codex(&self, access_token: &str, account_id: &str) -> Result<Value, FetchError>;
    /// Cursor's answer for its access token: this billing period's use,
    /// with the plan's `planInfo` beside it when that could be read.
    fn cursor(&self, access_token: &str) -> Result<Value, FetchError>;
    /// Anthropic's new tokens for a Claude Code login, asked with its refresh
    /// token as Claude Code asks; a refresh token it no longer takes is
    /// [`FetchError::Unauthorized`].
    fn renew_claude(&self, renewal: &ClaudeRenewal) -> Result<Value, FetchError>;
}

/// Anthropic and OpenAI over the network.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(READ_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

/// Both services answer usage the same way: JSON on 200, a refusal on
/// 401/403.
fn answer(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    service: &str,
) -> Result<Value, FetchError> {
    let mut response = response.map_err(|e| FetchError::Other(format!("{NETWORK}{e}")))?;
    match response.status().as_u16() {
        200 => {
            let body = response
                .body_mut()
                .read_to_string()
                .map_err(|e| FetchError::Other(format!("{NETWORK}{e}")))?;
            serde_json::from_str(&body)
                .map_err(|e| FetchError::Other(format!("unreadable answer: {e}")))
        }
        401 | 403 => Err(FetchError::Unauthorized),
        429 => Err(FetchError::Other(RATE_LIMITED.into())),
        status => Err(FetchError::Other(format!("{service} answered {status}"))),
    }
}

/// A request that got no answer at all is tried once more: on a flaky link
/// the next try usually goes through. One that got an answer never is.
fn call_twice(
    call: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    call().or_else(|_| {
        std::thread::sleep(RETRY_AFTER);
        call()
    })
}

impl UsageApi for Http {
    fn claude(&self, access_token: &str) -> Result<Value, FetchError> {
        let response = call_twice(|| {
            self.agent
                .get(USAGE_URL)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("anthropic-beta", "oauth-2025-04-20")
                .call()
        });
        answer(response, "Anthropic")
    }

    /// A standalone GET: no model request, no quota
    /// spent. `ChatGPT-Account-ID` is required.
    fn codex(&self, access_token: &str, account_id: &str) -> Result<Value, FetchError> {
        let response = call_twice(|| {
            self.agent
                .get(CODEX_USAGE_URL)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("ChatGPT-Account-ID", account_id)
                .call()
        });
        answer(response, "OpenAI")
    }

    fn cursor(&self, access_token: &str) -> Result<Value, FetchError> {
        let post = |url: &str| {
            self.agent
                .post(url)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("Connect-Protocol-Version", "1")
                .header("Content-Type", "application/json")
                .send("{}")
        };
        let mut body = answer(call_twice(|| post(CURSOR_USAGE_URL)), "Cursor")?;
        // The plan only names the use; the use reads without it.
        if let Ok(plan) = answer(post(CURSOR_PLAN_URL), "Cursor") {
            if let (Some(body), Some(info)) = (body.as_object_mut(), plan.get("planInfo")) {
                body.insert("planInfo".into(), info.clone());
            }
        }
        Ok(body)
    }

    /// Asked once, never twice: an answer lost on the way back may already
    /// have used the refresh token up.
    fn renew_claude(&self, renewal: &ClaudeRenewal) -> Result<Value, FetchError> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": renewal.refresh_token,
            "client_id": renewal.client_id,
            "scope": renewal.scopes,
        })
        .to_string();
        let mut response = self
            .agent
            .post(CLAUDE_TOKEN_URL)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|e| FetchError::Other(format!("{NETWORK}{e}")))?;
        let status = response.status().as_u16();
        if status == 200 {
            return answer(Ok(response), "Anthropic");
        }
        let said = response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok());
        Err(renewal_refused(status, said.as_ref()))
    }
}

/// What a renewal Anthropic answered with `status` (not 200) and `said`
/// came to. Only `invalid_grant` says the refresh token is no longer taken;
/// any other refusal (a wrong client, a scope) is no reason to sign in again.
fn renewal_refused(status: u16, said: Option<&Value>) -> FetchError {
    match (status, said.and_then(|s| s.get("error")?.as_str())) {
        (400 | 401, Some("invalid_grant")) => FetchError::Unauthorized,
        (429, _) => FetchError::Other(RATE_LIMITED.into()),
        (status, error) => FetchError::Other(format!(
            "Anthropic answered {status}{}",
            error.map(|e| format!(" ({e})")).unwrap_or_default()
        )),
    }
}

/// The access token inside a claude-code vault blob. Deliberately not `Debug`.
pub(crate) enum Token {
    Valid(String),
    Expired,
    /// Nothing in it works or renews: only signing in again mends it.
    SignedOut,
    Missing,
}

/// Pull `claudeAiOauth.accessToken` out of a claude-code blob, unless its
/// `expiresAt` (epoch ms) is already past. A login is signed out when its
/// tokens are blank, as Claude Code leaves them once a renewal is refused, or
/// when it lapsed with a refresh token that is blank or past
/// `refreshTokenExpiresAt`: switching to it would leave Claude Code signed out.
pub(crate) fn claude_token(blob: &[u8], now_secs: i64) -> Token {
    let credentials = serde_json::from_slice::<Value>(blob)
        .ok()
        .and_then(|b| b.get("credentials")?.as_str().map(str::to_owned))
        .and_then(|c| serde_json::from_str::<Value>(&c).ok());
    let Some(oauth) = credentials.as_ref().and_then(|c| c.get("claudeAiOauth")) else {
        return Token::Missing;
    };
    let Some(token) = oauth.get("accessToken").and_then(Value::as_str) else {
        return Token::Missing;
    };
    if token.is_empty() {
        return Token::SignedOut;
    }
    let past = |key: &str| {
        oauth
            .get(key)
            .and_then(Value::as_i64)
            .is_some_and(|ms| ms / 1000 <= now_secs)
    };
    if !past("expiresAt") {
        return Token::Valid(token.to_owned());
    }
    // A login with no refresh token at all is left for Claude Code to judge.
    let refresh_blank = oauth.get("refreshToken").and_then(Value::as_str) == Some("");
    if refresh_blank || past("refreshTokenExpiresAt") {
        Token::SignedOut
    } else {
        Token::Expired
    }
}

/// What renews a Claude Code login: its refresh token, the scopes it was
/// granted and the client it was issued to. Deliberately not `Debug`.
pub struct ClaudeRenewal {
    pub refresh_token: String,
    /// Space-separated, as the request sends them.
    pub scopes: String,
    pub client_id: String,
}

/// The `claudeAiOauth` inside a claude-code vault blob.
fn claude_oauth(blob: &[u8]) -> Option<Value> {
    let blob = serde_json::from_slice::<Value>(blob).ok()?;
    let credentials = serde_json::from_str::<Value>(blob.get("credentials")?.as_str()?).ok()?;
    credentials.get("claudeAiOauth").cloned()
}

/// The refresh token of a claude-code blob: two logins holding the same one
/// are one chain, whoever they say they belong to.
pub(crate) fn claude_refresh_token(blob: &[u8]) -> Option<String> {
    claude_oauth(blob)?
        .get("refreshToken")?
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// What renews the login in a claude-code blob, when its access token has
/// lapsed or lapses within `ahead` seconds of `now_secs` and the login holds
/// a refresh token and its scopes. A token without an expiry never lapses.
pub(crate) fn claude_renewal(blob: &[u8], now_secs: i64, ahead: i64) -> Option<ClaudeRenewal> {
    let oauth = claude_oauth(blob)?;
    let expires_ms = oauth.get("expiresAt").and_then(Value::as_i64)?;
    if expires_ms / 1000 > now_secs + ahead {
        return None;
    }
    let refresh_token = oauth.get("refreshToken")?.as_str()?.to_owned();
    let scopes: Vec<&str> = oauth
        .get("scopes")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if refresh_token.is_empty() || scopes.is_empty() {
        return None;
    }
    let client_id = oauth
        .get("clientId")
        .and_then(Value::as_str)
        .unwrap_or(CLAUDE_CLIENT_ID)
        .to_owned();
    Some(ClaudeRenewal {
        refresh_token,
        scopes: scopes.join(" "),
        client_id,
    })
}

/// `blob` holding the tokens of Anthropic's `answer` to a renewal at
/// `now_secs`, as Claude Code keeps them: the new access token and when it
/// lapses, the new refresh token (the old one when none came) and the scopes
/// granted. All else in the blob stays. An answer without a lifetime is
/// still kept, taken as good for an hour: the refresh token it carries may
/// be the only one left. `None` when the answer lacks a token.
pub(crate) fn claude_renewed(blob: &[u8], answer: &Value, now_secs: i64) -> Option<Vec<u8>> {
    const LIFETIME_UNSAID: i64 = 3_600;
    let access_token = answer.get("access_token")?.as_str()?;
    let lifetime = answer
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(LIFETIME_UNSAID);
    let mut blob = serde_json::from_slice::<Value>(blob).ok()?;
    let mut credentials = serde_json::from_str::<Value>(blob.get("credentials")?.as_str()?).ok()?;
    let oauth = credentials.get_mut("claudeAiOauth")?.as_object_mut()?;
    oauth.insert("accessToken".into(), access_token.into());
    oauth.insert(
        "expiresAt".into(),
        (now_secs.saturating_add(lifetime).saturating_mul(1000)).into(),
    );
    if let Some(refresh_token) = answer.get("refresh_token").and_then(Value::as_str) {
        oauth.insert("refreshToken".into(), refresh_token.into());
    }
    if let Some(scope) = answer.get("scope").and_then(Value::as_str) {
        let scopes: Vec<Value> = scope.split_whitespace().map(Value::from).collect();
        if !scopes.is_empty() {
            oauth.insert("scopes".into(), scopes.into());
        }
    }
    blob.as_object_mut()?
        .insert("credentials".into(), credentials.to_string().into());
    serde_json::to_vec(&blob).ok()
}

/// The access token and ChatGPT account id inside a codex vault blob, which
/// the usage request needs together. Deliberately not `Debug`.
pub(crate) enum CodexToken {
    Valid {
        access_token: String,
        account_id: String,
    },
    Expired,
    Missing,
}

/// The access token of a codex blob unless its `exp` claim is already past,
/// with the account id the request has to name (`tokens.account_id`, else
/// the id_token's `chatgpt_account_id`).
pub(crate) fn codex_token(blob: &[u8], now_secs: i64) -> CodexToken {
    use crate::provider::codex::jwt_claims;

    let Ok(auth) = serde_json::from_slice::<Value>(blob) else {
        return CodexToken::Missing;
    };
    let tokens = &auth["tokens"];
    let Some(access_token) = tokens.get("access_token").and_then(Value::as_str) else {
        return CodexToken::Missing;
    };
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let claims = jwt_claims(tokens.get("id_token")?.as_str()?)?;
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")?
                .as_str()
                .map(str::to_owned)
        });
    let Some(account_id) = account_id else {
        return CodexToken::Missing;
    };
    match jwt_claims(access_token).and_then(|c| c.get("exp")?.as_i64()) {
        Some(exp) if exp <= now_secs => CodexToken::Expired,
        _ => CodexToken::Valid {
            access_token: access_token.to_owned(),
            account_id,
        },
    }
}

/// Read OpenAI's answer: the windows under `rate_limit`, either of which
/// can be null. A window whose numbers do not make sense is dropped.
pub fn parse_codex(body: &Value) -> Usage {
    let limits = &body["rate_limit"];
    let windows = ["primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|which| codex_window(limits.get(which)?, which))
        .collect();
    Usage {
        windows,
        plan: None,
    }
}

fn codex_window(value: &Value, which: &str) -> Option<Window> {
    let percent = value.get("used_percent").and_then(Value::as_f64)?;
    if !percent.is_finite() || percent < 0.0 {
        return None;
    }
    let length = value
        .get("limit_window_seconds")
        .and_then(Value::as_i64)
        .filter(|s| *s > 0);
    // Unused, a window answers with all of its length still to run and a
    // reset that moves on with every read: it starts with the first use. A
    // minute's slack for the two being worked out a moment apart; an answer
    // without `reset_after_seconds` can't tell and keeps its reset.
    let idle = percent == 0.0
        && length.is_some_and(|l| {
            value
                .get("reset_after_seconds")
                .and_then(Value::as_i64)
                .is_some_and(|after| after >= l - IDLE_SLACK)
        });
    Some(Window {
        label: window_name(length, which),
        percent,
        resets_at: if idle {
            None
        } else {
            value.get("reset_at").and_then(moment)
        },
        length_secs: length,
        quiet: false,
        idle,
    })
}

/// The same short names Claude Code's windows get, where the lengths match;
/// anything else is named by its own length.
fn window_name(seconds: Option<i64>, which: &str) -> String {
    const HOUR: i64 = 3_600;
    match seconds {
        Some(FIVE_HOURS) => "5h".into(),
        Some(WEEK) => "week".into(),
        Some(s) if s % (24 * HOUR) == 0 => format!("{}d", s / (24 * HOUR)),
        Some(s) if s % HOUR == 0 => format!("{}h", s / HOUR),
        Some(s) if s % 60 == 0 => format!("{}m", s / 60),
        Some(s) => format!("{s}s"),
        None => which.trim_end_matches("_window").to_owned(),
    }
}

/// Epoch seconds, sent as a number or as an RFC 3339 timestamp.
fn moment(value: &Value) -> Option<i64> {
    if let Some(secs) = value.as_i64() {
        // Milliseconds would be a date thousands of years out.
        return (secs > 0 && secs < 100_000_000_000).then_some(secs);
    }
    value.as_str().and_then(parse_rfc3339)
}

const FIVE_HOURS: i64 = 18_000;
const WEEK: i64 = 604_800;
/// How far short of its whole length a Codex window may be and still be
/// taken as not started.
const IDLE_SLACK: i64 = 60;

pub(crate) fn claude_report(result: Result<Value, FetchError>) -> UsageReport {
    report(result, parse)
}

pub(crate) fn codex_report(result: Result<Value, FetchError>) -> UsageReport {
    report(result, parse_codex)
}

pub(crate) fn cursor_report(result: Result<Value, FetchError>) -> UsageReport {
    report(result, parse_cursor)
}

/// A number sent as one, or as a string of one (Cursor sends both).
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n: &f64| n.is_finite())
}

/// This billing period's share of the plan used, the period running from
/// `billingCycleStart` to `billingCycleEnd` (epoch milliseconds): a month
/// on the older plans, a week on those that renew weekly (Plus and up,
/// since 2026-10). `planInfo.planName` is the plan as Cursor's site shows it.
pub fn parse_cursor(body: &Value) -> Usage {
    let name = body
        .pointer("/planInfo/planName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned);
    let plan = &body["planUsage"];
    let percent = number(&plan["totalPercentUsed"]).or_else(|| {
        let limit = number(&plan["limit"]).filter(|l| *l > 0.0)?;
        Some(number(&plan["totalSpend"])? / limit * 100.0)
    });
    let Some(percent) = percent.filter(|p| *p >= 0.0) else {
        return Usage {
            windows: Vec::new(),
            plan: name,
        };
    };
    let secs = |key: &str| {
        number(&body[key])
            .map(|ms| (ms / 1000.0) as i64)
            .filter(|s| *s > 0)
    };
    let (start, end) = (secs("billingCycleStart"), secs("billingCycleEnd"));
    let length_secs = start.zip(end).map(|(s, e)| e - s).filter(|l| *l > 0);
    let weekly = match body
        .pointer("/planInfo/includedUsagePeriod")
        .and_then(Value::as_str)
    {
        Some(period) => period.ends_with("WEEKLY"),
        // Seven days, with a day to spare for clocks and time zones.
        None => length_secs.is_some_and(|l| l <= WEEK + 86_400),
    };
    Usage {
        windows: vec![Window {
            label: if weekly { "week" } else { "month" }.into(),
            percent,
            resets_at: end,
            length_secs,
            quiet: false,
            idle: false,
        }],
        plan: name,
    }
}

fn report(result: Result<Value, FetchError>, parse: fn(&Value) -> Usage) -> UsageReport {
    match result {
        Ok(body) => UsageReport::Ok(parse(&body)),
        Err(FetchError::Unauthorized) => UsageReport::Unauthorized,
        Err(FetchError::Other(msg)) => UsageReport::Failed(msg),
    }
}

/// Read the usage answer. Newer answers list every limit under `limits`;
/// older ones only carry `five_hour` / `seven_day`.
pub fn parse(body: &Value) -> Usage {
    let mut windows: Vec<Window> = body
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(window_from_limit)
        .collect();
    if windows.is_empty() {
        for (key, label, length) in [("five_hour", "5h", FIVE_HOURS), ("seven_day", "week", WEEK)] {
            let Some(w) = body.get(key) else { continue };
            let Some(percent) = w.get("utilization").and_then(Value::as_f64) else {
                continue;
            };
            let resets_at = w
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339);
            windows.push(Window {
                label: label.into(),
                percent,
                resets_at,
                length_secs: Some(length),
                quiet: false,
                idle: percent == 0.0 && resets_at.is_none(),
            });
        }
    }
    Usage {
        windows,
        plan: None,
    }
}

fn window_from_limit(limit: &Value) -> Option<Window> {
    let kind = limit.get("kind")?.as_str()?;
    let percent = limit.get("percent")?.as_f64()?;
    let model = limit
        .pointer("/scope/model/display_name")
        .and_then(Value::as_str);
    let untouched = model.is_some() && percent == 0.0;
    let (label, length_secs) = match (kind, model) {
        ("session", _) => ("5h".to_owned(), Some(FIVE_HOURS)),
        ("weekly_all", _) => ("week".to_owned(), Some(WEEK)),
        (_, Some(model)) => (format!("week {model}"), Some(WEEK)),
        (other, None) => (other.to_owned(), None),
    };
    // A per-model weekly limit nobody has touched is noise in a one-line
    // summary, but a total over accounts needs its 0%. Any other untouched
    // per-model limit would only repeat `5h` or `week`.
    let quiet = untouched && label.starts_with("week ");
    if untouched && !quiet {
        return None;
    }
    let resets_at = limit
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    Some(Window {
        label,
        percent,
        resets_at,
        length_secs,
        quiet,
        // Until the first message, the session has no reset to send.
        idle: percent == 0.0 && resets_at.is_none(),
    })
}

impl Usage {
    /// `5h 62% (2h10m) · week 48% (4d3h)`
    pub fn summary(&self, now_secs: i64) -> String {
        let shown: Vec<&Window> = self.windows.iter().filter(|w| !w.quiet).collect();
        if shown.is_empty() {
            return "no limits reported".into();
        }
        shown
            .into_iter()
            .map(|w| {
                let pct = format!("{} {:.0}%", w.label, w.percent);
                match w.resets_at {
                    Some(at) if at > now_secs => {
                        format!("{pct} ({})", short_duration(at - now_secs))
                    }
                    _ if w.idle => format!("{pct} (not started)"),
                    _ => pct,
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

impl UsageReport {
    pub fn summary(&self, now_secs: i64) -> String {
        match self {
            UsageReport::Ok(usage) => usage.summary(now_secs),
            UsageReport::Expired => "token expired — switch to it to refresh".into(),
            UsageReport::Unauthorized => "login no longer accepted — sign in again".into(),
            UsageReport::Failed(msg) => format!("usage unavailable: {msg}"),
        }
    }
}

fn short_duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{}m", m.max(1)),
        (0, _) => format!("{h}h{m:02}m"),
        _ => format!("{d}d{h}h"),
    }
}

/// `2026-09-20T22:20:00.095287+00:00` / `...Z` → epoch seconds.
pub(crate) fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let oh = rest.get(1..3)?.parse::<i64>().ok()?;
            let om = rest.get(4..6)?.parse::<i64>().ok()?;
            sign * (oh * 3_600 + om * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + sec - offset)
}

/// Howard Hinnant's days-from-civil (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_reads_as_one_window_over_its_billing_period() {
        let usage = parse_cursor(&serde_json::json!({
            "billingCycleStart": "1790000000000",
            "billingCycleEnd": "1792592000000",
            "planUsage": {"totalPercentUsed": 37.5, "limit": 2000},
            "enabled": true
        }));
        assert_eq!(
            usage.windows,
            [Window {
                label: "month".into(),
                percent: 37.5,
                resets_at: Some(1_792_592_000),
                length_secs: Some(2_592_000),
                quiet: false,
                idle: false,
            }]
        );
        assert_eq!(usage.plan, None);
        // only spend and a limit, in cents
        let spend = parse_cursor(&serde_json::json!({
            "planUsage": {"totalSpend": "500", "limit": "2000"}
        }));
        assert_eq!(spend.windows[0].percent, 25.0);
        assert!(parse_cursor(&serde_json::json!({"enabled": false}))
            .windows
            .is_empty());
    }

    /// A Plus account as Cursor answered it, 2026-10: the period is a week,
    /// and only `planInfo` names the plan (the stripe profile says "free").
    #[test]
    fn cursor_reads_a_weekly_plan_and_its_name() {
        let body = serde_json::json!({
            "billingCycleStart": "1792635360000",
            "billingCycleEnd": "1793240160000",
            "planUsage": {"totalPercentUsed": 0, "autoPercentUsed": 0, "apiPercentUsed": 0},
            "planInfo": {"planName": "Plus", "includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY"}
        });
        let usage = parse_cursor(&body);
        assert_eq!(usage.plan.as_deref(), Some("Plus"));
        assert_eq!(usage.windows[0].label, "week");
        assert_eq!(usage.windows[0].length_secs, Some(604_800));
        // without planInfo, a seven-day period still reads as a week
        let mut bare = body.clone();
        bare.as_object_mut().unwrap().remove("planInfo");
        let usage = parse_cursor(&bare);
        assert_eq!(usage.windows[0].label, "week");
        assert_eq!(usage.plan, None);
    }
    use serde_json::json;

    /// The usage endpoint's answer as Claude Code receives it (shape as of 2026-09).
    fn real_answer() -> Value {
        json!({
            "five_hour": {"utilization": 62, "resets_at": "2026-09-20T22:20:00.095287+00:00"},
            "seven_day": {"utilization": 48, "resets_at": "2026-09-27T02:00:00.095306+00:00"},
            "limits": [
                {"kind": "session", "group": "session", "percent": 62,
                 "resets_at": "2026-09-20T22:20:00.095287+00:00", "scope": null, "is_active": true},
                {"kind": "weekly_all", "group": "weekly", "percent": 48,
                 "resets_at": "2026-09-27T02:00:00.095306+00:00", "scope": null, "is_active": false},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 0,
                 "resets_at": "2026-09-27T02:00:00+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}}, "is_active": false},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 12,
                 "resets_at": "2026-09-27T02:00:00+00:00",
                 "scope": {"model": {"id": null, "display_name": "Opus"}}, "is_active": false}
            ]
        })
    }

    #[test]
    fn parses_limits_list_and_keeps_untouched_model_limits_quiet() {
        let usage = parse(&real_answer());
        let labels: Vec<_> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "week", "week Fable", "week Opus"]);
        let quiet: Vec<_> = usage.windows.iter().map(|w| w.quiet).collect();
        assert_eq!(quiet, [false, false, true, false]);
        assert_eq!(usage.windows[2].percent, 0.0);
        assert_eq!(usage.windows[0].percent, 62.0);
        assert_eq!(
            usage.windows[0].resets_at,
            parse_rfc3339("2026-09-20T22:20:00Z")
        );
    }

    #[test]
    fn claude_windows_know_how_long_they_run() {
        let lengths: Vec<_> = parse(&real_answer())
            .windows
            .iter()
            .map(|w| w.length_secs)
            .collect();
        assert_eq!(
            lengths,
            [Some(18_000), Some(604_800), Some(604_800), Some(604_800)]
        );
    }

    /// OpenAI's answer as measured (Codex 0.160, 2026-09).
    fn codex_answer() -> Value {
        json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 45,
                    "limit_window_seconds": 604_800,
                    "reset_after_seconds": 492_401,
                    "reset_at": 1_790_628_078i64
                },
                "secondary_window": null
            },
            "rate_limit_reached_type": null
        })
    }

    #[test]
    fn codex_answer_becomes_named_windows() {
        let usage = parse_codex(&codex_answer());
        assert_eq!(
            usage.windows,
            [Window {
                label: "week".into(),
                percent: 45.0,
                resets_at: Some(1_790_628_078),
                length_secs: Some(604_800),
                quiet: false,
                idle: false,
            }]
        );
    }

    #[test]
    fn codex_reads_both_windows_when_both_are_filled_in() {
        let mut body = codex_answer();
        body["rate_limit"]["primary_window"] = json!({
            "used_percent": 12.5, "limit_window_seconds": 18_000, "reset_at": 1_790_100_000i64
        });
        body["rate_limit"]["secondary_window"] = json!({
            "used_percent": 30, "limit_window_seconds": 604_800, "reset_at": "2026-09-27T02:00:00Z"
        });
        let usage = parse_codex(&body);
        let read: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.percent, w.resets_at))
            .collect();
        assert_eq!(
            read,
            [
                ("5h", 12.5, Some(1_790_100_000)),
                ("week", 30.0, parse_rfc3339("2026-09-27T02:00:00Z"))
            ]
        );
    }

    #[test]
    fn codex_windows_of_other_lengths_are_named_by_length() {
        let mut body = codex_answer();
        body["rate_limit"]["primary_window"] =
            json!({"used_percent": 1, "limit_window_seconds": 10_800});
        body["rate_limit"]["secondary_window"] =
            json!({"used_percent": 2, "limit_window_seconds": 86_400});
        let labels: Vec<_> = parse_codex(&body)
            .windows
            .into_iter()
            .map(|w| w.label)
            .collect();
        assert_eq!(labels, ["3h", "1d"]);
    }

    #[test]
    fn codex_drops_a_window_it_cannot_read() {
        let mut body = codex_answer();
        body["rate_limit"]["primary_window"] =
            json!({"used_percent": "quite a lot", "limit_window_seconds": 18_000});
        body["rate_limit"]["secondary_window"] =
            json!({"used_percent": -1.0, "limit_window_seconds": 604_800});
        assert!(parse_codex(&body).windows.is_empty());
    }

    /// A Plus account nobody had used, as OpenAI answered it (2026-10): each
    /// window has all of its length to run, from whenever it was asked.
    #[test]
    fn codex_windows_nobody_used_have_not_started() {
        let mut body = codex_answer();
        body["rate_limit"]["primary_window"] = json!({
            "used_percent": 0, "limit_window_seconds": 18_000,
            "reset_after_seconds": 18_000, "reset_at": 1_793_114_065i64
        });
        body["rate_limit"]["secondary_window"] = json!({
            "used_percent": 0, "limit_window_seconds": 604_800,
            "reset_after_seconds": 604_800, "reset_at": 1_793_700_865i64
        });
        let usage = parse_codex(&body);
        let read: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.idle, w.resets_at))
            .collect();
        assert_eq!(read, [("5h", true, None), ("week", true, None)]);
        assert_eq!(
            usage.summary(1_793_096_065),
            "5h 0% (not started) · week 0% (not started)"
        );
        // a second short of the whole length, worked out a moment apart
        body["rate_limit"]["primary_window"]["reset_after_seconds"] = json!(17_999);
        assert!(parse_codex(&body).windows[0].idle);
        // under way, even with nothing counted yet, a window has its reset
        body["rate_limit"]["primary_window"]["reset_after_seconds"] = json!(17_000);
        let started = &parse_codex(&body).windows[0];
        assert!(!started.idle);
        assert_eq!(started.resets_at, Some(1_793_114_065));
        // and anything used means it started
        body["rate_limit"]["secondary_window"]["used_percent"] = json!(1);
        let used = &parse_codex(&body).windows[1];
        assert!(!used.idle);
        assert_eq!(used.resets_at, Some(1_793_700_865));
    }

    fn codex_blob(exp: i64, account_id: Option<&str>) -> Vec<u8> {
        use base64::Engine as _;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let access = format!(
            "{}.{}.{}",
            b64(r#"{"alg":"RS256"}"#),
            b64(&format!(r#"{{"exp":{exp}}}"#)),
            b64("sig")
        );
        let mut tokens = json!({"access_token": access, "id_token": "x.y.z", "refresh_token": "R"});
        if let Some(id) = account_id {
            tokens["account_id"] = json!(id);
        }
        serde_json::to_vec(&json!({"auth_mode": "chatgpt", "tokens": tokens})).unwrap()
    }

    #[test]
    fn codex_token_is_used_only_while_unexpired_and_with_its_account() {
        let now = 1_800_000_000;
        assert!(matches!(
            codex_token(&codex_blob(now + 60, Some("acc-1")), now),
            CodexToken::Valid { access_token, account_id }
                if account_id == "acc-1" && access_token.split('.').count() == 3
        ));
        assert!(matches!(
            codex_token(&codex_blob(now - 1, Some("acc-1")), now),
            CodexToken::Expired
        ));
        assert!(matches!(
            codex_token(&codex_blob(now + 60, None), now),
            CodexToken::Missing
        ));
        assert!(matches!(codex_token(b"{}", now), CodexToken::Missing));
    }

    #[test]
    fn falls_back_to_named_windows_without_limits_list() {
        let usage = parse(&json!({
            "five_hour": {"utilization": 7.4, "resets_at": null},
            "seven_day": {"utilization": 30, "resets_at": "2026-09-27T02:00:00Z"}
        }));
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5h");
        assert_eq!(usage.windows[0].resets_at, None);
        assert!(!usage.windows[0].idle);
        assert_eq!(usage.windows[1].label, "week");
    }

    /// A Team seat between sessions (2026-10): the session sent with no
    /// reset, beside the one weekly limit the seat has.
    #[test]
    fn a_session_with_nothing_used_and_no_reset_has_not_started() {
        let body = json!({
            "five_hour": {"utilization": 0.0, "resets_at": null},
            "seven_day": null,
            "limits": [
                {"kind": "session", "group": "session", "percent": 0,
                 "resets_at": null, "scope": null, "is_active": false},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 2,
                 "resets_at": "2026-10-28T14:00:00.181226+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}}, "is_active": true}
            ]
        });
        let usage = parse(&body);
        let idle: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.idle))
            .collect();
        assert_eq!(idle, [("5h", true), ("week Fable", false)]);
        let now = parse_rfc3339("2026-10-27T10:10:00Z").unwrap();
        assert_eq!(
            usage.summary(now),
            "5h 0% (not started) · week Fable 2% (1d3h)"
        );
        // the first message starts it: a reset comes back before any share does
        let mut started = body.clone();
        started["limits"][0]["resets_at"] = json!("2026-10-27T15:10:00.181022+00:00");
        assert!(!parse(&started).windows[0].idle);
        // a share used is never not started, reset or no
        let mut used = body.clone();
        used["limits"][0]["percent"] = json!(3);
        assert!(!parse(&used).windows[0].idle);
        // the same between sessions without the `limits` list
        let older = parse(&json!({"five_hour": {"utilization": 0.0, "resets_at": null}}));
        assert!(older.windows[0].idle);
    }

    #[test]
    fn a_used_model_limit_is_not_quiet_and_only_quiet_ones_mean_none() {
        let usage = parse(&json!({"limits": [
            {"kind": "weekly_scoped", "percent": 0, "scope": {"model": {"display_name": "Fable"}}}
        ]}));
        assert!(usage.windows[0].quiet);
        assert_eq!(usage.summary(0), "no limits reported");
        let used = parse(&json!({"limits": [
            {"kind": "weekly_scoped", "percent": 3, "scope": {"model": {"display_name": "Fable"}}}
        ]}));
        assert!(!used.windows[0].quiet);
        let session = parse(&json!({"limits": [
            {"kind": "session", "percent": 0, "scope": {"model": {"display_name": "Fable"}}}
        ]}));
        assert!(session.windows.is_empty());
    }

    #[test]
    fn summary_shows_percent_and_time_to_reset() {
        let now = parse_rfc3339("2026-09-20T20:10:00Z").unwrap();
        let summary = parse(&real_answer()).summary(now);
        assert_eq!(
            summary,
            "5h 62% (2h10m) · week 48% (6d5h) · week Opus 12% (6d5h)"
        );
    }

    #[test]
    fn rfc3339_parses_offsets_and_fractions() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(
            parse_rfc3339("2000-03-01T00:00:00.5-00:00"),
            Some(951_868_800)
        );
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    fn blob(expires_at_ms: Option<i64>) -> Vec<u8> {
        let mut oauth = json!({"accessToken": "SECRET-AT", "refreshToken": "R"});
        if let Some(ms) = expires_at_ms {
            oauth["expiresAt"] = json!(ms);
        }
        let creds = json!({"claudeAiOauth": oauth}).to_string();
        serde_json::to_vec(&json!({"credentials": creds, "oauth_account": null})).unwrap()
    }

    #[test]
    fn token_is_used_only_while_unexpired() {
        let now = 1_800_000_000;
        assert!(
            matches!(claude_token(&blob(Some((now + 60) * 1000)), now), Token::Valid(t) if t == "SECRET-AT")
        );
        assert!(matches!(
            claude_token(&blob(Some((now - 1) * 1000)), now),
            Token::Expired
        ));
        assert!(matches!(claude_token(&blob(None), now), Token::Valid(_)));
        assert!(matches!(claude_token(b"{}", now), Token::Missing));
    }

    /// A parked login as the vault keeps it, with what Claude Code keeps
    /// beside the tokens.
    fn login(oauth: Value) -> Vec<u8> {
        let creds = json!({"claudeAiOauth": oauth, "mcpOAuth": {"kept": true}}).to_string();
        serde_json::to_vec(
            &json!({"credentials": creds, "oauth_account": {"emailAddress": "a@x.com"}}),
        )
        .unwrap()
    }

    fn lapsing_at(expires_at_secs: i64) -> Value {
        json!({
            "accessToken": "SECRET-AT",
            "refreshToken": "SECRET-RT",
            "expiresAt": expires_at_secs * 1000,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "team",
            "rateLimitTier": "default_claude_max_5x"
        })
    }

    #[test]
    fn a_login_is_renewed_once_its_token_lapses_or_is_about_to() {
        let now = 1_800_000_000;
        let renewal = |oauth: Value| claude_renewal(&login(oauth), now, RENEW_AHEAD);

        assert!(renewal(lapsing_at(now + RENEW_AHEAD + 60)).is_none());
        let soon = renewal(lapsing_at(now + 60)).expect("about to lapse");
        assert_eq!(soon.refresh_token, "SECRET-RT");
        assert_eq!(soon.scopes, "user:inference user:profile");
        assert_eq!(soon.client_id, CLAUDE_CLIENT_ID);
        assert!(renewal(lapsing_at(now - 3_600)).is_some());

        let mut own_client = lapsing_at(now - 1);
        own_client["clientId"] = json!("another-client");
        assert_eq!(renewal(own_client).unwrap().client_id, "another-client");
    }

    #[test]
    fn a_login_without_what_renews_it_is_not_renewed() {
        let now = 1_800_000_000;
        let without = |key: &str| {
            let mut oauth = lapsing_at(now - 1);
            oauth.as_object_mut().unwrap().remove(key);
            claude_renewal(&login(oauth), now, RENEW_AHEAD)
        };
        assert!(without("refreshToken").is_none());
        assert!(without("scopes").is_none());
        // No expiry is a token that never lapses.
        assert!(without("expiresAt").is_none());
        assert!(claude_renewal(b"{}", now, RENEW_AHEAD).is_none());
    }

    fn oauth_of(blob: &[u8]) -> Value {
        claude_oauth(blob).unwrap()
    }

    #[test]
    fn a_renewed_login_holds_the_new_tokens_and_keeps_the_rest() {
        let now = 1_800_000_000;
        let old = login(lapsing_at(now - 60));
        let answer = json!({
            "access_token": "SECRET-AT2",
            "refresh_token": "SECRET-RT2",
            "expires_in": 28_800,
            "scope": "user:inference user:profile user:sessions:claude_code",
            "token_type": "Bearer"
        });
        let renewed = claude_renewed(&old, &answer, now).unwrap();
        let oauth = oauth_of(&renewed);
        assert_eq!(oauth["accessToken"], "SECRET-AT2");
        assert_eq!(oauth["refreshToken"], "SECRET-RT2");
        assert_eq!(oauth["expiresAt"], (now + 28_800) * 1000);
        assert_eq!(
            oauth["scopes"],
            json!([
                "user:inference",
                "user:profile",
                "user:sessions:claude_code"
            ])
        );
        assert_eq!(oauth["subscriptionType"], "team");
        assert_eq!(oauth["rateLimitTier"], "default_claude_max_5x");

        let blob: Value = serde_json::from_slice(&renewed).unwrap();
        assert_eq!(blob["oauth_account"]["emailAddress"], "a@x.com");
        let creds: Value = serde_json::from_str(blob["credentials"].as_str().unwrap()).unwrap();
        assert_eq!(creds["mcpOAuth"]["kept"], true);
        assert!(matches!(claude_token(&renewed, now), Token::Valid(t) if t == "SECRET-AT2"));
    }

    #[test]
    fn a_renewal_answer_without_a_new_refresh_token_keeps_the_old_one() {
        let now = 1_800_000_000;
        let old = login(lapsing_at(now - 60));
        let answer = json!({"access_token": "SECRET-AT2", "expires_in": 3_600});
        let oauth = oauth_of(&claude_renewed(&old, &answer, now).unwrap());
        assert_eq!(oauth["refreshToken"], "SECRET-RT");
        assert_eq!(oauth["scopes"], json!(["user:inference", "user:profile"]));

        assert!(claude_renewed(&old, &json!({"expires_in": 3_600}), now).is_none());
    }

    #[test]
    fn a_renewal_answer_without_a_lifetime_is_kept_for_an_hour() {
        let now = 1_800_000_000;
        let old = login(lapsing_at(now - 60));
        let answer = json!({"access_token": "SECRET-AT2", "refresh_token": "SECRET-RT2"});
        let oauth = oauth_of(&claude_renewed(&old, &answer, now).unwrap());
        assert_eq!(oauth["refreshToken"], "SECRET-RT2");
        assert_eq!(oauth["expiresAt"], (now + 3_600) * 1000);
    }

    #[test]
    fn only_an_invalid_grant_is_a_refused_renewal() {
        let refused = |status, said: Value| {
            matches!(
                renewal_refused(status, Some(&said)),
                FetchError::Unauthorized
            )
        };
        assert!(refused(400, json!({"error": "invalid_grant"})));
        assert!(refused(401, json!({"error": "invalid_grant"})));
        assert!(!refused(401, json!({"error": "invalid_client"})));
        assert!(!refused(400, json!({"error": "invalid_scope"})));
        assert!(!refused(
            401,
            json!({"error": {"type": "authentication_error"}})
        ));
        assert!(!refused(500, json!({"error": "invalid_grant"})));
        assert!(matches!(
            renewal_refused(429, None),
            FetchError::Other(why) if why == RATE_LIMITED
        ));
        assert!(matches!(
            renewal_refused(400, Some(&json!({"error": "invalid_scope"}))),
            FetchError::Other(why) if why == "Anthropic answered 400 (invalid_scope)"
        ));
    }

    #[test]
    fn a_login_that_nothing_renews_is_signed_out_not_expired() {
        let now = 1_800_000_000;
        let (lapsed, later) = ((now - 1) * 1000, (now + 60) * 1000);
        // what Claude Code left after a renewal was refused
        let blanked = json!({"accessToken": "", "refreshToken": "", "expiresAt": 0,
            "refreshTokenExpiresAt": later});
        assert!(matches!(
            claude_token(&login(blanked), now),
            Token::SignedOut
        ));
        let no_refresh = json!({"accessToken": "AT", "refreshToken": "", "expiresAt": lapsed});
        assert!(matches!(
            claude_token(&login(no_refresh), now),
            Token::SignedOut
        ));
        let refresh_lapsed = json!({"accessToken": "AT", "refreshToken": "R",
            "expiresAt": lapsed, "refreshTokenExpiresAt": lapsed});
        assert!(matches!(
            claude_token(&login(refresh_lapsed), now),
            Token::SignedOut
        ));

        // a refresh token still good is renewed by Claude Code on a switch
        let renewable = json!({"accessToken": "AT", "refreshToken": "R",
            "expiresAt": lapsed, "refreshTokenExpiresAt": later});
        assert!(matches!(
            claude_token(&login(renewable), now),
            Token::Expired
        ));
        // and a live access token reads whatever its refresh token says
        let live = json!({"accessToken": "AT", "refreshToken": "R",
            "expiresAt": later, "refreshTokenExpiresAt": lapsed});
        assert!(matches!(claude_token(&login(live), now), Token::Valid(t) if t == "AT"));
    }
}
