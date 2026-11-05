//! The agent's network ports and what lets them through a firewall, for
//! the agent, `inputweave ctl` and the apps alike.

/// The UDP port the agent takes connections on unless its configuration's
/// `[network] listen` says otherwise.
pub const DEFAULT_PORT: u16 = 47820;

/// mDNS, which other devices find this one with.
pub const MDNS_PORT: u16 = 5353;

/// The agent's port as reported (`FirewallStatus.port`), 0 (not known,
/// from older agents) being the default.
pub fn effective_port(port: u32) -> u32 {
    if port == 0 {
        u32::from(DEFAULT_PORT)
    } else {
        port
    }
}

/// What lets other devices through ufw to an agent on UDP `port`: the
/// package's profile "InputWeave", which has the default port and mDNS,
/// and that port too when it is another.
pub fn ufw_allow(port: u32) -> String {
    if effective_port(port) == u32::from(DEFAULT_PORT) {
        "sudo ufw allow InputWeave".to_owned()
    } else {
        format!("sudo ufw allow InputWeave && sudo ufw allow {port}/udp")
    }
}

/// Which of the agent's ports a firewall keeps out, from what the agent
/// reports (`FirewallStatus.blocked_ports`): the agent's own `port` (other
/// devices cannot connect) and mDNS (they cannot find this one). Agents
/// before `blocked_ports` sent none, which means their own port.
pub fn blocked(port: u32, blocked_ports: &[u32]) -> (bool, bool) {
    let mdns = u32::from(MDNS_PORT);
    (
        blocked_ports.is_empty() || blocked_ports.contains(&port),
        port != mdns && blocked_ports.contains(&mdns),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_and_the_port_when_it_is_another() {
        assert_eq!(ufw_allow(47820), "sudo ufw allow InputWeave");
        assert_eq!(ufw_allow(0), "sudo ufw allow InputWeave");
        assert_eq!(
            ufw_allow(50000),
            "sudo ufw allow InputWeave && sudo ufw allow 50000/udp"
        );
    }

    #[test]
    fn a_port_not_known_is_the_default() {
        assert_eq!(effective_port(0), 47820);
        assert_eq!(effective_port(50000), 50000);
    }

    #[test]
    fn the_ports_kept_out_are_the_ones_listed() {
        assert_eq!(blocked(47820, &[]), (true, false), "from older agents");
        assert_eq!(blocked(47820, &[47820]), (true, false));
        assert_eq!(blocked(47820, &[5353]), (false, true));
        assert_eq!(blocked(47820, &[47820, 5353]), (true, true));
        assert_eq!(blocked(47820, &[12345]), (false, false), "neither is ours");
    }
}
