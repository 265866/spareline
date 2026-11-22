//! The in-process channels that tell open streams what changed.

use spareline_db::{DeviceEvent, Message};
use tokio::sync::broadcast::{self, Receiver, Sender};

/// How many device events or texts a slow subscriber may fall behind before it misses some.
/// Every one of them is in the database too, so a subscriber that lags reads what it missed from
/// there.
const EVENT_CAPACITY: usize = 1024;

/// How many auth changes a subscriber may fall behind. Each one only says "check again", so
/// missing some loses nothing once the subscriber checks.
const AUTH_CAPACITY: usize = 64;

/// Something that can end a session or an API key happened: a sign-out, a password change, a
/// revoked key, a removed user, or expired sessions pruned. Open streams and sockets check that
/// their principal is still valid when they see one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthChange;

/// The service's broadcast channels: device events after their transaction commits, texts after
/// theirs (new ones, and ones attributed to a number at a new seq), and auth changes.
///
/// Cloning is cheap; every clone publishes to and subscribes from the same channels. A
/// subscription sees what is published after it is taken.
#[derive(Clone, Debug)]
pub struct Buses {
    device: Sender<DeviceEvent>,
    text: Sender<Message>,
    auth: Sender<AuthChange>,
}

impl Buses {
    /// New channels with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            device: broadcast::channel(EVENT_CAPACITY).0,
            text: broadcast::channel(EVENT_CAPACITY).0,
            auth: broadcast::channel(AUTH_CAPACITY).0,
        }
    }

    /// Publishes device events, in order, once their transaction has committed. Call it inside
    /// the [`DbHandle::call`](crate::DbHandle::call) that committed them, while it still holds
    /// the database, so the events of concurrent writers reach the channel in id order: a stream
    /// drops an id at or below its cursor. Nobody listening isn't a failure: the events are in
    /// the database.
    pub fn publish_device_events(&self, events: &[DeviceEvent]) {
        for event in events {
            send(&self.device, event.clone());
        }
    }

    /// Publishes texts, in seq order, once their transaction has committed: new ones, and ones
    /// moved to a new seq when they were attributed to a number. Call it inside the
    /// [`DbHandle::call`](crate::DbHandle::call) that committed them, as
    /// [`Buses::publish_device_events`] says.
    pub fn publish_texts(&self, texts: &[Message]) {
        for text in texts {
            send(&self.text, text.clone());
        }
    }

    /// Tells open streams and sockets to check their principal again.
    pub fn publish_auth_change(&self) {
        send(&self.auth, AuthChange);
    }

    /// Device events published from now on.
    #[must_use]
    pub fn device_events(&self) -> Receiver<DeviceEvent> {
        self.device.subscribe()
    }

    /// Texts published from now on.
    #[must_use]
    pub fn texts(&self) -> Receiver<Message> {
        self.text.subscribe()
    }

    /// Auth changes published from now on.
    #[must_use]
    pub fn auth_changes(&self) -> Receiver<AuthChange> {
        self.auth.subscribe()
    }
}

impl Default for Buses {
    fn default() -> Self {
        Self::new()
    }
}

/// Sends `value` to every current subscriber of `sender`. With none, the value is dropped, which
/// is what a broadcast to nobody means.
fn send<T>(sender: &Sender<T>, value: T) {
    match sender.send(value) {
        Ok(_subscribers) => {}
        Err(_nobody_listening) => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::value::RawValue;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    fn event(id: i64) -> DeviceEvent {
        DeviceEvent {
            id,
            phone_id: None,
            serial: None,
            at: "2026-11-22T07:32:46.343Z".to_owned(),
            kind: "adb.reachable".to_owned(),
            detail: RawValue::from_string(r#"{"error":null}"#.to_owned()).unwrap(),
        }
    }

    #[test]
    fn subscribers_see_what_is_published_after_they_subscribe_in_order() {
        let buses = Buses::new();
        buses.publish_device_events(&[event(1)]);
        let mut events = buses.device_events();
        let mut auth = buses.auth_changes();
        let clone = buses.clone();
        clone.publish_device_events(&[event(2), event(3)]);
        clone.publish_auth_change();
        assert_eq!(events.try_recv().unwrap().id, 2);
        assert_eq!(events.try_recv().unwrap().id, 3);
        assert_eq!(events.try_recv().unwrap_err(), TryRecvError::Empty);
        assert_eq!(auth.try_recv().unwrap(), AuthChange);
    }

    #[test]
    fn publishing_to_nobody_is_fine() {
        let buses = Buses::new();
        buses.publish_device_events(&[event(1)]);
        buses.publish_texts(&[]);
        buses.publish_auth_change();
        let mut texts = buses.texts();
        assert_eq!(texts.try_recv().unwrap_err(), TryRecvError::Empty);
    }

    #[test]
    fn a_subscriber_that_falls_behind_learns_how_many_it_missed() {
        let buses = Buses::new();
        let mut events = buses.device_events();
        let many: Vec<DeviceEvent> = (1..=1030).map(event).collect();
        buses.publish_device_events(&many);
        assert_eq!(events.try_recv().unwrap_err(), TryRecvError::Lagged(6));
        assert_eq!(events.try_recv().unwrap().id, 7);
    }
}
