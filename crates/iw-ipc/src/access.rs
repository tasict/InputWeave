//! Names and access rules of InputWeave's named pipes, free of FFI so they
//! can be tested anywhere.

#![cfg_attr(not(windows), allow(dead_code))]

/// The agent's pipe in session `session`. One agent runs per console
/// session, and clients find it by their own session id.
pub fn agent_pipe_name(session: u32) -> String {
    format!(r"\\.\pipe\InputWeave\agent-{session}")
}

/// The pipe the agent in session `session` serves the broker on, which
/// runs as the user logged on there.
pub fn broker_pipe_name(session: u32) -> String {
    format!(r"\\.\pipe\InputWeave\broker-{session}")
}

/// Who may open a pipe, besides SYSTEM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeAccess {
    /// SYSTEM and the user logged on to this session, identified by the
    /// logon SID of that logon, so the same user's processes in another
    /// session are kept out. With nobody logged on, only SYSTEM. This is
    /// the agent's pipes, for the UI and `ctl`, and for the broker.
    Session,
    /// SYSTEM only, as for the service's own pipe.
    SystemOnly,
}

/// The security descriptor of a pipe as SDDL: a protected DACL (nothing
/// inherited) granting SYSTEM full access and, for [`PipeAccess::Session`],
/// read and write to `user_sid`.
///
/// The user does not get full access, which would include changing the
/// DACL. Write access does include creating further instances of the pipe,
/// which lets the same user's processes stand up a server under the name;
/// those processes can already act as the user, so this exposes nothing
/// new. `None` if `user_sid` is not a SID string.
pub fn pipe_sddl(access: PipeAccess, user_sid: Option<&str>) -> Option<String> {
    let mut sddl = "D:P(A;;GA;;;SY)".to_owned();
    if let (PipeAccess::Session, Some(sid)) = (access, user_sid) {
        if !is_sid(sid) {
            return None;
        }
        sddl.push_str(&format!("(A;;GRGW;;;{sid})"));
    }
    Some(sddl)
}

/// Whether `s` has the shape of a SID string (`S-1-5-5-0-123456`), which
/// also keeps it from carrying SDDL syntax.
fn is_sid(s: &str) -> bool {
    let mut parts = s.split('-');
    parts.next() == Some("S")
        && parts.clone().count() >= 3
        && parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_pipes_are_per_session() {
        assert_eq!(agent_pipe_name(1), r"\\.\pipe\InputWeave\agent-1");
        assert_eq!(agent_pipe_name(12), r"\\.\pipe\InputWeave\agent-12");
        assert_eq!(broker_pipe_name(1), r"\\.\pipe\InputWeave\broker-1");
    }

    #[test]
    fn session_pipes_admit_system_and_the_logon() {
        assert_eq!(
            pipe_sddl(PipeAccess::Session, Some("S-1-5-5-0-473125")).as_deref(),
            Some("D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-5-0-473125)")
        );
        // Nobody logged on yet.
        assert_eq!(
            pipe_sddl(PipeAccess::Session, None).as_deref(),
            Some("D:P(A;;GA;;;SY)")
        );
        assert_eq!(
            pipe_sddl(PipeAccess::SystemOnly, Some("S-1-5-5-0-473125")).as_deref(),
            Some("D:P(A;;GA;;;SY)")
        );
    }

    #[test]
    fn only_sid_strings_reach_the_descriptor() {
        for bad in [
            "",
            "S-1",
            "S-1-5",
            "WD",
            "S-1-5-5-0-1)(A;;GA;;;WD",
            "S-1-5--1",
            "s-1-5-18",
        ] {
            assert_eq!(pipe_sddl(PipeAccess::Session, Some(bad)), None, "{bad:?}");
        }
        assert!(pipe_sddl(PipeAccess::Session, Some("S-1-5-21-1-2-3-1001")).is_some());
    }
}
