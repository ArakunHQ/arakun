//! Signing in to a Doop instance in the browser: the OAuth flow its MCP
//! server asks of every client (authorization code with PKCE, RFC 8414
//! discovery, RFC 7591 registration). Nothing here is tied to one instance:
//! the endpoints come from the instance's own metadata, so a self-hosted one
//! works the same way.
//!
//! The browser comes back to a listener on 127.0.0.1 that lives only for the
//! sign-in. The code it brings is useless without the verifier, which never
//! leaves this process.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use super::{Credential, Error, Session, encode, host_label, mcp_url, run};

/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(5 * 60);
/// What the tab asks for: who the user is, and a token that can be renewed.
const SCOPES: [&str; 4] = ["openid", "profile", "email", "offline_access"];

/// What a browser sign-in leaves behind. Debug never shows the secrets.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    /// Empty in the sessions file: the keyring keeps it, with the refresh
    /// token and the client secret.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    /// Unix seconds; the token is traded for a new one a minute before.
    pub expires_at: u64,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(…)")
    }
}

/// The endpoints of an instance's authorization server.
#[derive(Clone, Debug, PartialEq)]
struct Meta {
    authorize: String,
    token: String,
    register: Option<String>,
    userinfo: Option<String>,
    scopes: Vec<String>,
}

fn get_json(url: &str, token: Option<&str>) -> Result<(u16, Value), Error> {
    let mut builder = ureq::http::Request::builder()
        .method("GET")
        .uri(url)
        .header("Accept", "application/json");
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let (status, bytes) = run(builder, None, 1024 * 1024)?;
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

/// An endpoint from the metadata, if it is one a token may be sent to:
/// https, or plain http to a local address when the instance itself is
/// reached that way. A local instance can't send tokens to a public http one.
fn endpoint(host: &str, v: &Value) -> Option<String> {
    let url = v.as_str()?.trim();
    if url.contains(char::is_whitespace) {
        return None;
    }
    let plain = url.starts_with("http://");
    // `normalize_host` takes https anywhere and http only on a local address.
    let ok = super::normalize_host(url).is_ok()
        && url.contains("://")
        && (!plain || host.starts_with("http://"));
    ok.then(|| url.to_string())
}

fn parse_meta(host: &str, v: &Value) -> Option<Meta> {
    Some(Meta {
        authorize: endpoint(host, &v["authorization_endpoint"])?,
        token: endpoint(host, &v["token_endpoint"])?,
        register: endpoint(host, &v["registration_endpoint"]),
        userinfo: endpoint(host, &v["userinfo_endpoint"]),
        scopes: v["scopes_supported"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// `https://login.example/tenant` → (`https://login.example`, `/tenant`).
fn split_issuer(issuer: &str) -> (&str, &str) {
    let issuer = issuer.trim_end_matches('/');
    let after_scheme = issuer.find("://").map_or(0, |i| i + 3);
    match issuer[after_scheme..].find('/') {
        Some(i) => issuer.split_at(after_scheme + i),
        None => (issuer, ""),
    }
}

/// Where the metadata of the MCP endpoint may be, in the order to ask, with
/// the resources each may describe: the endpoint itself (RFC 9728 puts the
/// well-known part before its path), then the instance as a whole. Doop
/// names the endpoint in both, so the instance-wide one may too.
fn resource_urls(host: &str) -> [(String, Vec<String>); 2] {
    let mcp = super::mcp_url(host);
    [
        (
            format!("{host}/.well-known/oauth-protected-resource/mcp"),
            vec![mcp.clone()],
        ),
        (
            format!("{host}/.well-known/oauth-protected-resource"),
            vec![host.to_string(), mcp],
        ),
    ]
}

/// The same identifier, give or take a trailing slash.
fn same_id(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The authorization server a protected-resource document names, if the
/// document is about one of `resources` (RFC 9728 §3.3). A document about
/// another resource says nothing about this one.
fn resource_issuer(host: &str, resources: &[String], v: &Value) -> Option<String> {
    let resource = v["resource"].as_str()?;
    if !resources.iter().any(|r| same_id(r, resource)) {
        return None;
    }
    endpoint(host, &v["authorization_servers"][0])
}

/// The metadata in `v`, if it is the metadata of `issuer` (RFC 8414 §3.3):
/// one issuer's document can't send a sign-in to another's endpoints.
fn issuer_meta(host: &str, issuer: &str, v: &Value) -> Option<Meta> {
    if !same_id(v["issuer"].as_str()?, issuer) {
        return None;
    }
    parse_meta(host, v)
}

/// Where an authorization server's metadata may be, in the order to ask.
/// RFC 8414 and OpenID Connect each put the well-known part before the
/// issuer's path; servers from before them put it after.
fn issuer_urls(issuer: &str) -> Vec<String> {
    let (origin, path) = split_issuer(issuer);
    let mut urls = vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
    ];
    if !path.is_empty() {
        urls.push(format!(
            "{origin}{path}/.well-known/oauth-authorization-server"
        ));
        urls.push(format!("{origin}{path}/.well-known/openid-configuration"));
    }
    urls
}

/// Where `host` wants its users to sign in: the authorization server its MCP
/// endpoint names (itself, when it names none), then that server's metadata.
fn discover(host: &str) -> Result<Meta, Error> {
    let issuer = resource_urls(host)
        .iter()
        .find_map(|(url, resources)| {
            let (status, v) = get_json(url, None).ok()?;
            (200..=299)
                .contains(&status)
                .then(|| resource_issuer(host, resources, &v))
                .flatten()
        })
        .unwrap_or_else(|| host.to_string());
    for url in issuer_urls(&issuer) {
        if let (200..=299, v) = get_json(&url, None)?
            && let Some(meta) = issuer_meta(host, &issuer, &v)
        {
            return Ok(meta);
        }
    }
    Err(Error::Failed(format!(
        "{} doesn't offer a sign-in in the browser. Use an agent key instead.",
        host_label(host)
    )))
}

fn post_json(url: &str, body: &Value) -> Result<(u16, Value), Error> {
    let builder = crate::http::request("POST", url, None).header("Accept", "application/json");
    let (status, bytes) = run(builder, Some(body.to_string()), 1024 * 1024)?;
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<(u16, Value), Error> {
    let builder = ureq::http::Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json");
    let (status, bytes) = run(builder, Some(form(fields)), 1024 * 1024)?;
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

/// What the server said went wrong, in its own words when it gave any.
fn said(v: &Value, status: u16) -> String {
    v["error_description"]
        .as_str()
        .or(v["error"].as_str())
        .or(v["message"].as_str())
        .map_or_else(|| format!("it answered {status}"), str::to_string)
}

fn random(bytes: usize) -> Result<String, Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|e| Error::Failed(format!("No random bytes for the sign-in: {e}")))?;
    Ok(B64.encode(buf))
}

fn challenge(verifier: &str) -> String {
    B64.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// The address the browser opens.
fn authorize_url(
    meta: &Meta,
    host: &str,
    client_id: &str,
    redirect: &str,
    verifier: &str,
    state: &str,
) -> String {
    let scopes: Vec<&str> = SCOPES
        .into_iter()
        .filter(|s| meta.scopes.is_empty() || meta.scopes.iter().any(|o| o == s))
        .collect();
    let query = form(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("scope", &scopes.join(" ")),
        ("resource", &mcp_url(host)),
    ]);
    let sep = if meta.authorize.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{}{sep}{query}", meta.authorize)
}

/// `%41` → `A`, `+` → a space: a query value as the browser sent it.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a request to the listener means for the sign-in.
#[derive(Debug, PartialEq)]
enum Callback {
    Code(String),
    /// The instance or the user said no.
    Refused(String),
    /// Not the callback (a favicon), or not from this sign-in: keep waiting.
    Other,
}

/// Reads `GET /callback?code=…&state=… HTTP/1.1`. A callback with another
/// state did not come from the page this sign-in opened.
fn callback(request_line: &str, state: &str) -> Callback {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return Callback::Other;
    };
    let Some(query) = target.strip_prefix("/callback?") else {
        return Callback::Other;
    };
    let value = |name: &str| {
        query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| decode(v))
    };
    if value("state").as_deref() != Some(state) {
        return Callback::Other;
    }
    match (value("code"), value("error")) {
        (Some(code), _) if !code.is_empty() => Callback::Code(code),
        (_, Some(e)) => Callback::Refused(value("error_description").unwrap_or(e)),
        _ => Callback::Other,
    }
}

fn respond(mut stream: &TcpStream, status: &str, line: &str) {
    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Arakun</title>\
         <body style=\"font:16px system-ui;background:#0b0b0b;color:#fff;display:grid;place-items:center;height:100vh;margin:0\">\
         <p>{line}</p></body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
}

/// Waits for the browser to come back with the code.
fn wait_for_code(
    listener: &TcpListener,
    state: &str,
    canceled: &dyn Fn() -> bool,
) -> Result<String, Error> {
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::Failed(e.to_string()))?;
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if canceled() {
            return Err(Error::Failed("The sign-in was canceled.".into()));
        }
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(150));
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        // The first line holds everything; a longer one is not the callback.
        let _ = BufReader::new(&stream).take(16 * 1024).read_line(&mut line);
        match callback(&line, state) {
            Callback::Code(code) => {
                respond(
                    &stream,
                    "200 OK",
                    "Signed in to Doop. You can go back to Arakun.",
                );
                return Ok(code);
            }
            Callback::Refused(why) => {
                respond(
                    &stream,
                    "200 OK",
                    "The sign-in was not approved. You can close this page.",
                );
                return Err(Error::Failed(format!(
                    "The sign-in was not approved ({why})."
                )));
            }
            Callback::Other => respond(&stream, "404 Not Found", "Nothing here."),
        }
    }
    Err(Error::Failed(
        "Nobody approved the sign-in in the browser. Try again when you are ready.".into(),
    ))
}

fn tokens_from(
    v: &Value,
    client_id: &str,
    client_secret: Option<&str>,
    token_endpoint: &str,
    old_refresh: Option<&str>,
    now: u64,
) -> Option<Tokens> {
    Some(Tokens {
        client_id: client_id.to_string(),
        client_secret: client_secret.map(str::to_string),
        token_endpoint: token_endpoint.to_string(),
        access: v["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())?
            .to_string(),
        // A server that doesn't rotate the refresh token leaves the old one valid.
        refresh: v["refresh_token"]
            .as_str()
            .or(old_refresh)
            .map(str::to_string),
        expires_at: now + v["expires_in"].as_u64().unwrap_or(3600),
    })
}

/// Trades the refresh token for a new access token.
pub(super) fn refresh(t: &Tokens) -> Result<Tokens, Error> {
    let again = || Error::SignedOut("The Doop sign-in ran out. Sign in again.".to_string());
    let refresh = t.refresh.as_deref().ok_or_else(again)?;
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", t.client_id.as_str()),
    ];
    if let Some(secret) = &t.client_secret {
        fields.push(("client_secret", secret));
    }
    let (status, v) = post_form(&t.token_endpoint, &fields)?;
    match status {
        200..=299 => tokens_from(
            &v,
            &t.client_id,
            t.client_secret.as_deref(),
            &t.token_endpoint,
            Some(refresh),
            crate::model::unix_now(),
        )
        .ok_or_else(again),
        400 | 401 | 403 => Err(again()),
        _ => Err(Error::Failed(format!(
            "Couldn't renew the Doop sign-in: {}.",
            said(&v, status)
        ))),
    }
}

/// Who the token belongs to: (subject, email, name), empty where unknown.
fn who(meta: &Meta, access: &str) -> (String, String, String) {
    let Some(url) = &meta.userinfo else {
        return Default::default();
    };
    let Ok((200..=299, v)) = get_json(url, Some(access)) else {
        return Default::default();
    };
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    (s("sub"), s("email"), s("name"))
}

/// Signs in to `host` in the browser: `open` gets the address to show, and
/// the call returns once the user approved or refused, or `canceled` says so.
pub fn sign_in(
    host: &str,
    canceled: &dyn Fn() -> bool,
    open: impl FnOnce(&str),
) -> Result<Session, Error> {
    let meta = discover(host)?;
    let register = meta.register.as_deref().ok_or_else(|| {
        Error::Failed(format!(
            "{} doesn't let apps register for a sign-in. Use an agent key instead.",
            host_label(host)
        ))
    })?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| Error::Failed(format!("Couldn't listen for the sign-in: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Failed(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let (status, client) = post_json(
        register,
        &json!({
            "client_name": "Arakun",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }),
    )?;
    let client_id = client["client_id"].as_str().ok_or_else(|| {
        Error::Failed(format!(
            "{} didn't register Arakun for a sign-in: {}.",
            host_label(host),
            said(&client, status)
        ))
    })?;
    let client_secret = client["client_secret"].as_str().filter(|s| !s.is_empty());
    // Registering doesn't stop for Cancel; a tab opened after it would lead nowhere.
    if canceled() {
        return Err(Error::Failed("The sign-in was canceled.".into()));
    }
    let verifier = random(32)?;
    let state = random(16)?;
    open(&authorize_url(
        &meta, host, client_id, &redirect, &verifier, &state,
    ));
    let code = wait_for_code(&listener, &state, canceled)?;
    drop(listener);
    let resource = mcp_url(host);
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id),
        ("code_verifier", verifier.as_str()),
        ("resource", resource.as_str()),
    ];
    if let Some(secret) = client_secret {
        fields.push(("client_secret", secret));
    }
    let (status, v) = post_form(&meta.token, &fields)?;
    let now = crate::model::unix_now();
    let tokens =
        tokens_from(&v, client_id, client_secret, &meta.token, None, now).ok_or_else(|| {
            Error::Failed(format!(
                "{} didn't finish the sign-in: {}.",
                host_label(host),
                said(&v, status)
            ))
        })?;
    let (sub, email, name) = who(&meta, &tokens.access);
    let mark = [sub.as_str(), email.as_str(), client_id]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string();
    Ok(Session {
        id: format!("{host}#{mark}"),
        host: host.to_string(),
        email,
        name,
        credential: Credential::OAuth { tokens },
        added_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        parse_meta(
            "https://doop.arakun.dev",
            &json!({
                "authorization_endpoint": "https://doop.arakun.dev/api/auth/mcp/authorize",
                "token_endpoint": "https://doop.arakun.dev/api/auth/mcp/token",
                "registration_endpoint": "https://doop.arakun.dev/api/auth/mcp/register",
                "userinfo_endpoint": "https://doop.arakun.dev/api/auth/mcp/userinfo",
                "scopes_supported": ["openid", "profile", "email", "offline_access"],
            }),
        )
        .unwrap()
    }

    #[test]
    fn metadata_keeps_only_endpoints_a_token_may_go_to() {
        let m = meta();
        assert_eq!(m.token, "https://doop.arakun.dev/api/auth/mcp/token");
        assert!(m.register.is_some() && m.userinfo.is_some());
        let plain = json!({
            "authorization_endpoint": "http://evil.example/authorize",
            "token_endpoint": "https://doop.acme.dev/token",
        });
        assert_eq!(parse_meta("https://doop.acme.dev", &plain), None);
        // A local instance can't name a public http endpoint for the tokens.
        let leaky = json!({
            "authorization_endpoint": "http://localhost:3000/authorize",
            "token_endpoint": "http://tokens.example.com/token",
        });
        assert_eq!(parse_meta("http://localhost:3000", &leaky), None);
        assert_eq!(
            endpoint("http://localhost:3000", &json!("localhost:3000/token")),
            None
        );
        let local = json!({
            "authorization_endpoint": "http://localhost:3000/authorize",
            "token_endpoint": "http://localhost:3000/token",
        });
        let m = parse_meta("http://localhost:3000", &local).unwrap();
        assert_eq!(m.register, None);
        assert!(m.scopes.is_empty());
        assert_eq!(parse_meta("https://doop.acme.dev", &json!("<html>")), None);
    }

    #[test]
    fn metadata_is_asked_for_where_the_standards_put_it() {
        assert_eq!(
            resource_urls("https://d.acme.dev").map(|(url, _)| url),
            [
                "https://d.acme.dev/.well-known/oauth-protected-resource/mcp",
                "https://d.acme.dev/.well-known/oauth-protected-resource",
            ]
        );
        assert_eq!(
            issuer_urls("https://doop.design/"),
            [
                "https://doop.design/.well-known/oauth-authorization-server",
                "https://doop.design/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            issuer_urls("https://login.example/tenant"),
            [
                "https://login.example/.well-known/oauth-authorization-server/tenant",
                "https://login.example/.well-known/openid-configuration/tenant",
                "https://login.example/tenant/.well-known/oauth-authorization-server",
                "https://login.example/tenant/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn metadata_counts_only_when_it_is_about_what_was_asked() {
        let h = "https://d.acme.dev";
        let [(_, scoped), (_, root)] = resource_urls(h);
        let doc = |resource: &str| json!({"resource": resource, "authorization_servers": ["https://login.acme.dev"]});
        let login = Some("https://login.acme.dev".to_string());
        assert_eq!(
            resource_issuer(h, &scoped, &doc("https://d.acme.dev/mcp")),
            login
        );
        // Doop names its MCP endpoint in the instance-wide document too.
        assert_eq!(
            resource_issuer(h, &root, &doc("https://d.acme.dev/mcp")),
            login
        );
        assert_eq!(
            resource_issuer(h, &root, &doc("https://d.acme.dev/")),
            login
        );
        assert_eq!(
            resource_issuer(h, &scoped, &doc("https://d.acme.dev")),
            None
        );
        assert_eq!(
            resource_issuer(h, &scoped, &doc("https://d.acme.dev/other")),
            None
        );
        let mut nameless = doc("");
        nameless.as_object_mut().unwrap().remove("resource");
        assert_eq!(resource_issuer(h, &root, &nameless), None);

        let mut tenant = json!({
            "issuer": "https://login.example/a",
            "authorization_endpoint": "https://login.example/a/authorize",
            "token_endpoint": "https://login.example/a/token",
        });
        assert!(issuer_meta(h, "https://login.example/a", &tenant).is_some());
        assert!(issuer_meta(h, "https://login.example/a/", &tenant).is_some());
        assert_eq!(issuer_meta(h, "https://login.example/b", &tenant), None);
        tenant.as_object_mut().unwrap().remove("issuer");
        assert_eq!(issuer_meta(h, "https://login.example/a", &tenant), None);
    }

    #[test]
    fn the_browser_gets_a_pkce_request_for_this_instance() {
        // The verifier and challenge of RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let url = authorize_url(
            &meta(),
            "https://doop.arakun.dev",
            "abc",
            "http://127.0.0.1:4100/callback",
            verifier,
            "st4te",
        );
        assert!(url.starts_with(
            "https://doop.arakun.dev/api/auth/mcp/authorize?response_type=code&client_id=abc&"
        ));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A4100%2Fcallback"));
        assert!(url.contains(
            "code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256"
        ));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(url.contains("resource=https%3A%2F%2Fdoop.arakun.dev%2Fmcp"));
        assert!(!url.contains(verifier));
        let mut few = meta();
        few.scopes = vec!["openid".into()];
        few.authorize.push_str("?tenant=a");
        let url = authorize_url(&few, "https://d.lan", "abc", "r", verifier, "s");
        assert!(url.contains("?tenant=a&response_type=code"));
        assert!(url.contains("scope=openid&"));
    }

    #[test]
    fn only_this_sign_ins_callback_counts() {
        assert_eq!(
            callback("GET /callback?code=a%2Fb+c&state=s1 HTTP/1.1\r\n", "s1"),
            Callback::Code("a/b c".into())
        );
        assert_eq!(
            callback(
                "GET /callback?state=s1&error=access_denied&error_description=No+thanks HTTP/1.1",
                "s1"
            ),
            Callback::Refused("No thanks".into())
        );
        assert_eq!(
            callback("GET /callback?code=abc&state=other HTTP/1.1", "s1"),
            Callback::Other
        );
        assert_eq!(
            callback("GET /callback?code=abc HTTP/1.1", "s1"),
            Callback::Other
        );
        assert_eq!(callback("GET /favicon.ico HTTP/1.1", "s1"), Callback::Other);
        assert_eq!(
            callback("POST /callback?code=abc&state=s1 HTTP/1.1", "s1"),
            Callback::Other
        );
        assert_eq!(callback("", "s1"), Callback::Other);
    }

    #[test]
    fn the_listener_hands_over_the_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut noise = TcpStream::connect(("127.0.0.1", port)).unwrap();
            noise
                .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut page = String::new();
            let _ = noise.read_to_string(&mut page);
            assert!(page.starts_with("HTTP/1.1 404"));
            let mut back = TcpStream::connect(("127.0.0.1", port)).unwrap();
            back.write_all(b"GET /callback?code=c0de&state=s1 HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut page = String::new();
            let _ = back.read_to_string(&mut page);
            page
        });
        let code = wait_for_code(&listener, "s1", &|| false);
        assert_eq!(code, Ok("c0de".into()));
        assert!(browser.join().unwrap().contains("Signed in to Doop"));
        let canceled = wait_for_code(&listener, "s1", &|| true);
        assert!(matches!(canceled, Err(Error::Failed(m)) if m.contains("canceled")));
    }

    #[test]
    fn tokens_keep_the_old_refresh_token_when_none_comes_back() {
        let v = json!({"access_token": "a2", "expires_in": 600});
        let t = tokens_from(&v, "abc", None, "https://d/token", Some("r1"), 1000).unwrap();
        assert_eq!(
            (t.access.as_str(), t.refresh.as_deref(), t.expires_at),
            ("a2", Some("r1"), 1600)
        );
        let v = json!({"access_token": "a3", "refresh_token": "r2"});
        let t = tokens_from(&v, "abc", Some("s"), "https://d/token", Some("r1"), 1000).unwrap();
        assert_eq!((t.refresh.as_deref(), t.expires_at), (Some("r2"), 4600));
        assert_eq!(
            tokens_from(
                &json!({"error": "invalid_grant"}),
                "abc",
                None,
                "t",
                None,
                0
            ),
            None
        );
        assert_eq!(format!("{t:?}"), "Tokens(…)");
    }
}
