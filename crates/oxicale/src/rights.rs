use oxicale_core::{Permissions, UserName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    OwnerOnly,
    OwnerWrite,
    Authenticated,
}

impl Policy {
    pub fn from_config(config: &crate::config::Config) -> Self {
        match config.rights.backend {
            crate::config::RightsBackend::OwnerOnly | crate::config::RightsBackend::FromFile => {
                Self::OwnerOnly
            }
            crate::config::RightsBackend::OwnerWrite => Self::OwnerWrite,
            crate::config::RightsBackend::Authenticated => Self::Authenticated,
        }
    }

    pub fn permissions(self, current: &UserName, owner: &UserName) -> Permissions {
        match self {
            Self::OwnerOnly => {
                if current == owner {
                    Permissions::OWNER
                } else {
                    Permissions::NONE
                }
            }
            Self::OwnerWrite => {
                if current == owner {
                    Permissions::OWNER
                } else {
                    Permissions::READ_ONLY
                }
            }
            Self::Authenticated => Permissions::OWNER,
        }
    }
}

/// Subscription collections always remove content-write capabilities.
pub fn apply_read_only(permissions: Permissions) -> Permissions {
    permissions
        .without(Permissions::WRITE)
        .without(Permissions::DELETE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_only() {
        let alice = UserName::parse("alice").unwrap();
        let bob = UserName::parse("bob").unwrap();
        assert!(Policy::OwnerOnly
            .permissions(&alice, &alice)
            .contains(Permissions::WRITE));
        assert_eq!(
            Policy::OwnerOnly.permissions(&bob, &alice),
            Permissions::NONE
        );
    }

    #[test]
    fn owner_write_and_authenticated() {
        let alice = UserName::parse("alice").unwrap();
        let bob = UserName::parse("bob").unwrap();
        let read = Policy::OwnerWrite.permissions(&bob, &alice);
        assert!(read.contains(Permissions::READ));
        assert!(!read.contains(Permissions::WRITE));
        assert!(Policy::Authenticated
            .permissions(&bob, &alice)
            .contains(Permissions::DELETE));
    }

    #[test]
    fn read_only_overlay() {
        let permissions = apply_read_only(Permissions::OWNER);
        assert!(permissions.contains(Permissions::READ));
        assert!(permissions.contains(Permissions::WRITE_PROPERTIES));
        assert!(!permissions.contains(Permissions::WRITE));
        assert!(!permissions.contains(Permissions::DELETE));
    }
}
