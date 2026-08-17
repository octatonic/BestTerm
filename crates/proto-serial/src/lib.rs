//! Serial ports, as a [`Transport`].
//!
//! A console cable is the connection people reach for when the network one has stopped working, which
//! makes this the protocol that has to behave when everything else does not.
//!
//! # Blocking reads on a thread, not a runtime
//!
//! `serialport` is synchronous, and deliberately so: there is no portable asynchronous serial API
//! worth having, and the crates that pretend otherwise put a thread underneath anyway. So this uses a
//! thread per open port and says so, rather than wrapping a thread in a future and calling it async.
//!
//! The read has a timeout, and the timeout expiring is not an error — it is the normal state of a
//! serial port, which is silent most of the time. Treating it as a failure is the classic way to make
//! a console session that disconnects itself every hundred milliseconds.
//!
//! # There is no such thing as a closed serial port
//!
//! A network peer hangs up. A serial port just stops saying anything, and says nothing in exactly the
//! same way when the device at the far end is switched off, unplugged, or simply idle. So a session
//! ends when *this* end closes it, or when the operating system reports the handle is gone — an
//! unplugged USB adapter — and never merely because the far end went quiet.
//!
//! # Why the crate underneath is behind a trait
//!
//! `serialport` is looking for maintainers, particularly on Windows. `docs/ROADMAP.md` names
//! `serial2` as the replacement if that becomes a problem. Everything specific to it is in
//! [`SerialTransport::open`] and [`settings`], so replacing it is a day rather than a rewrite.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bestterm_core_model::{FlowControl, Parity, SerialConfig};
use bestterm_transport::{
    ExitInfo, GridSize, OpenTransport, Transport, TransportEvent, TransportKind,
};

/// How long a read waits before going round again.
///
/// Short enough that closing the port is noticed promptly, long enough not to spin. It is not a
/// deadline for data: a silent port is the normal case.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Bytes read in one go.
///
/// Small, because serial is slow and a large buffer only adds latency between a character arriving
/// and it being drawn.
const READ_CHUNK: usize = 4096;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SerialError {
    /// The port could not be opened.
    ///
    /// Overwhelmingly this is one of three things, and the message says which the operating system
    /// reported: the device does not exist, something else already has it, or the account is not in
    /// the group that may use it — which on Linux is `dialout` and is the single most common reason a
    /// console cable does not work on a fresh machine.
    #[error("could not open {device}: {source}")]
    Open {
        /// What was asked for.
        device: String,
        /// What the operating system said.
        #[source]
        source: serialport::Error,
    },

    /// The settings are not ones a port can take.
    #[error("{0}")]
    Settings(String),
}

/// Turn a configuration into what the port library wants.
///
/// Separate from opening so the mapping is testable without a device, which matters because nobody
/// has a serial port on a build machine.
pub fn settings(
    config: &SerialConfig,
) -> Result<
    (
        serialport::DataBits,
        serialport::Parity,
        serialport::StopBits,
        serialport::FlowControl,
    ),
    SerialError,
> {
    let data_bits = match config.data_bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        8 => serialport::DataBits::Eight,
        other => {
            return Err(SerialError::Settings(format!(
                "{other} data bits is not something a serial port can do; it is 5, 6, 7 or 8"
            )));
        }
    };

    let stop_bits = match config.stop_bits {
        1 => serialport::StopBits::One,
        2 => serialport::StopBits::Two,
        other => {
            return Err(SerialError::Settings(format!(
                "{other} stop bits is not something a serial port can do; it is 1 or 2"
            )));
        }
    };

    if config.baud == 0 {
        return Err(SerialError::Settings(
            "a baud rate of zero is not a speed".to_string(),
        ));
    }

    Ok((
        data_bits,
        match config.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        },
        stop_bits,
        match config.flow_control {
            FlowControl::None => serialport::FlowControl::None,
            FlowControl::Software => serialport::FlowControl::Software,
            FlowControl::Hardware => serialport::FlowControl::Hardware,
        },
    ))
}

/// Every serial port this machine can see, by name.
///
/// For the session dialog, so somebody picks `COM3` from a list rather than remembering it. Errors
/// are flattened to an empty list: a machine with no ports and a machine whose enumeration failed are
/// the same thing to a person choosing from a menu, and neither is worth an error dialog.
pub fn available() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => {
            let mut names: Vec<String> = ports.into_iter().map(|port| port.port_name).collect();
            names.sort();
            names
        }
        Err(error) => {
            tracing::debug!(%error, "serial: could not enumerate ports");
            Vec::new()
        }
    }
}

/// A live serial port.
pub struct SerialTransport {
    /// The write half. `serialport` hands out a clone for exactly this.
    port: Box<dyn serialport::SerialPort>,
    /// Set when this end closes, so the reader thread stops.
    closed: Arc<AtomicBool>,
    label: String,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialTransport")
            .field("label", &self.label)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl SerialTransport {
    /// Open the port `config` names.
    pub fn open(config: &SerialConfig) -> Result<OpenTransport, SerialError> {
        let (data_bits, parity, stop_bits, flow_control) = settings(config)?;

        let port = serialport::new(&config.device, config.baud)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            .timeout(READ_TIMEOUT)
            .open()
            .map_err(|source| SerialError::Open {
                device: config.device.clone(),
                source,
            })?;

        // Two handles on one port: `serialport` supports this and it is how the reading thread and
        // the writing caller stay out of each other's way.
        let reader = port.try_clone().map_err(|source| SerialError::Open {
            device: config.device.clone(),
            source,
        })?;

        let label = describe(config);
        tracing::info!(device = %config.device, baud = config.baud, "serial: port open");

        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let closed = Arc::new(AtomicBool::new(false));
        spawn_reader(reader, events_tx, Arc::clone(&closed), label.clone());

        Ok(OpenTransport {
            transport: Box::new(Self {
                port,
                closed,
                label,
            }),
            events: events_rx,
        })
    }
}

/// A port as somebody reads it: `COM3 115200 8N1`.
///
/// The settings are in the label because they are the thing that is wrong when a console session
/// shows nothing but rubbish, and having them on screen saves opening a dialog to check.
fn describe(config: &SerialConfig) -> String {
    let parity = match config.parity {
        Parity::None => 'N',
        Parity::Odd => 'O',
        Parity::Even => 'E',
    };
    format!(
        "{} {} {}{}{}",
        config.device, config.baud, config.data_bits, parity, config.stop_bits
    )
}

/// The thread that reads the port.
fn spawn_reader(
    mut port: Box<dyn serialport::SerialPort>,
    events: crossbeam_channel::Sender<TransportEvent>,
    closed: Arc<AtomicBool>,
    label: String,
) {
    let thread = std::thread::Builder::new()
        .name(format!("serial-{label}"))
        .spawn(move || {
            let mut buffer = vec![0u8; READ_CHUNK];
            loop {
                if closed.load(Ordering::Relaxed) {
                    return;
                }

                match port.read(&mut buffer) {
                    Ok(0) => {}
                    Ok(read) => {
                        if events
                            .send(TransportEvent::Output(buffer[..read].to_vec()))
                            .is_err()
                        {
                            return;
                        }
                    }
                    // The normal state of a serial port, not a failure. Treating it as one is how a
                    // console session disconnects itself every fifth of a second.
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        // The handle is gone: a USB adapter was unplugged, or the driver dropped it.
                        // This is the only way a serial session ends by itself -- silence never is.
                        tracing::info!(%label, %error, "serial: the port went away");
                        let _ = events.send(TransportEvent::Closed(ExitInfo {
                            code: None,
                            signal: None,
                            message: Some(error.to_string()),
                        }));
                        return;
                    }
                }
            }
        });

    if let Err(error) = thread {
        // A port with no reader would accept typing and show nothing back, which looks like a dead
        // device rather than a failure to start a thread.
        tracing::error!(%error, "serial: could not start the reader thread");
    }
}

impl Transport for SerialTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Serial
    }

    fn write(&mut self, data: &[u8]) -> bestterm_transport::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(bestterm_transport::TransportError::Closed);
        }
        self.port.write_all(data)?;
        // Flushed every time. A console session is one keystroke at a time, and buffering them until
        // something else forces a flush is indistinguishable from a device that has stopped
        // listening.
        self.port.flush()?;
        Ok(())
    }

    fn resize(&mut self, _size: GridSize) -> bestterm_transport::Result<()> {
        // A serial line has no idea how big the terminal is and no way to be told. Programs at the
        // far end use whatever `stty` says, which is theirs to set. Reporting an error would make a
        // window drag fail; doing nothing is both correct and what the trait allows.
        Ok(())
    }

    fn size(&self) -> GridSize {
        // Whatever the emulator decided. Nothing on the wire has an opinion.
        GridSize::new(80, 24)
    }

    fn shutdown(&mut self) -> bestterm_transport::Result<()> {
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SerialConfig {
        SerialConfig {
            device: "COM3".to_string(),
            baud: 115_200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow_control: FlowControl::None,
        }
    }

    #[test]
    fn the_usual_console_settings_map_across() {
        // 115200 8N1 is what console cables are wired for, and it is the one combination that has to
        // be right without anybody thinking about it.
        let (bits, parity, stop, flow) = settings(&config()).expect("8N1 is valid");
        assert_eq!(bits, serialport::DataBits::Eight);
        assert_eq!(parity, serialport::Parity::None);
        assert_eq!(stop, serialport::StopBits::One);
        assert_eq!(flow, serialport::FlowControl::None);
    }

    #[test]
    fn every_parity_and_flow_control_has_a_mapping() {
        for (ours, theirs) in [
            (Parity::None, serialport::Parity::None),
            (Parity::Odd, serialport::Parity::Odd),
            (Parity::Even, serialport::Parity::Even),
        ] {
            let mut c = config();
            c.parity = ours;
            assert_eq!(settings(&c).expect("valid").1, theirs);
        }

        for (ours, theirs) in [
            (FlowControl::None, serialport::FlowControl::None),
            (FlowControl::Software, serialport::FlowControl::Software),
            (FlowControl::Hardware, serialport::FlowControl::Hardware),
        ] {
            let mut c = config();
            c.flow_control = ours;
            assert_eq!(settings(&c).expect("valid").3, theirs);
        }
    }

    #[test]
    fn settings_a_port_cannot_take_are_refused_with_the_range_in_the_message() {
        // The message says what the answer is, because "invalid data bits" sends somebody to a manual
        // and "it is 5, 6, 7 or 8" does not.
        let mut c = config();
        c.data_bits = 9;
        let message = settings(&c).expect_err("nine data bits").to_string();
        assert!(message.contains("5, 6, 7 or 8"), "{message}");

        let mut c = config();
        c.stop_bits = 3;
        let message = settings(&c).expect_err("three stop bits").to_string();
        assert!(message.contains("1 or 2"), "{message}");

        let mut c = config();
        c.baud = 0;
        assert!(settings(&c).is_err(), "zero baud is not a speed");
    }

    #[test]
    fn the_label_carries_the_settings_because_they_are_what_is_wrong() {
        // A console showing rubbish is a console at the wrong speed, and having the speed on screen
        // saves opening a dialog to find out what it was set to.
        assert_eq!(describe(&config()), "COM3 115200 8N1");

        let mut c = config();
        c.parity = Parity::Even;
        c.data_bits = 7;
        c.stop_bits = 2;
        c.baud = 9600;
        assert_eq!(describe(&c), "COM3 9600 7E2");
    }

    #[test]
    fn opening_a_device_that_is_not_there_names_it_and_says_why() {
        let mut c = config();
        c.device = "no-such-port-anywhere".to_string();
        // Taken apart by hand: `OpenTransport` has no `Debug`, on purpose -- there is nothing in a
        // file handle worth printing.
        let error = match SerialTransport::open(&c) {
            Ok(_) => panic!("there is no such port"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("no-such-port-anywhere"), "{message}");
        // And it is the open failure rather than a settings one, because the settings were fine.
        assert!(matches!(error, SerialError::Open { .. }), "{error:?}");
    }

    #[test]
    fn enumerating_ports_never_fails_it_only_comes_back_empty() {
        // A build machine has no serial ports, and a machine whose enumeration failed is the same
        // thing to somebody choosing from a menu.
        let ports = available();
        // Sorted, so the list does not shuffle between openings of the dialog.
        let mut sorted = ports.clone();
        sorted.sort();
        assert_eq!(ports, sorted);
    }
}
