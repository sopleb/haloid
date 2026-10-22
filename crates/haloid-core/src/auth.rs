//! Who is making a request, and what that lets them do.

use crate::ids::UserId;
use std::str::FromStr;

/// A user's role. An enum rather than separate admin and manager flags,
/// which would allow the impossible both-true state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    User,
    Manager,
    Admin,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Manager => "manager",
            Self::Admin => "admin",
        }
    }

    /// Managers moderate content; admins do that and everything else.
    pub const fn can_moderate(self) -> bool {
        matches!(self, Self::Manager | Self::Admin)
    }

    pub const fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }
}

impl FromStr for Role {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "manager" => Ok(Self::Manager),
            "admin" => Ok(Self::Admin),
            _ => Err(()),
        }
    }
}

/// The actor behind a request. Guests are first-class: anonymous uploads are a
/// supported configuration, not an edge case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Guest,
    User { id: UserId, role: Role },
}

impl Actor {
    pub const fn user_id(self) -> Option<UserId> {
        match self {
            Self::Guest => None,
            Self::User { id, .. } => Some(id),
        }
    }

    pub const fn role(self) -> Option<Role> {
        match self {
            Self::Guest => None,
            Self::User { role, .. } => Some(role),
        }
    }

    pub const fn is_guest(self) -> bool {
        matches!(self, Self::Guest)
    }

    pub const fn can_moderate(self) -> bool {
        match self {
            Self::Guest => false,
            Self::User { role, .. } => role.can_moderate(),
        }
    }

    pub const fn is_admin(self) -> bool {
        match self {
            Self::Guest => false,
            Self::User { role, .. } => role.is_admin(),
        }
    }

    /// Whether this actor owns the given resource. Ownership is separate from
    /// role: a user may edit their own file, a moderator may edit anyone's.
    pub fn owns(self, owner: Option<UserId>) -> bool {
        match (self.user_id(), owner) {
            // A guest upload has no owner, and no one inherits it.
            (Some(actor), Some(owner)) => actor == owner,
            _ => false,
        }
    }

    /// Whether this actor may edit or delete a resource owned by `owner`.
    pub fn can_manage(self, owner: Option<UserId>) -> bool {
        self.can_moderate() || self.owns(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: UserId = UserId(1);
    const BOB: UserId = UserId(2);

    fn user(id: UserId, role: Role) -> Actor {
        Actor::User { id, role }
    }

    #[test]
    fn role_wire_names_round_trip() {
        for role in [Role::User, Role::Manager, Role::Admin] {
            assert_eq!(role.as_str().parse::<Role>(), Ok(role));
        }
        for input in ["", "Admin", "moderator", "root"] {
            assert!(input.parse::<Role>().is_err(), "{input:?} parsed");
        }
    }

    #[test]
    fn roles_order_by_privilege() {
        assert!(Role::Admin > Role::Manager);
        assert!(Role::Manager > Role::User);
    }

    #[test]
    fn only_managers_and_admins_moderate() {
        assert!(!Role::User.can_moderate());
        assert!(Role::Manager.can_moderate());
        assert!(Role::Admin.can_moderate());
        assert!(!Role::Manager.is_admin());
        assert!(Role::Admin.is_admin());
    }

    #[test]
    fn guests_have_no_identity_and_no_powers() {
        let guest = Actor::Guest;
        assert!(guest.is_guest());
        assert_eq!(guest.user_id(), None);
        assert_eq!(guest.role(), None);
        assert!(!guest.can_moderate());
        assert!(!guest.is_admin());
    }

    #[test]
    fn ownership_requires_matching_ids() {
        assert!(user(ALICE, Role::User).owns(Some(ALICE)));
        assert!(!user(ALICE, Role::User).owns(Some(BOB)));
    }

    #[test]
    fn nobody_owns_a_guest_upload() {
        // A guest upload has owner None. No logged-in user inherits it, and a
        // later guest is not the same guest.
        assert!(!user(ALICE, Role::Admin).owns(None));
        assert!(!Actor::Guest.owns(None));
        assert!(!Actor::Guest.owns(Some(ALICE)));
    }

    #[test]
    fn owners_and_moderators_may_manage_but_other_users_may_not() {
        let file_owner = Some(ALICE);
        assert!(user(ALICE, Role::User).can_manage(file_owner));
        assert!(!user(BOB, Role::User).can_manage(file_owner));
        assert!(user(BOB, Role::Manager).can_manage(file_owner));
        assert!(user(BOB, Role::Admin).can_manage(file_owner));
        assert!(!Actor::Guest.can_manage(file_owner));
    }

    #[test]
    fn moderators_can_manage_guest_uploads() {
        // Guest uploads are unowned, so moderation is the only route to them —
        // that and the one-shot delete token, which is not an identity.
        assert!(user(ALICE, Role::Manager).can_manage(None));
        assert!(!user(ALICE, Role::User).can_manage(None));
    }
}
