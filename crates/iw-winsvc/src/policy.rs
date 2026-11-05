//! The `SoftwareSASGeneration` policy, free of FFI so it is tested on
//! every OS.
//!
//! `SendSAS` only works if
//! `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\SoftwareSASGeneration`
//! (Group Policy: "Disable or enable software Secure Attention Sequence")
//! allows the caller: 0 or unset allows nobody, 1 services, 2 Ease of
//! Access applications, 3 both. The installer is to set it to 1 or 3;
//! until then `SendSAS` silently does nothing, so the service checks first
//! and says why.

#![cfg_attr(not(windows), allow(dead_code))]

pub(crate) const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
pub(crate) const VALUE: &str = "SoftwareSASGeneration";

/// Whether a service may send Ctrl+Alt+Del under the policy value.
pub(crate) fn services_may_send_sas(policy: Option<u32>) -> bool {
    matches!(policy, Some(1 | 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_values_including_services_allow_it() {
        assert!(!services_may_send_sas(None));
        assert!(!services_may_send_sas(Some(0)));
        assert!(services_may_send_sas(Some(1)));
        assert!(!services_may_send_sas(Some(2)));
        assert!(services_may_send_sas(Some(3)));
        assert!(!services_may_send_sas(Some(4)));
    }
}
