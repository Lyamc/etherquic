//! Whether an incoming player may complete the handshake.
//!
//! Unattended mode accepts immediately. Otherwise the host holds the attempt
//! until `control/approve` appears, `control/deny` appears, or the wait ends.

#![forbid(unsafe_code)]

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Refuse,
    Wait,
}

pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// Deny wins over approve. Unattended ignores both files.
pub fn decision(unattended: bool, approve_present: bool, deny_present: bool) -> Decision {
    if unattended {
        return Decision::Accept;
    }
    if deny_present {
        Decision::Refuse
    } else if approve_present {
        Decision::Accept
    } else {
        Decision::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unattended_needs_no_confirmation() {
        assert_eq!(decision(true, false, false), Decision::Accept);
        assert_eq!(decision(true, false, true), Decision::Accept);
        assert_eq!(decision(false, true, false), Decision::Accept);
        assert_eq!(decision(false, false, true), Decision::Refuse);
        assert_eq!(decision(false, true, true), Decision::Refuse);
        assert_eq!(decision(false, false, false), Decision::Wait);
    }
}
