//! The telnet option dance, separated from the socket so it can be tested without one.
//!
//! Telnet is a byte stream with an escape byte in it. `IAC` (255) introduces a command; everything
//! else is data. That is nearly the whole protocol — the rest is which options to agree to.
//!
//! # What is agreed to, and why so little
//!
//! A terminal wants exactly four things, and refusing everything else is not laziness but the
//! specification's own advice: RFC 1143 warns that an implementation which says yes to whatever it is
//! offered can be talked into a negotiation loop that never settles.
//!
//! * **Suppress Go Ahead**, both ways. Without it the connection is half-duplex in principle, and
//!   servers wait for a turn that never comes.
//! * **Echo**, from the server. The remote end echoes what is typed, which is what makes a shell feel
//!   like a shell. Nothing is echoed locally.
//! * **Binary**, both ways. Telnet is a seven-bit protocol by default and UTF-8 is not seven bits, so
//!   without this every non-ASCII character is mangled.
//! * **Terminal Type** and **Window Size**, from us. The server is told what it is talking to and how
//!   big it is, which is what makes `vim` and `less` usable.
//!
//! Everything else is refused. An unknown option is refused rather than ignored, because silence in
//! telnet means "still waiting" and a server can block on it.
//!
//! # Why refusals are remembered
//!
//! RFC 1143's loop: a server offers an option, we refuse, and if it re-offers and we re-refuse
//! forever, neither side is wrong and nothing progresses. The rule that stops it is to answer a
//! request only when it changes something — so a second `DO` for an option already refused produces
//! nothing at all.

use std::collections::HashSet;

/// Interpret As Command: the escape byte.
pub(crate) const IAC: u8 = 255;

// Commands, in the order RFC 854 lists them.
/// End of a sub-negotiation.
pub(crate) const SE: u8 = 240;
/// No operation.
pub(crate) const NOP: u8 = 241;
/// Data mark, which arrives with an urgent notification we do not act on.
pub(crate) const DM: u8 = 242;
/// Break.
pub(crate) const BRK: u8 = 243;
/// Interrupt process.
pub(crate) const IP: u8 = 244;
/// Abort output.
pub(crate) const AO: u8 = 245;
/// Are you there.
pub(crate) const AYT: u8 = 246;
/// Erase character.
pub(crate) const EC: u8 = 247;
/// Erase line.
pub(crate) const EL: u8 = 248;
/// Go ahead.
pub(crate) const GA: u8 = 249;
/// Start of a sub-negotiation.
pub(crate) const SB: u8 = 250;
/// "I will."
pub(crate) const WILL: u8 = 251;
/// "I will not."
pub(crate) const WONT: u8 = 252;
/// "You do."
pub(crate) const DO: u8 = 253;
/// "You do not."
pub(crate) const DONT: u8 = 254;

// Options.
/// Binary transmission, RFC 856.
pub(crate) const OPT_BINARY: u8 = 0;
/// Server-side echo, RFC 857.
pub(crate) const OPT_ECHO: u8 = 1;
/// Suppress go ahead, RFC 858.
pub(crate) const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
/// Terminal type, RFC 1091.
pub(crate) const OPT_TERMINAL_TYPE: u8 = 24;
/// Window size, RFC 1073.
pub(crate) const OPT_NAWS: u8 = 31;

/// Sub-negotiation: "send me your terminal type".
const TT_SEND: u8 = 1;
/// Sub-negotiation: "here is my terminal type".
const TT_IS: u8 = 0;

/// Options this end will turn on when asked (`DO` → `WILL`).
const WE_MAY_ENABLE: &[u8] = &[
    OPT_BINARY,
    OPT_SUPPRESS_GO_AHEAD,
    OPT_TERMINAL_TYPE,
    OPT_NAWS,
];

/// Options this end will let the far end turn on (`WILL` → `DO`).
const THEY_MAY_ENABLE: &[u8] = &[OPT_BINARY, OPT_ECHO, OPT_SUPPRESS_GO_AHEAD];

/// What came out of a chunk of the stream.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// Bytes for the terminal.
    pub(crate) data: Vec<u8>,
    /// Bytes for the socket.
    pub(crate) reply: Vec<u8>,
}

/// The negotiation state, and the parser it drives.
///
/// One per connection. It has to persist across reads because a command can straddle a packet
/// boundary — an `IAC` at the end of one read and its command byte at the start of the next is
/// ordinary, not exotic.
#[derive(Debug)]
pub(crate) struct Telnet {
    state: State,
    /// The option byte of a command being assembled.
    command: u8,
    /// Bytes gathered since `SB`.
    subnegotiation: Vec<u8>,
    /// Options this end has said `WILL` for.
    enabled_here: HashSet<u8>,
    /// Options this end has said `DO` for.
    enabled_there: HashSet<u8>,
    /// Options this end has already refused, so a re-offer is not re-answered.
    refused_here: HashSet<u8>,
    /// Likewise for the far end's offers.
    refused_there: HashSet<u8>,
    /// What to answer a terminal-type request with.
    terminal_type: String,
    /// The size to report, once anything asks.
    size: (u16, u16),
}

/// Where the parser is in a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Ordinary data.
    Data,
    /// An `IAC` was seen.
    Iac,
    /// A `WILL`/`WONT`/`DO`/`DONT` was seen; the option byte is next.
    Option,
    /// Inside a sub-negotiation.
    Sub,
    /// Inside a sub-negotiation, having just seen an `IAC`.
    SubIac,
}

impl Telnet {
    /// A fresh connection that will call itself `terminal_type`.
    pub(crate) fn new(terminal_type: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            state: State::Data,
            command: 0,
            subnegotiation: Vec::new(),
            enabled_here: HashSet::new(),
            enabled_there: HashSet::new(),
            refused_here: HashSet::new(),
            refused_there: HashSet::new(),
            terminal_type: terminal_type.into(),
            size: (cols.max(1), rows.max(1)),
        }
    }

    /// What to send before anything else.
    ///
    /// Offered rather than waited for. A server that intends to negotiate will do so on its own, but
    /// plenty of them say nothing until spoken to, and a session that sits silent because both ends
    /// are being polite is indistinguishable from one that failed to connect.
    pub(crate) fn opening(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for option in [OPT_BINARY, OPT_SUPPRESS_GO_AHEAD] {
            out.extend_from_slice(&[IAC, WILL, option]);
            out.extend_from_slice(&[IAC, DO, option]);
        }
        out.extend_from_slice(&[IAC, DO, OPT_ECHO]);
        out
    }

    /// Whether the far end agreed to eight-bit data in this direction.
    pub(crate) fn binary_out(&self) -> bool {
        self.enabled_here.contains(&OPT_BINARY)
    }

    /// Record a new window size, and return what to tell the server about it.
    ///
    /// Empty when the server never asked for window size, which is most of them: sending a
    /// sub-negotiation for an option that was not agreed is a protocol error, not a harmless extra.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        let size = (cols.max(1), rows.max(1));
        if size == self.size {
            return Vec::new();
        }
        self.size = size;
        if !self.enabled_here.contains(&OPT_NAWS) {
            return Vec::new();
        }
        self.window_size()
    }

    /// Escape data on its way to the server.
    ///
    /// `IAC` in the payload has to be doubled or the server reads it as a command. This is the one
    /// piece of telnet that bites data going the *other* way, and forgetting it turns a byte 255 in a
    /// file being pasted into a hung session.
    pub(crate) fn escape(&self, data: &[u8], out: &mut Vec<u8>) {
        for byte in data {
            out.push(*byte);
            if *byte == IAC {
                out.push(IAC);
            }
        }
    }

    /// Split a chunk of the stream into data and replies.
    pub(crate) fn receive(&mut self, chunk: &[u8]) -> Parsed {
        let mut parsed = Parsed::default();

        for byte in chunk {
            match self.state {
                State::Data => {
                    if *byte == IAC {
                        self.state = State::Iac;
                    } else {
                        parsed.data.push(*byte);
                    }
                }

                State::Iac => match *byte {
                    // A doubled IAC is one literal 255.
                    IAC => {
                        parsed.data.push(IAC);
                        self.state = State::Data;
                    }
                    WILL | WONT | DO | DONT => {
                        self.command = *byte;
                        self.state = State::Option;
                    }
                    SB => {
                        self.subnegotiation.clear();
                        self.state = State::Sub;
                    }
                    // Commands with nothing to answer. `AYT` is the exception: it is a question, and
                    // a server that asks it is waiting.
                    AYT => {
                        parsed
                            .reply
                            .extend_from_slice(b"\r\n[BestTerm is here]\r\n");
                        self.state = State::Data;
                    }
                    NOP | DM | BRK | IP | AO | EC | EL | GA | SE => self.state = State::Data,
                    other => {
                        tracing::debug!(command = other, "telnet: ignoring an unknown command");
                        self.state = State::Data;
                    }
                },

                State::Option => {
                    let command = self.command;
                    self.answer(command, *byte, &mut parsed.reply);
                    self.state = State::Data;
                }

                State::Sub => {
                    if *byte == IAC {
                        self.state = State::SubIac;
                    } else {
                        // Bounded, because the length is the server's to choose and an unterminated
                        // sub-negotiation would otherwise grow without limit.
                        if self.subnegotiation.len() < 1024 {
                            self.subnegotiation.push(*byte);
                        }
                    }
                }

                State::SubIac => match *byte {
                    IAC => {
                        if self.subnegotiation.len() < 1024 {
                            self.subnegotiation.push(IAC);
                        }
                        self.state = State::Sub;
                    }
                    SE => {
                        self.finish_subnegotiation(&mut parsed.reply);
                        self.state = State::Data;
                    }
                    // Anything else ends the sub-negotiation as malformed rather than being folded
                    // into it, which is how a truncated one would otherwise swallow the rest of the
                    // stream.
                    _ => {
                        tracing::debug!("telnet: a sub-negotiation ended without SE");
                        self.subnegotiation.clear();
                        self.state = State::Data;
                    }
                },
            }
        }

        parsed
    }

    /// Answer one `WILL`/`WONT`/`DO`/`DONT`.
    fn answer(&mut self, command: u8, option: u8, reply: &mut Vec<u8>) {
        match command {
            // "You do" -- a request to turn something on at this end.
            DO => {
                if WE_MAY_ENABLE.contains(&option) {
                    // Answered only when it changes something. See the module documentation on loops.
                    if self.enabled_here.insert(option) {
                        reply.extend_from_slice(&[IAC, WILL, option]);
                        if option == OPT_NAWS {
                            // Sent immediately: the server asked because it wants to lay out a screen,
                            // and waiting for the first resize would leave it guessing until somebody
                            // dragged a window.
                            reply.extend_from_slice(&self.window_size());
                        }
                    }
                } else if self.refused_here.insert(option) {
                    reply.extend_from_slice(&[IAC, WONT, option]);
                }
            }

            DONT => {
                if self.enabled_here.remove(&option) {
                    reply.extend_from_slice(&[IAC, WONT, option]);
                }
            }

            // "I will" -- the far end offering to turn something on.
            WILL => {
                if THEY_MAY_ENABLE.contains(&option) {
                    if self.enabled_there.insert(option) {
                        reply.extend_from_slice(&[IAC, DO, option]);
                    }
                } else if self.refused_there.insert(option) {
                    reply.extend_from_slice(&[IAC, DONT, option]);
                }
            }

            WONT if self.enabled_there.remove(&option) => {
                reply.extend_from_slice(&[IAC, DONT, option]);
            }

            _ => {}
        }
    }

    /// Act on a completed sub-negotiation.
    fn finish_subnegotiation(&mut self, reply: &mut Vec<u8>) {
        let payload = std::mem::take(&mut self.subnegotiation);
        match payload.as_slice() {
            // "Send me your terminal type."
            [OPT_TERMINAL_TYPE, TT_SEND, ..] => {
                reply.extend_from_slice(&[IAC, SB, OPT_TERMINAL_TYPE, TT_IS]);
                // Escaped like any other payload: a terminal type with a 255 in it is absurd, but the
                // rule is the rule and the alternative is a special case that is wrong once.
                self.escape(self.terminal_type.as_bytes(), reply);
                reply.extend_from_slice(&[IAC, SE]);
            }
            other => {
                tracing::debug!(bytes = other.len(), "telnet: ignoring a sub-negotiation");
            }
        }
    }

    /// The window size sub-negotiation.
    fn window_size(&self) -> Vec<u8> {
        let (cols, rows) = self.size;
        let mut out = vec![IAC, SB, OPT_NAWS];
        // Each byte escaped, because a width of 255 columns is entirely ordinary and would otherwise
        // read as a command in the middle of the message.
        self.escape(&cols.to_be_bytes(), &mut out);
        self.escape(&rows.to_be_bytes(), &mut out);
        out.extend_from_slice(&[IAC, SE]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telnet() -> Telnet {
        Telnet::new("xterm-256color", 80, 24)
    }

    #[test]
    fn ordinary_bytes_pass_through_untouched() {
        let mut t = telnet();
        assert_eq!(t.receive(b"hello").data, b"hello");
        assert!(t.receive(b"hello").reply.is_empty());
    }

    #[test]
    fn a_doubled_escape_is_one_literal_byte() {
        // Byte 255 in a file being catted. Getting this wrong hangs the session on the next command
        // byte, which is the sort of failure nobody connects to the file they printed.
        let mut t = telnet();
        let parsed = t.receive(&[b'a', IAC, IAC, b'b']);
        assert_eq!(parsed.data, vec![b'a', 255, b'b']);
        assert!(parsed.reply.is_empty());
    }

    #[test]
    fn a_command_split_across_two_reads_is_still_one_command() {
        // Not exotic: an IAC at the end of a packet and its option at the start of the next is what a
        // network does. A parser that resets between reads mangles both.
        let mut t = telnet();
        assert!(t.receive(&[IAC]).reply.is_empty());
        assert!(t.receive(&[DO]).reply.is_empty());
        let parsed = t.receive(&[OPT_SUPPRESS_GO_AHEAD]);
        assert_eq!(parsed.reply, vec![IAC, WILL, OPT_SUPPRESS_GO_AHEAD]);
    }

    #[test]
    fn the_four_options_a_terminal_needs_are_agreed_to() {
        let mut t = telnet();
        for option in [OPT_BINARY, OPT_SUPPRESS_GO_AHEAD, OPT_TERMINAL_TYPE] {
            let parsed = t.receive(&[IAC, DO, option]);
            assert_eq!(parsed.reply, vec![IAC, WILL, option], "option {option}");
        }
        // The far end's echo, which is what makes a shell feel like one.
        let parsed = t.receive(&[IAC, WILL, OPT_ECHO]);
        assert_eq!(parsed.reply, vec![IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn anything_else_is_refused_rather_than_ignored() {
        // Silence in telnet means "still thinking", and a server can wait on it forever.
        let mut t = telnet();
        let parsed = t.receive(&[IAC, DO, 77]);
        assert_eq!(parsed.reply, vec![IAC, WONT, 77]);

        let parsed = t.receive(&[IAC, WILL, 77]);
        assert_eq!(parsed.reply, vec![IAC, DONT, 77]);
    }

    #[test]
    fn a_repeated_request_is_answered_once() {
        // RFC 1143's loop. Both ends re-answering forever is a session that never starts and neither
        // side doing anything wrong.
        let mut t = telnet();
        assert!(!t.receive(&[IAC, DO, 77]).reply.is_empty());
        assert!(
            t.receive(&[IAC, DO, 77]).reply.is_empty(),
            "a second refusal of the same option must say nothing"
        );

        assert!(!t.receive(&[IAC, DO, OPT_BINARY]).reply.is_empty());
        assert!(
            t.receive(&[IAC, DO, OPT_BINARY]).reply.is_empty(),
            "a second agreement to the same option must say nothing"
        );
    }

    #[test]
    fn a_terminal_type_request_is_answered_with_the_name() {
        let mut t = telnet();
        let parsed = t.receive(&[IAC, SB, OPT_TERMINAL_TYPE, TT_SEND, IAC, SE]);

        let mut expected = vec![IAC, SB, OPT_TERMINAL_TYPE, TT_IS];
        expected.extend_from_slice(b"xterm-256color");
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(parsed.reply, expected);
    }

    #[test]
    fn the_window_size_is_sent_when_the_server_asks_for_the_option() {
        let mut t = telnet();
        let parsed = t.receive(&[IAC, DO, OPT_NAWS]);

        // The agreement, and the size immediately after it: the server asked because it wants to lay
        // out a screen now, not after somebody drags a window.
        let mut expected = vec![IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS];
        expected.extend_from_slice(&[0, 80, 0, 24]);
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(parsed.reply, expected);
    }

    #[test]
    fn a_width_of_255_columns_is_escaped_in_the_size_message() {
        // 255 is a perfectly ordinary terminal width and is also the escape byte. Unescaped, it reads
        // as a command in the middle of the message and the rest of the stream goes with it.
        let mut t = Telnet::new("xterm", 255, 24);
        t.receive(&[IAC, DO, OPT_NAWS]);
        let sent = t.resize(511, 24);

        assert!(!sent.is_empty());
        // 511 is 0x01FF: the low byte is the escape, so it must be doubled.
        assert_eq!(
            sent,
            vec![IAC, SB, OPT_NAWS, 0x01, IAC, IAC, 0, 24, IAC, SE]
        );
    }

    #[test]
    fn no_size_is_sent_before_the_option_is_agreed() {
        // A sub-negotiation for an option nobody agreed to is a protocol error, not a spare hint.
        let mut t = telnet();
        assert!(t.resize(100, 40).is_empty());
    }

    #[test]
    fn resizing_to_the_same_size_says_nothing() {
        let mut t = telnet();
        t.receive(&[IAC, DO, OPT_NAWS]);
        assert!(!t.resize(100, 40).is_empty());
        assert!(t.resize(100, 40).is_empty());
    }

    #[test]
    fn are_you_there_is_answered_because_it_is_a_question() {
        let mut t = telnet();
        let parsed = t.receive(&[IAC, AYT]);
        assert!(!parsed.reply.is_empty());
        assert!(parsed.data.is_empty());
    }

    #[test]
    fn commands_with_no_answer_produce_no_answer_and_no_data() {
        let mut t = telnet();
        for command in [NOP, DM, BRK, IP, AO, EC, EL, GA] {
            let parsed = t.receive(&[IAC, command, b'x']);
            assert_eq!(parsed.data, b"x", "command {command}");
            assert!(parsed.reply.is_empty(), "command {command}");
        }
    }

    #[test]
    fn an_unterminated_subnegotiation_cannot_grow_without_limit() {
        // The length is the server's to choose, which makes it the server's to abuse.
        let mut t = telnet();
        let mut flood = vec![IAC, SB, OPT_TERMINAL_TYPE];
        flood.extend(std::iter::repeat_n(b'A', 100_000));
        t.receive(&flood);
        assert!(t.subnegotiation.len() <= 1024);
    }

    #[test]
    fn a_malformed_subnegotiation_does_not_swallow_the_rest_of_the_stream() {
        let mut t = telnet();
        let parsed = t.receive(&[IAC, SB, OPT_TERMINAL_TYPE, IAC, WILL, b'h', b'i']);
        assert_eq!(parsed.data, b"hi");
    }

    #[test]
    fn outbound_data_has_its_escape_bytes_doubled() {
        let t = telnet();
        let mut out = Vec::new();
        t.escape(&[b'a', 255, b'b'], &mut out);
        assert_eq!(out, vec![b'a', 255, 255, b'b']);
    }

    #[test]
    fn the_opening_offer_asks_for_what_a_terminal_needs() {
        // Offered rather than waited for: plenty of servers say nothing until spoken to, and a
        // session that sits silent because both ends are being polite looks like a failed connection.
        let mut t = telnet();
        let opening = t.opening();
        assert!(opening.windows(3).any(|w| w == [IAC, WILL, OPT_BINARY]));
        assert!(opening.windows(3).any(|w| w == [IAC, DO, OPT_BINARY]));
        assert!(
            opening
                .windows(3)
                .any(|w| w == [IAC, WILL, OPT_SUPPRESS_GO_AHEAD])
        );
        assert!(opening.windows(3).any(|w| w == [IAC, DO, OPT_ECHO]));
    }

    #[test]
    fn eight_bit_data_is_only_claimed_once_it_was_agreed() {
        let mut t = telnet();
        assert!(!t.binary_out());
        t.receive(&[IAC, DO, OPT_BINARY]);
        assert!(t.binary_out());
        t.receive(&[IAC, DONT, OPT_BINARY]);
        assert!(!t.binary_out());
    }
}
