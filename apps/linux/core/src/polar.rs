//! Asking Polar about this computer's license (docs/architecture.md §11):
//! the requests to its public license-key endpoints and what their answers
//! mean. Sending them is a [`Transport`]'s job, so everything here is tested
//! against Polar's recorded answers (testdata/polar) without a network.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Polar's customer-portal license-key endpoints; `INPUTWEAVE_POLAR_URL`
/// replaces it (scripts/mock-polar.py serves the same paths).
pub const API: &str = "https://api.polar.sh/v1/customer-portal/license-keys/";
/// The organization that sells InputWeave's license.
pub const ORGANIZATION_ID: &str = "95b9d4a7-5dad-4df5-b309-2cd3383420ae";
/// Where Buy a License goes.
pub const CHECKOUT_URL: &str =
    "https://buy.polar.sh/polar_cl_QlXeUC208E1POhQcAZe6AgpFHGbpsKGBXmhmu0x5LVd";
/// Where buyers free computers (Manage Computers).
pub const PORTAL_URL: &str = "https://polar.sh/ctsitecc/portal";

/// How long to wait after a 429 without a usable `Retry-After`.
const DEFAULT_RETRY: Duration = Duration::from_secs(60);
/// How long a request may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// How long activate may take: an answer given up on may still have
/// granted a seat, so it is waited for longer.
pub const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest label an activation gets.
const MAX_LABEL: usize = 64;

/// The base URL requests go to.
pub fn api() -> String {
    match std::env::var("INPUTWEAVE_POLAR_URL") {
        Ok(url) if !url.is_empty() => {
            if url.ends_with('/') {
                url
            } else {
                format!("{url}/")
            }
        }
        _ => API.to_owned(),
    }
}

/// What every request sends as its User-Agent: Cloudflare, in front of
/// Polar, refuses some clients' default ones.
pub fn user_agent() -> String {
    format!("InputWeave/{}", env!("CARGO_PKG_VERSION"))
}

/// An HTTP answer, as much of it as the license needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    /// The `Date` header.
    pub date: Option<String>,
    /// The `Retry-After` header.
    pub retry_after: Option<String>,
}

/// Sends a JSON body with POST and returns the answer, whatever its status;
/// `Err` only when there was no answer within `timeout` (no network, TLS,
/// too slow).
pub trait Transport {
    fn post(&self, url: &str, json: &str, timeout: Duration) -> Result<Response, String>;
}

/// An activation's label from this computer's name: trimmed, each run of
/// whitespace one space, at most 64 characters.
pub fn label(name: &str) -> String {
    let label: String = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LABEL)
        .collect();
    if label.is_empty() {
        "Linux".to_owned()
    } else {
        label.trim_end().to_owned()
    }
}

/// Why Polar gave no usable answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// No answer, or one that is not Polar's (Cloudflare's own 403, a 5xx,
    /// a body that does not parse): try again later.
    Unreachable(String),
    /// 429: try again after this long.
    RateLimited(Duration),
}

/// What `activate` answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    Activated {
        activation_id: String,
        customer_email: String,
    },
    /// 403 `NotPermitted`: every seat is taken, or the key is no longer
    /// active; a `validate` without an activation tells which.
    NotPermitted,
    /// 404, or a 422 about the key: the key is invalid.
    Invalid,
    /// Another 422: Polar could not read what the app sent; asking again
    /// will not change it.
    Rejected,
    /// 200 without an id the app can read: a seat may be taken all the
    /// same, so it is not asked for again by itself.
    Unreadable,
    Trouble(Trouble),
}

/// What `validate` answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Validation {
    /// 200; the email is empty when Polar left it out.
    Valid {
        customer_email: String,
    },
    /// 404, or a 422 about the key or the activation: the key, or this
    /// activation, is unknown, revoked or refunded.
    NotFound,
    /// Another 422.
    Rejected,
    Trouble(Trouble),
}

/// What `deactivate` answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deactivation {
    /// 204; 404 because it was already gone; or an answer asking again
    /// would not change (Polar's own 403, a 422).
    Done,
    Trouble(Trouble),
}

/// An answer and when Polar gave it (the `Date` header, when there is one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered<T> {
    pub outcome: T,
    pub at: Option<SystemTime>,
}

fn post(
    transport: &dyn Transport,
    base: &str,
    endpoint: &str,
    body: Value,
) -> Result<Response, Trouble> {
    let timeout = if endpoint == "activate" {
        ACTIVATE_TIMEOUT
    } else {
        TIMEOUT
    };
    transport
        .post(&format!("{base}{endpoint}"), &body.to_string(), timeout)
        .map_err(Trouble::Unreachable)
}

/// Activates `key` for this computer; `name` is its name, made into the
/// label ([`label`]).
pub fn activate(
    transport: &dyn Transport,
    base: &str,
    key: &str,
    name: &str,
) -> Answered<Activation> {
    let body = json!({ "key": key, "organization_id": ORGANIZATION_ID, "label": label(name) });
    match post(transport, base, "activate", body) {
        Ok(r) => Answered {
            at: r.date.as_deref().and_then(parse_http_date),
            outcome: read_activation(&r),
        },
        Err(t) => Answered {
            outcome: Activation::Trouble(t),
            at: None,
        },
    }
}

pub fn validate(
    transport: &dyn Transport,
    base: &str,
    key: &str,
    activation_id: Option<&str>,
) -> Answered<Validation> {
    let mut body = json!({ "key": key, "organization_id": ORGANIZATION_ID });
    if let Some(id) = activation_id {
        body["activation_id"] = Value::from(id);
    }
    match post(transport, base, "validate", body) {
        Ok(r) => Answered {
            at: r.date.as_deref().and_then(parse_http_date),
            outcome: read_validation(&r),
        },
        Err(t) => Answered {
            outcome: Validation::Trouble(t),
            at: None,
        },
    }
}

pub fn deactivate(
    transport: &dyn Transport,
    base: &str,
    key: &str,
    activation_id: &str,
) -> Deactivation {
    let body = json!({
        "key": key,
        "organization_id": ORGANIZATION_ID,
        "activation_id": activation_id,
    });
    match post(transport, base, "deactivate", body) {
        Ok(r) => match r.status {
            200..=299 | 404 | 422 => Deactivation::Done,
            403 if parse(&r.body).is_some_and(|b| is_not_permitted(&b)) => Deactivation::Done,
            _ => Deactivation::Trouble(trouble(&r)),
        },
        Err(t) => Deactivation::Trouble(t),
    }
}

/// The trouble an answer that is not the endpoint's own means.
fn trouble(r: &Response) -> Trouble {
    if r.status == 429 {
        return Trouble::RateLimited(retry_after(r.retry_after.as_deref()));
    }
    Trouble::Unreachable(format!("Polar answered {}", r.status))
}

fn retry_after(header: Option<&str>) -> Duration {
    header
        .and_then(|h| h.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.clamp(1, 3600)))
        .unwrap_or(DEFAULT_RETRY)
}

/// Polar's own refusal, as opposed to Cloudflare's 403 in front of it.
fn is_not_permitted(body: &Value) -> bool {
    body.get("error").and_then(Value::as_str) == Some("NotPermitted")
}

fn parse(body: &str) -> Option<Value> {
    serde_json::from_str(body).ok()
}

/// Whether a 422 is about one of `fields` of the request (its `detail`
/// names them in `loc`).
fn about(r: &Response, fields: &[&str]) -> bool {
    let Some(body) = parse(&r.body) else {
        return false;
    };
    body.get("detail")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("loc").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .any(|field| fields.contains(&field))
}

fn read_activation(r: &Response) -> Activation {
    match r.status {
        200 | 201 => {
            let Some(body) = parse(&r.body) else {
                return Activation::Unreadable;
            };
            match body.get("id").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => Activation::Activated {
                    activation_id: id.to_owned(),
                    customer_email: body
                        .pointer("/license_key/customer/email")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                },
                _ => Activation::Unreadable,
            }
        }
        403 if parse(&r.body).is_some_and(|b| is_not_permitted(&b)) => Activation::NotPermitted,
        404 => Activation::Invalid,
        422 if about(r, &["key"]) => Activation::Invalid,
        422 => Activation::Rejected,
        _ => Activation::Trouble(trouble(r)),
    }
}

fn read_validation(r: &Response) -> Validation {
    match r.status {
        200 => match parse(&r.body) {
            Some(body) => Validation::Valid {
                customer_email: body
                    .pointer("/customer/email")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            },
            None => Validation::Trouble(Trouble::Unreachable("an unreadable answer".into())),
        },
        404 => Validation::NotFound,
        422 if about(r, &["key", "activation_id"]) => Validation::NotFound,
        422 => Validation::Rejected,
        _ => Validation::Trouble(trouble(r)),
    }
}

/// An HTTP date in its preferred form (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn parse_http_date(date: &str) -> Option<SystemTime> {
    let mut parts = date.split_whitespace();
    let _weekday = parts.next()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let mut clock = parts.next()?.split(':');
    let hour: u64 = clock.next()?.parse().ok()?;
    let minute: u64 = clock.next()?.parse().ok()?;
    let second: u64 = clock.next()?.parse().ok()?;
    if parts.next()? != "GMT" || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    let secs = (days as u64)
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar
/// (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../../../testdata/polar/", $name))
        };
    }

    /// Answers every request with one response and remembers what was
    /// asked.
    struct Canned {
        answer: Result<Response, String>,
        asked: RefCell<Vec<(String, Value)>>,
        timeouts: RefCell<Vec<Duration>>,
    }

    impl Canned {
        fn status(status: u16, body: &str) -> Canned {
            Canned {
                answer: Ok(Response {
                    status,
                    body: body.into(),
                    date: Some("Wed, 04 Nov 2026 08:00:00 GMT".into()),
                    retry_after: None,
                }),
                asked: RefCell::default(),
                timeouts: RefCell::default(),
            }
        }

        fn asked(&self) -> (String, Value) {
            self.asked.borrow().last().cloned().expect("a request")
        }
    }

    impl Transport for Canned {
        fn post(&self, url: &str, json: &str, timeout: Duration) -> Result<Response, String> {
            self.asked
                .borrow_mut()
                .push((url.into(), serde_json::from_str(json).unwrap()));
            self.timeouts.borrow_mut().push(timeout);
            self.answer.clone()
        }
    }

    const KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";

    #[test]
    fn activate_sends_the_key_the_organization_and_the_label_only() {
        let t = Canned::status(200, fixture!("activate-200.json"));
        let answer = activate(&t, API, KEY, "Linux \"box\"");
        let (url, body) = t.asked();
        assert_eq!(url, format!("{API}activate"));
        assert_eq!(body["key"], KEY);
        assert_eq!(body["organization_id"], ORGANIZATION_ID);
        assert_eq!(body["label"], "Linux \"box\"");
        assert!(body.get("conditions").is_none());
        assert_eq!(body.as_object().unwrap().len(), 3);
        match answer.outcome {
            Activation::Activated {
                activation_id,
                customer_email,
            } => {
                assert!(!activation_id.is_empty());
                assert!(customer_email.contains('@'));
            }
            other => panic!("{other:?}"),
        }
        assert!(answer.at.is_some());
    }

    #[test]
    fn a_403_is_a_refusal_only_when_polar_says_not_permitted() {
        for name in [
            fixture!("activate-403-limit.json"),
            fixture!("activate-403-revoked.json"),
        ] {
            let t = Canned::status(403, name);
            assert_eq!(
                activate(&t, API, KEY, "x").outcome,
                Activation::NotPermitted
            );
        }
        let t = Canned::status(403, fixture!("403-cloudflare.json"));
        assert!(matches!(
            activate(&t, API, KEY, "x").outcome,
            Activation::Trouble(Trouble::Unreachable(_))
        ));
        let t = Canned::status(403, fixture!("403-cloudflare.json"));
        assert!(matches!(
            validate(&t, API, KEY, Some("a")).outcome,
            Validation::Trouble(Trouble::Unreachable(_))
        ));
    }

    /// A 422 like testdata/polar/422-validation.json, about `field`.
    fn unreadable(field: &str) -> String {
        fixture!("422-validation.json").replace("\"activation_id\"]", &format!("\"{field}\"]"))
    }

    #[test]
    fn unknown_keys_and_422s_about_them_are_invalid() {
        let about_key = unreadable("key");
        for (status, body) in [
            (404, fixture!("404-not-found.json")),
            (404, fixture!("404-no-longer-active.json")),
            (422, about_key.as_str()),
        ] {
            let t = Canned::status(status, body);
            assert_eq!(activate(&t, API, KEY, "x").outcome, Activation::Invalid);
            let t = Canned::status(status, body);
            assert_eq!(
                validate(&t, API, KEY, Some("a")).outcome,
                Validation::NotFound
            );
        }
        // About the activation: unknown to validate, nothing activate sends.
        let t = Canned::status(422, fixture!("422-validation.json"));
        assert_eq!(
            validate(&t, API, KEY, Some("xyz")).outcome,
            Validation::NotFound
        );
        let t = Canned::status(422, fixture!("422-validation.json"));
        assert_eq!(activate(&t, API, KEY, "x").outcome, Activation::Rejected);
        // About anything else, or unreadable: not the key's fault.
        for body in [unreadable("label"), "{}".to_owned(), "<html>".to_owned()] {
            let t = Canned::status(422, &body);
            assert_eq!(activate(&t, API, KEY, "x").outcome, Activation::Rejected);
            let t = Canned::status(422, &body);
            assert_eq!(validate(&t, API, KEY, None).outcome, Validation::Rejected);
        }
    }

    #[test]
    fn the_label_is_the_name_tidied_and_activate_waits_longer() {
        assert_eq!(label("  ubuntu \t desk\n"), "ubuntu desk");
        assert_eq!(label(""), "Linux");
        assert_eq!(label(" \n "), "Linux");
        let long = label(&format!("{} x", "名".repeat(70)));
        assert_eq!(long.chars().count(), 64);
        assert_eq!(label(&format!("{}  y", "a".repeat(63))), "a".repeat(63));
        let t = Canned::status(200, fixture!("activate-200.json"));
        activate(&t, API, KEY, "  my   box ");
        assert_eq!(t.asked().1["label"], "my box");
        assert_eq!(t.timeouts.borrow().last(), Some(&ACTIVATE_TIMEOUT));
        let t = Canned::status(200, fixture!("validate-200.json"));
        validate(&t, API, KEY, None);
        assert_eq!(t.timeouts.borrow().last(), Some(&TIMEOUT));
    }

    #[test]
    fn validate_reads_the_email_and_sends_the_activation_when_it_has_one() {
        let t = Canned::status(200, fixture!("validate-200.json"));
        match validate(&t, API, KEY, Some("act-1")).outcome {
            Validation::Valid { customer_email } => assert!(customer_email.contains('@')),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.asked().1["activation_id"], "act-1");
        let t = Canned::status(200, fixture!("validate-200-key-only.json"));
        assert!(matches!(
            validate(&t, API, KEY, None).outcome,
            Validation::Valid { .. }
        ));
        assert!(t.asked().1.get("activation_id").is_none());
    }

    #[test]
    fn a_429_says_when_to_try_again() {
        let mut t = Canned::status(429, fixture!("429-rate-limited.json"));
        if let Ok(r) = &mut t.answer {
            r.retry_after = Some("7".into());
        }
        assert_eq!(
            activate(&t, API, KEY, "x").outcome,
            Activation::Trouble(Trouble::RateLimited(Duration::from_secs(7)))
        );
        let t = Canned::status(429, fixture!("429-rate-limited.json"));
        assert_eq!(
            validate(&t, API, KEY, Some("a")).outcome,
            Validation::Trouble(Trouble::RateLimited(DEFAULT_RETRY))
        );
    }

    #[test]
    fn a_date_out_of_range_is_no_date() {
        for date in [
            "Wed, 04 Nov 99999999999999 08:00:00 GMT",
            "Wed, 04 Nov -5 08:00:00 GMT",
            "Wed, 04 Nov 1969 08:00:00 GMT",
            "Wed, 04 Nov 10000 08:00:00 GMT",
        ] {
            assert_eq!(parse_http_date(date), None, "{date}");
        }
        assert!(parse_http_date("Fri, 31 Dec 9999 23:59:59 GMT").is_some());
    }

    #[test]
    fn deactivate_is_done_unless_asking_again_could_change_it() {
        for (status, body) in [
            (204, ""),
            (200, ""),
            (404, fixture!("404-not-found.json")),
            (422, fixture!("422-validation.json")),
            (403, fixture!("activate-403-revoked.json")),
        ] {
            let t = Canned::status(status, body);
            assert_eq!(
                deactivate(&t, API, KEY, "a"),
                Deactivation::Done,
                "{status}"
            );
            let body = t.asked().1;
            assert_eq!(body["activation_id"], "a");
            assert_eq!(body["organization_id"], ORGANIZATION_ID);
        }
        for (status, body) in [
            (503, ""),
            (403, fixture!("403-cloudflare.json")),
            (429, fixture!("429-rate-limited.json")),
        ] {
            let t = Canned::status(status, body);
            assert!(
                matches!(deactivate(&t, API, KEY, "a"), Deactivation::Trouble(_)),
                "{status}"
            );
        }
    }

    #[test]
    fn no_answer_and_server_errors_are_trouble() {
        let t = Canned {
            answer: Err("no route".into()),
            asked: RefCell::default(),
            timeouts: RefCell::default(),
        };
        assert_eq!(
            activate(&t, API, KEY, "x").outcome,
            Activation::Trouble(Trouble::Unreachable("no route".into()))
        );
        let t = Canned::status(502, "<html>");
        assert!(matches!(
            validate(&t, API, KEY, None).outcome,
            Validation::Trouble(Trouble::Unreachable(_))
        ));
        // A grant the app cannot read is not trouble to try again.
        for body in ["not json", "{}", r#"{"id":""}"#] {
            let t = Canned::status(200, body);
            assert_eq!(activate(&t, API, KEY, "x").outcome, Activation::Unreadable);
        }
    }

    #[test]
    fn http_dates() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(UNIX_EPOCH + Duration::from_secs(784_111_777))
        );
        assert_eq!(
            parse_http_date("Wed, 04 Nov 2026 08:00:00 GMT"),
            Some(UNIX_EPOCH + Duration::from_secs(1_793_779_200))
        );
        for bad in [
            "",
            "yesterday",
            "Sun, 06 Nov 1994 08:49:37 PST",
            "Sun, 32 Nov 1994 08:49:37 GMT",
        ] {
            assert_eq!(parse_http_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_user_agent_names_the_app_and_its_version() {
        assert_eq!(
            user_agent(),
            format!("InputWeave/{}", env!("CARGO_PKG_VERSION"))
        );
    }
}
