//! Guss inside Friday: the kernel's Guss endpoints (Friday repo,
//! kernel/internal/guss/link.go). With a Friday URL and service key Guss
//! asks Friday who each Telegram user is (their Friday role is their Guss
//! role) and gets a short-lived token per person to act in Friday apps,
//! limited to what the org allowed per app.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use guss_core::people::Role;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;

/// How long Guss trusts its copy of who is linked (and their roles).
const PEOPLE_TTL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Error)]
pub enum FridayError {
    #[error("Friday couldn't be reached: {0}")]
    Network(String),
    #[error("Friday refused Guss's key (check Friday key in the Guss settings)")]
    BadKey,
    /// The org didn't give Guss this access, or the person's own role
    /// doesn't allow it. Not retried.
    #[error("Friday said no: {0}")]
    Forbidden(String),
    #[error("Friday answered {status}: {body}")]
    Status { status: u16, body: String },
}

impl FridayError {
    /// Worth retrying later (network trouble, Friday down), as opposed to
    /// a decision that won't change by itself.
    pub fn is_transient(&self) -> bool {
        match self {
            FridayError::Network(_) => true,
            FridayError::Status { status, .. } => *status >= 500 || *status == 429,
            FridayError::BadKey | FridayError::Forbidden(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub telegram_id: i64,
    pub name: String,
    /// None: linked, but their Friday role (viewer) can't use Guss.
    pub role: Option<Role>,
}

#[derive(Debug, Clone, Default)]
pub struct People {
    pub by_telegram: HashMap<i64, Person>,
    /// app -> "none" | "read" | "write"
    pub access: HashMap<String, String>,
}

impl People {
    pub fn can_write(&self, app: &str) -> bool {
        self.access.get(app).map(String::as_str) == Some("write")
    }
}

#[derive(Deserialize)]
struct PeopleWire {
    people: Vec<PersonWire>,
    #[serde(default)]
    access: HashMap<String, String>,
}

#[derive(Deserialize)]
struct PersonWire {
    telegram_id: i64,
    #[serde(default)]
    telegram_name: String,
    #[serde(default)]
    guss_role: String,
}

#[derive(Deserialize)]
struct TokenWire {
    token: String,
    expires_in: u64,
}

pub struct FridayClient {
    base: String,
    key: String,
    http: reqwest::Client,
    people: Mutex<Option<(Instant, Arc<People>)>>,
    tokens: Mutex<HashMap<i64, (String, Instant)>>,
}

impl FridayClient {
    /// `base` is the Friday API address (the kernel), e.g.
    /// https://friday-api.example.com.
    pub fn new(base: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            key: key.into(),
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("reqwest client builds"),
            people: Mutex::new(None),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, FridayError> {
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or(body);
        Err(match status {
            401 => FridayError::BadKey,
            403 => FridayError::Forbidden(msg),
            _ => FridayError::Status { status, body: msg },
        })
    }

    fn net(e: reqwest::Error) -> FridayError {
        FridayError::Network(e.without_url().to_string())
    }

    /// A 6-digit code the person enters in Friday to link their account.
    pub async fn link_code(&self, telegram_id: i64, name: &str) -> Result<String, FridayError> {
        let resp = self
            .http
            .post(format!("{}/guss/svc/link-code", self.base))
            .bearer_auth(&self.key)
            .json(&json!({"telegram_id": telegram_id, "name": name}))
            .send()
            .await
            .map_err(Self::net)?;
        let v: Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        Ok(v.get("code")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Linked people and the org's access matrix, cached for a minute.
    pub async fn people(&self) -> Result<Arc<People>, FridayError> {
        let mut cached = self.people.lock().await;
        if let Some((at, people)) = cached.as_ref()
            && at.elapsed() < PEOPLE_TTL
        {
            return Ok(people.clone());
        }
        let resp = self
            .http
            .get(format!("{}/guss/svc/people", self.base))
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(Self::net)?;
        let wire: PeopleWire = Self::check(resp).await?.json().await.map_err(Self::net)?;
        let people = Arc::new(People {
            by_telegram: wire
                .people
                .into_iter()
                .map(|p| {
                    (
                        p.telegram_id,
                        Person {
                            telegram_id: p.telegram_id,
                            name: p.telegram_name,
                            role: Role::parse(&p.guss_role),
                        },
                    )
                })
                .collect(),
            access: wire.access,
        });
        *cached = Some((Instant::now(), people.clone()));
        Ok(people)
    }

    /// Forget the cached people (after someone links, so it shows at once).
    pub async fn refresh(&self) {
        *self.people.lock().await = None;
    }

    /// A token to act in Friday as this person; None if they aren't linked.
    pub async fn token_for(&self, telegram_id: i64) -> Result<Option<String>, FridayError> {
        {
            let tokens = self.tokens.lock().await;
            if let Some((token, until)) = tokens.get(&telegram_id)
                && Instant::now() < *until
            {
                return Ok(Some(token.clone()));
            }
        }
        let resp = self
            .http
            .post(format!("{}/guss/svc/token", self.base))
            .bearer_auth(&self.key)
            .json(&json!({"telegram_id": telegram_id}))
            .send()
            .await
            .map_err(Self::net)?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let wire: TokenWire = Self::check(resp).await?.json().await.map_err(Self::net)?;
        // Renew a minute early so a token never expires mid-request.
        let until = Instant::now() + Duration::from_secs(wire.expires_in.saturating_sub(60));
        self.tokens
            .lock()
            .await
            .insert(telegram_id, (wire.token.clone(), until));
        Ok(Some(wire.token))
    }

    /// GET an app route (`/api/{app}/...`) as `token`.
    pub async fn get(&self, token: &str, path: &str) -> Result<Value, FridayError> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await?.json().await.map_err(Self::net)
    }

    /// POST to an app route as `token`.
    pub async fn post(&self, token: &str, path: &str, body: &Value) -> Result<Value, FridayError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await?.json().await.map_err(Self::net)
    }
}

/// "telegram:12345" -> 12345.
pub fn telegram_id_of(actor_id: &str) -> Option<i64> {
    actor_id.strip_prefix("telegram:")?.parse().ok()
}
