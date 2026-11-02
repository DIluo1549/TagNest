use anyhow::{Context, Result};
use axum::http::{header, HeaderMap};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "tagnest_session";
pub const COOKIE_MAX_AGE: u64 = 60 * 60 * 24 * 30;

#[derive(Clone)]
pub struct Auth {
    password: Option<String>,
    secret: Vec<u8>,
}

impl Auth {
    pub fn new(password: Option<String>, secret_path: &Path) -> Result<Self> {
        let secret = if secret_path.exists() {
            fs::read(secret_path).context("读取会话密钥失败")?
        } else {
            let mut secret = vec![0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            fs::write(secret_path, &secret).context("写入会话密钥失败")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(secret_path, fs::Permissions::from_mode(0o600));
            }
            secret
        };
        Ok(Self {
            password: password.filter(|v| !v.is_empty()),
            secret,
        })
    }

    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    pub fn verify_password(&self, candidate: &str) -> bool {
        let Some(expected) = &self.password else {
            return true;
        };
        constant_time_eq(expected.as_bytes(), candidate.as_bytes())
    }

    pub fn issue_cookie(&self) -> String {
        let expires = now() + COOKIE_MAX_AGE;
        let payload = expires.to_string();
        let sig = self.sign(payload.as_bytes());
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload.as_bytes()),
            hex(&sig)
        )
    }

    pub fn check_cookie(&self, value: &str) -> bool {
        let mut parts = value.split('.');
        let (Some(payload_b64), Some(sig_hex), None) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let Ok(payload) = URL_SAFE_NO_PAD.decode(payload_b64) else {
            return false;
        };
        let Ok(expires) = std::str::from_utf8(&payload)
            .unwrap_or_default()
            .parse::<u64>()
        else {
            return false;
        };
        if expires < now() {
            return false;
        }
        let Ok(sig) = hex_decode(sig_hex) else {
            return false;
        };
        let mut mac = match HmacSha256::new_from_slice(&self.secret) {
            Ok(mac) => mac,
            Err(_) => return false,
        };
        mac.update(&payload);
        mac.verify_slice(&sig).is_ok()
    }

    pub fn authenticated(&self, headers: &HeaderMap) -> bool {
        if !self.enabled() {
            return true;
        }
        cookie_value(headers, COOKIE_NAME)
            .map(|value| self.check_cookie(&value))
            .unwrap_or(false)
    }

    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(payload);
        mac.finalize().into_bytes().to_vec()
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    for cookie in cookies.split(';') {
        let mut bits = cookie.trim().splitn(2, '=');
        if bits.next()? == name {
            return bits.next().map(ToOwned::to_owned);
        }
    }
    None
}

pub fn set_cookie_header(value: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={COOKIE_MAX_AGE}{secure}"
    )
}

pub fn clear_cookie_header() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(value: &str) -> Result<Vec<u8>, ()> {
    if value.len() % 2 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for i in (0..value.len()).step_by(2) {
        out.push(u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| ())?);
    }
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
