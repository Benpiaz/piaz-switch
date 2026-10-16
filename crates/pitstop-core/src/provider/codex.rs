//! Codex CLI provider.
//!
//! The whole credential lives in `~/.codex/auth.json`
//! (`{auth_mode, OPENAI_API_KEY, tokens: {id_token, …}, last_refresh}`,
//! verified on a real install, 2026-10). The vault blob is that file verbatim.
//! The label comes from the `email` claim of the id_token JWT payload
//! (decoded, never verified — it's a display label, not an auth decision).

use std::path::{Path, PathBuf};

use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::error::Result;
use crate::host::Host;

pub struct Codex;

impl Codex {
    pub fn auth_path(host: &dyn Host) -> PathBuf {
        host.home().join(".codex").join("auth.json")
    }
}

impl Provider for Codex {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        let Some(blob) = host.read(&Self::auth_path(host))? else {
            return Ok(None);
        };
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        host.write_atomic(&Self::auth_path(host), blob)
    }

    /// `codex login` with `CODEX_HOME=dir` and a file store writes `dir/auth.json`.
    fn read_signed_in(&self, host: &dyn Host, dir: &Path) -> Result<Option<ActiveCredential>> {
        let Some(blob) = host.read(&dir.join("auth.json"))? else {
            return Ok(None);
        };
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    /// Everything `codex login` writes stays inside its home.
    fn discard_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<()> {
        Ok(())
    }

    /// Identity is `<user>:<workspace>` from the id_token's OpenAI claim
    /// (`chatgpt_user_id`, `chatgpt_account_id`), falling back to `sub` and
    /// `tokens.account_id`; a personal plan and each workspace are separate
    /// accounts. An API-key login has no account, so nothing to say.
    fn details(&self, blob: &[u8]) -> Details {
        let Ok(auth) = serde_json::from_slice::<serde_json::Value>(blob) else {
            return Details::default();
        };
        let tokens = &auth["tokens"];
        let claims = tokens
            .get("id_token")
            .and_then(|t| t.as_str())
            .and_then(jwt_claims)
            .unwrap_or(serde_json::Value::Null);
        let openai = &claims["https://api.openai.com/auth"];

        let user = super::text(openai, "chatgpt_user_id").or_else(|| super::text(&claims, "sub"));
        let workspace =
            super::text(openai, "chatgpt_account_id").or_else(|| super::text(tokens, "account_id"));
        Details {
            label: email_in(&claims),
            identity: user.map(|user| format!("{user}:{}", workspace.unwrap_or_default())),
            plan: super::text(openai, "chatgpt_plan_type").map(|p| super::capitalized(&p)),
        }
    }
}

/// A JWT's payload claims, decoded and never verified: they label things,
/// they decide nothing.
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    let payload_b64 = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()
}

fn email_in(claims: &serde_json::Value) -> Option<String> {
    claims
        .get("email")
        .or_else(|| claims.get("https://api.openai.com/profile")?.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// Best-effort email from a JWT's payload claims (`email`, or the ChatGPT
/// profile claim). Returns None on anything unparseable.
pub fn email_from_jwt(id_token: &str) -> Option<String> {
    email_in(&jwt_claims(id_token)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use base64::Engine as _;

    fn fake_jwt(payload: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!(
            "{}.{}.{}",
            b64(r#"{"alg":"RS256"}"#),
            b64(payload),
            b64("sig")
        )
    }

    fn auth_json(jwt: &str) -> String {
        format!(
            r#"{{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{{"id_token":"{jwt}","access_token":"AT","refresh_token":"RT","account_id":"acc"}},"last_refresh":"2026-10-01T00:00:00Z"}}"#
        )
    }

    #[test]
    fn read_active_none_when_logged_out() {
        let host = FakeHost::new();
        assert!(Codex.read_active(&host).unwrap().is_none());
    }

    #[test]
    fn blob_is_verbatim_file_and_label_is_jwt_email() {
        let host = FakeHost::new();
        let json = auth_json(&fake_jwt(r#"{"email":"cx@x.com","sub":"s"}"#));
        host.put_file(Codex::auth_path(&host), json.clone());
        let active = Codex.read_active(&host).unwrap().unwrap();
        assert_eq!(String::from_utf8(active.blob.clone()).unwrap(), json);
        assert_eq!(active.details.label.as_deref(), Some("cx@x.com"));
    }

    #[test]
    fn label_falls_back_to_profile_claim_then_none() {
        let jwt = fake_jwt(r#"{"https://api.openai.com/profile":{"email":"p@x.com"}}"#);
        assert_eq!(email_from_jwt(&jwt).as_deref(), Some("p@x.com"));
        assert_eq!(email_from_jwt("not-a-jwt"), None);
        assert_eq!(email_from_jwt(&fake_jwt(r#"{"sub":"s"}"#)), None);
    }

    #[test]
    fn details_name_the_user_the_workspace_and_the_plan() {
        let jwt = fake_jwt(
            r#"{"email":"cx@x.com","sub":"auth0|u","https://api.openai.com/auth":{"chatgpt_user_id":"user-1","chatgpt_account_id":"ws-1","chatgpt_plan_type":"pro"}}"#,
        );
        assert_eq!(
            Codex.details(auth_json(&jwt).as_bytes()),
            Details {
                label: Some("cx@x.com".into()),
                identity: Some("user-1:ws-1".into()),
                plan: Some("Pro".into()),
            }
        );
    }

    #[test]
    fn identity_falls_back_to_sub_and_the_token_account_id() {
        // auth_json puts "account_id":"acc" beside the tokens
        let jwt = fake_jwt(r#"{"email":"cx@x.com","sub":"auth0|u"}"#);
        let details = Codex.details(auth_json(&jwt).as_bytes());
        assert_eq!(details.identity.as_deref(), Some("auth0|u:acc"));
        assert_eq!(details.plan, None);
    }

    #[test]
    fn a_private_sign_in_is_read_from_its_home() {
        let host = FakeHost::new();
        let dir = Path::new("/tmp/pitstop-signin-2");
        let json = auth_json(&fake_jwt(r#"{"email":"n@x.com"}"#));
        host.put_file(dir.join("auth.json"), json.clone());
        let signed_in = Codex.read_signed_in(&host, dir).unwrap().unwrap();
        assert_eq!(signed_in.blob, json.as_bytes());
        assert_eq!(signed_in.details.label.as_deref(), Some("n@x.com"));
    }

    #[test]
    fn write_active_restores_bytes_verbatim() {
        let host = FakeHost::new();
        let json = auth_json(&fake_jwt(r#"{"email":"cx@x.com"}"#));
        Codex.write_active(&host, json.as_bytes()).unwrap();
        assert_eq!(
            host.file(&Codex::auth_path(&host)).unwrap(),
            json.as_bytes()
        );
    }
}
