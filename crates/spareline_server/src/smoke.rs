//! The read-only device smoke that the deploy kit runs in a new image before it goes live.

use std::io::{self, Write};
use std::time::Duration;

use spareline_adb::{AdbClient, AdbError, DeviceState};
use thiserror::Error;

/// What the smoke runs on each online phone. It only reads.
pub const SMOKE_COMMAND: &str = "getprop ro.product.model; getprop ro.build.version.release";

/// Why the smoke failed. It shows as the text after `smoke failed: `.
#[derive(Debug, Error)]
#[must_use]
pub enum SmokeError {
    /// The adb server didn't answer.
    #[error(transparent)]
    Adb(#[from] AdbError),
    /// Some online phones didn't answer the read.
    #[error("{count} online phone(s) did not answer")]
    PhonesSilent {
        /// How many didn't answer.
        count: usize,
    },
    /// Fewer phones were online than `SMOKE_MIN_PHONES` asks for.
    #[error("expected at least {minimum} online phone(s)")]
    TooFewPhones {
        /// The number asked for.
        minimum: u32,
    },
    /// Writing the report failed.
    #[error("cannot write the report: {source}")]
    Output {
        /// The failure.
        #[from]
        source: io::Error,
    },
}

/// The device smoke against one adb server.
#[derive(Clone, Debug)]
pub struct Smoke<'client> {
    adb: &'client AdbClient,
    minimum: u32,
}

impl<'client> Smoke<'client> {
    /// The smoke against `adb`, needing at least `minimum` online phones.
    #[must_use]
    pub fn new(adb: &'client AdbClient, minimum: u32) -> Self {
        Self { adb, minimum }
    }

    /// Runs the smoke, writing the report to `out` as spareline-old did, ending with
    /// `smoke ok`.
    ///
    /// # Errors
    ///
    /// Returns the first reason the smoke fails.
    pub async fn run<W: Write>(&self, out: &mut W) -> Result<(), SmokeError> {
        run(self.adb, self.minimum, out).await
    }
}

/// Checks that the adb server answers, that every online phone answers a read-only `getprop`,
/// and that at least `minimum` phones are online, writing the report to `out` as spareline-old
/// did, ending with `smoke ok`.
///
/// # Errors
///
/// Returns the first reason the smoke fails; the report up to that point is already written.
async fn run<W: Write>(adb: &AdbClient, minimum: u32, out: &mut W) -> Result<(), SmokeError> {
    let quick = adb.clone().with_call_timeout(Duration::from_secs(5));
    let version = quick.version().await?;
    let devices = quick.list_devices().await?;
    let online: Vec<&str> = devices
        .iter()
        .filter(|device| device.state == DeviceState::Device)
        .map(|device| device.serial.as_str())
        .collect();
    writeln!(
        out,
        "adb server {}: protocol {version}, {} device(s), {} online",
        adb.endpoint(),
        devices.len(),
        online.len()
    )?;
    let not_online = devices
        .iter()
        .filter(|device| device.state != DeviceState::Device);
    for device in not_online {
        writeln!(out, "  {}: {}", device.serial, device.raw_state)?;
    }
    let reader = adb.clone().with_call_timeout(Duration::from_secs(10));
    let mut failed = 0;
    for serial in &online {
        match read_phone(&reader, serial).await {
            Ok(line) => writeln!(out, "  {serial}: {line}")?,
            Err(message) => {
                failed += 1;
                writeln!(out, "  {serial}: FAILED {message}")?;
            }
        }
    }
    if failed > 0 {
        return Err(SmokeError::PhonesSilent { count: failed });
    }
    if online.len() < usize::try_from(minimum).unwrap_or(usize::MAX) {
        return Err(SmokeError::TooFewPhones { minimum });
    }
    writeln!(out, "smoke ok")?;
    Ok(())
}

/// The phone's model and Android version, as `<model>, Android <release>`, or why it failed.
async fn read_phone(adb: &AdbClient, serial: &str) -> Result<String, String> {
    let output = adb
        .shell(serial, SMOKE_COMMAND)
        .await
        .map_err(|error| error.to_string())?;
    let text = output.stdout_text();
    let mut lines = text.trim().lines();
    let model = lines.next().unwrap_or_default();
    let release = lines.next();
    if output.exit_code != 0 || model.is_empty() {
        return Err(format!("exit {}, model \"{model}\"", output.exit_code));
    }
    Ok(format!("{model}, Android {}", release.unwrap_or("?")))
}
