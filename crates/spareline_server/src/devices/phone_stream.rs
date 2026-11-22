//! A byte stream to a socket on a phone, closed when the phone disconnects.

use std::fmt::{self, Debug, Formatter};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::devices::session::SessionToken;

/// The future that finishes when the phone's session ends.
type Ended = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A duplex stream to a service on a phone, such as `localabstract:scrcpy`, from
/// [`Devices::open_stream`](crate::Devices::open_stream). It reads and writes like the
/// connection to the adb server it is, until the phone disconnects: from then on every read and
/// write fails with [`io::ErrorKind::ConnectionAborted`], as spareline-old's session signal
/// destroyed the socket. Dropping it closes the connection.
pub struct PhoneStream {
    stream: TcpStream,
    serial: String,
    ended: Ended,
    disconnected: bool,
}

impl PhoneStream {
    pub(crate) fn new(stream: TcpStream, serial: &str, token: SessionToken) -> Self {
        Self {
            stream,
            serial: serial.to_owned(),
            ended: Box::pin(async move { token.ended().await }),
            disconnected: false,
        }
    }

    /// The serial of the phone the stream goes to.
    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// An error once the phone's session ended; registers the task to wake when it ends.
    fn check(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if !self.disconnected && self.ended.as_mut().poll(context).is_ready() {
            self.disconnected = true;
        }
        if self.disconnected {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("phone {} disconnected", self.serial),
            ));
        }
        Ok(())
    }
}

impl AsyncRead for PhoneStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for PhoneStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.check(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.stream).poll_write(context, data)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.stream).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(context)
    }
}

impl Debug for PhoneStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhoneStream")
            .field("stream", &self.stream)
            .field("serial", &self.serial)
            .field("disconnected", &self.disconnected)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use spareline_db::PhoneState;
    use tokio::sync::mpsc;

    use crate::clock::Clock;
    use crate::devices::handle::Devices;
    use crate::fake_adb::{self, B30, FakeAdb};

    #[test]
    fn a_stream_to_a_phone_sends_each_write_at_once() {
        fake_adb::block_on(async {
            let adb = FakeAdb::start().await;
            let devices = Devices::new(adb.client(), Clock::tokio(fake_adb::START));
            {
                let now = devices.clock().now();
                let mut table = devices.table();
                table.apply_state(B30, PhoneState::Device, None, now);
                table
                    .start_session(B30, mpsc::unbounded_channel().0)
                    .unwrap();
            }
            let stream = devices
                .open_local(B30, "echo", Duration::from_secs(5))
                .await
                .unwrap();
            assert!(
                stream.stream.nodelay().unwrap(),
                "touch input to scrcpy isn't held back by Nagle's algorithm"
            );
        });
    }
}
