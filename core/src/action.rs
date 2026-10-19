//! Typed actions (Phase 2). See `docs/adr/0017-typed-actions.md`.
//!
//! A free-text request ("Buy 3 GPU hours") is only as honest as the agent that
//! wrote it. A *typed* request also carries structured fields for a known
//! action type, e.g. `payments.send {payee, currency}` plus the request's
//! amount. The fields are signed inside the request, the approval prompt is
//! rendered from them (not from the agent's words), and the service's verifier
//! compares them with what the service is actually about to do.
//!
//! The type name is the request's scope, so grants keep naming scopes.
//! The registry is fixed in code: a new type is a reviewed code change, never
//! something an agent can define.

use std::collections::BTreeMap;

/// Structured fields of a typed action: field name -> value. A `BTreeMap`
/// serializes in key order, so the signed bytes are canonical.
pub type Fields = BTreeMap<String, String>;

/// What a field may hold. Checks are strict because the human reads these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// An identifier: printable ASCII, no spaces, 1 to 128 characters
    /// (account ids, payee handles, contact names). ASCII only, so look-alike
    /// letters from other scripts cannot impersonate a known payee.
    Id,
    /// Three upper-case letters (ISO 4217 style), e.g. `USD`.
    Currency,
    /// A path: 1 to 512 characters, no control or invisible formatting
    /// characters, no `..` component.
    Path,
    /// Free text a human reads: 1 to 200 characters, no control or invisible
    /// formatting characters.
    Text,
}

/// Whether an action type moves money.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Money {
    /// `amount_cents` must be set (and above zero).
    Required,
    /// `amount_cents` must be absent.
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ActionType {
    /// Also the scope a grant must allow.
    pub name: &'static str,
    pub fields: &'static [(&'static str, FieldKind)],
    pub money: Money,
    /// One line for the human. `{field}` and `{amount}` are replaced.
    pub template: &'static str,
}

/// Every known action type. Adding one is a code change and a review.
pub const REGISTRY: &[ActionType] = &[
    ActionType {
        name: "payments.send",
        fields: &[("payee", FieldKind::Id), ("currency", FieldKind::Currency)],
        money: Money::Required,
        template: "Pay {amount} to {payee}",
    },
    ActionType {
        name: "files.read",
        fields: &[("path", FieldKind::Path)],
        money: Money::None,
        template: "Read the file {path}",
    },
    ActionType {
        name: "files.write",
        fields: &[("path", FieldKind::Path)],
        money: Money::None,
        template: "Create or overwrite the file {path}",
    },
    ActionType {
        name: "files.delete",
        fields: &[("path", FieldKind::Path)],
        money: Money::None,
        template: "Delete the file {path}",
    },
    ActionType {
        name: "messages.send",
        fields: &[("to", FieldKind::Id), ("subject", FieldKind::Text)],
        money: Money::None,
        template: "Send a message to {to} about \"{subject}\"",
    },
];

pub fn lookup(name: &str) -> Option<&'static ActionType> {
    REGISTRY.iter().find(|t| t.name == name)
}

/// Characters that change how text displays without being visible:
/// bidirectional overrides and isolates, zero-width characters, BOM.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{061c}' | '\u{feff}' | '\u{00ad}')
}

/// Safe to show a human: no control or invisible formatting characters.
pub fn displayable(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || invisible(c))
}

fn valid_value(kind: FieldKind, v: &str) -> bool {
    match kind {
        FieldKind::Id => (1..=128).contains(&v.len()) && v.bytes().all(|c| c.is_ascii_graphic()),
        FieldKind::Currency => v.len() == 3 && v.bytes().all(|c| c.is_ascii_uppercase()),
        FieldKind::Path => {
            (1..=512).contains(&v.len())
                && displayable(v)
                && !v.split(['/', '\\']).any(|part| part == "..")
        }
        FieldKind::Text => (1..=200).contains(&v.chars().count()) && displayable(v),
    }
}

/// Why typed fields were rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    #[error("'{0}' is not a known action type")]
    UnknownType(String),
    #[error("missing field '{0}'")]
    Missing(&'static str),
    #[error("unknown field '{0}'")]
    Unexpected(String),
    #[error("field '{0}' has an invalid value")]
    Invalid(&'static str),
    #[error("this action type needs an amount above zero")]
    AmountRequired,
    #[error("this action type moves no money; remove the amount")]
    AmountForbidden,
}

/// Check `fields` and `amount_cents` against the type named `scope`.
pub fn validate(
    scope: &str,
    fields: &Fields,
    amount_cents: Option<u64>,
) -> Result<&'static ActionType, FieldError> {
    let t = lookup(scope).ok_or_else(|| FieldError::UnknownType(scope.into()))?;
    for (name, kind) in t.fields {
        let v = fields.get(*name).ok_or(FieldError::Missing(name))?;
        if !valid_value(*kind, v) {
            return Err(FieldError::Invalid(name));
        }
    }
    if let Some(extra) = fields
        .keys()
        .find(|k| !t.fields.iter().any(|(n, _)| n == k))
    {
        return Err(FieldError::Unexpected(extra.clone()));
    }
    match (t.money, amount_cents) {
        (Money::Required, None | Some(0)) => Err(FieldError::AmountRequired),
        (Money::None, Some(_)) => Err(FieldError::AmountForbidden),
        _ => Ok(t),
    }
}

/// Digits after the decimal point for a currency's minor unit. Most use two;
/// these are the common exceptions.
fn minor_digits(currency: &str) -> u32 {
    match currency {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "PYG" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" | "IQD" | "LYD" => 3,
        _ => 2,
    }
}

/// `1500` minor units of `USD` -> `15.00 USD`.
pub fn format_money(minor: u64, currency: &str) -> String {
    let d = minor_digits(currency);
    if d == 0 {
        return format!("{minor} {currency}");
    }
    let unit = 10u64.pow(d);
    format!(
        "{}.{:0width$} {currency}",
        minor / unit,
        minor % unit,
        width = d as usize
    )
}

/// The one line a human approves, rendered from validated fields only.
/// `None` if the fields do not validate (callers then show nothing typed).
pub fn describe(scope: &str, fields: &Fields, amount_cents: Option<u64>) -> Option<String> {
    let t = validate(scope, fields, amount_cents).ok()?;
    // One pass over the template, so a value containing `{other}` stays literal.
    let mut out = String::new();
    let mut rest = t.template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = open + rest[open..].find('}')?;
        let name = &rest[open + 1..close];
        if name == "amount" {
            let cur = fields.get("currency").map_or("", String::as_str);
            out.push_str(&format_money(amount_cents?, cur));
        } else {
            out.push_str(fields.get(name)?);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Parse `key=value` pairs (from the command line).
pub fn parse_pairs<'a>(pairs: impl IntoIterator<Item = &'a str>) -> Result<Fields, String> {
    let mut out = Fields::new();
    for p in pairs {
        let (k, v) = p
            .split_once('=')
            .ok_or_else(|| format!("'{p}' is not key=value"))?;
        if out.insert(k.to_string(), v.to_string()).is_some() {
            return Err(format!("field '{k}' given twice"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(pairs: &[&str]) -> Fields {
        parse_pairs(pairs.iter().copied()).unwrap()
    }

    #[test]
    fn fields_must_match_the_type_exactly() {
        let pay = f(&["payee=acme-gpu", "currency=USD"]);
        assert!(validate("payments.send", &pay, Some(1500)).is_ok());
        assert_eq!(
            describe("payments.send", &pay, Some(1500)).unwrap(),
            "Pay 15.00 USD to acme-gpu"
        );
        assert_eq!(
            validate("payments.send", &f(&["payee=acme-gpu"]), Some(1)),
            Err(FieldError::Missing("currency"))
        );
        assert_eq!(
            validate(
                "payments.send",
                &f(&["payee=a", "currency=USD", "memo=x"]),
                Some(1)
            ),
            Err(FieldError::Unexpected("memo".into()))
        );
        assert_eq!(
            validate("payments.send", &pay, None),
            Err(FieldError::AmountRequired)
        );
        assert_eq!(
            validate("payments.send", &pay, Some(0)),
            Err(FieldError::AmountRequired)
        );
        assert_eq!(
            validate("files.read", &f(&["path=/a"]), Some(5)),
            Err(FieldError::AmountForbidden)
        );
        assert!(matches!(
            validate("admin.wipe", &Fields::new(), None),
            Err(FieldError::UnknownType(_))
        ));
        assert!(parse_pairs(["a=1", "a=2"]).is_err());
        assert!(parse_pairs(["novalue"]).is_err());
    }

    #[test]
    fn values_a_human_could_misread_are_refused() {
        let bad_payees = [
            "acme gpu",        // space
            "\u{0430}cme-gpu", // Cyrillic a
            "acme\u{200b}gpu", // zero-width space
            "",                // empty
            &"x".repeat(129),  // too long
        ];
        for p in bad_payees {
            let fields = f(&[&format!("payee={p}"), "currency=USD"]);
            assert_eq!(
                validate("payments.send", &fields, Some(1)),
                Err(FieldError::Invalid("payee")),
                "{p:?}"
            );
        }
        for c in ["usd", "US", "USDT", "U$D"] {
            let fields = f(&["payee=a", &format!("currency={c}")]);
            assert!(validate("payments.send", &fields, Some(1)).is_err(), "{c}");
        }
        for p in ["/a/../etc/passwd", "..\\x", "a\u{202e}txt.exe", "a\nb"] {
            assert_eq!(
                validate("files.delete", &f(&[&format!("path={p}")]), None),
                Err(FieldError::Invalid("path")),
                "{p:?}"
            );
        }
        // A value that looks like a placeholder is shown as itself.
        assert_eq!(
            describe(
                "payments.send",
                &f(&["payee={currency}", "currency=USD"]),
                Some(100)
            )
            .unwrap(),
            "Pay 1.00 USD to {currency}"
        );
        // Non-ASCII paths and subjects are fine when nothing is hidden.
        assert!(validate("files.read", &f(&["path=/home/ดาว/รายงาน.pdf"]), None).is_ok());
        assert_eq!(
            describe(
                "messages.send",
                &f(&["to=bob", "subject=Lunch on Friday"]),
                None
            )
            .unwrap(),
            "Send a message to bob about \"Lunch on Friday\""
        );
    }

    #[test]
    fn money_is_shown_in_the_currency_minor_unit() {
        assert_eq!(format_money(1500, "USD"), "15.00 USD");
        assert_eq!(format_money(5, "EUR"), "0.05 EUR");
        assert_eq!(format_money(1500, "JPY"), "1500 JPY");
        assert_eq!(format_money(1500, "KWD"), "1.500 KWD");
        assert_eq!(format_money(u64::MAX, "THB"), "184467440737095516.15 THB");
    }

    #[test]
    fn templates_name_only_their_own_fields() {
        for t in REGISTRY {
            let mut rest = t.template.to_string();
            for (n, _) in t.fields {
                rest = rest.replace(&format!("{{{n}}}"), "");
            }
            rest = rest.replace("{amount}", "");
            assert!(!rest.contains('{'), "{}: unknown placeholder", t.name);
            assert_eq!(
                t.template.contains("{amount}"),
                t.money == Money::Required,
                "{}",
                t.name
            );
            assert!(crate::agent::valid_scope(t.name));
        }
    }
}
