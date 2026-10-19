use anyhow::{Result, ensure};
use std::{collections::HashMap, sync::Mutex};
use zeroize::Zeroizing;
/// Deliberately no Debug/Serialize implementation.
pub struct Secret(pub Zeroizing<String>);
impl Secret {
    pub fn new(s: String) -> Result<Self> {
        ensure!(
            !s.is_empty() && s.len() <= 8192 && !s.contains(['\r', '\n']),
            "invalid API key"
        );
        Ok(Self(Zeroizing::new(s)))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
pub trait Secrets: Send + Sync {
    fn get(&self, agent: &str) -> Result<Secret>;
    fn set(&self, agent: &str, key: Secret) -> Result<()>;
    fn delete(&self, agent: &str) -> Result<()>;
}
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, Zeroizing<String>>>);
impl Secrets for MemorySecrets {
    fn get(&self, agent: &str) -> Result<Secret> {
        let map = self.0.lock().unwrap();
        let s = map
            .get(agent)
            .ok_or_else(|| anyhow::anyhow!("API credential missing"))?;
        Secret::new(s.to_string())
    }
    fn set(&self, agent: &str, key: Secret) -> Result<()> {
        self.0.lock().unwrap().insert(agent.into(), key.0);
        Ok(())
    }
    fn delete(&self, agent: &str) -> Result<()> {
        self.0.lock().unwrap().remove(agent);
        Ok(())
    }
}
pub struct Keychain;
#[cfg(target_os = "macos")]
impl Secrets for Keychain {
    fn get(&self, agent: &str) -> Result<Secret> {
        let bytes = security_framework::passwords::get_generic_password("dev.codex.collab", agent)
            .map_err(|_| anyhow::anyhow!("Keychain credential unavailable"))?;
        Secret::new(
            String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid credential encoding"))?,
        )
    }
    fn set(&self, agent: &str, key: Secret) -> Result<()> {
        security_framework::passwords::set_generic_password(
            "dev.codex.collab",
            agent,
            key.expose().as_bytes(),
        )
        .map_err(|_| anyhow::anyhow!("Keychain write failed"))?;
        Ok(())
    }
    fn delete(&self, agent: &str) -> Result<()> {
        security_framework::passwords::delete_generic_password("dev.codex.collab", agent)
            .map_err(|_| anyhow::anyhow!("Keychain delete failed"))?;
        Ok(())
    }
}
#[cfg(not(target_os = "macos"))]
impl Secrets for Keychain {
    fn get(&self, _: &str) -> Result<Secret> {
        anyhow::bail!("Keychain requires macOS")
    }
    fn set(&self, _: &str, _: Secret) -> Result<()> {
        anyhow::bail!("Keychain requires macOS")
    }
    fn delete(&self, _: &str) -> Result<()> {
        anyhow::bail!("Keychain requires macOS")
    }
}
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut s = text.to_owned();
    for key in secrets {
        if !key.is_empty() {
            s = s.replace(key, "[REDACTED]");
        }
    }
    s
}
/// Hold a trailing prefix so credentials split over model deltas never reach a client.
pub struct StreamRedactor {
    pending: Zeroizing<String>,
    keys: Vec<Zeroizing<String>>,
    hold: usize,
}
impl StreamRedactor {
    pub fn new(keys: Vec<String>) -> Self {
        let hold = keys
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(1)
            .saturating_sub(1);
        Self {
            pending: Zeroizing::new(String::new()),
            keys: keys.into_iter().map(Zeroizing::new).collect(),
            hold,
        }
    }
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        self.replace();
        let mut cut = self.pending.len().saturating_sub(self.hold);
        while !self.pending.is_char_boundary(cut) {
            cut -= 1;
        }
        let out = self.pending[..cut].to_owned();
        self.pending.drain(..cut);
        out
    }
    fn replace(&mut self) {
        for key in &self.keys {
            if !key.is_empty() {
                *self.pending = self.pending.replace(key.as_str(), "[REDACTED]");
            }
        }
    }
    pub fn finish(&mut self) -> String {
        self.replace();
        std::mem::take(&mut *self.pending)
    }
}
