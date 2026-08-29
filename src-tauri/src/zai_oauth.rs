//! Official Z.AI / ZCode GLM coding-plan OAuth.
//!
//! CLIProxyAPI 7.2.139 has no `-zai-login`. Basiliskos owns the same ZCode CLI
//! poll flow the official client uses, then mints a coding-plan API key through
//! the documented Z.AI business API. Inference is served on the OpenAI-compatible
//! coding endpoint because the pinned runtime has no native `zai` provider.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) const OAUTH_BASE_URL: &str = "https://zcode.z.ai/api/v1";
pub(crate) const BIZ_BASE_URL: &str = "https://api.z.ai";
pub(crate) const CODING_OPENAI_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const MINT_KEY_NAME: &str = "zcode-api-key";
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_POLL_DURATION: Duration = Duration::from_secs(10 * 60);
const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 5;
const USER_AGENT: &str = "Basiliskos/3.0";

#[derive(Debug, Clone)]
pub(crate) struct ZaiCliInit {
    pub(crate) flow_id: String,
    pub(crate) poll_token: String,
    pub(crate) authorize_url: String,
    pub(crate) expires_at: Option<i64>,
    pub(crate) poll_interval: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct ZaiReady {
    pub(crate) access_token: String,
    pub(crate) email: String,
    pub(crate) user_id: String,
    pub(crate) name: String,
}

pub(crate) struct ZaiOAuth {
    client: reqwest::Client,
    oauth_base: String,
    biz_base: String,
}

impl ZaiOAuth {
    pub(crate) fn production() -> Result<Self, String> {
        Self::new(OAUTH_BASE_URL, BIZ_BASE_URL)
    }

    fn new(oauth_base: &str, biz_base: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| "Could not prepare the Z.AI login client")?;
        Ok(Self {
            client,
            oauth_base: oauth_base.trim_end_matches('/').to_string(),
            biz_base: biz_base.trim_end_matches('/').to_string(),
        })
    }

    pub(crate) async fn start_cli_flow(&self) -> Result<ZaiCliInit, String> {
        let poll_token = new_poll_token();
        let data = self
            .oauth_envelope(
                self.client
                    .post(format!("{}/oauth/cli/init", self.oauth_base))
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {poll_token}"),
                    )
                    .json(&json!({ "provider": "zai" })),
            )
            .await?;
        let flow_id = json_string(&data, "flow_id")?;
        let authorize_url = json_string(&data, "authorize_url")?;
        if flow_id.is_empty() || authorize_url.is_empty() {
            return Err("Z.AI login did not return a flow id or authorization URL".into());
        }
        let server_poll = data
            .get("poll_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&poll_token)
            .to_string();
        let interval_secs = data
            .get("poll_interval_sec")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(ZaiCliInit {
            flow_id,
            poll_token: server_poll,
            authorize_url,
            expires_at: data.get("expires_at").and_then(Value::as_i64),
            poll_interval: Duration::from_secs(interval_secs).max(DEFAULT_POLL_INTERVAL),
        })
    }

    pub(crate) async fn wait_for_authorization(
        &self,
        init: &ZaiCliInit,
        cancel: &AtomicBool,
    ) -> Result<ZaiReady, String> {
        let mut deadline = Instant::now() + MAX_POLL_DURATION;
        if let Some(expires_at) = init.expires_at.filter(|value| *value > 0) {
            let until = Duration::from_secs(
                expires_at
                    .saturating_sub(chrono::Utc::now().timestamp())
                    .max(0) as u64,
            );
            deadline = deadline.min(Instant::now() + until);
        }
        let mut consecutive_errors = 0u32;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Z.AI login was cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("Z.AI authorization timed out".into());
            }
            match self.poll_once(init).await {
                Ok(Some(ready)) => return Ok(ready),
                Ok(None) => consecutive_errors = 0,
                Err(error)
                    if error.contains("denied") || error.contains("authorization failed") =>
                {
                    return Err(error);
                }
                Err(_) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                        return Err("Z.AI login could not reach the authorization service".into());
                    }
                }
            }
            tokio::time::sleep(init.poll_interval).await;
        }
    }

    async fn poll_once(&self, init: &ZaiCliInit) -> Result<Option<ZaiReady>, String> {
        let data = self
            .oauth_envelope(
                self.client
                    .get(format!(
                        "{}/oauth/cli/poll/{}",
                        self.oauth_base, init.flow_id
                    ))
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {}", init.poll_token),
                    ),
            )
            .await?;
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        match status {
            "pending" | "" => Ok(None),
            "failed" => Err("Z.AI authorization failed or was denied".into()),
            "ready" => {
                let user = data.get("user").cloned().unwrap_or(Value::Null);
                let zai = data.get("zai").cloned().unwrap_or(Value::Null);
                let access_token = zai
                    .get("access_token")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .or_else(|| data.get("token").and_then(Value::as_str).map(str::trim))
                    .filter(|value| !value.is_empty())
                    .ok_or("Z.AI login completed without an access token")?
                    .to_string();
                Ok(Some(ZaiReady {
                    access_token,
                    email: json_string(&user, "email").unwrap_or_default(),
                    user_id: json_string(&user, "user_id").unwrap_or_default(),
                    name: json_string(&user, "name").unwrap_or_default(),
                }))
            }
            other => Err(format!(
                "Z.AI login returned an unexpected status ({other})"
            )),
        }
    }

    pub(crate) async fn mint_coding_plan_key(&self, ready: &ZaiReady) -> Result<String, String> {
        let biz_token = self.business_login(&ready.access_token).await?;
        let authorization = format!("Bearer {biz_token}");
        let customer = self
            .biz_request(
                reqwest::Method::GET,
                format!("{}/api/biz/customer/getCustomerInfo", self.biz_base),
                &authorization,
                None,
            )
            .await?;
        let (org_id, project_id) = select_org_project(&customer)?;
        let keys_url = format!(
            "{}/api/biz/v1/organization/{org_id}/projects/{project_id}/api_keys",
            self.biz_base
        );
        let mut api_key = find_named_key(
            &self
                .biz_request(reqwest::Method::GET, keys_url.clone(), &authorization, None)
                .await
                .unwrap_or(Value::Null),
        );
        if api_key.is_empty() {
            let created = self
                .biz_request(
                    reqwest::Method::POST,
                    keys_url.clone(),
                    &authorization,
                    Some(json!({ "name": MINT_KEY_NAME })),
                )
                .await?;
            api_key = json_string(&created, "apiKey")?;
        }
        if api_key.is_empty() {
            return Err("Z.AI did not return a coding-plan API key".into());
        }
        let copied = self
            .biz_request(
                reqwest::Method::GET,
                format!("{keys_url}/copy/{}", urlencoding(&api_key)),
                &authorization,
                None,
            )
            .await?;
        let secret = json_string(&copied, "secretKey")?;
        if secret.is_empty() {
            return Err("Z.AI did not return the coding-plan key secret".into());
        }
        Ok(format!("{api_key}.{secret}"))
    }

    async fn business_login(&self, access_token: &str) -> Result<String, String> {
        let data = self
            .biz_request(
                reqwest::Method::POST,
                format!("{}/api/auth/z/login", self.biz_base),
                "",
                Some(json!({ "token": access_token })),
            )
            .await?;
        let token = json_string(&data, "access_token")?;
        if token.is_empty() {
            return Err("Z.AI business login did not return an access token".into());
        }
        Ok(token)
    }

    async fn oauth_envelope(&self, request: reqwest::RequestBuilder) -> Result<Value, String> {
        unwrap_envelope(send_json(request).await?, true)
    }

    async fn biz_request(
        &self,
        method: reqwest::Method,
        url: String,
        authorization: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut request = self.client.request(method, url);
        if !authorization.is_empty() {
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        unwrap_envelope(send_json(request).await?, false)
    }
}

fn new_poll_token() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    hex::encode(bytes)
}

pub(crate) fn is_allowed_authorize_url(url: &str) -> bool {
    let candidate = url.trim();
    candidate.starts_with("https://zcode.z.ai/")
        || candidate.starts_with("https://chat.z.ai/")
        || candidate.starts_with("https://z.ai/")
        || candidate.starts_with("https://www.z.ai/")
}

pub(crate) fn credential_file_name(ready: &ZaiReady) -> String {
    let seed = if !ready.email.trim().is_empty() {
        ready.email.as_str()
    } else if !ready.user_id.trim().is_empty() {
        ready.user_id.as_str()
    } else {
        "account"
    };
    let slug: String = seed
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || character == '.'
                || character == '@'
                || character == '-'
            {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    format!("zai-{slug}.json")
}

pub(crate) fn credential_json(ready: &ZaiReady, api_key: &str) -> Value {
    json!({
        "type": "zai",
        "provider": "zai",
        "access_token": api_key,
        "zai_access_token": ready.access_token,
        "base_url": CODING_OPENAI_BASE_URL,
        "email": ready.email,
        "user_id": ready.user_id,
        "name": ready.name,
        "disabled": true
    })
}

async fn send_json(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Z.AI login could not reach Z.AI")?;
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("Z.AI login was rejected ({status})"));
    }
    Ok(body)
}

fn unwrap_envelope(body: Value, oauth: bool) -> Result<Value, String> {
    let code = body.get("code").cloned().unwrap_or(Value::Null);
    let ok = match &code {
        Value::Number(number) => {
            let value = number.as_i64().unwrap_or(-1);
            value == 0 || value == 200
        }
        Value::String(value) => value == "0" || value == "200" || value.is_empty(),
        Value::Null => true,
        _ => false,
    };
    if !ok {
        let message = body
            .get("msg")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        return Err(match message {
            Some(_) if oauth => "Z.AI login failed".into(),
            Some(_) => "Z.AI coding-plan provisioning failed".into(),
            None => "Z.AI login failed".into(),
        });
    }
    Ok(body.get("data").cloned().unwrap_or(body))
}

fn json_string(value: &Value, key: &str) -> Result<String, String> {
    Ok(value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string())
}

fn find_named_key(list: &Value) -> String {
    let items = match list {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map
            .get("list")
            .or_else(|| map.get("apiKeys"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    items
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(MINT_KEY_NAME))
        .and_then(|item| item.get("apiKey").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

fn select_org_project(customer: &Value) -> Result<(String, String), String> {
    let orgs = customer
        .get("organizations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if orgs.is_empty() {
        return Err(
            "This Z.AI account has no coding-plan organization. Subscribe to a GLM Coding Plan, then try again."
                .into(),
        );
    }
    let mut chosen = orgs[0].clone();
    for org in &orgs {
        let projects = org
            .get("projects")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        if projects == 0 {
            continue;
        }
        let name = org
            .get("organizationName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let is_default = name.contains("默认") || name.to_ascii_lowercase().contains("default");
        let chosen_empty = chosen
            .get("projects")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(true);
        if chosen_empty || is_default {
            chosen = org.clone();
            if is_default {
                break;
            }
        }
    }
    let projects = chosen
        .get("projects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if projects.is_empty() {
        return Err("This Z.AI account has no coding-plan project".into());
    }
    let mut project = projects[0].clone();
    for candidate in &projects {
        let name = candidate
            .get("projectName")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name.contains("默认") || name.to_ascii_lowercase().contains("default") {
            project = candidate.clone();
            break;
        }
    }
    let org_id = json_string(&chosen, "organizationId")?;
    let project_id = json_string(&project, "projectId")?;
    if org_id.is_empty() || project_id.is_empty() {
        return Err("Z.AI did not return an organization or project id".into());
    }
    Ok((org_id, project_id))
}

fn urlencoding(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_allowlist_rejects_lookalikes() {
        assert!(is_allowed_authorize_url(
            "https://chat.z.ai/auth?client=zcode"
        ));
        assert!(is_allowed_authorize_url(
            "https://zcode.z.ai/oauth/cli?flow=1"
        ));
        assert!(is_allowed_authorize_url("https://z.ai/authorize?x=1"));
        assert!(!is_allowed_authorize_url(
            "https://chat.z.ai.evil.example/auth"
        ));
        assert!(!is_allowed_authorize_url("http://chat.z.ai/auth"));
        assert!(!is_allowed_authorize_url("https://evil.example/z.ai"));
    }

    #[test]
    fn poll_token_is_32_random_bytes_hex() {
        let token = new_poll_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, new_poll_token());
    }

    #[test]
    fn credential_filename_is_provider_prefixed_and_safe() {
        let ready = ZaiReady {
            access_token: "oauth-token".into(),
            email: "Charles.3Ready@Gmail.com".into(),
            user_id: "u1".into(),
            name: "Charles".into(),
        };
        assert_eq!(
            credential_file_name(&ready),
            "zai-charles.3ready@gmail.com.json"
        );
        let no_email = ZaiReady {
            email: String::new(),
            user_id: "user/id with space".into(),
            ..ready
        };
        assert_eq!(
            credential_file_name(&no_email),
            "zai-user_id_with_space.json"
        );
    }

    #[test]
    fn credential_json_stores_minted_key_not_oauth_token_as_bearer() {
        let ready = ZaiReady {
            access_token: "oauth-token".into(),
            email: "a@b.com".into(),
            user_id: "u1".into(),
            name: "A".into(),
        };
        let value = credential_json(&ready, "key.secret");
        assert_eq!(value["type"], "zai");
        assert_eq!(value["provider"], "zai");
        assert_eq!(value["access_token"], "key.secret");
        assert_eq!(value["zai_access_token"], "oauth-token");
        assert_eq!(value["base_url"], CODING_OPENAI_BASE_URL);
        assert_eq!(value["email"], "a@b.com");
        assert_eq!(value["disabled"], true);
    }

    #[test]
    fn org_selection_prefers_default_org_with_projects() {
        let customer = json!({
            "organizations": [
                {"organizationId": "empty", "organizationName": "Empty", "projects": []},
                {
                    "organizationId": "org-default",
                    "organizationName": "Default Org",
                    "projects": [
                        {"projectId": "other", "projectName": "Other"},
                        {"projectId": "proj-default", "projectName": "Default Project"}
                    ]
                }
            ]
        });
        let (org, project) = select_org_project(&customer).unwrap();
        assert_eq!(org, "org-default");
        assert_eq!(project, "proj-default");
    }

    #[test]
    fn cli_poll_and_mint_against_a_local_zcode_contract() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..6 {
                let request = server.recv().unwrap();
                let url = request.url().to_string();
                let body = match url.as_str() {
                    "/api/v1/oauth/cli/init" => json!({
                        "code": 0,
                        "data": {
                            "flow_id": "flow-1",
                            "poll_token": "poll-1",
                            "authorize_url": "https://chat.z.ai/auth?flow=1",
                            "poll_interval_sec": 1
                        }
                    }),
                    "/api/v1/oauth/cli/poll/flow-1" => json!({
                        "code": 0,
                        "data": {
                            "status": "ready",
                            "token": "zcode-plan-token",
                            "zai": { "access_token": "oauth-access" },
                            "user": { "email": "glm@z.ai", "user_id": "u1", "name": "GLM" }
                        }
                    }),
                    "/api/auth/z/login" => json!({
                        "code": 0,
                        "data": { "access_token": "biz-token" }
                    }),
                    "/api/biz/customer/getCustomerInfo" => json!({
                        "code": 0,
                        "data": {
                            "organizations": [{
                                "organizationId": "org1",
                                "organizationName": "Default",
                                "projects": [{ "projectId": "proj1", "projectName": "Default" }]
                            }]
                        }
                    }),
                    "/api/biz/v1/organization/org1/projects/proj1/api_keys" => json!({
                        "code": 0,
                        "data": [{ "name": "zcode-api-key", "apiKey": "key1" }]
                    }),
                    "/api/biz/v1/organization/org1/projects/proj1/api_keys/copy/key1" => json!({
                        "code": 0,
                        "data": { "secretKey": "secret1" }
                    }),
                    other => panic!("unexpected path {other}"),
                };
                let payload = serde_json::to_string(&body).unwrap();
                request
                    .respond(tiny_http::Response::from_string(payload))
                    .unwrap();
            }
        });

        let origin = format!("http://{addr}");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let client = ZaiOAuth::new(&format!("{origin}/api/v1"), &origin).unwrap();
            let init = client.start_cli_flow().await.unwrap();
            assert_eq!(init.flow_id, "flow-1");
            assert_eq!(init.authorize_url, "https://chat.z.ai/auth?flow=1");
            let cancel = AtomicBool::new(false);
            let ready = client.wait_for_authorization(&init, &cancel).await.unwrap();
            assert_eq!(ready.email, "glm@z.ai");
            assert_eq!(ready.access_token, "oauth-access");
            let minted = client.mint_coding_plan_key(&ready).await.unwrap();
            assert_eq!(minted, "key1.secret1");
        });
        handle.join().unwrap();
    }
}
