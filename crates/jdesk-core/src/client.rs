//! Talking to Jira. [`Jira`] is what sync and the change queue need; [`HttpJira`] does it over
//! REST, and [`crate::demo::DemoJira`] does it in memory for demo mode and tests.

use crate::adf;
use crate::config::{Api, Config};
use crate::model::Person;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::time::Duration;

/// One page of search results: raw issue JSON, and the token for the next page.
pub struct Page {
    pub issues: Vec<Value>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub id: String,
    pub name: String,
    /// The status it leads to.
    pub to: String,
}

pub trait Jira: Send + Sync {
    /// The signed-in user.
    fn myself(&self) -> Result<Person>;
    /// `/field`: every field, for [`crate::parse::FieldMap::discover`].
    fn fields(&self) -> Result<Value>;
    fn search(&self, jql: &str, fields: &[String], next: Option<&str>) -> Result<Page>;
    fn issue(&self, key: &str, fields: &[String]) -> Result<Value>;
    fn transitions(&self, key: &str) -> Result<Vec<Transition>>;
    fn transition(&self, key: &str, id: &str) -> Result<()>;
    /// `PUT /issue/{key}` with `{"fields": …}` and/or `{"update": …}`.
    fn edit(&self, key: &str, body: &Value) -> Result<()>;
    fn assign(&self, key: &str, account: Option<&str>) -> Result<()>;
    fn comment(&self, key: &str, text: &str) -> Result<()>;
    fn move_to_sprint(&self, sprint: u64, key: &str) -> Result<()>;
    /// People who can be assigned issues in `project`.
    fn assignable(&self, project: &str) -> Result<Vec<Person>>;
    fn browse_url(&self, key: &str) -> String;
    /// An attachment's bytes (at most [`MAX_DOWNLOAD`]).
    fn download(&self, url: &str) -> Result<Vec<u8>>;
}

/// The largest attachment downloaded for showing inline.
pub const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;

pub struct HttpJira {
    site: String,
    api: Api,
    auth: String,
    agent: ureq::Agent,
}

impl HttpJira {
    pub fn new(config: &Config) -> Result<Self> {
        let token = config.token()?;
        let auth = match (config.api, &config.email) {
            (Api::Cloud, Some(email)) => format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}"))
            ),
            _ => format!("Bearer {token}"),
        };
        Ok(Self::build(config.site(), config.api, auth, true))
    }

    /// A client with a ready-made `Authorization` header, ignoring proxy settings (tests).
    pub fn with_auth(site: &str, api: Api, auth: String) -> Self {
        Self::build(site, api, auth, false)
    }

    fn build(site: &str, api: Api, auth: String, proxy_from_env: bool) -> Self {
        Self {
            site: site.trim_end_matches('/').to_string(),
            api,
            auth,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(15))
                .timeout(Duration::from_secs(60))
                .user_agent("jira-desk")
                // HTTPS_PROXY / ALL_PROXY, for networks that need one.
                .try_proxy_from_env(proxy_from_env)
                .build(),
        }
    }

    fn v(&self) -> &'static str {
        match self.api {
            Api::Cloud => "3",
            Api::Server => "2",
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.site, path.replace("{v}", self.v()))
    }

    /// Sends a request, retrying when Jira says to slow down (429/503 with Retry-After).
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Option<Value>> {
        let url = self.url(path);
        let mut attempt = 0;
        loop {
            let req = self
                .agent
                .request(method, &url)
                .set("Authorization", &self.auth)
                .set("Accept", "application/json");
            let res = match body {
                Some(b) => req.send_json(b.clone()),
                None => req.call(),
            };
            match res {
                Ok(r) => {
                    if r.status() == 204 {
                        return Ok(None);
                    }
                    let text = r.into_string().context("reading Jira's reply")?;
                    if text.trim().is_empty() {
                        return Ok(None);
                    }
                    return serde_json::from_str(&text)
                        .map(Some)
                        .with_context(|| format!("Jira's reply to {method} {path} is not JSON"));
                }
                Err(ureq::Error::Status(code, r))
                    if (code == 429 || code == 503) && attempt < 4 =>
                {
                    let wait = r
                        .header("Retry-After")
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(2u64.pow(attempt))
                        .min(60);
                    std::thread::sleep(Duration::from_secs(wait));
                    attempt += 1;
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    bail!("{method} {path}: {code} {}", jira_message(&text));
                }
                Err(e) => return Err(anyhow!("{method} {path}: {e}")),
            }
        }
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.call("GET", path, None)?
            .ok_or_else(|| anyhow!("GET {path}: empty reply"))
    }
}

/// Jira's error text from an error body (`errorMessages`, `errors`), else the body itself.
fn jira_message(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(300).collect();
    };
    let mut parts: Vec<String> = v
        .get("errorMessages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect();
    if let Some(errs) = v.get("errors").and_then(Value::as_object) {
        for (k, m) in errs {
            parts.push(format!("{k}: {}", m.as_str().unwrap_or_default()));
        }
    }
    if parts.is_empty() {
        body.chars().take(300).collect()
    } else {
        parts.join("; ")
    }
}

pub(crate) fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn people(v: &Value) -> Vec<Person> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let id = u
                .get("accountId")
                .or_else(|| u.get("name"))?
                .as_str()?
                .to_string();
            let name = u
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            Some(Person { id, name })
        })
        .collect()
}

impl Jira for HttpJira {
    fn myself(&self) -> Result<Person> {
        let v = self.get("/rest/api/{v}/myself")?;
        people(&json!([v]))
            .pop()
            .ok_or_else(|| anyhow!("/myself returned no user"))
    }

    fn fields(&self) -> Result<Value> {
        self.get("/rest/api/{v}/field")
    }

    fn search(&self, jql: &str, fields: &[String], next: Option<&str>) -> Result<Page> {
        match self.api {
            Api::Cloud => {
                let mut body = json!({"jql": jql, "fields": fields, "maxResults": 100});
                if let Some(t) = next {
                    body["nextPageToken"] = json!(t);
                }
                let v = self
                    .call("POST", "/rest/api/3/search/jql", Some(&body))?
                    .unwrap_or_default();
                Ok(Page {
                    issues: v["issues"].as_array().cloned().unwrap_or_default(),
                    next: v["nextPageToken"].as_str().map(str::to_string),
                })
            }
            Api::Server => {
                let start: u64 = next.and_then(|n| n.parse().ok()).unwrap_or(0);
                let body =
                    json!({"jql": jql, "fields": fields, "maxResults": 100, "startAt": start});
                let v = self
                    .call("POST", "/rest/api/2/search", Some(&body))?
                    .unwrap_or_default();
                let issues = v["issues"].as_array().cloned().unwrap_or_default();
                let total = v["total"].as_u64().unwrap_or(0);
                let after = start + issues.len() as u64;
                Ok(Page {
                    next: (!issues.is_empty() && after < total).then(|| after.to_string()),
                    issues,
                })
            }
        }
    }

    fn issue(&self, key: &str, fields: &[String]) -> Result<Value> {
        self.get(&format!(
            "/rest/api/{{v}}/issue/{}?fields={}",
            enc(key),
            enc(&fields.join(","))
        ))
    }

    fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let v = self.get(&format!("/rest/api/{{v}}/issue/{}/transitions", enc(key)))?;
        Ok(v["transitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(Transition {
                    id: t["id"].as_str()?.to_string(),
                    name: t["name"].as_str().unwrap_or_default().to_string(),
                    to: t["to"]["name"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    fn transition(&self, key: &str, id: &str) -> Result<()> {
        self.call(
            "POST",
            &format!("/rest/api/{{v}}/issue/{}/transitions", enc(key)),
            Some(&json!({"transition": {"id": id}})),
        )
        .map(|_| ())
    }

    fn edit(&self, key: &str, body: &Value) -> Result<()> {
        self.call(
            "PUT",
            &format!("/rest/api/{{v}}/issue/{}", enc(key)),
            Some(body),
        )
        .map(|_| ())
    }

    fn assign(&self, key: &str, account: Option<&str>) -> Result<()> {
        let body = match self.api {
            Api::Cloud => json!({"accountId": account}),
            Api::Server => json!({"name": account}),
        };
        self.call(
            "PUT",
            &format!("/rest/api/{{v}}/issue/{}/assignee", enc(key)),
            Some(&body),
        )
        .map(|_| ())
    }

    fn comment(&self, key: &str, text: &str) -> Result<()> {
        let body = match self.api {
            Api::Cloud => json!({"body": adf::doc_from_text(text)}),
            Api::Server => json!({"body": text}),
        };
        self.call(
            "POST",
            &format!("/rest/api/{{v}}/issue/{}/comment", enc(key)),
            Some(&body),
        )
        .map(|_| ())
    }

    fn move_to_sprint(&self, sprint: u64, key: &str) -> Result<()> {
        self.call(
            "POST",
            &format!("/rest/agile/1.0/sprint/{sprint}/issue"),
            Some(&json!({"issues": [key]})),
        )
        .map(|_| ())
    }

    fn assignable(&self, project: &str) -> Result<Vec<Person>> {
        let mut out = Vec::new();
        let mut start = 0;
        loop {
            let v = self.get(&format!(
                "/rest/api/{{v}}/user/assignable/search?project={}&startAt={start}&maxResults=100",
                enc(project)
            ))?;
            let page = people(&v);
            let n = page.len();
            out.extend(page);
            if n < 100 || out.len() >= 1000 {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.site)
    }

    fn download(&self, url: &str) -> Result<Vec<u8>> {
        // Only from this site: an attachment URL is data from Jira, and the token goes with it.
        if !url.starts_with(&format!("{}/", self.site)) {
            bail!("not an attachment on {}: {url}", self.site);
        }
        let res = self
            .agent
            .get(url)
            .set("Authorization", &self.auth)
            .call()
            .map_err(|e| anyhow!("GET {url}: {e}"))?;
        let mut bytes = Vec::new();
        use std::io::Read;
        res.into_reader()
            .take(MAX_DOWNLOAD + 1)
            .read_to_end(&mut bytes)
            .context("reading the attachment")?;
        if bytes.len() as u64 > MAX_DOWNLOAD {
            bail!("larger than {} MB", MAX_DOWNLOAD / 1024 / 1024);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(
            jira_message(r#"{"errorMessages":["Issue does not exist"],"errors":{}}"#),
            "Issue does not exist"
        );
        assert_eq!(
            jira_message(r#"{"errorMessages":[],"errors":{"summary":"too long"}}"#),
            "summary: too long"
        );
        assert_eq!(enc("project = ABC"), "project%20%3D%20ABC");
    }
}
