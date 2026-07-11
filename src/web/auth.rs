//! Session auth for the web GUI.
//!
//! One admin credential, supplied via `REPUBLISHER_ADMIN_PASSWORD` (plain, for
//! turnkey env-driven deployments) or `REPUBLISHER_ADMIN_PASSWORD_HASH` (argon2
//! PHC string, preferred where the environment is visible to other processes).
//! If neither is set the daemon generates a random password at boot and prints
//! it once to stdout. `REPUBLISHER_AUTH=disabled` is honoured only on loopback
//! binds — there is no silent-open failure mode on a LAN interface.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use argon2::password_hash::PasswordHash;
use argon2::{Argon2, PasswordVerifier};
use rand::distributions::Alphanumeric;
use rand::rngs::OsRng;
use rand::Rng;
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE: &str = "republisher_sid";
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_MAX_FAILURES: usize = 10;

enum Credential {
    Plain(String),
    Hash(String),
}

pub struct Auth {
    /// False only when explicitly disabled on a loopback bind.
    pub required: bool,
    /// Send `Secure` on the session cookie (TLS termination here or upstream).
    pub cookie_secure: bool,
    credential: Option<Credential>,
    sessions: Mutex<HashMap<String, Instant>>,
    failures: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl Auth {
    /// Resolve the auth mode from the environment for the given bind address.
    pub fn from_env(bind: SocketAddr, tls_enabled: bool) -> anyhow::Result<Self> {
        let disabled = std::env::var("REPUBLISHER_AUTH")
            .map(|v| v.eq_ignore_ascii_case("disabled"))
            .unwrap_or(false);
        let cookie_secure = tls_enabled
            || std::env::var("REPUBLISHER_COOKIE_SECURE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);

        if disabled {
            if !bind.ip().is_loopback() {
                bail!(
                    "REPUBLISHER_AUTH=disabled is only allowed for loopback binds; \
                     refusing to serve an unauthenticated GUI on {bind}"
                );
            }
            eprintln!("[republisherd] auth disabled (loopback bind)");
            return Ok(Self {
                required: false,
                cookie_secure,
                credential: None,
                sessions: Mutex::new(HashMap::new()),
                failures: Mutex::new(HashMap::new()),
            });
        }

        let hash = std::env::var("REPUBLISHER_ADMIN_PASSWORD_HASH").ok();
        let plain = std::env::var("REPUBLISHER_ADMIN_PASSWORD").ok();
        let credential = match (hash, plain) {
            (Some(hash), _) => {
                PasswordHash::new(&hash)
                    .map_err(|error| anyhow::anyhow!("{error}"))
                    .context("REPUBLISHER_ADMIN_PASSWORD_HASH is not a valid PHC string")?;
                Credential::Hash(hash)
            }
            (None, Some(plain)) if !plain.trim().is_empty() => Credential::Plain(plain),
            _ => {
                let generated: String = OsRng
                    .sample_iter(&Alphanumeric)
                    .take(24)
                    .map(char::from)
                    .collect();
                // Printed once, deliberately, so a turnkey boot is never locked
                // out; set REPUBLISHER_ADMIN_PASSWORD to make it stable.
                println!("[republisherd] generated admin password: {generated}");
                println!(
                    "[republisherd] set REPUBLISHER_ADMIN_PASSWORD (or *_HASH) to use a stable credential"
                );
                Credential::Plain(generated)
            }
        };

        Ok(Self {
            required: true,
            cookie_secure,
            credential: Some(credential),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        })
    }

    pub fn verify_password(&self, candidate: &str) -> bool {
        match &self.credential {
            None => false,
            Some(Credential::Plain(expected)) => {
                expected.as_bytes().ct_eq(candidate.as_bytes()).unwrap_u8() == 1
            }
            Some(Credential::Hash(hash)) => PasswordHash::new(hash)
                .map(|parsed| {
                    Argon2::default()
                        .verify_password(candidate.as_bytes(), &parsed)
                        .is_ok()
                })
                .unwrap_or(false),
        }
    }

    pub fn create_session(&self) -> String {
        let token: String = OsRng
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect();
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        sessions.retain(|_, expiry| *expiry > Instant::now());
        sessions.insert(token.clone(), Instant::now() + SESSION_TTL);
        token
    }

    pub fn check_session(&self, token: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match sessions.get(token) {
            Some(expiry) if *expiry > Instant::now() => true,
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn remove_session(&self, token: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(token);
    }

    /// True when this IP has exceeded the login-failure budget for the window.
    pub fn throttled(&self, ip: IpAddr) -> bool {
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if let Some(attempts) = failures.get_mut(&ip) {
            attempts.retain(|at| now.duration_since(*at) < LOGIN_WINDOW);
            attempts.len() >= LOGIN_MAX_FAILURES
        } else {
            false
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        failures.entry(ip).or_default().push(Instant::now());
        // Bounded: drop other IPs' stale windows so the map can't grow forever.
        let now = Instant::now();
        failures.retain(|_, attempts| {
            attempts.retain(|at| now.duration_since(*at) < LOGIN_WINDOW);
            !attempts.is_empty()
        });
    }

    pub fn session_cookie(&self, token: &str) -> String {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict{secure}")
    }

    pub fn clear_cookie(&self) -> String {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}")
    }
}

/// Extract the session token from a Cookie header value.
pub fn session_from_cookie_header(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_auth(password: &str) -> Auth {
        Auth {
            required: true,
            cookie_secure: false,
            credential: Some(Credential::Plain(password.to_string())),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn plain_password_verifies_exactly() {
        let auth = plain_auth("hunter2hunter2");
        assert!(auth.verify_password("hunter2hunter2"));
        assert!(!auth.verify_password("hunter2"));
        assert!(!auth.verify_password(""));
    }

    #[test]
    fn sessions_round_trip_and_logout() {
        let auth = plain_auth("pw");
        let token = auth.create_session();
        assert!(auth.check_session(&token));
        auth.remove_session(&token);
        assert!(!auth.check_session(&token));
        assert!(!auth.check_session("forged"));
    }

    #[test]
    fn throttle_kicks_in_after_budget() {
        let auth = plain_auth("pw");
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        assert!(!auth.throttled(ip));
        for _ in 0..LOGIN_MAX_FAILURES {
            auth.record_failure(ip);
        }
        assert!(auth.throttled(ip));
    }

    #[test]
    fn cookie_header_parsing_finds_session() {
        assert_eq!(
            session_from_cookie_header("a=b; republisher_sid=tok123; c=d"),
            Some("tok123")
        );
        assert_eq!(session_from_cookie_header("a=b"), None);
    }
}
