//! Whether ufw keeps other devices from reaching the agent.
//! The package ships a ufw
//! application profile (`sudo ufw allow InputWeave`) and changes no rule
//! itself; the app says so when ufw is on and does not let the agent's
//! port in.
//!
//! Read without privileges and without running `ufw` (which needs root):
//! `/etc/ufw/ufw.conf` says whether it is on, `/etc/default/ufw` the policy
//! for what no rule matches, and `/etc/ufw/user.rules` the user's rules,
//! which only root may read on most systems: then whether the port is let
//! in is not known ([`Firewall::OnUnreadable`]). Only the IPv4 rules are
//! read (not user6.rules): the agent listens on IPv4 (`0.0.0.0`) unless
//! its configuration says otherwise.

use crate::{Blocked, Firewall, MDNS_PORT};

/// What a rule does to what it matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Accept,
    Drop,
}

/// What a rule of the user's input chain says of UDP to the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Says {
    /// It matches, and does this.
    Does(Target),
    /// It may match or not (it also wants a source port), and does this
    /// if it does.
    MayDo(Target),
    /// It matches, but what it does is not known here.
    Unknown,
}

/// Whether ufw lets other devices in: on UDP `port` (to connect) and on
/// mDNS's ([`MDNS_PORT`], to find this device). `conf` is ufw.conf (`None`
/// without ufw), `defaults` /etc/default/ufw, `rules` user.rules (`None`
/// when it cannot be read: then it is not known, whatever the policy, as a
/// rule unread may keep the ports out). Either port kept out is
/// [`Firewall::Blocks`]; either not known, and the other let in, is
/// [`Firewall::Unknown`].
pub(crate) fn ufw(
    port: u16,
    conf: Option<&str>,
    defaults: Option<&str>,
    rules: Option<&str>,
) -> Firewall {
    let Some(conf) = conf else {
        return Firewall::Unknown;
    };
    if value_of(conf, "ENABLED").as_deref() != Some("yes") {
        return Firewall::Off;
    }
    let Some(rules) = rules else {
        return Firewall::OnUnreadable;
    };
    let policy = policy_of(defaults);
    match [port, MDNS_PORT].map(|p| lets_in(rules, p, policy)) {
        [Some(true), Some(true)] => Firewall::Allows,
        [agent, mdns] if agent == Some(false) || mdns == Some(false) => Firewall::Blocks(Blocked {
            port: agent == Some(false),
            mdns: mdns == Some(false),
        }),
        _ => Firewall::Unknown,
    }
}

/// What ufw does to what no rule decides, from /etc/default/ufw.
fn policy_of(defaults: Option<&str>) -> Target {
    match defaults.and_then(|d| value_of(d, "DEFAULT_INPUT_POLICY")) {
        Some(p) if p == "ACCEPT" => Target::Accept,
        // DROP, REJECT, or not said: ufw's default drops.
        _ => Target::Drop,
    }
}

/// Whether `rules` let UDP `port` in, `policy` deciding where no rule
/// does. `None` where that is not known: a rule that may match says
/// otherwise than what decides, or a matching rule does what is not known
/// here. Never said to keep it out when it may not.
fn lets_in(rules: &str, port: u16, policy: Target) -> Option<bool> {
    let mut maybe = Vec::new();
    let mut unknown = false;
    let decided = rules
        .lines()
        .find_map(|line| match matching(line.trim(), port)? {
            Says::Does(target) => Some(target),
            Says::MayDo(target) => {
                maybe.push(target);
                None
            }
            Says::Unknown => {
                unknown = true;
                None
            }
        })
        .unwrap_or(policy);
    if unknown || maybe.iter().any(|t| *t != decided) {
        return None;
    }
    Some(decided == Target::Accept)
}

/// The value of `KEY=value` in a shell-style file, without its quotes.
fn value_of(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_owned())
    })
}

/// What a rule of the user's IPv4 input chain says of UDP to `port`, if it
/// may match it: `-A ufw-user-input [-p udp|all] [--dport N[:M] | -m
/// multiport --dports a,b:c] … [-j target]`, as ufw writes them. Rules for
/// another protocol or port, or for a source port without a destination
/// port, do not match; a rule for a destination port that also wants a
/// source port may. The devices' addresses are not known here: a rule
/// that lets in from some addresses or on an interface (`-s`, `-d`, `-i`,
/// `-o`) is taken to let them in, as it is how one lets the local network
/// in; one that keeps some out, as of a host banned, may or may not be of
/// them, so it never says on its own that they are kept out.
/// ACCEPT lets in; DROP and REJECT do not; `ufw limit` lets in at a
/// limited rate (`-j ufw-user-limit` over the rate, then
/// `-j ufw-user-limit-accept`). Lines without a target (the rate's
/// bookkeeping) and logging ones decide nothing.
fn matching(rule: &str, port: u16) -> Option<Says> {
    let words: Vec<&str> = rule.split_whitespace().collect();
    if words.get(..2) != Some(&["-A", "ufw-user-input"]) {
        return None;
    }
    let after = |flag: &str| {
        words
            .iter()
            .position(|w| *w == flag)
            .and_then(|i| words.get(i + 1))
            .copied()
    };
    if let Some(protocol) = after("-p")
        && protocol != "udp"
        && protocol != "all"
    {
        return None;
    }
    let source_port = after("--sport").or_else(|| after("--sports")).is_some();
    let addressed = ["-s", "-d", "-i", "-o"]
        .iter()
        .any(|flag| after(flag).is_some());
    match after("--dport").or_else(|| after("--dports")) {
        Some(ports) if !ports.split(',').any(|range| in_range(range, port)) => return None,
        // A source port alone says nothing of the agent's port.
        None if source_port => return None,
        _ => {}
    }
    let target = match after("-j")? {
        "ACCEPT" | "ufw-user-limit" | "ufw-user-limit-accept" => Target::Accept,
        "DROP" | "REJECT" => Target::Drop,
        "LOG" | "ufw-user-logging-input" => return None,
        _ => return Some(Says::Unknown),
    };
    Some(match target {
        _ if source_port => Says::MayDo(target),
        Target::Drop if addressed => Says::MayDo(target),
        _ => Says::Does(target),
    })
}

/// Whether `range` (`N` or `N:M`) holds `port`.
fn in_range(range: &str, port: u16) -> bool {
    match range.split_once(':') {
        Some((low, high)) => {
            let (Ok(low), Ok(high)) = (low.parse::<u16>(), high.parse::<u16>()) else {
                return false;
            };
            (low..=high).contains(&port)
        }
        None => range.parse::<u16>() == Ok(port),
    }
}

/// What ufw does to UDP `port` on this system. Its files are read again
/// only when one of them changed (its modification time or size), or the
/// port did: asked every second, most answers are three `stat`s.
#[cfg(target_os = "linux")]
pub(crate) fn ufw_now(port: u16) -> Firewall {
    use std::sync::Mutex;
    use std::time::SystemTime;

    /// A file's modification time and size; `None` if it is not there.
    type Stamp = Option<(Option<SystemTime>, u64)>;
    const FILES: [&str; 3] = [
        "/etc/ufw/ufw.conf",
        "/etc/default/ufw",
        "/etc/ufw/user.rules",
    ];
    static LAST: Mutex<Option<(u16, [Stamp; 3], Firewall)>> = Mutex::new(None);

    let stamps = FILES.map(|path| {
        std::fs::metadata(path)
            .ok()
            .map(|m| (m.modified().ok(), m.len()))
    });
    let mut last = crate::linux::common::lock(&LAST);
    if let Some((was, stamped, firewall)) = last.as_ref()
        && *was == port
        && *stamped == stamps
    {
        return *firewall;
    }
    let [conf, defaults, rules] = FILES.map(|path| std::fs::read_to_string(path).ok());
    let firewall = ufw(port, conf.as_deref(), defaults.as_deref(), rules.as_deref());
    *last = Some((port, stamps, firewall));
    firewall
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: &str = "# /etc/ufw/ufw.conf\nENABLED=yes\nLOGLEVEL=low\n";
    const OFF: &str = "ENABLED=no\n";
    const DROPS: &str = "IPV6=yes\nDEFAULT_INPUT_POLICY=\"DROP\"\n";
    const ACCEPTS: &str = "DEFAULT_INPUT_POLICY=\"ACCEPT\"\n";

    fn rules(lines: &[&str]) -> String {
        let mut text = String::from("*filter\n:ufw-user-input - [0:0]\n### RULES ###\n");
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("### END RULES ###\nCOMMIT\n");
        text
    }

    #[test]
    fn without_ufw_nothing_is_known_and_off_lets_everything_in() {
        assert_eq!(ufw(47820, None, None, None), Firewall::Unknown);
        assert_eq!(ufw(47820, Some(OFF), Some(DROPS), None), Firewall::Off);
    }

    #[test]
    fn rules_that_cannot_be_read_leave_it_unknown_whatever_the_policy() {
        assert_eq!(
            ufw(47820, Some(ON), Some(DROPS), None),
            Firewall::OnUnreadable
        );
        assert_eq!(ufw(47820, Some(ON), None, None), Firewall::OnUnreadable);
        // A deny rule unread may keep the port out all the same.
        assert_eq!(
            ufw(47820, Some(ON), Some(ACCEPTS), None),
            Firewall::OnUnreadable
        );
    }

    #[test]
    fn both_the_port_and_mdns_must_be_let_in() {
        let blocks = |port, mdns| Firewall::Blocks(Blocked { port, mdns });
        // ufw allow InputWeave: both.
        let profile = rules(&[
            "-A ufw-user-input -p udp -m multiport --dports 47820,5353 -j ACCEPT -m comment --comment 'dapp_InputWeave'",
        ]);
        assert_eq!(
            ufw(47820, Some(ON), Some(DROPS), Some(&profile)),
            Firewall::Allows
        );
        // ufw allow 47820/udp: connecting works, finding this one does not.
        let port_only = rules(&["-A ufw-user-input -p udp --dport 47820 -j ACCEPT"]);
        assert_eq!(
            ufw(47820, Some(ON), Some(DROPS), Some(&port_only)),
            blocks(false, true)
        );
        // Neither.
        let none = rules(&[]);
        assert_eq!(
            ufw(47820, Some(ON), Some(DROPS), Some(&none)),
            blocks(true, true)
        );
        assert_eq!(
            ufw(47820, Some(ON), Some(ACCEPTS), Some(&none)),
            Firewall::Allows
        );
        // Kept out on one, not known on the other: kept out.
        let odd = rules(&[
            "-A ufw-user-input -p udp --dport 5353 -j RETURN",
            "-A ufw-user-input -p udp --dport 47820 -j DROP",
        ]);
        assert_eq!(
            ufw(47820, Some(ON), Some(ACCEPTS), Some(&odd)),
            blocks(true, false)
        );
        // Let in on one, not known on the other: not known.
        let unsure = rules(&["-A ufw-user-input -p udp --dport 5353 -j RETURN"]);
        assert_eq!(
            ufw(47820, Some(ON), Some(ACCEPTS), Some(&unsure)),
            Firewall::Unknown
        );
    }

    /// What `rules` do to UDP `port` alone under `defaults`' policy:
    /// `Some(true)` let in.
    fn lets(port: u16, defaults: &str, rules: &str) -> Option<bool> {
        lets_in(rules, port, policy_of(Some(defaults)))
    }

    #[test]
    fn the_application_profile_lets_the_port_in() {
        let allowed = rules(&[
            "### tuple ### allow any any 0.0.0.0/0 any 0.0.0.0/0 InputWeave - in",
            "-A ufw-user-input -p udp -m multiport --dports 47820,5353 -j ACCEPT -m comment --comment 'dapp_InputWeave'",
        ]);
        assert_eq!(lets(47820, DROPS, &allowed), Some(true));
        // Another port is not let in by it.
        assert_eq!(lets(47821, DROPS, &allowed), Some(false));
    }

    #[test]
    fn ports_ranges_and_protocols_are_matched() {
        let tcp_only = rules(&["-A ufw-user-input -p tcp --dport 47820 -j ACCEPT"]);
        assert_eq!(lets(47820, DROPS, &tcp_only), Some(false));
        let range = rules(&["-A ufw-user-input -p udp --dport 47000:48000 -j ACCEPT"]);
        assert_eq!(lets(47820, DROPS, &range), Some(true));
        // From a network, every port.
        let subnet = rules(&["-A ufw-user-input -s 192.168.0.0/16 -j ACCEPT"]);
        assert_eq!(lets(47820, DROPS, &subnet), Some(true));
        // No rule: the policy.
        let none = rules(&[]);
        assert_eq!(lets(47820, DROPS, &none), Some(false));
        assert_eq!(lets(47820, ACCEPTS, &none), Some(true));
    }

    #[test]
    fn the_first_rule_that_matches_decides() {
        let denied_first = rules(&[
            "-A ufw-user-input -p udp --dport 47820 -j DROP",
            "-A ufw-user-input -p udp --dport 47820 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, ACCEPTS, &denied_first), Some(false));
        let other_chain = rules(&["-A ufw-user-limit -p udp --dport 47820 -j ACCEPT"]);
        assert_eq!(lets(47820, DROPS, &other_chain), Some(false));
    }

    // What `ufw` writes into user.rules for each command (ufw 0.36).

    #[test]
    fn ufw_allow_lets_the_port_in() {
        // ufw allow 47820/udp
        let udp = rules(&[
            "### tuple ### allow udp 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &udp), Some(true));
        // ufw allow 47820: both protocols.
        let both = rules(&[
            "### tuple ### allow any 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p tcp --dport 47820 -j ACCEPT",
            "-A ufw-user-input -p udp --dport 47820 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &both), Some(true));
        // ufw allow from 192.168.0.0/16 to any port 47820 proto udp
        let from = rules(&[
            "### tuple ### allow udp 47820 0.0.0.0/0 any 192.168.0.0/16 in",
            "-A ufw-user-input -p udp --dport 47820 -s 192.168.0.0/16 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &from), Some(true));
        // ufw allow log 47820/udp: the logging line decides nothing.
        let logged = rules(&[
            "### tuple ### allow udp 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 -j ufw-user-logging-input",
            "-A ufw-user-input -p udp --dport 47820 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &logged), Some(true));
    }

    #[test]
    fn ufw_limit_lets_the_port_in() {
        // ufw limit 47820/udp
        let limited = rules(&[
            "### tuple ### limit udp 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 -m conntrack --ctstate NEW -m recent --set",
            "-A ufw-user-input -p udp --dport 47820 -m conntrack --ctstate NEW -m recent --update --seconds 30 --hitcount 6 -j ufw-user-limit",
            "-A ufw-user-input -p udp --dport 47820 -j ufw-user-limit-accept",
        ]);
        assert_eq!(lets(47820, DROPS, &limited), Some(true));
    }

    #[test]
    fn ufw_deny_and_reject_keep_the_port_out() {
        // ufw deny 47820/udp, though the policy lets everything in.
        let denied = rules(&[
            "### tuple ### deny udp 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 -j DROP",
        ]);
        assert_eq!(lets(47820, ACCEPTS, &denied), Some(false));
        // ufw reject 47820/udp
        let rejected = rules(&[
            "### tuple ### reject udp 47820 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 -j REJECT",
        ]);
        assert_eq!(lets(47820, ACCEPTS, &rejected), Some(false));
    }

    #[test]
    fn a_source_port_alone_does_not_let_the_port_in() {
        // ufw allow proto udp from any port 53
        let dns = rules(&[
            "### tuple ### allow udp any 0.0.0.0/0 53 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --sport 53 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &dns), Some(false));
        // A destination port with a source port may match or not: where it
        // says otherwise than what decides, it is not known.
        let both = rules(&[
            "### tuple ### allow udp 47820 0.0.0.0/0 47820 0.0.0.0/0 in",
            "-A ufw-user-input -p udp --dport 47820 --sport 47820 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &both), None);
        assert_eq!(lets(47820, ACCEPTS, &both), Some(true));
    }

    #[test]
    fn port_ranges_are_matched() {
        // ufw allow 47000:48000/udp
        let range = rules(&[
            "### tuple ### allow udp 47000:48000 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp -m multiport --dports 47000:48000 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &range), Some(true));
        assert_eq!(lets(48001, DROPS, &range), Some(false));
        // ufw allow 5353,47800:47900/udp
        let list = rules(&[
            "### tuple ### allow udp 5353,47800:47900 0.0.0.0/0 any 0.0.0.0/0 in",
            "-A ufw-user-input -p udp -m multiport --dports 5353,47800:47900 -j ACCEPT",
        ]);
        assert_eq!(lets(47820, DROPS, &list), Some(true));
        assert_eq!(lets(47790, DROPS, &list), Some(false));
    }

    #[test]
    fn ipv6_rules_do_not_decide_for_the_ipv4_port() {
        // From user6.rules: ufw allow 47820/udp writes it there too, but
        // the agent listens on IPv4.
        let v6 = "*filter\n:ufw6-user-input - [0:0]\n### RULES ###\n\
            ### tuple ### allow udp 47820 ::/0 any ::/0 in\n\
            -A ufw6-user-input -p udp --dport 47820 -j ACCEPT\n\
            ### END RULES ###\nCOMMIT\n";
        assert_eq!(lets(47820, DROPS, v6), Some(false));
    }

    #[test]
    fn a_rule_that_does_what_is_not_known_leaves_it_unknown() {
        let odd = rules(&["-A ufw-user-input -p udp --dport 47820 -j RETURN"]);
        assert_eq!(lets(47820, DROPS, &odd), None);
        // After what decides, it does not matter.
        let late = rules(&[
            "-A ufw-user-input -p udp --dport 47820 -j ACCEPT",
            "-A ufw-user-input -p udp --dport 47820 -j RETURN",
        ]);
        assert_eq!(lets(47820, DROPS, &late), Some(true));
    }

    #[test]
    fn keeping_some_addresses_out_does_not_say_the_devices_are() {
        // ufw deny from 203.0.113.4, then ufw allow InputWeave.
        let banned = rules(&[
            "### tuple ### deny any any 0.0.0.0/0 any 203.0.113.4 in",
            "-A ufw-user-input -s 203.0.113.4 -j DROP",
            "### tuple ### allow any any 0.0.0.0/0 any 0.0.0.0/0 InputWeave - in",
            "-A ufw-user-input -p udp -m multiport --dports 47820,5353 -j ACCEPT -m comment --comment 'dapp_InputWeave'",
        ]);
        assert_eq!(lets(47820, DROPS, &banned), None);
        let alone = rules(&["-A ufw-user-input -s 203.0.113.4 -j DROP"]);
        assert_eq!(lets(47820, ACCEPTS, &alone), None);
        // Where nothing lets the port in, the policy keeps it out whatever
        // the ban (and allowing InputWeave would help).
        assert_eq!(lets(47820, DROPS, &alone), Some(false));
        // ufw deny in on wlan0; ufw deny from 203.0.113.4 to any port 47820
        for line in [
            "-A ufw-user-input -i wlan0 -j DROP",
            "-A ufw-user-input -p udp --dport 47820 -s 203.0.113.4 -j REJECT",
        ] {
            let ruled = rules(&[line, "-A ufw-user-input -p udp --dport 47820 -j ACCEPT"]);
            assert_eq!(lets(47820, DROPS, &ruled), None, "{line}");
        }
    }
}
