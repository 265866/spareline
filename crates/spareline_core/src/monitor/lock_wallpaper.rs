//! What user 0's lock screen shows behind the clock, from `dumpsys wallpaper`.

use std::sync::LazyLock;

use regex::Regex;

use crate::text;

/// The read the lock screen rule makes, trimmed on the phone to the lines it parses.
pub const LOCK_WALLPAPER_READ: &str =
    "dumpsys wallpaper | grep -E 'wallpaper state:|^ User |mName=|mWallpaperComponent='";

static HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+) wallpaper state:").expect("a valid pattern"));
static IMAGE_WALLPAPER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ComponentInfo\{com\.android\.systemui/[A-Za-z0-9_.]+\.ImageWallpaper\}$")
        .expect("a valid pattern")
});

/// User 0's lock screen wallpaper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockWallpaper {
    /// An image wallpaper of its own, with the name it was set under, possibly empty.
    Own {
        /// The wallpaper's `mName`.
        name: String,
    },
    /// A live wallpaper, named by its component.
    Live {
        /// The wallpaper's `mWallpaperComponent`.
        component: String,
    },
    /// No wallpaper of its own: the lock screen shows the home screen's.
    Shared,
    /// The read printed no lock wallpaper section at all.
    Unreadable,
}

impl LockWallpaper {
    /// Reads [`LOCK_WALLPAPER_READ`]'s output: a `<Kind> wallpaper state:` header per wallpaper,
    /// then per user a ` User N: ...` line and that wallpaper's `mName` and
    /// `mWallpaperComponent`, which for an image is `null` or `SystemUI`'s `ImageWallpaper` (the B30
    /// prints both). A lock screen that shows the home screen's wallpaper has no user under its
    /// header. Setting a live wallpaper keeps the old `mName`, so only an image counts as the
    /// notice.
    #[must_use]
    pub fn parse(output: &str) -> Self {
        let mut kind: Option<&str> = None;
        let mut user: Option<&str> = None;
        let mut saw_lock = false;
        let mut name: Option<&str> = None;
        let mut component: Option<&str> = None;
        for line in text::lines(output) {
            if let Some(header) = HEADER.captures(line) {
                kind = header.get(1).map(|kind| kind.as_str());
                user = None;
                saw_lock |= kind == Some("Lock");
                continue;
            }
            if kind != Some("Lock") {
                continue;
            }
            if let Some(owner) = user_number(line) {
                user = Some(owner);
                continue;
            }
            if !user.is_some_and(|user| user.parse::<u64>().is_ok_and(|user| user == 0)) {
                continue;
            }
            if name.is_none() {
                name = field(line, "mName=");
            }
            if component.is_none() {
                component = field(line, "mWallpaperComponent=");
            }
        }
        if !saw_lock {
            return Self::Unreadable;
        }
        let Some(name) = name else {
            return Self::Shared;
        };
        if let Some(component) = component
            && !component.is_empty()
            && component != "null"
            && !IMAGE_WALLPAPER.is_match(component)
        {
            return Self::Live {
                component: component.to_owned(),
            };
        }
        Self::Own {
            name: name.to_owned(),
        }
    }
}

/// The digits of a `/^\s*User (\d+):/` line.
fn user_number(line: &str) -> Option<&str> {
    let rest = text::trim_start(line).strip_prefix("User ")?;
    let digits = text::leading_digits(rest);
    (!digits.is_empty() && rest[digits.len()..].starts_with(':')).then_some(digits)
}

/// The trimmed value of a `/^\s*<key>(.*)$/` line, where `.` stops at a line terminator.
fn field<'t>(line: &'t str, key: &str) -> Option<&'t str> {
    let value = text::trim_start(line).strip_prefix(key)?;
    (!value.contains(text::is_line_terminator)).then(|| text::trim(value))
}

#[cfg(test)]
mod tests {
    use regex::Captures;

    use super::*;
    use crate::android;

    fn recorded() -> String {
        android::split_sections(include_str!(
            "../../tests/fixtures/nuu-s6702x/policy-read.txt"
        ))
        .get("wallpaper")
        .unwrap()
        .to_owned()
    }

    fn with_lock_entries(entries: &str) -> String {
        let pattern = Regex::new(r"(Lock wallpaper state:\n)(?: .*\n)*").unwrap();
        pattern
            .replace(&recorded(), |captures: &Captures<'_>| {
                format!("{}{entries}", &captures[1])
            })
            .into_owned()
    }

    fn entry(user: u32, name: &str, component: &str) -> String {
        format!(
            " User {user}: id=10: mWhich=2: mSystemWasBoth=true\n  mName={name}\n  \
             mWallpaperComponent={component}\n"
        )
    }

    fn own(name: &str) -> LockWallpaper {
        LockWallpaper::Own {
            name: name.to_owned(),
        }
    }

    #[test]
    fn the_lock_wallpapers_name_is_read_from_its_own_section_only() {
        let recorded = recorded();
        assert_eq!(
            LockWallpaper::parse(&recorded),
            own("spareline-7d53969d19f0d42e")
        );
        assert!(
            recorded.find("System wallpaper state:") < recorded.find("Lock wallpaper state:"),
            "the home screen's wallpaper is listed first and is unnamed"
        );
        assert_eq!(
            LockWallpaper::parse(&recorded.replace('\n', "\r\n")),
            LockWallpaper::parse(&recorded)
        );
    }

    #[test]
    fn a_lock_screen_without_its_own_wallpaper_or_no_read_is_not_mistaken_for_one() {
        let shared = include_str!("../../tests/fixtures/nuu-s6702x/lock-wallpaper-shared.txt");
        assert!(shared.contains("Lock wallpaper state:\nFallback wallpaper state:"));
        assert_eq!(LockWallpaper::parse(shared), LockWallpaper::Shared);
        assert_eq!(LockWallpaper::parse(""), LockWallpaper::Unreadable);
        assert_eq!(
            LockWallpaper::parse(&recorded().replace("spareline-7d53969d19f0d42e", "")),
            own("")
        );
    }

    #[test]
    fn a_live_lock_wallpaper_keeps_the_notices_name_but_is_not_the_notice() {
        let live = "ComponentInfo{com.example.live/com.example.live.Engine}";
        assert_eq!(
            LockWallpaper::parse(&with_lock_entries(&entry(0, "spareline-x", live))),
            LockWallpaper::Live {
                component: live.to_owned()
            }
        );
        assert_eq!(
            LockWallpaper::parse(&with_lock_entries(&entry(
                0,
                "x",
                "ComponentInfo{com.android.systemui/com.android.systemui.wallpapers.ImageWallpaper}"
            ))),
            own("x")
        );
        let lookalike = "ComponentInfo{com.example.live/com.example.ImageWallpaper}";
        assert_eq!(
            LockWallpaper::parse(&with_lock_entries(&entry(0, "x", lookalike))),
            LockWallpaper::Live {
                component: lookalike.to_owned()
            }
        );
    }

    #[test]
    fn only_user_0s_lock_wallpaper_counts_the_user_the_program_sets() {
        let both = format!(
            "{}{}",
            entry(10, "theirs", "null"),
            entry(0, "ours", "null")
        );
        assert_eq!(LockWallpaper::parse(&with_lock_entries(&both)), own("ours"));
        assert_eq!(
            LockWallpaper::parse(&with_lock_entries(&entry(10, "theirs", "null"))),
            LockWallpaper::Shared
        );
    }
}
