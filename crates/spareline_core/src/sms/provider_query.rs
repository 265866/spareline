//! The `content` commands that read the phone's SMS and MMS providers.

/// One of the phone's message providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// `content://sms`.
    Sms,
    /// `content://mms`.
    Mms,
}

impl Provider {
    /// The provider's name as the database spells it: `sms` or `mms`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "sms",
            Self::Mms => "mms",
        }
    }
}

/// The columns of an SMS inbox row. `hex(body)` goes last, since a value runs to the next
/// expected column (see [`parse_content_rows`](crate::parse_content_rows)).
pub const SMS_COLUMNS: &[&str] = &[
    "_id",
    "thread_id",
    "address",
    "date",
    "date_sent",
    "sub_id",
    "read",
    "type",
    "hex(body)",
];

/// The columns of an MMS inbox row.
pub const MMS_COLUMNS: &[&str] = &["_id", "date", "date_sent", "sub_id", "m_type", "hex(sub)"];

/// The columns of an MMS address row.
pub const MMS_ADDR_COLUMNS: &[&str] = &["type", "address"];

/// The columns of an MMS part row.
pub const MMS_PART_COLUMNS: &[&str] = &[
    "_id",
    "mid",
    "seq",
    "ct",
    "hex(name)",
    "hex(fn)",
    "hex(cl)",
    "hex(text)",
];

/// The columns of an inbox key row: the provider `_id` and `date` of every inbox row, for
/// reconciling.
pub const INBOX_KEY_COLUMNS: &[&str] = &["_id", "date"];

/// MMS rows with `m_type` 132 (retrieve-conf) are downloaded messages. A notification (130)
/// stays in the inbox until the download finishes and has no parts yet, so it is skipped until
/// then.
const MMS_RETRIEVED: &str = "m_type=132";

/// A `content query` the ingest runs, each with the columns its rows hold. Each `content` call
/// starts `app_process` on the phone, about 1.1 s on the B30, so callers batch what they can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderQuery {
    /// Inbox rows with an `_id` above `after_id`, in `_id` order.
    After {
        /// The provider.
        provider: Provider,
        /// The cursor: the highest `_id` already read.
        after_id: i64,
    },
    /// The inbox rows with these `_id`s, in `_id` order.
    ByIds {
        /// The provider.
        provider: Provider,
        /// The ids.
        ids: Vec<i64>,
    },
    /// The `_id` and `date` of every inbox row, in `_id` order.
    Keys {
        /// The provider.
        provider: Provider,
    },
    /// The addresses of one MMS.
    MmsAddresses {
        /// The MMS's `_id`.
        mms_id: i64,
    },
    /// The parts of these MMS, in part `_id` order.
    MmsParts {
        /// The MMS `_id`s.
        mms_ids: Vec<i64>,
    },
}

impl ProviderQuery {
    /// The exact shell command, such as
    /// `content query --uri content://sms/inbox --projection _id:date --sort '_id ASC'`.
    #[must_use]
    pub fn command(&self) -> String {
        match self {
            Self::After { provider, after_id } => match provider {
                Provider::Sms => query(
                    "content://sms/inbox",
                    SMS_COLUMNS,
                    Some(&format!("_id>{after_id}")),
                ),
                Provider::Mms => query(
                    "content://mms/inbox",
                    MMS_COLUMNS,
                    Some(&format!("{MMS_RETRIEVED} AND _id>{after_id}")),
                ),
            },
            Self::ByIds { provider, ids } => {
                let ids = id_list(ids);
                match provider {
                    Provider::Sms => query(
                        "content://sms/inbox",
                        SMS_COLUMNS,
                        Some(&format!("_id IN ({ids})")),
                    ),
                    Provider::Mms => query(
                        "content://mms/inbox",
                        MMS_COLUMNS,
                        Some(&format!("{MMS_RETRIEVED} AND _id IN ({ids})")),
                    ),
                }
            }
            Self::Keys { provider } => match provider {
                Provider::Sms => query("content://sms/inbox", INBOX_KEY_COLUMNS, None),
                Provider::Mms => query(
                    "content://mms/inbox",
                    INBOX_KEY_COLUMNS,
                    Some(MMS_RETRIEVED),
                ),
            },
            Self::MmsAddresses { mms_id } => format!(
                "content query --uri content://mms/{mms_id}/addr --projection {}",
                MMS_ADDR_COLUMNS.join(":")
            ),
            Self::MmsParts { mms_ids } => query(
                "content://mms/part",
                MMS_PART_COLUMNS,
                Some(&format!("mid IN ({})", id_list(mms_ids))),
            ),
        }
    }

    /// The columns of the rows the command prints, for
    /// [`check_content_output`](crate::check_content_output).
    #[must_use]
    pub fn columns(&self) -> &'static [&'static str] {
        match self {
            Self::After { provider, .. } | Self::ByIds { provider, .. } => match provider {
                Provider::Sms => SMS_COLUMNS,
                Provider::Mms => MMS_COLUMNS,
            },
            Self::Keys { .. } => INBOX_KEY_COLUMNS,
            Self::MmsAddresses { .. } => MMS_ADDR_COLUMNS,
            Self::MmsParts { .. } => MMS_PART_COLUMNS,
        }
    }
}

/// The command that reads one MMS part's bytes. Run it over the shell protocol, which keeps the
/// binary stdout apart from stderr and the exit status; `exec:` would mix an error message into
/// the bytes.
#[must_use]
pub fn read_part_command(part_id: i64) -> String {
    format!("content read --uri content://mms/part/{part_id}")
}

fn query(uri: &str, columns: &[&str], filter: Option<&str>) -> String {
    let projection: Vec<String> = columns
        .iter()
        .map(|column| {
            if column.contains('(') {
                format!("'{column}'")
            } else {
                (*column).to_owned()
            }
        })
        .collect();
    let mut parts = vec![format!(
        "content query --uri {uri} --projection {}",
        projection.join(":")
    )];
    if let Some(filter) = filter.filter(|filter| !filter.is_empty()) {
        parts.push(format!("--where '{filter}'"));
    }
    parts.push("--sort '_id ASC'".to_owned());
    parts.join(" ")
}

fn id_list(ids: &[i64]) -> String {
    let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
    ids.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMS: &str = "_id:thread_id:address:date:date_sent:sub_id:read:type:'hex(body)'";
    const MMS: &str = "_id:date:date_sent:sub_id:m_type:'hex(sub)'";
    const PART: &str = "_id:mid:seq:ct:'hex(name)':'hex(fn)':'hex(cl)':'hex(text)'";

    #[test]
    fn the_commands_are_the_ones_spareline_old_sent() {
        let command = |query: ProviderQuery| query.command();
        assert_eq!(
            command(ProviderQuery::After {
                provider: Provider::Sms,
                after_id: 7
            }),
            format!(
                "content query --uri content://sms/inbox --projection {SMS} --where '_id>7' --sort '_id ASC'"
            )
        );
        assert_eq!(
            command(ProviderQuery::ByIds {
                provider: Provider::Sms,
                ids: vec![4, 5]
            }),
            format!(
                "content query --uri content://sms/inbox --projection {SMS} --where '_id IN (4,5)' --sort '_id ASC'"
            )
        );
        assert_eq!(
            command(ProviderQuery::Keys {
                provider: Provider::Sms
            }),
            "content query --uri content://sms/inbox --projection _id:date --sort '_id ASC'"
        );
        assert_eq!(
            command(ProviderQuery::After {
                provider: Provider::Mms,
                after_id: 0
            }),
            format!(
                "content query --uri content://mms/inbox --projection {MMS} --where 'm_type=132 AND _id>0' --sort '_id ASC'"
            )
        );
        assert_eq!(
            command(ProviderQuery::ByIds {
                provider: Provider::Mms,
                ids: vec![9]
            }),
            format!(
                "content query --uri content://mms/inbox --projection {MMS} --where 'm_type=132 AND _id IN (9)' --sort '_id ASC'"
            )
        );
        assert_eq!(
            command(ProviderQuery::Keys {
                provider: Provider::Mms
            }),
            "content query --uri content://mms/inbox --projection _id:date --where 'm_type=132' --sort '_id ASC'"
        );
        assert_eq!(
            command(ProviderQuery::MmsAddresses { mms_id: 3 }),
            "content query --uri content://mms/3/addr --projection type:address"
        );
        assert_eq!(
            command(ProviderQuery::MmsParts {
                mms_ids: vec![3, 4]
            }),
            format!(
                "content query --uri content://mms/part --projection {PART} --where 'mid IN (3,4)' --sort '_id ASC'"
            )
        );
        assert_eq!(
            read_part_command(12),
            "content read --uri content://mms/part/12"
        );
    }

    #[test]
    fn an_empty_id_list_still_names_the_filter() {
        assert_eq!(
            ProviderQuery::ByIds {
                provider: Provider::Sms,
                ids: Vec::new()
            }
            .command(),
            format!(
                "content query --uri content://sms/inbox --projection {SMS} --where '_id IN ()' --sort '_id ASC'"
            )
        );
    }

    #[test]
    fn columns_follow_the_query() {
        assert_eq!(
            ProviderQuery::Keys {
                provider: Provider::Mms
            }
            .columns(),
            INBOX_KEY_COLUMNS
        );
        assert_eq!(
            ProviderQuery::After {
                provider: Provider::Mms,
                after_id: 1
            }
            .columns(),
            MMS_COLUMNS
        );
        assert_eq!(
            ProviderQuery::MmsAddresses { mms_id: 1 }.columns(),
            MMS_ADDR_COLUMNS
        );
        assert_eq!(Provider::Mms.as_str(), "mms");
        assert_eq!(Provider::Sms.as_str(), "sms");
    }
}
