use anyhow::{Result, bail};
use rand::RngCore;
use tokio::sync::watch;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

pub const COOKIE: &str = "cctv_session";
pub const SESSION_TTL: Duration = Duration::from_secs(300);
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

pub enum LoginError {
    Wrong,
    Locked(u64),
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, watch::Sender<Instant>>,
    failures: u32,
    locked_until: Option<Instant>,
}
/// PIN gate with sessions that expire after five minutes without explicit user activity.
pub struct Auth {
    pin: Vec<u8>,
    ttl: Duration,
    state: Mutex<State>,
}
impl Auth {
    pub fn new(pin: &str) -> Result<Self> {
        Self::with_ttl(pin, SESSION_TTL)
    }
    pub fn with_ttl(pin: &str, ttl: Duration) -> Result<Self> {
        if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            bail!("ACCESS_PIN harus 6 digit angka.")
        }
        Ok(Self {
            pin: pin.as_bytes().to_vec(),
            ttl,
            state: Mutex::default(),
        })
    }
    pub fn from_env() -> Result<Self> {
        match std::env::var("ACCESS_PIN") {
            Ok(pin) => Self::new(pin.trim()),
            Err(_) => bail!("ACCESS_PIN belum diatur (6 digit angka); server tidak dijalankan tanpa PIN."),
        }
    }
    pub fn login(&self, pin: &str) -> Result<String, LoginError> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        if let Some(until) = state.locked_until {
            if until > now {
                return Err(LoginError::Locked((until - now).as_secs() + 1));
            }
            state.locked_until = None;
            state.failures = 0;
        }
        // Constant-time comparison; the length is public (always 6).
        let matches = pin.len() == self.pin.len()
            && pin
                .bytes()
                .zip(&self.pin)
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0;
        if !matches {
            state.failures += 1;
            if state.failures >= MAX_FAILURES {
                state.locked_until = Some(now + LOCKOUT);
                return Err(LoginError::Locked(LOCKOUT.as_secs()));
            }
            return Err(LoginError::Wrong);
        }
        state.failures = 0;
        state.sessions.retain(|_, expires| *expires.borrow() > now);
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        state.sessions.insert(token.clone(), watch::channel(now + self.ttl).0);
        Ok(token)
    }
    pub fn valid(&self, token: &str) -> bool {
        self.deadline(token).is_some()
    }
    pub fn deadline(&self, token: &str) -> Option<Instant> {
        self.subscribe(token).map(|deadline| *deadline.borrow())
    }
    pub fn subscribe(&self, token: &str) -> Option<watch::Receiver<Instant>> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        if let Some(expires) = state.sessions.get(token)
            && *expires.borrow() > now
        {
            return Some(expires.subscribe());
        }
        state.sessions.remove(token);
        None
    }
    /// Only the activity endpoint renews sessions; background requests never do.
    pub fn touch(&self, token: &str) -> Option<Instant> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        if let Some(expires) = state.sessions.get(token)
            && *expires.borrow() > now
        {
            let deadline = now + self.ttl;
            expires.send_replace(deadline);
            return Some(deadline);
        }
        state.sessions.remove(token);
        None
    }
}
/// Existing streams observe extensions as well as expiry, without reconnecting.
pub async fn expired(mut deadline: watch::Receiver<Instant>) {
    loop {
        let at = *deadline.borrow_and_update();
        tokio::select! {
            biased;
            changed = deadline.changed() => if changed.is_err() { return; },
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => {
                if *deadline.borrow() <= Instant::now() { return; }
            }
        }
    }
}
