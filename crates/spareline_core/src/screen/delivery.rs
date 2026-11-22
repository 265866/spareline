//! What each browser viewer receives of a phone's video, under backpressure.

use std::time::Duration;

use crate::Timestamp;
use crate::screen::control_kind::ControlKind;
use crate::screen::scrcpy::VIDEO_BIT_RATE;
use crate::screen::video_packet::PacketKind;

/// Bytes a viewer's socket may hold queued before the viewer skips to the next keyframe: about
/// one second of video at the relay's bit rate.
pub const BACKLOG_LIMIT: usize = VIDEO_BIT_RATE as usize / 8;

/// How long a viewer's keyframe request may go unanswered before it asks again.
pub const KEYFRAME_RETRY: Duration = Duration::from_secs(2);

/// What to do with one packet for one viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Send the packet's bytes to the viewer.
    Send,
    /// Drop it.
    Drop,
    /// Drop it and ask the session for a keyframe, through [`crate::screen::KeyframeRequests`].
    DropAndRequestKeyframe,
}

/// One viewer's place in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// Dropping everything but config packets: the viewer's decoder is new, for a new viewer or
    /// a new session, and cannot decode even a keyframe yet.
    pub needs_config: bool,
    /// Dropping until a keyframe, because frames after a gap cannot decode.
    pub waiting_for_keyframe: bool,
    /// When this viewer last asked for a keyframe, if it is waiting on one it asked for.
    pub keyframe_requested_at: Option<Timestamp>,
}

impl Delivery {
    /// A viewer whose decoder is new: it waits for a config packet and the keyframe after it,
    /// which the relay requested at `now`.
    #[must_use]
    pub const fn fresh_decoder(now: Timestamp) -> Self {
        Self {
            needs_config: true,
            waiting_for_keyframe: true,
            keyframe_requested_at: Some(now),
        }
    }

    /// The backpressure policy for one packet, given the bytes queued on the viewer's socket.
    ///
    /// A slow viewer never grows an unbounded queue: once its socket holds more than `limit`
    /// bytes it drops frames, and once the queue has drained it asks for a keyframe and resumes
    /// there. Config packets are tiny and always sent, since a decoder cannot start without one;
    /// a viewer that has not had one gets nothing else, not even a keyframe the encoder sends on
    /// its own.
    pub fn decide(
        &mut self,
        kind: PacketKind,
        buffered: usize,
        limit: usize,
        now: Timestamp,
    ) -> Decision {
        if kind == PacketKind::Config {
            self.needs_config = false;
            self.waiting_for_keyframe = true;
            self.keyframe_requested_at = Some(now);
            return Decision::Send;
        }
        if buffered > limit {
            self.waiting_for_keyframe = true;
            self.keyframe_requested_at = None;
            return Decision::Drop;
        }
        if self.needs_config || (self.waiting_for_keyframe && kind != PacketKind::Keyframe) {
            if let Some(requested) = self.keyframe_requested_at
                && now.since(requested) < KEYFRAME_RETRY
            {
                return Decision::Drop;
            }
            self.keyframe_requested_at = Some(now);
            return Decision::DropAndRequestKeyframe;
        }
        self.waiting_for_keyframe = false;
        Decision::Send
    }
}

/// A viewer's counts, which its `control_sessions` row records when it leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewerCounts {
    /// Video packets dropped for this viewer.
    pub dropped: u64,
    /// Allowed key, touch, scroll and back-or-screen-on messages.
    pub inputs: u64,
    /// Allowed text messages.
    pub texts: u64,
    /// Refused messages, and every message while the session wasn't running.
    pub rejected: u64,
}

/// The delivery state and counts of one viewer: what the server consults for each packet and
/// each message, with the socket and the session left to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerDelivery {
    /// Where the viewer is in the stream.
    pub delivery: Delivery,
    /// What it received and sent.
    pub counts: ViewerCounts,
}

impl ViewerDelivery {
    /// A viewer that joined at `now`. It starts mid-stream, so it waits for the config packet and
    /// keyframe the relay asks for when it joins.
    #[must_use]
    pub const fn joined(now: Timestamp) -> Self {
        Self {
            delivery: Delivery::fresh_decoder(now),
            counts: ViewerCounts {
                dropped: 0,
                inputs: 0,
                texts: 0,
                rejected: 0,
            },
        }
    }

    /// Resets the viewer for a session's hello at `now`: it gets packets only after it, starting
    /// from a fresh config packet and keyframe.
    pub const fn greeted(&mut self, now: Timestamp) {
        self.delivery = Delivery::fresh_decoder(now);
    }

    /// Decides one packet for this viewer, counting a drop. On [`Decision::Send`] the server
    /// sends the bytes and reports a socket that dropped them with [`Self::send_dropped`].
    pub fn deliver(&mut self, kind: PacketKind, buffered: usize, now: Timestamp) -> Decision {
        let decision = self.delivery.decide(kind, buffered, BACKLOG_LIMIT, now);
        if decision != Decision::Send {
            self.counts.dropped += 1;
        }
        decision
    }

    /// Records that the socket dropped a packet it was given: the viewer waits for a keyframe
    /// again, and for a config packet too when the dropped one was a config packet.
    pub fn send_dropped(&mut self, kind: PacketKind) {
        self.counts.dropped += 1;
        if kind == PacketKind::Config {
            self.delivery.needs_config = true;
        }
        self.delivery.waiting_for_keyframe = true;
        self.delivery.keyframe_requested_at = None;
    }

    /// Counts a control message the viewer sent while the session runs, and says whether to
    /// forward it: only an allowed message goes to the server.
    pub fn control(&mut self, message: &[u8]) -> Option<ControlKind> {
        let kind = ControlKind::classify(message);
        match kind {
            None => self.counts.rejected += 1,
            Some(ControlKind::Text) => self.counts.texts += 1,
            Some(ControlKind::Input) => self.counts.inputs += 1,
        }
        kind
    }

    /// Counts a control message the viewer sent while the session wasn't running, which the
    /// relay drops whatever it holds.
    pub const fn reject(&mut self) {
        self.counts.rejected += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 100_000;

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    fn fresh() -> Delivery {
        Delivery {
            needs_config: false,
            waiting_for_keyframe: false,
            keyframe_requested_at: None,
        }
    }

    fn frame(state: &mut Delivery, buffered: usize, millis: i64) -> Decision {
        state.decide(PacketKind::Frame, buffered, LIMIT, at(millis))
    }

    fn key(state: &mut Delivery, buffered: usize, millis: i64) -> Decision {
        state.decide(PacketKind::Keyframe, buffered, LIMIT, at(millis))
    }

    fn config(state: &mut Delivery, buffered: usize, millis: i64) -> Decision {
        state.decide(PacketKind::Config, buffered, LIMIT, at(millis))
    }

    #[test]
    fn a_viewer_with_room_gets_every_frame() {
        let mut state = fresh();
        assert_eq!(frame(&mut state, 0, 0), Decision::Send);
        assert_eq!(frame(&mut state, LIMIT, 1), Decision::Send);
        assert!(!state.waiting_for_keyframe);
    }

    #[test]
    fn a_full_queue_drops_frames_then_resumes_at_a_keyframe_it_asks_for_once() {
        let mut state = fresh();
        assert_eq!(frame(&mut state, LIMIT + 1, 0), Decision::Drop);
        assert_eq!(key(&mut state, LIMIT + 1, 10), Decision::Drop);
        assert_eq!(frame(&mut state, LIMIT + 1, 20), Decision::Drop);
        assert_eq!(frame(&mut state, 0, 30), Decision::DropAndRequestKeyframe);
        assert_eq!(frame(&mut state, 0, 40), Decision::Drop);
        assert_eq!(key(&mut state, 0, 50), Decision::Send);
        assert_eq!(frame(&mut state, 0, 60), Decision::Send);
    }

    #[test]
    fn an_unanswered_keyframe_request_is_repeated_after_2_s() {
        let mut state = Delivery {
            needs_config: false,
            waiting_for_keyframe: true,
            keyframe_requested_at: Some(at(0)),
        };
        assert_eq!(frame(&mut state, 0, 1_999), Decision::Drop);
        assert_eq!(
            frame(&mut state, 0, 2_000),
            Decision::DropAndRequestKeyframe
        );
        assert_eq!(frame(&mut state, 0, 2_500), Decision::Drop);
    }

    #[test]
    fn config_packets_always_go_out_even_to_a_full_queue_and_expect_a_keyframe() {
        let mut state = fresh();
        assert_eq!(config(&mut state, LIMIT * 10, 0), Decision::Send);
        assert!(state.waiting_for_keyframe);
        assert_eq!(frame(&mut state, 0, 1), Decision::Drop);
        assert_eq!(key(&mut state, 0, 2), Decision::Send);
    }

    #[test]
    fn a_viewer_without_a_config_packet_gets_nothing_else_not_even_a_keyframe() {
        let mut state = Delivery::fresh_decoder(at(0));
        assert_eq!(key(&mut state, 0, 10), Decision::Drop);
        assert_eq!(frame(&mut state, 0, 20), Decision::Drop);
        assert_eq!(key(&mut state, 0, 2_000), Decision::DropAndRequestKeyframe);
        assert_eq!(config(&mut state, 0, 2_100), Decision::Send);
        assert_eq!(key(&mut state, 0, 2_110), Decision::Send);
        assert_eq!(frame(&mut state, 0, 2_120), Decision::Send);
    }

    #[test]
    fn the_limit_is_about_a_second_of_video() {
        assert_eq!(BACKLOG_LIMIT, 500_000);
    }

    #[test]
    fn a_new_viewer_starts_at_a_config_packet_then_a_keyframe_without_asking_again() {
        let mut viewer = ViewerDelivery::joined(at(0));
        let mut sent = Vec::new();
        for (kind, millis, label) in [
            (PacketKind::Keyframe, 5, 1),
            (PacketKind::Frame, 10, 2),
            (PacketKind::Config, 15, 0),
            (PacketKind::Frame, 18, 2),
            (PacketKind::Keyframe, 20, 1),
            (PacketKind::Frame, 30, 2),
        ] {
            let decision = viewer.deliver(kind, 0, at(millis));
            assert_ne!(decision, Decision::DropAndRequestKeyframe);
            if decision == Decision::Send {
                sent.push(label);
            }
        }
        assert_eq!(sent, [0, 1, 2]);
        assert_eq!(viewer.counts.dropped, 3);
    }

    #[test]
    fn a_config_packet_the_socket_dropped_is_needed_again() {
        let mut viewer = ViewerDelivery::joined(at(0));
        assert_eq!(viewer.deliver(PacketKind::Config, 0, at(0)), Decision::Send);
        viewer.send_dropped(PacketKind::Config);
        assert!(viewer.delivery.needs_config);
    }

    #[test]
    fn a_slow_viewer_stops_receiving_and_asks_for_a_keyframe_once_drained() {
        let mut viewer = ViewerDelivery::joined(at(0));
        assert_eq!(viewer.deliver(PacketKind::Config, 0, at(0)), Decision::Send);
        assert_eq!(
            viewer.deliver(PacketKind::Keyframe, 0, at(0)),
            Decision::Send
        );
        for millis in 0..10 {
            assert_eq!(
                viewer.deliver(PacketKind::Frame, BACKLOG_LIMIT + 1, at(millis)),
                Decision::Drop
            );
        }
        assert_eq!(
            viewer.deliver(PacketKind::Frame, 0, at(20)),
            Decision::DropAndRequestKeyframe
        );
        assert_eq!(
            viewer.deliver(PacketKind::Keyframe, 0, at(30)),
            Decision::Send
        );
        assert_eq!(viewer.counts.dropped, 11);
    }

    #[test]
    fn a_frame_the_socket_dropped_makes_the_viewer_wait_for_a_keyframe() {
        let mut viewer = ViewerDelivery::joined(at(0));
        viewer.delivery.needs_config = false;
        assert_eq!(
            viewer.deliver(PacketKind::Keyframe, 0, at(0)),
            Decision::Send
        );
        viewer.send_dropped(PacketKind::Frame);
        assert!(viewer.delivery.waiting_for_keyframe);
        assert!(!viewer.delivery.needs_config);
        assert_eq!(viewer.counts.dropped, 1);
    }

    #[test]
    fn controls_count_as_inputs_texts_or_rejected() {
        let mut viewer = ViewerDelivery::joined(at(0));
        assert_eq!(viewer.control(&[4, 0]), Some(ControlKind::Input));
        assert_eq!(
            viewer.control(&[1, 0, 0, 0, 1, b'a']),
            Some(ControlKind::Text)
        );
        viewer.reject();
        assert_eq!(viewer.control(&[10, 0]), None);
        assert_eq!(
            viewer.counts,
            ViewerCounts {
                dropped: 0,
                inputs: 1,
                texts: 1,
                rejected: 2
            }
        );
    }
}
