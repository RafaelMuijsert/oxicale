use serde::{Deserialize, Serialize};

/// Capability bits used by the rights layer and exposed through DAV properties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Permissions(u16);

impl Permissions {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const DELETE: Self = Self(1 << 2);
    pub const READ_FREEBUSY: Self = Self(1 << 3);
    pub const WRITE_PROPERTIES: Self = Self(1 << 4);

    pub const NONE: Self = Self(0);
    pub const OWNER: Self = Self(
        Self::READ.0
            | Self::WRITE.0
            | Self::DELETE.0
            | Self::READ_FREEBUSY.0
            | Self::WRITE_PROPERTIES.0,
    );
    pub const READ_ONLY: Self = Self(Self::READ.0 | Self::READ_FREEBUSY.0);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub fn bits(self) -> u16 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions() {
        assert!(Permissions::OWNER.contains(Permissions::WRITE));
        assert!(!Permissions::READ_ONLY.contains(Permissions::WRITE));
        assert!(Permissions::READ_ONLY.contains(Permissions::READ_FREEBUSY));
        assert_eq!(
            Permissions::OWNER.without(Permissions::DELETE),
            Permissions::OWNER.without(Permissions::DELETE)
        );
        assert!(!Permissions::OWNER
            .without(Permissions::DELETE)
            .contains(Permissions::DELETE));
    }
}
