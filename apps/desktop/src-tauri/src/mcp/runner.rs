//! Running an agent's command inside a visible, interactive terminal session.
//!
//! The alternative — a one-off exec channel — is invisible and has no tty, so a
//! command that prompts for a sudo password simply hangs until its timeout. Here
//! the command is *typed into a real session the user can see and type into*, so
//! answering that prompt is something a person can actually do.
//!
//! There is no exec-channel exit status on this path, so completion is detected
//! from the output itself. A helper is defined once per session:
//!
//! ```text
//! __luma() { printf '\n__LUMA_MCP_<uuid>:%s\n' "$?"; }
//! ```
//!
//! and every command afterwards is submitted as:
//!
//! ```text
//! <command>; __luma
//! ```
//!
//! The helper exists so the user is not looking at 60 characters of `printf`
//! after everything they watch an agent run. Defining it once also removes a
//! whole class of false matches: the command line no longer *contains* the
//! marker text, so the terminal's echo of it cannot be mistaken for the real
//! thing.
//!
//! Two properties are load-bearing:
//!
//! * **The command and `__luma` are submitted as one line.** Nothing is left
//!   queued in the tty input buffer afterwards, so when the command runs `sudo`,
//!   its password prompt reads from an empty tty and waits for the user — rather
//!   than silently consuming a queued sentinel line *as* the password.
//! * **The marker is unique per session.** A fixed one would match output left
//!   over from a previous session in the same buffer. It is a session rather
//!   than a per-command value because the helper that prints it is defined once.
//!
//! The known limit is a final line that reads its own stdin (a heredoc, a bare
//! `cat`): it swallows the sentinel and the call ends at its timeout instead of
//! on completion. The tool description says so.

/// Marker prefix. Deliberately unlikely to occur in real output, and paired
/// with a per-session uuid so output from an earlier session cannot match.
const MARKER_PREFIX: &str = "__LUMA_MCP_";

/// Shell function name. Short, because the user reads it after every command,
/// and underscore-prefixed to stay out of the way of anything real.
const HELPER: &str = "__luma";

/// One session's sentinel: the helper that prints it and the parser that finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sentinel {
    /// The full marker *including* its trailing separator, exactly as it appears
    /// in the output. Both the printf format and the parser derive from this one
    /// string, so the two can never disagree about the separator.
    marker: String,
}

impl Sentinel {
    pub(crate) fn new(token: &str) -> Self {
        Self {
            marker: format!("{MARKER_PREFIX}{token}:"),
        }
    }

    /// The helper definition, without a terminator.
    ///
    /// `printf` rather than `echo` because `echo` interprets backslashes in some
    /// shells. `$?` is read as the function's first act, so it is the status of
    /// whatever ran before it rather than anything the helper itself did.
    ///
    /// The marker line is printed and then erased in the *same* write: `ESC[1A`
    /// moves back up onto it and `ESC[2K` clears it, leaving the cursor where
    /// the shell's next prompt will draw. Luma still sees the text — the tap
    /// strips the escape sequences and keeps the marker — but the user never
    /// sees a line of hex after everything an agent runs.
    ///
    /// Erasing rather than hiding: an OSC wrapper would be invisible to the
    /// terminal *and* to Luma, because the tap's stripper discards OSC payloads
    /// (see `taps::AnsiStripper`). This keeps producer and parser on plain text.
    fn definition(&self) -> String {
        format!(
            "{HELPER}() {{ printf '\\n{}%s\\n\\033[1A\\r\\033[2K' \"$?\"; }}",
            self.marker
        )
    }

    /// The line to type: the command, then the helper call, then Enter.
    ///
    /// `define` prepends the helper definition, for the first command in a
    /// session. It rides the *same* line rather than being written first,
    /// because two writes would leave the command queued in the tty input
    /// buffer while the definition runs — the same hazard that makes a queued
    /// sentinel get eaten by a password prompt. One line, one submission.
    pub(crate) fn command_line(&self, command: &str, define: bool) -> String {
        // A trailing newline in the agent's command would submit it early and
        // leave the helper call on its own line, where it would report the
        // status of the wrong thing. Trimming keeps them one submission.
        let command = command.trim_end_matches(['\r', '\n']);
        if define {
            format!("{}; {command}; {HELPER}\r", self.definition())
        } else {
            format!("{command}; {HELPER}\r")
        }
    }

    /// Locate the sentinel in stripped output.
    ///
    /// Returns the text before it and the reported exit code. `None` while the
    /// command is still running.
    ///
    /// Only a marker at the start of a line counts: the terminal echoes the
    /// command line back, and that echo contains the marker inside the `printf`
    /// argument rather than at a line start.
    pub(crate) fn parse<'a>(&self, output: &'a str) -> Option<(&'a str, Option<u32>)> {
        let needle = self.marker.as_str();
        let mut search_from = 0;
        while let Some(offset) = output[search_from..].find(needle) {
            let start = search_from + offset;
            let at_line_start = start == 0 || output.as_bytes()[start - 1] == b'\n';
            if !at_line_start {
                search_from = start + needle.len();
                continue;
            }
            let after = &output[start + needle.len()..];
            // The code is terminated by the newline printf emits. Without one
            // the line is still arriving, so this is not yet a complete match.
            let newline = after.find('\n')?;
            let code = after[..newline].trim().parse::<u32>().ok();
            return Some((&output[..start], code));
        }
        None
    }
}

/// Clean up what the terminal echoed so the agent sees output, not a transcript.
///
/// The session echoes the line that was typed before the command produces
/// anything, and a shell prompt usually precedes it. Everything up to and
/// including the newline that ends that echo belongs to the input, not the
/// output — but only when the echo is actually there: a session with echo off
/// (a password prompt is the common case) produces output with no leading line
/// to drop, and dropping one anyway would eat a real line.
pub(crate) fn strip_echo<'a>(output: &'a str, typed: &str) -> &'a str {
    // The echo wraps at the terminal's width, so it is not necessarily one
    // line. Anchor on the tail of what was typed — `; __luma`, which ends the
    // submitted line — rather than on the whole thing.
    let line = typed.trim_end_matches(['\r', '\n']);
    let Some(cut) = line.rfind(';') else {
        return output;
    };
    let anchor = &line[cut..];

    // Scan for the LAST occurrence that actually ends a line. An interactive
    // shell redraws its input as it is typed (zsh-autosuggestions, syntax
    // highlighting), so the anchor appears once per redraw and only the final
    // one is followed by the command running. Requiring end-of-line also keeps
    // a command whose *output* happens to mention the anchor from being treated
    // as the echo — real output rarely ends a line with exactly this.
    let mut best = None;
    let mut from = 0;
    while let Some(offset) = output[from..].find(anchor) {
        let start = from + offset;
        let after = &output[start + anchor.len()..];
        let rest = after.strip_prefix('\r').unwrap_or(after);
        if let Some(remainder) = rest.strip_prefix('\n') {
            best = Some(output.len() - remainder.len());
        }
        from = start + anchor.len();
    }
    best.map_or(output, |offset| &output[offset..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentinel() -> Sentinel {
        Sentinel::new("abc123")
    }

    #[test]
    fn the_command_line_submits_command_and_helper_together() {
        let line = sentinel().command_line("uptime", false);
        // One submission: exactly one carriage return, at the very end.
        assert_eq!(line.matches('\r').count(), 1);
        assert!(line.ends_with('\r'));
        assert_eq!(line, "uptime; __luma\r");
        // The marker itself is no longer on the command line, so the terminal's
        // echo of it cannot be confused for the real sentinel.
        assert!(!line.contains("__LUMA_MCP_"));
    }

    /// The first command in a session carries the definition with it.
    ///
    /// Still ONE submission: writing the definition separately would leave the
    /// command queued in the tty buffer while it ran, which is the same hazard
    /// that lets a password prompt eat a queued line.
    #[test]
    fn the_first_command_defines_the_helper_on_the_same_line() {
        let line = sentinel().command_line("uptime", true);
        assert_eq!(line.matches('\r').count(), 1);
        assert!(line.ends_with('\r'));
        assert!(line.starts_with("__luma() {"));
        assert!(line.contains("__LUMA_MCP_abc123:"));
        assert!(line.contains("\"$?\""));
        assert!(line.ends_with("; uptime; __luma\r"));
    }

    /// A trailing newline would submit the command on its own and leave the
    /// helper call to run separately, reporting the status of the wrong thing.
    #[test]
    fn a_trailing_newline_in_the_command_is_trimmed() {
        let line = sentinel().command_line("uptime\n", false);
        assert_eq!(line.matches('\r').count(), 1);
        assert_eq!(line, "uptime; __luma\r");
    }

    /// The producer and the parser must agree on the marker's exact bytes.
    ///
    /// The other tests here write the expected sentinel out by hand, so they
    /// all agree with each other and none of them check that the helper
    /// actually *emits* what the parser looks for. It did not: the format
    /// dropped the ':' separator the parser required, so no command ever
    /// completed and every call ran to its timeout.
    #[test]
    fn what_the_helper_prints_is_what_the_parser_matches() {
        let sentinel = sentinel();

        // Extract the literal prefix the shell will print, straight from the
        // helper's format string: between the leading '\n' and the '%s'.
        let setup = sentinel.command_line("true", true);
        let format = setup
            .split_once("printf '")
            .and_then(|(_, rest)| rest.split_once("%s"))
            .map(|(format, _)| format)
            .expect("defining command line should contain a printf format");
        let emitted = format.strip_prefix("\\n").expect("format starts with \\n");

        // Simulate the shell running it: the format, then the exit code.
        let output = format!("prior output\n{emitted}0\n");
        let (text, code) = sentinel
            .parse(&output)
            .expect("the parser must match what the helper emits");
        assert_eq!(code, Some(0));
        assert_eq!(text, "prior output\n");
    }

    #[test]
    fn parse_returns_output_and_exit_code() {
        let output = "hello\nworld\n__LUMA_MCP_abc123:0\n";
        let (text, code) = sentinel().parse(output).unwrap();
        assert_eq!(text, "hello\nworld\n");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn parse_reads_a_nonzero_exit_code() {
        let (_, code) = sentinel().parse("nope\n__LUMA_MCP_abc123:127\n").unwrap();
        assert_eq!(code, Some(127));
    }

    #[test]
    fn parse_waits_while_the_command_is_still_running() {
        assert!(sentinel().parse("partial output\n").is_none());
        // The marker has arrived but its line has not finished.
        assert!(sentinel().parse("out\n__LUMA_MCP_abc123:").is_none());
        assert!(sentinel().parse("out\n__LUMA_MCP_abc123:1").is_none());
    }

    /// The terminal echoes the setup line back, and that echo contains the
    /// marker inside the function body. Matching it would report success before
    /// any command had run at all.
    #[test]
    fn the_echoed_setup_line_is_not_mistaken_for_the_sentinel() {
        let sentinel = sentinel();
        let echo = format!(
            "$ {}\n",
            sentinel.command_line("uptime", true).trim_end_matches('\r')
        );
        assert!(sentinel.parse(&echo).is_none());

        let full = format!("{echo}12:00:00 up 3 days\n__LUMA_MCP_abc123:0\n");
        let (text, code) = sentinel.parse(&full).unwrap();
        assert_eq!(code, Some(0));
        assert!(text.contains("up 3 days"));
    }

    /// The tap delivers whatever arrived; a sentinel split across two reads is
    /// normal. Parsing the accumulated buffer must converge on the same answer.
    #[test]
    fn a_sentinel_split_across_reads_parses_once_complete() {
        let stream = "output line\n__LUMA_MCP_abc123:0\n";
        for split in 1..stream.len() {
            let first = &stream[..split];
            // Incomplete prefixes must not report completion...
            if sentinel().parse(first).is_some() {
                assert_eq!(first, stream, "reported completion early at {split}");
            }
            // ...and the whole buffer always parses.
            let (_, code) = sentinel().parse(stream).unwrap();
            assert_eq!(code, Some(0));
        }
    }

    #[test]
    fn output_containing_the_marker_text_does_not_end_the_command() {
        // A different run's marker, and this run's marker mid-line.
        let output = "saw __LUMA_MCP_abc123:0 inline\n__LUMA_MCP_other:0\n";
        assert!(sentinel().parse(output).is_none());
    }

    #[test]
    fn a_non_numeric_exit_code_still_completes_the_command() {
        let (text, code) = sentinel().parse("done\n__LUMA_MCP_abc123:?\n").unwrap();
        assert_eq!(text, "done\n");
        assert_eq!(code, None);
    }

    /// The marker is printed and then erased from the screen. The erase is what
    /// keeps the tab readable, so it must not also erase the marker from what
    /// Luma reads: the tap strips escape sequences but keeps their surrounding
    /// text, which is exactly the property this depends on.
    #[tokio::test]
    async fn the_erased_marker_is_still_readable_through_the_tap() {
        use crate::mcp::taps::{PaneKind, TapRegistry};
        use std::sync::Arc;

        let registry = Arc::new(TapRegistry::default());
        let tap = registry.new_tap(PaneKind::Local);
        tap.attach("session-1", "bash");
        registry.share("session-1", "grant-a", "bash");

        let sentinel = sentinel();
        // Exactly what the helper emits: marker line, then cursor-up + erase.
        tap.push(b"hi\r\n\n__LUMA_MCP_abc123:0\n\x1b[1A\r\x1b[2K");

        let read = registry
            .read(
                "session-1",
                "grant-a",
                None,
                64 * 1024,
                std::time::Duration::ZERO,
            )
            .await
            .unwrap();

        let (output, code) = sentinel
            .parse(&read.text)
            .expect("the erase must not remove the marker from the tap");
        assert_eq!(code, Some(0));
        assert!(output.contains("hi"));
    }

    /// The sentinel is parsed out of *stripped* text, so it has to survive the
    /// tap's ANSI stripper — including a colourised prompt and CRLF line endings
    /// from a real PTY, and a chunk boundary landing inside the sentinel.
    #[tokio::test]
    async fn a_sentinel_survives_the_tap_pipeline() {
        use crate::mcp::taps::{PaneKind, TapRegistry};
        use std::sync::Arc;

        let registry = Arc::new(TapRegistry::default());
        let tap = registry.new_tap(PaneKind::Local);
        tap.attach("session-1", "bash");
        registry.share("session-1", "grant-a", "bash");

        let sentinel = sentinel();
        let typed = sentinel.command_line("echo hi", false);
        let cursor = registry.cursor("session-1", "grant-a");

        // What a real terminal sends back: a coloured prompt, the echo of the
        // typed line, the output, then the sentinel — CRLF throughout.
        let stream = format!(
            "\x1b[32muser@host\x1b[0m:~$ {}\r\nhi\r\n__LUMA_MCP_abc123:0\r\n",
            typed.trim_end_matches('\r')
        );
        // Split mid-sentinel: a chunk boundary there is the normal case.
        let bytes = stream.as_bytes();
        let split = stream.find("_abc123").unwrap() + 3;
        tap.push(&bytes[..split]);
        tap.push(&bytes[split..]);

        let read = registry
            .read(
                "session-1",
                "grant-a",
                cursor,
                64 * 1024,
                std::time::Duration::ZERO,
            )
            .await
            .unwrap();

        let (output, code) = sentinel
            .parse(&read.text)
            .expect("sentinel should be found in stripped output");
        assert_eq!(code, Some(0));
        // The prompt and the echoed command are input, not output.
        assert_eq!(strip_echo(output, &typed), "hi\n");
    }

    #[test]
    fn strip_echo_drops_the_echoed_input_line() {
        let typed = sentinel().command_line("uptime", false);
        let output = format!("user@host:~$ {}\nreal output\n", typed.trim_end());
        assert_eq!(strip_echo(&output, &typed), "real output\n");
    }

    /// An interactive zsh with autosuggestions reprints the input line as it
    /// arrives, so the echo appears several times before the command runs.
    /// Taken from a real capture against Ubuntu 25.04 running oh-my-zsh, which
    /// is what exposed this: anchoring on the FIRST echo left two partial
    /// redraws in the output the agent received.
    #[test]
    fn strip_echo_survives_a_shell_that_redraws_its_input_line() {
        let typed = sentinel().command_line("echo hello", false);
        let line = typed.trim_end_matches('\r');
        // A redraw, then the real line, then the output.
        let output = format!(" ~ ✔ ubuntu@vps {line}\n ~ ✔ ubuntu@vps e{line}\nhello\n");
        assert_eq!(strip_echo(&output, &typed), "hello\n");
    }

    /// With echo off there is no input line to drop, and removing one anyway
    /// would eat the first line of real output.
    #[test]
    fn strip_echo_leaves_output_alone_when_nothing_was_echoed() {
        let typed = sentinel().command_line("uptime", false);
        assert_eq!(strip_echo("just output\n", &typed), "just output\n");
    }

    /// CRLF is what a real PTY sends; the stripper collapses it, but a raw
    /// buffer may still carry it.
    #[test]
    fn strip_echo_accepts_crlf_after_the_echo() {
        let typed = sentinel().command_line("echo hello", false);
        let output = format!("$ {}\r\nhello\n", typed.trim_end_matches('\r'));
        assert_eq!(strip_echo(&output, &typed), "hello\n");
    }

    /// The anchor is short now, so output that merely *mentions* it must not be
    /// mistaken for the echo — only an occurrence that ends a line counts.
    #[test]
    fn strip_echo_ignores_the_anchor_inside_real_output() {
        let typed = sentinel().command_line("grep luma file", false);
        let output = format!(
            "$ {}\nmatched: run it with ; __luma appended for status\n",
            typed.trim_end_matches('\r')
        );
        assert_eq!(
            strip_echo(&output, &typed),
            "matched: run it with ; __luma appended for status\n"
        );
    }
}
