//! Texts in SQLite: the single writer's inserts, attribution to SIMs and numbers, and the reads
//! behind the API, as spareline-old's `sms/store.ts` kept them.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Row};
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::js_text;
use crate::message::{
    AttachmentFile, AttachmentView, Attribution, Message, MessageKey, MessageNumber, MessagePage,
    MessageScope, NewMessage, Paging, Provider,
};

const SELECT_MESSAGES: &str = "SELECT m.seq, m.provider, m.provider_id, p.serial, m.sub_id, m.sim_id, m.number_id, n.e164, n.note,
    m.sender, m.body, m.subject, m.sent_at, m.received_at, m.ingested_at
    FROM messages m JOIN phones p ON p.id = m.phone_id LEFT JOIN numbers n ON n.id = m.number_id";

/// The order of "nearest" records to `$at`: one covering it first, then the earliest starting
/// after it, then the latest that ended before it. `sim_placements` and `number_bindings` start
/// when spareline first saw them, so a text older than that, from an inbox backfilled on first
/// sight, takes the earliest record after it. Every comparison is between ISO strings.
const NEAREST: &str = "CASE WHEN from_at <= $at AND (to_at IS NULL OR to_at > $at) THEN 0
    WHEN from_at > $at THEN 1 ELSE 2 END,
    CASE WHEN from_at > $at THEN from_at END ASC, to_at DESC";

/// Texts, their attachments' metadata and the ingest cursors.
#[derive(Debug)]
pub struct MessageStore<'db> {
    connection: &'db mut Connection,
}

impl<'db> MessageStore<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl MessageStore<'_> {
    /// Stores texts from one phone in one transaction, each attributed to its SIM and number,
    /// and returns the ones that were new, in seq order, for publishing. The unique key
    /// `(phone, provider, provider_id, received_at)` makes storing a text again a no-op.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does. A failure before the commit stores nothing; a failure reading the
    /// stored rows back after it means they are stored but not returned, so clients get them by
    /// paging with `after` rather than live.
    pub fn insert(
        &mut self,
        phone_id: i64,
        messages: &[NewMessage],
        ingested_at: Timestamp,
    ) -> Result<Vec<Message>, DatabaseError> {
        let ingested_at = ingested_at.to_string();
        let transaction = self.connection.transaction()?;
        let mut seqs = Vec::new();
        {
            let mut insert = transaction.prepare(
                "INSERT INTO messages (phone_id, provider, provider_id, sub_id, sim_id, number_id, sender, body, subject, sent_at, received_at, ingested_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (phone_id, provider, provider_id, received_at) DO NOTHING RETURNING seq",
            )?;
            let mut insert_attachment = transaction.prepare(
                "INSERT INTO attachments (message_seq, content_type, size, sha256, filename)
                VALUES (?, ?, ?, ?, ?)",
            )?;
            for message in messages {
                let received_at = message.received_at.to_string();
                let found = attribute(&transaction, phone_id, message.sub_id, &received_at)?;
                let seq: Option<i64> = insert
                    .query_row(
                        rusqlite::params![
                            phone_id,
                            message.provider,
                            message.provider_id,
                            message.sub_id,
                            found.sim_id,
                            found.number_id,
                            message.sender,
                            message.body,
                            message.subject,
                            message.sent_at.map(|sent| sent.to_string()),
                            received_at,
                            ingested_at,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(seq) = seq else {
                    continue;
                };
                for attachment in &message.attachments {
                    insert_attachment.execute((
                        seq,
                        &attachment.content_type,
                        attachment.size,
                        &attachment.sha256,
                        &attachment.filename,
                    ))?;
                }
                seqs.push(seq);
            }
        }
        transaction.commit()?;
        self.messages_at(&seqs)
    }

    /// The SIM and number a text from the phone on `sub_id`, received at `received_at`, belongs
    /// to. With a sub id, the phone's placement with that sub id nearest the time names the
    /// SIM; Android keeps one sub id per ICCID. Without one, only a phone with exactly one SIM
    /// at the time is unambiguous. The SIM's binding nearest the time names the number.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn attribute(
        &self,
        phone_id: i64,
        sub_id: Option<i64>,
        received_at: &str,
    ) -> Result<Attribution, DatabaseError> {
        attribute(self.connection, phone_id, sub_id, received_at)
    }

    /// Gives the texts stored without a number their SIM and number where those are known now.
    /// A text that gains a number moves to a new seq above every other, with its attachments,
    /// so a client polling that number with `after` sees it; its old seq stays a gap. A text
    /// that gains only its SIM keeps its seq. One transaction, with foreign keys checked at
    /// commit. Returns the moved texts, in order, for publishing again.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does. A failure before the commit moves nothing; a failure reading the
    /// moved rows back after it means they moved but are not returned.
    pub fn attribute_pending(&mut self) -> Result<Vec<Message>, DatabaseError> {
        let transaction = self.connection.transaction()?;
        let mut moved = Vec::new();
        {
            let mut pending = transaction.prepare(
                "SELECT seq, phone_id, sub_id, sim_id, received_at FROM messages WHERE number_id IS NULL ORDER BY seq",
            )?;
            let rows = pending
                .query_map([], |row| {
                    Ok(Pending {
                        seq: row.get(0)?,
                        phone_id: row.get(1)?,
                        sub_id: row.get(2)?,
                        sim_id: row.get(3)?,
                        received_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<Pending>, _>>()?;
            if rows.is_empty() {
                return Ok(Vec::new());
            }
            {
                let mut statement = transaction.prepare("PRAGMA defer_foreign_keys = ON")?;
                let mut answer = statement.query([])?;
                while answer.next()?.is_some() {}
            }
            for row in rows {
                let found = attribute(&transaction, row.phone_id, row.sub_id, &row.received_at)?;
                let sim_id = found.sim_id.or(row.sim_id);
                let Some(number_id) = found.number_id else {
                    if sim_id != row.sim_id {
                        transaction.execute(
                            "UPDATE messages SET sim_id = ? WHERE seq = ?",
                            (sim_id, row.seq),
                        )?;
                    }
                    continue;
                };
                let seq: i64 = transaction.query_row(
                    "SELECT max(coalesce((SELECT max(seq) FROM messages), 0), coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'messages'), 0)) + 1 AS seq",
                    [],
                    |answer| answer.get(0),
                )?;
                transaction.execute(
                    "UPDATE messages SET seq = ?, sim_id = ?, number_id = ? WHERE seq = ?",
                    (seq, sim_id, number_id, row.seq),
                )?;
                transaction.execute(
                    "UPDATE attachments SET message_seq = ? WHERE message_seq = ?",
                    (seq, row.seq),
                )?;
                transaction.execute(
                    "UPDATE sqlite_sequence SET seq = ? WHERE name = 'messages'",
                    [seq],
                )?;
                moved.push(seq);
            }
        }
        transaction.commit()?;
        self.messages_at(&moved)
    }

    /// One page of texts from `scope`. With [`Paging::Newest`] the cursor is read before the
    /// page, so a text committed in between is newer than the cursor and not lost by a client
    /// that polls from it.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn page(&self, scope: &MessageScope, paging: Paging) -> Result<MessagePage, DatabaseError> {
        match paging {
            Paging::Newest { before, limit } => {
                let cursor = self.max_seq()?;
                let messages = self.list(scope, Order::Descending, Bound::Before(before), limit)?;
                Ok(MessagePage { messages, cursor })
            }
            Paging::After { after, limit } => {
                let messages = self.list(scope, Order::Ascending, Bound::After(after), limit)?;
                let cursor = messages.last().map_or(after, |last| last.seq);
                Ok(MessagePage { messages, cursor })
            }
        }
    }

    /// Up to `limit` texts with seqs above `after`, oldest first: the event stream's replay.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn after(&self, after: i64, limit: i64) -> Result<Vec<Message>, DatabaseError> {
        self.list(
            &MessageScope::default(),
            Order::Ascending,
            Bound::After(after),
            limit,
        )
    }

    /// The text with `seq`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn get(&self, seq: i64) -> Result<Option<Message>, DatabaseError> {
        Ok(self.messages_at(&[seq])?.pop())
    }

    /// The highest seq of all texts, or 0.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn max_seq(&self) -> Result<i64, DatabaseError> {
        let seq: Option<i64> =
            self.connection
                .query_row("SELECT max(seq) AS seq FROM messages", [], |row| row.get(0))?;
        Ok(seq.unwrap_or(0))
    }

    /// The attachment with the id, for serving its file.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn attachment(&self, id: i64) -> Result<Option<AttachmentFile>, DatabaseError> {
        let file = self
            .connection
            .query_row(
                "SELECT sha256, content_type, filename FROM attachments WHERE id = ?",
                [id],
                |row| {
                    Ok(AttachmentFile {
                        sha256: row.get(0)?,
                        content_type: row.get(1)?,
                        filename: row.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(file)
    }

    /// The key of every stored text of the phone from `provider`, which ingest compares the
    /// provider's rows against.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn stored_keys(
        &self,
        phone_id: i64,
        provider: Provider,
    ) -> Result<HashSet<MessageKey>, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT provider_id, received_at FROM messages WHERE phone_id = ? AND provider = ?",
        )?;
        let keys = statement
            .query_map((phone_id, provider), |row| {
                Ok(MessageKey {
                    provider_id: row.get(0)?,
                    received_at: row.get(1)?,
                })
            })?
            .collect::<Result<HashSet<MessageKey>, _>>()?;
        Ok(keys)
    }

    /// The highest provider `_id` ingested from the phone's `provider`, or 0.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn ingest_cursor(&self, phone_id: i64, provider: Provider) -> Result<i64, DatabaseError> {
        let cursor: Option<i64> = self
            .connection
            .query_row(
                "SELECT last_provider_id FROM ingest_cursors WHERE phone_id = ? AND provider = ?",
                (phone_id, provider),
                |row| row.get(0),
            )
            .optional()?;
        Ok(cursor.unwrap_or(0))
    }

    /// Sets the highest provider `_id` ingested from the phone's `provider`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn set_ingest_cursor(
        &mut self,
        phone_id: i64,
        provider: Provider,
        last_provider_id: i64,
    ) -> Result<(), DatabaseError> {
        self.connection.execute(
            "INSERT INTO ingest_cursors (phone_id, provider, last_provider_id) VALUES (?, ?, ?)
            ON CONFLICT (phone_id, provider) DO UPDATE SET last_provider_id = excluded.last_provider_id",
            (phone_id, provider, last_provider_id),
        )?;
        Ok(())
    }

    /// The texts from `scope` past `bound`, in `order`, at most `limit`.
    fn list(
        &self,
        scope: &MessageScope,
        order: Order,
        bound: Bound,
        limit: i64,
    ) -> Result<Vec<Message>, DatabaseError> {
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(number_id) = scope.number_id {
            conditions.push("m.number_id = ?".to_owned());
            values.push(Value::Integer(number_id));
        }
        let search = scope.search.as_deref().map_or("", js_text::trim);
        if !search.is_empty() {
            let mut terms: Vec<(&str, String)> =
                ["m.body", "m.sender", "m.subject", "n.e164", "n.note"]
                    .into_iter()
                    .map(|field| (field, like(search)))
                    .collect();
            let digits: String = search.chars().filter(char::is_ascii_digit).collect();
            if is_phone_number_like(search) && !digits.is_empty() {
                terms.push(("n.e164", like(&digits)));
                terms.push(("m.sender", like(&digits)));
            }
            let clauses: Vec<String> = terms
                .iter()
                .map(|(field, _)| format!("{field} LIKE ? ESCAPE '\\'"))
                .collect();
            conditions.push(format!("({})", clauses.join(" OR ")));
            values.extend(terms.into_iter().map(|(_, pattern)| Value::Text(pattern)));
        }
        match bound {
            Bound::After(after) => {
                conditions.push("m.seq > ?".to_owned());
                values.push(Value::Integer(after));
            }
            Bound::Before(Some(before)) => {
                conditions.push("m.seq < ?".to_owned());
                values.push(Value::Integer(before));
            }
            Bound::Before(None) => {}
        }
        let filter = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let direction = match order {
            Order::Ascending => "ASC",
            Order::Descending => "DESC",
        };
        values.push(Value::Integer(limit));
        let mut statement = self.connection.prepare(&format!(
            "{SELECT_MESSAGES}{filter}\n    ORDER BY m.seq {direction} LIMIT ?"
        ))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), stored_message_from)?
            .collect::<Result<Vec<StoredMessage>, _>>()?;
        self.views(rows)
    }

    /// The texts with `seqs` that exist, in that order.
    fn messages_at(&self, seqs: &[i64]) -> Result<Vec<Message>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_MESSAGES} WHERE m.seq = ?"))?;
        let mut rows = Vec::with_capacity(seqs.len());
        for seq in seqs {
            rows.extend(statement.query_row([seq], stored_message_from).optional()?);
        }
        self.views(rows)
    }

    /// The rows with their attachments, ordered by id.
    fn views(&self, rows: Vec<StoredMessage>) -> Result<Vec<Message>, DatabaseError> {
        let mut attachments: HashMap<i64, Vec<AttachmentView>> = HashMap::new();
        if !rows.is_empty() {
            let placeholders = vec!["?"; rows.len()].join(",");
            let mut statement = self.connection.prepare(&format!(
                "SELECT id, message_seq, content_type, size, filename FROM attachments
                WHERE message_seq IN ({placeholders}) ORDER BY id"
            ))?;
            let found = statement
                .query_map(
                    rusqlite::params_from_iter(rows.iter().map(|row| row.seq)),
                    |row| {
                        Ok(StoredAttachment {
                            message_seq: row.get(1)?,
                            view: AttachmentView {
                                id: row.get(0)?,
                                content_type: row.get(2)?,
                                size: row.get(3)?,
                                filename: row.get(4)?,
                                url: format!("/api/v1/attachments/{}", row.get::<_, i64>(0)?),
                            },
                        })
                    },
                )?
                .collect::<Result<Vec<StoredAttachment>, _>>()?;
            for attachment in found {
                attachments
                    .entry(attachment.message_seq)
                    .or_default()
                    .push(attachment.view);
            }
        }
        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            let row_attachments = attachments.remove(&row.seq).unwrap_or_default();
            messages.push(Message {
                seq: row.seq,
                provider: row.provider,
                provider_id: row.provider_id,
                phone: row.serial,
                sub_id: row.sub_id,
                sim_id: row.sim_id,
                number: row.number_id.map(|id| MessageNumber {
                    id,
                    e164: row.e164,
                    note: row.note.unwrap_or_default(),
                }),
                from: row.sender,
                body: row.body,
                subject: row.subject,
                sent_at: row.sent_at,
                received_at: row.received_at,
                ingested_at: row.ingested_at,
                attachments: row_attachments,
            });
        }
        Ok(messages)
    }
}

/// [`MessageStore::attribute`], on any connection, a transaction included.
fn attribute(
    connection: &Connection,
    phone_id: i64,
    sub_id: Option<i64>,
    at: &str,
) -> Result<Attribution, DatabaseError> {
    let sim_id: Option<i64> = match sub_id {
        Some(sub) if sub >= 0 => connection
            .query_row(
                &format!(
                    "SELECT sim_id FROM sim_placements WHERE phone_id = $phone AND sub_id = $sub
                    ORDER BY {NEAREST} LIMIT 1"
                ),
                rusqlite::named_params! { "$phone": phone_id, "$sub": sub, "$at": at },
                |row| row.get(0),
            )
            .optional()?,
        _ => {
            let mut statement = connection.prepare(
                "SELECT sim_id FROM sim_placements WHERE phone_id = $phone
                AND from_at <= $at AND (to_at IS NULL OR to_at > $at)",
            )?;
            let covering = statement
                .query_map(
                    rusqlite::named_params! { "$phone": phone_id, "$at": at },
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<i64>, _>>()?;
            match covering.as_slice() {
                [only] => Some(*only),
                _ => None,
            }
        }
    };
    let Some(sim_id) = sim_id else {
        return Ok(Attribution {
            sim_id: None,
            number_id: None,
        });
    };
    let number_id = connection
        .query_row(
            &format!(
                "SELECT number_id FROM number_bindings WHERE sim_id = $sim
                ORDER BY {NEAREST} LIMIT 1"
            ),
            rusqlite::named_params! { "$sim": sim_id, "$at": at },
            |row| row.get(0),
        )
        .optional()?;
    Ok(Attribution {
        sim_id: Some(sim_id),
        number_id,
    })
}

/// `%text%` for `LIKE .. ESCAPE '\'`, with `\`, `%` and `_` in `text` taken literally.
fn like(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for character in text.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// Whether `text` matches `^[\d\s()+.-]+$` as JavaScript reads it, such as `(512) 555-0101`.
fn is_phone_number_like(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_digit()
                || js_text::is_whitespace(character)
                || matches!(character, '(' | ')' | '+' | '.' | '-')
        })
}

#[derive(Clone, Copy)]
enum Order {
    Ascending,
    Descending,
}

#[derive(Clone, Copy)]
enum Bound {
    After(i64),
    Before(Option<i64>),
}

/// A text without its number's attribution checked.
struct Pending {
    seq: i64,
    phone_id: i64,
    sub_id: Option<i64>,
    sim_id: Option<i64>,
    received_at: String,
}

/// A row of [`SELECT_MESSAGES`].
struct StoredMessage {
    seq: i64,
    provider: Provider,
    provider_id: i64,
    serial: String,
    sub_id: Option<i64>,
    sim_id: Option<i64>,
    number_id: Option<i64>,
    e164: Option<String>,
    note: Option<String>,
    sender: Option<String>,
    body: String,
    subject: Option<String>,
    sent_at: Option<String>,
    received_at: String,
    ingested_at: String,
}

fn stored_message_from(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        seq: row.get(0)?,
        provider: row.get(1)?,
        provider_id: row.get(2)?,
        serial: row.get(3)?,
        sub_id: row.get(4)?,
        sim_id: row.get(5)?,
        number_id: row.get(6)?,
        e164: row.get(7)?,
        note: row.get(8)?,
        sender: row.get(9)?,
        body: row.get(10)?,
        subject: row.get(11)?,
        sent_at: row.get(12)?,
        received_at: row.get(13)?,
        ingested_at: row.get(14)?,
    })
}

struct StoredAttachment {
    message_seq: i64,
    view: AttachmentView,
}

#[cfg(test)]
mod tests {
    use rusqlite::Params;

    use super::*;
    use crate::database::Database;
    use crate::message::NewAttachment;
    use crate::test_clock;

    /// Noon on a day of October 2026, or another hour.
    fn day(day: i64, hour: i64) -> Timestamp {
        test_clock::at(&format!("2026-10-{day:02}T{hour:02}:00:00.000Z"))
    }

    fn t(of: i64) -> Timestamp {
        day(of, 12)
    }

    fn iso(of: i64) -> String {
        t(of).to_string()
    }

    struct Fixture {
        database: Database,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                database: Database::open_in_memory(test_clock::fixed).unwrap(),
            }
        }

        fn id(&self, sql: &str, values: impl Params) -> i64 {
            self.database
                .connection
                .query_row(sql, values, |row| row.get(0))
                .unwrap()
        }

        fn phone(&self, serial: &str) -> i64 {
            self.id(
                "INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES (?, 'home', 'device', ?) RETURNING id",
                (serial, iso(1)),
            )
        }

        fn sim(&self, iccid: &str) -> i64 {
            self.id(
                "INSERT INTO sims (iccid, first_seen_at, last_seen_at) VALUES (?, ?2, ?2) RETURNING id",
                (iccid, iso(1)),
            )
        }

        fn number(&self, e164: Option<&str>) -> i64 {
            self.id(
                "INSERT INTO numbers (e164, e164_source, created_at) VALUES (?, ?, ?) RETURNING id",
                (
                    e164,
                    if e164.is_some() { "detected" } else { "none" },
                    iso(1),
                ),
            )
        }

        fn place(&self, sim: i64, phone: i64, slot: i64, sub_id: i64, from: i64, to: Option<i64>) {
            self.database
                .connection
                .execute(
                    "INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at, to_at) VALUES (?, ?, ?, ?, ?, ?)",
                    (sim, phone, slot, sub_id, iso(from), to.map(iso)),
                )
                .unwrap();
        }

        fn bind(&self, number: i64, sim: i64, from: i64, to: Option<i64>) {
            self.database
                .connection
                .execute(
                    "INSERT INTO number_bindings (number_id, sim_id, from_at, to_at) VALUES (?, ?, ?, ?)",
                    (number, sim, iso(from), to.map(iso)),
                )
                .unwrap();
        }

        fn insert(&mut self, phone: i64, messages: &[NewMessage], ingested: i64) -> Vec<i64> {
            self.database
                .messages()
                .insert(phone, messages, t(ingested))
                .unwrap()
                .iter()
                .map(|message| message.seq)
                .collect()
        }

        fn pending(&mut self) -> Vec<i64> {
            self.database
                .messages()
                .attribute_pending()
                .unwrap()
                .iter()
                .map(|message| message.seq)
                .collect()
        }

        fn seqs(&mut self, scope: &MessageScope, paging: Paging) -> Vec<i64> {
            self.database
                .messages()
                .page(scope, paging)
                .unwrap()
                .messages
                .iter()
                .map(|message| message.seq)
                .collect()
        }

        fn attribute(&mut self, phone: i64, sub_id: Option<i64>, at: Timestamp) -> Attribution {
            self.database
                .messages()
                .attribute(phone, sub_id, &at.to_string())
                .unwrap()
        }
    }

    fn text(provider_id: i64, received_at: Timestamp, sub_id: Option<i64>) -> NewMessage {
        NewMessage {
            provider: Provider::Sms,
            provider_id,
            sub_id,
            sender: Some("+15125550150".to_owned()),
            body: format!("Fake text {provider_id}"),
            subject: None,
            sent_at: None,
            received_at,
            attachments: Vec::new(),
        }
    }

    fn found(sim_id: i64, number_id: i64) -> Attribution {
        Attribution {
            sim_id: Some(sim_id),
            number_id: Some(number_id),
        }
    }

    const NOTHING: Attribution = Attribution {
        sim_id: None,
        number_id: None,
    };

    const ALL: Paging = Paging::After {
        after: 0,
        limit: 10,
    };

    #[test]
    fn storing_a_text_again_adds_nothing_and_a_reset_providers_reused_ids_are_new() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        assert_eq!(
            fixture.insert(a, &[text(1, t(2), Some(1)), text(2, t(3), Some(1))], 5),
            [1, 2]
        );
        assert_eq!(
            fixture.insert(a, &[text(1, t(2), Some(1)), text(2, t(3), Some(1))], 6),
            Vec::<i64>::new()
        );
        let reset = fixture.insert(a, &[text(1, t(4), Some(1))], 6);
        assert!(reset[0] > 2, "{reset:?}");
        let b = fixture.phone("B");
        let other = fixture.insert(b, &[text(1, t(2), Some(1))], 6);
        assert!(other[0] > reset[0]);
        assert_eq!(fixture.database.messages().max_seq().unwrap(), other[0]);
        assert_eq!(fixture.seqs(&MessageScope::default(), ALL).len(), 4);
        let keys = fixture
            .database
            .messages()
            .stored_keys(a, Provider::Sms)
            .unwrap();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&MessageKey {
            provider_id: 1,
            received_at: iso(4)
        }));
        assert!(
            fixture
                .database
                .messages()
                .stored_keys(a, Provider::Mms)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn attribution_follows_a_sim_across_phones_and_slots() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let b = fixture.phone("B");
        let s1 = fixture.sim("8901555000000000011");
        let s2 = fixture.sim("8901555000000000029");
        let n1 = fixture.number(Some("+15125550101"));
        let n2 = fixture.number(Some("+15125550102"));
        fixture.place(s1, a, 0, 1, 10, Some(20));
        fixture.place(s1, b, 1, 1, 20, None);
        fixture.bind(n1, s1, 10, None);
        fixture.place(s2, a, 0, 2, 21, None);
        fixture.bind(n2, s2, 21, None);
        assert_eq!(fixture.attribute(a, Some(1), t(15)), found(s1, n1));
        assert_eq!(fixture.attribute(b, Some(1), t(25)), found(s1, n1));
        assert_eq!(fixture.attribute(a, Some(2), t(25)), found(s2, n2));
        assert_eq!(
            fixture.attribute(a, Some(1), t(3)),
            found(s1, n1),
            "an inbox backfill"
        );
        assert_eq!(
            fixture.attribute(a, Some(1), t(22)),
            found(s1, n1),
            "same sub id after it left"
        );
        assert_eq!(fixture.attribute(a, None, t(25)), found(s2, n2));
        assert_eq!(
            fixture.attribute(a, Some(-1), t(25)),
            found(s2, n2),
            "a negative sub id is none"
        );
        assert_eq!(
            fixture.attribute(a, None, day(21, 0)),
            NOTHING,
            "two SIMs could have it"
        );
        assert_eq!(fixture.attribute(a, Some(7), t(25)), NOTHING);
    }

    #[test]
    fn a_number_change_on_the_same_sim_splits_its_texts_at_the_change() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let s = fixture.sim("8901555000000000011");
        let before = fixture.number(Some("+15125550101"));
        let after = fixture.number(Some("+15125550109"));
        fixture.place(s, a, 0, 1, 10, None);
        fixture.bind(before, s, 10, Some(15));
        fixture.bind(after, s, 15, None);
        assert_eq!(fixture.attribute(a, Some(1), t(12)), found(s, before));
        assert_eq!(fixture.attribute(a, Some(1), t(16)), found(s, after));
    }

    #[test]
    fn texts_from_an_unknown_sim_wait_then_get_their_number_once() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        fixture.insert(a, &[text(1, t(2), Some(1)), text(2, t(3), Some(1))], 5);
        let page = fixture
            .database
            .messages()
            .page(&MessageScope::default(), ALL)
            .unwrap();
        assert!(page.messages.iter().all(|message| message.number.is_none()));
        assert_eq!(fixture.pending(), Vec::<i64>::new());
        let s = fixture.sim("8901555000000000011");
        let n = fixture.number(None);
        fixture.place(s, a, 0, 1, 6, None);
        fixture.bind(n, s, 6, None);
        assert_eq!(fixture.pending(), [3, 4]);
        assert_eq!(fixture.pending(), Vec::<i64>::new(), "idempotent");
        let page = fixture
            .database
            .messages()
            .page(&MessageScope::default(), ALL)
            .unwrap();
        let attributed: Vec<(i64, Option<i64>, Option<MessageNumber>)> = page
            .messages
            .into_iter()
            .map(|message| (message.seq, message.sim_id, message.number))
            .collect();
        let number = MessageNumber {
            id: n,
            e164: None,
            note: String::new(),
        };
        assert_eq!(
            attributed,
            [
                (3, Some(s), Some(number.clone())),
                (4, Some(s), Some(number))
            ]
        );
        fixture
            .database
            .connection
            .execute(
                "UPDATE numbers SET e164 = '+15125550101', e164_source = 'detected'",
                [],
            )
            .unwrap();
        assert_eq!(
            fixture.database.messages().get(3).unwrap().unwrap().number,
            Some(MessageNumber {
                id: n,
                e164: Some("+15125550101".to_owned()),
                note: String::new(),
            })
        );
    }

    #[test]
    fn a_text_that_gains_only_its_sim_keeps_its_seq() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        fixture.insert(a, &[text(1, t(2), Some(1))], 5);
        let s = fixture.sim("8901555000000000011");
        fixture.place(s, a, 0, 1, 6, None);
        assert_eq!(fixture.pending(), Vec::<i64>::new());
        let message = fixture.database.messages().get(1).unwrap().unwrap();
        assert_eq!((message.sim_id, message.number), (Some(s), None));
    }

    #[test]
    fn a_text_that_gains_its_number_moves_above_every_seq_with_its_attachments() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let s1 = fixture.sim("8901555000000000011");
        let n1 = fixture.number(Some("+15125550101"));
        fixture.place(s1, a, 0, 1, 1, None);
        fixture.bind(n1, s1, 1, None);
        let picture = NewMessage {
            provider: Provider::Mms,
            attachments: vec![NewAttachment {
                content_type: "image/png".to_owned(),
                size: 3,
                sha256: "a".repeat(64),
                filename: None,
            }],
            ..text(1, t(2), Some(2))
        };
        fixture.insert(a, &[picture, text(2, t(3), Some(1))], 5);
        let n2 = fixture.number(Some("+15125550102"));
        let s2 = fixture.sim("8901555000000000029");
        fixture.place(s2, a, 1, 2, 6, None);
        fixture.bind(n2, s2, 6, None);
        assert_eq!(fixture.pending(), [3]);
        let page = fixture
            .database
            .messages()
            .page(
                &MessageScope {
                    number_id: Some(n2),
                    search: None,
                },
                Paging::After {
                    after: 2,
                    limit: 10,
                },
            )
            .unwrap();
        assert_eq!(page.cursor, 3);
        assert_eq!(page.messages.len(), 1);
        let moved = &page.messages[0];
        assert_eq!((moved.seq, moved.provider), (3, Provider::Mms));
        assert_eq!(moved.attachments.len(), 1);
        assert_eq!(moved.attachments[0].content_type, "image/png");
        assert_eq!(
            moved.attachments[0].url,
            format!("/api/v1/attachments/{}", moved.attachments[0].id)
        );
        assert_eq!(fixture.database.messages().get(1).unwrap(), None);
        let violations: i64 = fixture
            .database
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        let sequence: i64 = fixture
            .database
            .connection
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'messages'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sequence, 3);
        assert_eq!(
            fixture.insert(a, &[text(3, t(7), Some(1))], 8),
            [4],
            "the next seq is above the moved one"
        );
        let file = fixture
            .database
            .messages()
            .attachment(moved.attachments[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(file.sha256, "a".repeat(64));
        assert_eq!(fixture.database.messages().attachment(99).unwrap(), None);
    }

    /// Two numbers on two SIMs of phone A, with three texts: seq 1 to the first number, seqs 2
    /// and 3 to the second.
    struct TwoNumbers {
        fixture: Fixture,
        first: i64,
        second: i64,
    }

    fn two_numbers() -> TwoNumbers {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let s1 = fixture.sim("8901555000000000011");
        let s2 = fixture.sim("8901555000000000029");
        let first = fixture.number(Some("+15125550101"));
        let second = fixture.number(Some("+15125550102"));
        fixture.place(s1, a, 0, 1, 1, None);
        fixture.place(s2, a, 1, 2, 1, None);
        fixture.bind(first, s1, 1, None);
        fixture.bind(second, s2, 1, None);
        let body = |body: &str, message: NewMessage| NewMessage {
            body: body.to_owned(),
            ..message
        };
        fixture.insert(
            a,
            &[
                body("100% interested", text(1, t(2), Some(1))),
                body("Is it still for sale?", text(2, t(3), Some(2))),
                body("under_score", text(3, t(4), Some(2))),
            ],
            5,
        );
        TwoNumbers {
            fixture,
            first,
            second,
        }
    }

    fn search(fixture: &mut Fixture, text: &str) -> Vec<i64> {
        let scope = MessageScope {
            number_id: None,
            search: Some(text.to_owned()),
        };
        fixture.seqs(&scope, ALL)
    }

    #[test]
    fn pages_filter_by_number_both_ways() {
        let TwoNumbers {
            mut fixture,
            first,
            second,
        } = two_numbers();
        let everything = MessageScope::default();
        let number = |id| MessageScope {
            number_id: Some(id),
            search: None,
        };
        let newest = Paging::Newest {
            before: None,
            limit: 10,
        };
        assert_eq!(fixture.seqs(&everything, newest), [3, 2, 1]);
        assert_eq!(fixture.seqs(&number(first), newest), [1]);
        assert_eq!(fixture.seqs(&number(second), ALL), [2, 3]);
        let before = Paging::Newest {
            before: Some(3),
            limit: 1,
        };
        assert_eq!(fixture.seqs(&everything, before), [2]);
        let after = Paging::After {
            after: 1,
            limit: 10,
        };
        assert_eq!(fixture.seqs(&everything, after), [2, 3]);
        let page = fixture
            .database
            .messages()
            .page(&number(first), newest)
            .unwrap();
        assert_eq!(
            page.cursor, 3,
            "the cursor of a newest page is the highest seq of all"
        );
        let past = Paging::After {
            after: 3,
            limit: 10,
        };
        let empty = fixture.database.messages().page(&everything, past).unwrap();
        assert_eq!((empty.messages.len(), empty.cursor), (0, 3));
        let replay: Vec<i64> = fixture
            .database
            .messages()
            .after(1, 1)
            .unwrap()
            .iter()
            .map(|message| message.seq)
            .collect();
        assert_eq!(replay, [2]);
    }

    #[test]
    fn searches_match_text_senders_and_numbers_literally() {
        let TwoNumbers { mut fixture, .. } = two_numbers();
        assert_eq!(search(&mut fixture, "SALE"), [2]);
        assert_eq!(search(&mut fixture, "%"), [1], "LIKE wildcards are literal");
        assert_eq!(search(&mut fixture, "_"), [3]);
        assert_eq!(search(&mut fixture, "5550150"), [1, 2, 3]);
        assert_eq!(search(&mut fixture, "5550101"), [1]);
        assert_eq!(search(&mut fixture, "(512) 555-0102"), [2, 3]);
        assert_eq!(search(&mut fixture, "+1 512 555 0101"), [1]);
        assert_eq!(search(&mut fixture, "512.555.0150"), [1, 2, 3]);
        assert_eq!(
            search(&mut fixture, "  sale\u{a0}"),
            [2],
            "the search is trimmed as JavaScript trims"
        );
        assert_eq!(
            search(&mut fixture, "   "),
            [1, 2, 3],
            "an empty search is none"
        );
        assert_eq!(search(&mut fixture, "\u{c9}"), Vec::<i64>::new());
    }

    #[test]
    fn searches_match_a_numbers_note() {
        let TwoNumbers {
            mut fixture,
            second,
            ..
        } = two_numbers();
        fixture
            .database
            .connection
            .execute(
                "UPDATE numbers SET note = 'Facebook ad, blue couch' WHERE id = ?",
                [second],
            )
            .unwrap();
        assert_eq!(search(&mut fixture, "facebook AD"), [2, 3]);
        assert_eq!(search(&mut fixture, "ad 0101"), Vec::<i64>::new());
        let message = fixture.database.messages().get(2).unwrap().unwrap();
        assert_eq!(message.number.unwrap().note, "Facebook ad, blue couch");
    }

    #[test]
    fn a_message_serves_the_api_json() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let s = fixture.sim("8901555000000000011");
        let n = fixture.number(Some("+15125550101"));
        fixture.place(s, a, 0, 1, 1, None);
        fixture.bind(n, s, 1, None);
        let message = NewMessage {
            provider: Provider::Mms,
            subject: Some("Hi \"there\"".to_owned()),
            sent_at: Some(t(2)),
            attachments: vec![NewAttachment {
                content_type: "image/jpeg".to_owned(),
                size: 10,
                sha256: "b".repeat(64),
                filename: Some("cat.jpg".to_owned()),
            }],
            ..text(7, t(2), Some(1))
        };
        fixture.insert(a, &[message, text(8, t(3), None)], 5);
        let page = fixture
            .database
            .messages()
            .page(&MessageScope::default(), ALL)
            .unwrap();
        assert_eq!(
            serde_json::to_string(&page).unwrap(),
            r#"{"messages":[{"seq":1,"provider":"mms","providerId":7,"phone":"A","subId":1,"simId":1,"number":{"id":1,"e164":"+15125550101","note":""},"from":"+15125550150","body":"Fake text 7","subject":"Hi \"there\"","sentAt":"2026-10-02T12:00:00.000Z","receivedAt":"2026-10-02T12:00:00.000Z","ingestedAt":"2026-10-05T12:00:00.000Z","attachments":[{"id":1,"contentType":"image/jpeg","size":10,"filename":"cat.jpg","url":"/api/v1/attachments/1"}]},{"seq":2,"provider":"sms","providerId":8,"phone":"A","subId":null,"simId":1,"number":{"id":1,"e164":"+15125550101","note":""},"from":"+15125550150","body":"Fake text 8","subject":null,"sentAt":null,"receivedAt":"2026-10-03T12:00:00.000Z","ingestedAt":"2026-10-05T12:00:00.000Z","attachments":[]}],"cursor":2}"#
        );
    }

    #[test]
    fn ingest_cursors_start_at_zero_and_follow_the_last_set() {
        let mut fixture = Fixture::new();
        let a = fixture.phone("A");
        let mut store = fixture.database.messages();
        assert_eq!(store.ingest_cursor(a, Provider::Sms).unwrap(), 0);
        store.set_ingest_cursor(a, Provider::Sms, 41).unwrap();
        store.set_ingest_cursor(a, Provider::Sms, 42).unwrap();
        store.set_ingest_cursor(a, Provider::Mms, 7).unwrap();
        assert_eq!(store.ingest_cursor(a, Provider::Sms).unwrap(), 42);
        assert_eq!(store.ingest_cursor(a, Provider::Mms).unwrap(), 7);
    }
}
