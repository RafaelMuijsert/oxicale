use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use oxicale_core::UserName;
use rand::RngCore;

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub csrf: String,
    pub user: UserName,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    lifetime: Duration,
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

impl SessionStore {
    pub fn new(lifetime: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            lifetime,
        }
    }

    pub fn create(&self, user: UserName) -> Session {
        let now = Utc::now();
        let session = Session {
            id: random_token(),
            csrf: random_token(),
            user,
            created_at: now,
            expires_at: now + chrono::Duration::from_std(self.lifetime).unwrap_or_default(),
        };
        let mut sessions = self.sessions.lock().expect("session mutex");
        sessions.insert(session.id.clone(), session.clone());
        session
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        let mut sessions = self.sessions.lock().expect("session mutex");
        let now = Utc::now();
        sessions.retain(|_, session| session.expires_at > now);
        sessions.get(id).cloned()
    }

    pub fn delete(&self, id: &str) {
        let mut sessions = self.sessions.lock().expect("session mutex");
        sessions.remove(id);
    }

    pub fn invalidate_user(&self, user: &UserName) {
        let mut sessions = self.sessions.lock().expect("session mutex");
        sessions.retain(|_, session| &session.user != user);
    }

    pub fn update_user(&self, id: &str, user: UserName) -> Option<Session> {
        let mut sessions = self.sessions.lock().expect("session mutex");
        let session = sessions.get_mut(id)?;
        session.user = user;
        session.csrf = random_token();
        Some(session.clone())
    }

    pub fn count(&self) -> usize {
        self.sessions.lock().expect("session mutex").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_get_delete() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = store.create(UserName::parse("alice").unwrap());
        assert_eq!(session.id.len(), 43);
        assert_eq!(session.csrf.len(), 43);
        assert_eq!(store.get(&session.id).unwrap().user.as_str(), "alice");
        store.delete(&session.id);
        assert!(store.get(&session.id).is_none());
    }

    #[test]
    fn invalidates_user_sessions() {
        let store = SessionStore::new(Duration::from_secs(60));
        let alice = UserName::parse("alice").unwrap();
        let bob = UserName::parse("bob").unwrap();
        let a1 = store.create(alice.clone());
        let a2 = store.create(alice.clone());
        let b1 = store.create(bob);
        store.invalidate_user(&alice);
        assert!(store.get(&a1.id).is_none());
        assert!(store.get(&a2.id).is_none());
        assert!(store.get(&b1.id).is_some());
    }

    #[test]
    fn rotates_csrf_on_user_update() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = store.create(UserName::parse("alice").unwrap());
        let updated = store
            .update_user(&session.id, UserName::parse("alice2").unwrap())
            .unwrap();
        assert_eq!(updated.user.as_str(), "alice2");
        assert_ne!(updated.csrf, session.csrf);
    }
}
