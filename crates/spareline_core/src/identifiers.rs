//! Phone identifiers: the IMEI and ICCID shapes, E.164 numbers, and the `IPhoneSubInfo` binder
//! codes the identity probe reads them with.

use crate::text;

/// `IPhoneSubInfo` binder transaction codes for `service call iphonesubinfo`.
///
/// The codes follow the method order in `IPhoneSubInfo.aidl`, which changes between Android
/// releases and can change in OEM builds, so they are looked up by SDK in one table here. A code
/// is used only when the table names it, and any value it returns is still checked (the IMEI's
/// check digit, the ICCID's shape), so a wrong code yields no identifier rather than a wrong one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubInfoCodes {
    /// `getDeviceIdForPhone(int phoneId, String callingPackage, String featureId)`.
    pub imei_for_slot: u32,
    /// `getIccSerialNumberForSubscriber(int subId, String callingPackage, String featureId)`.
    pub iccid_for_sub: u32,
    /// `getLine1NumberForSubscriber(int subId, String callingPackage, String featureId)`.
    pub number_for_sub: u32,
}

/// The result of looking up the codes for a build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubInfoLookup {
    /// The table has codes for the build's SDK.
    Codes {
        /// The codes.
        codes: SubInfoCodes,
        /// Whether every code was checked on a real phone with this exact build fingerprint.
        verified: bool,
    },
    /// The table has no codes for the build's SDK.
    Missing {
        /// Why, for the identity's warnings.
        reason: String,
    },
}

/// One SDK's codes, and the builds they were checked on.
struct SubInfoEntry {
    sdk: u64,
    codes: SubInfoCodes,
    verified_fingerprints: &'static [&'static str],
}

const SUB_INFO_TABLE: &[SubInfoEntry] = &[SubInfoEntry {
    sdk: 34,
    codes: SubInfoCodes {
        imei_for_slot: 4,
        iccid_for_sub: 14,
        number_for_sub: 16,
    },
    verified_fingerprints: &["NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys"],
}];

/// The codes for a build with this SDK and fingerprint. A build of a listed SDK with another
/// fingerprint gets the codes, unverified.
#[must_use]
pub fn sub_info_codes(sdk: Option<u64>, fingerprint: Option<&str>) -> SubInfoLookup {
    let Some(entry) = SUB_INFO_TABLE.iter().find(|entry| Some(entry.sdk) == sdk) else {
        let sdk = sdk.map_or_else(|| "unknown".to_owned(), |sdk| sdk.to_string());
        return SubInfoLookup::Missing {
            reason: format!(
                "no iphonesubinfo codes for Android SDK {sdk}; add them to \
                 crates/spareline_core/src/identifiers.rs"
            ),
        };
    };
    SubInfoLookup::Codes {
        codes: entry.codes,
        verified: fingerprint
            .is_some_and(|fingerprint| entry.verified_fingerprints.contains(&fingerprint)),
    }
}

/// The shell command for one `IPhoneSubInfo` getter, called as the shell user, such as
/// `service call iphonesubinfo 4 i32 0 s16 com.android.shell s16 null`.
#[must_use]
pub fn sub_info_call(code: u32, index: i64) -> String {
    format!("service call iphonesubinfo {code} i32 {index} s16 com.android.shell s16 null")
}

/// Whether `value` is an IMEI: 15 ASCII digits with a valid Luhn check digit.
#[must_use]
pub fn is_imei(value: &str) -> bool {
    value.len() == 15 && value.bytes().all(|byte| byte.is_ascii_digit()) && luhn_valid(value)
}

/// Whether `value` is an ICCID: the telecom industry prefix 89 (ITU-T E.118), then 16 to 19
/// digits, then optionally the hex check character some carriers append.
#[must_use]
pub fn is_iccid(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("89") else {
        return false;
    };
    let Some(last) = rest.chars().next_back() else {
        return false;
    };
    let body = &rest[..rest.len() - last.len_utf8()];
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    match rest.len() {
        16 => digits(rest),
        17..=20 => digits(body) && last.is_ascii_hexdigit(),
        _ => false,
    }
}

fn luhn_valid(digits: &str) -> bool {
    let sum: u32 = digits
        .bytes()
        .rev()
        .enumerate()
        .map(|(index, byte)| {
            let digit = u32::from(byte - b'0');
            if index % 2 == 1 {
                let doubled = digit * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                digit
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Normalizes a phone number read from a SIM or typed by the owner to E.164, such as
/// `+15125550101`. White space, parentheses, dots and dashes are dropped first. A number already
/// in E.164 form is kept whatever the country; a national number is read only for the North
/// American SIM countries (`us`, `ca`, `pr`, in any case), as 11 digits starting with 1 or as 10
/// digits. Anything else is `None`.
#[must_use]
pub fn to_e164(raw: &str, country_iso: Option<&str>) -> Option<String> {
    let digits: String = text::trim(raw)
        .chars()
        .filter(|&c| !(text::is_space(c) || matches!(c, '(' | ')' | '.' | '-')))
        .collect();
    if let Some(number) = digits.strip_prefix('+') {
        let international = (8..=15).contains(&number.len())
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && !number.starts_with('0');
        return international.then_some(digits);
    }
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let north_american = country_iso
        .filter(|iso| !iso.is_empty())
        .is_some_and(|iso| matches!(iso.to_lowercase().as_str(), "us" | "ca" | "pr"));
    if !north_american {
        return None;
    }
    match digits.len() {
        11 if digits.starts_with('1') => Some(format!("+{digits}")),
        10 => Some(format!("+1{digits}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imeis_need_15_digits_and_a_valid_check_digit() {
        assert!(
            is_imei("490154203237518"),
            "the example IMEI from 3GPP TS 23.003"
        );
        assert!(!is_imei("490154203237517"), "a wrong check digit");
        assert!(!is_imei("49015420323751"), "14 digits");
        assert!(!is_imei(""), "empty");
        assert!(!is_imei("49015420323751a"), "a letter");
    }

    #[test]
    fn iccids_start_with_89_and_have_19_or_20_digits() {
        assert!(is_iccid("8901555000000000001"), "19 digits");
        assert!(is_iccid("89015550000000000012"), "20 digits");
        assert!(
            is_iccid("890155500000000000F"),
            "18 digits and a hex check character"
        );
        assert!(
            is_iccid("891234567890123456"),
            "the shortest: 89 and 16 digits"
        );
        assert!(
            is_iccid("891234567890123456789f"),
            "the longest: 89, 19 digits and hex"
        );
        assert!(!is_iccid("891234567890123456789012"), "too long");
        assert!(!is_iccid("8901260123456789F"), "too short");
        assert!(!is_iccid("1234567890123456789"), "no 89 prefix");
        assert!(!is_iccid("89123456789012345G"), "not hex");
        assert!(!is_iccid("89"), "only the prefix");
    }

    #[test]
    fn e164_for_north_american_sims() {
        let e164 = |raw: &str, iso: Option<&str>| to_e164(raw, iso);
        assert_eq!(
            e164("15125550101", Some("us")).as_deref(),
            Some("+15125550101")
        );
        assert_eq!(
            e164("5125550101", Some("us")).as_deref(),
            Some("+15125550101")
        );
        assert_eq!(
            e164("(512) 555-0101", Some("US")).as_deref(),
            Some("+15125550101")
        );
        assert_eq!(
            e164(" 512.555.0101 ", Some("ca")).as_deref(),
            Some("+15125550101")
        );
        assert_eq!(
            e164("+44 20 7946 0000", Some("us")).as_deref(),
            Some("+442079460000")
        );
        assert_eq!(
            e164("+44 20 7946 0000", None).as_deref(),
            Some("+442079460000")
        );
        assert_eq!(e164("5125550101", None), None);
        assert_eq!(e164("5125550101", Some("")), None);
        assert_eq!(e164("5125550101", Some("gb")), None);
        assert_eq!(e164("25125550101", Some("us")), None);
        assert_eq!(e164("not a number", Some("us")), None);
        assert_eq!(e164("", Some("us")), None);
        assert_eq!(e164("+", Some("us")), None);
    }

    #[test]
    fn e164_rejects_a_leading_0_country_code_and_over_long_numbers() {
        assert_eq!(to_e164("+0123456789", Some("us")), None);
        assert_eq!(to_e164("+1234567890123456", Some("us")), None);
        assert_eq!(to_e164("+1234567", Some("us")), None);
        assert_eq!(
            to_e164("+123456789012345", Some("us")).as_deref(),
            Some("+123456789012345")
        );
        assert_eq!(
            to_e164("+12345678", Some("us")).as_deref(),
            Some("+12345678")
        );
    }

    #[test]
    fn codes_are_looked_up_by_sdk_and_verified_by_fingerprint() {
        let codes = SubInfoCodes {
            imei_for_slot: 4,
            iccid_for_sub: 14,
            number_for_sub: 16,
        };
        assert_eq!(
            sub_info_codes(
                Some(34),
                Some("NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys")
            ),
            SubInfoLookup::Codes {
                codes,
                verified: true,
            }
        );
        assert_eq!(
            sub_info_codes(Some(34), Some("other")),
            SubInfoLookup::Codes {
                codes,
                verified: false,
            }
        );
        assert_eq!(
            sub_info_codes(Some(34), None),
            SubInfoLookup::Codes {
                codes,
                verified: false,
            }
        );
        assert!(
            matches!(
                sub_info_codes(Some(35), None),
                SubInfoLookup::Missing { reason } if reason.starts_with("no iphonesubinfo codes for Android SDK 35;")
            ),
            "SDK 35 has no codes"
        );
        assert!(
            matches!(
                sub_info_codes(None, None),
                SubInfoLookup::Missing { reason } if reason.contains("Android SDK unknown;")
            ),
            "an unknown SDK has no codes"
        );
    }

    #[test]
    fn getter_command() {
        assert_eq!(
            sub_info_call(14, 1),
            "service call iphonesubinfo 14 i32 1 s16 com.android.shell s16 null"
        );
    }
}
