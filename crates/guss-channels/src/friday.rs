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
/// One sync round (friday_sync::SYNC_EVERY).
const PRODUCTS_TTL: Duration = Duration::from_secs(30);
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
    /// Their Friday user id, and who they report to ("" for nobody).
    pub user_id: String,
    pub reports_to: String,
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

    /// A team leader's team as actor ids: everyone linked who reports to
    /// them, by Telegram and by the Guss widget in Friday.
    pub fn team_of(&self, telegram_id: i64) -> Vec<String> {
        let Some(lead) = self
            .by_telegram
            .get(&telegram_id)
            .filter(|p| !p.user_id.is_empty())
        else {
            return Vec::new();
        };
        let mut team = Vec::new();
        for p in self
            .by_telegram
            .values()
            .filter(|p| p.reports_to == lead.user_id)
        {
            team.push(format!("telegram:{}", p.telegram_id));
            team.push(format!("friday:{}", p.user_id));
        }
        team
    }

    /// The team leader `actor_id` reports to, if they're linked.
    pub fn team_leader_of(&self, actor_id: &str) -> Option<i64> {
        let me = self.by_telegram.values().find(|p| {
            actor_id == format!("telegram:{}", p.telegram_id)
                || actor_id == format!("friday:{}", p.user_id)
        })?;
        self.by_telegram
            .values()
            .find(|p| {
                p.role == Some(Role::TeamLeader)
                    && !me.reports_to.is_empty()
                    && p.user_id == me.reports_to
            })
            .map(|p| p.telegram_id)
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
    /// Friday's own role ("team_leader"); Friday sends guss_role "staff"
    /// for team leaders so older Guss versions keep them as staff.
    #[serde(default)]
    role: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    reports_to: String,
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
    /// Keyed by the actor id: "telegram:<id>" or "friday:<user id>".
    tokens: Mutex<HashMap<String, (String, Instant)>>,
    /// Inventory's product list per token, for one sync round (each entry
    /// used to download the whole catalogue).
    products: Mutex<HashMap<String, (Instant, Arc<Value>)>>,
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
            products: Mutex::new(HashMap::new()),
        }
    }

    /// Inventory's products as `token` sees them, reused for PRODUCTS_TTL.
    pub async fn products(&self, token: &str) -> Result<Arc<Value>, FridayError> {
        if let Some((at, list)) = self.products.lock().await.get(token)
            && at.elapsed() < PRODUCTS_TTL
        {
            return Ok(list.clone());
        }
        let list = Arc::new(self.get(token, "/api/inventory/products").await?);
        let mut cache = self.products.lock().await;
        if cache.len() > 200 {
            cache.clear();
        }
        cache.insert(token.to_string(), (Instant::now(), list.clone()));
        Ok(list)
    }

    /// After adding a product, so the next lookup sees it.
    pub async fn forget_products(&self) {
        self.products.lock().await.clear();
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

    /// Starts a step-up check (plan 16.4) for a linked person: Friday's
    /// check id and the one-time link they confirm on.
    pub async fn start_step_up(
        &self,
        telegram_id: i64,
        action: &str,
    ) -> Result<(String, String), FridayError> {
        let resp = self
            .http
            .post(format!("{}/guss/svc/step-up", self.base))
            .bearer_auth(&self.key)
            .json(&json!({"telegram_id": telegram_id, "action": action}))
            .send()
            .await
            .map_err(Self::net)?;
        let v: Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        let field = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok((field("id"), field("link")))
    }

    /// Whether the person has confirmed step-up check `id`.
    pub async fn step_up_done(&self, id: &str) -> Result<bool, FridayError> {
        let resp = self
            .http
            .get(format!("{}/guss/svc/step-up/{id}", self.base))
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(Self::net)?;
        let v: Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        Ok(v.get("done").and_then(Value::as_bool).unwrap_or(false))
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

    /// Tells Friday this bot's username, so its "Link my Telegram" button
    /// can open the bot (plan 16.2).
    pub async fn report_bot(&self, username: &str) -> Result<(), FridayError> {
        let resp = self
            .http
            .post(format!("{}/guss/svc/bot", self.base))
            .bearer_auth(&self.key)
            .json(&json!({"username": username}))
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await?;
        Ok(())
    }

    /// Redeems the one-time code from Friday's "Link my Telegram" (the
    /// /start link-<code> payload): links this Telegram user to the person
    /// who made it. Returns their Guss role ("owner", "manager", "staff").
    pub async fn redeem(
        &self,
        code: &str,
        telegram_id: i64,
        name: &str,
    ) -> Result<String, FridayError> {
        let resp = self
            .http
            .post(format!("{}/guss/svc/redeem", self.base))
            .bearer_auth(&self.key)
            .json(&json!({"code": code, "telegram_id": telegram_id, "name": name}))
            .send()
            .await
            .map_err(Self::net)?;
        let v: Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        Ok(v.get("guss_role")
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
                            role: match (p.guss_role.as_str(), p.role.as_str()) {
                                ("staff", "team_leader") => Some(Role::TeamLeader),
                                (r, _) => Role::parse(r),
                            },
                            user_id: p.user_id,
                            reports_to: p.reports_to,
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
        self.token_as(
            format!("telegram:{telegram_id}"),
            json!({"telegram_id": telegram_id}),
        )
        .await
    }

    /// A token for whoever made an entry: a linked Telegram person, or a
    /// Friday user who typed into the Guss widget in Friday
    /// ("friday:<user id>"). None for anyone else, or if not linked.
    pub async fn token_for_actor(&self, actor_id: &str) -> Result<Option<String>, FridayError> {
        if let Some(t) = telegram_id_of(actor_id) {
            return self.token_for(t).await;
        }
        match actor_id.strip_prefix("friday:") {
            Some(user) if !user.is_empty() => {
                self.token_as(actor_id.to_string(), json!({"user_id": user}))
                    .await
            }
            _ => Ok(None),
        }
    }

    async fn token_as(&self, key: String, body: Value) -> Result<Option<String>, FridayError> {
        {
            let tokens = self.tokens.lock().await;
            if let Some((token, until)) = tokens.get(&key)
                && Instant::now() < *until
            {
                return Ok(Some(token.clone()));
            }
        }
        let resp = self
            .http
            .post(format!("{}/guss/svc/token", self.base))
            .bearer_auth(&self.key)
            .json(&body)
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
            .insert(key, (wire.token.clone(), until));
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

    /// PUT raw bytes (a photo) to an app route as `token`.
    pub async fn put_bytes(
        &self,
        token: &str,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Value, FridayError> {
        let resp = self
            .http
            .put(format!("{}{path}", self.base))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_team_leader_gets_their_team_and_their_teams_requests() {
        let person = |tg: i64, role: Role, user: &str, boss: &str| {
            (
                tg,
                Person {
                    telegram_id: tg,
                    name: user.into(),
                    role: Some(role),
                    user_id: user.into(),
                    reports_to: boss.into(),
                },
            )
        };
        let people = People {
            by_telegram: [
                person(1, Role::TeamLeader, "u-tl", ""),
                person(2, Role::Staff, "u-mary", "u-tl"),
                person(3, Role::Staff, "u-ann", ""),
            ]
            .into_iter()
            .collect(),
            access: HashMap::new(),
        };
        assert_eq!(
            people.team_of(1),
            vec!["telegram:2".to_string(), "friday:u-mary".to_string()]
        );
        assert!(people.team_of(3).is_empty());
        assert_eq!(people.team_leader_of("telegram:2"), Some(1));
        assert_eq!(people.team_leader_of("friday:u-mary"), Some(1));
        assert_eq!(people.team_leader_of("telegram:3"), None);
    }
}
