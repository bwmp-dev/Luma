//! Finding where an agent's command ends inside a shell it shares with the user.
//!
//! An agent tab is a real interactive shell: the user can type in it, and `cd`
//! or `export` carry over between the agent's commands and the user's. There is
//! no exec-channel exit status on that path, so completion is read from the
//! output itself. A helper is defined once per shell:
//!
//! ```text
//! __luma() { printf '\n__LUMA_MCP_<tab>:%s:%s\n' "$1" "$?"; }
//! ```
//!
//! and every command afterwards is submitted as:
//!
//! ```text
//! <command>; __luma <token>
//! ```
//!
//! This is the one place Luma reads meaning out of terminal output, and it is
//! deliberately narrow: the marker only ends the agent's own capture of its own
//! command. It never drives authentication, prompts or any other session state,
//! and a host that prints a fake marker can at worst cut short the output of a
//! command it is already running.
//!
//! Load-bearing properties:
//!
//! * **The command and `__luma` are submitted as one line.** Nothing is left
//!   queued in the tty input buffer, so a `sudo` password prompt reads from an
//!   empty tty and waits for the user instead of eating a queued sentinel line.
//! * **The line starts with Ctrl+U.** The user may have half a command typed at
//!   the prompt; without clearing it first, the agent's command would be
//!   appended to theirs and run as one.
//! * **Each command has its own token.** A command that outlived its timeout
//!   prints its marker later, into the next command's capture; the token keeps
//!   that late marker from completing a command it does not belong to.
//!
//! Known limits: the helper is POSIX shell syntax, so fish and Windows shells
//! cannot run it (Windows hosts use exec channels instead), and a command whose
//! last part reads stdin (a heredoc, a bare `cat`) swallows the helper call and
//! runs to its timeout.

/// Marker prefix, paired with a per-tab id so output from another tab or an
/// earlier shell cannot match.
const MARKER_PREFIX: &str = "__LUMA_MCP_";

/// Shell function name. Short, because the user reads it after every command.
const HELPER: &str = "__luma";

/// Readline's kill-line: clears whatever the user had typed at the prompt.
const CLEAR_LINE: char = '\u{15}';

/// One shell's sentinel: the helper that prints it and the parser that finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sentinel {
    /// The marker *including* its trailing separator, exactly as printed. Both
    /// the printf format and the parser derive from this one string.
    marker: String,
}

impl Sentinel {
    pub(crate) fn new(tab_id: &str) -> Self {
        Self {
            marker: format!("{MARKER_PREFIX}{tab_id}:"),
        }
    }

    /// What the helper prints for `token`, up to the exit code.
    pub(crate) fn needle(&self, token: &str) -> String {
        format!("{}{token}:", self.marker)
    }

    /// The helper definition, without a terminator.
    ///
    /// `printf` rather than `echo` because `echo` interprets backslashes in some
    /// shells. `$?` still holds the status of the command before the call: the
    /// call itself does not reset it, and expanding `$1` does not either.
    ///
    /// The marker line is printed and erased in the same write (`ESC[1A`, then
    /// `ESC[2K`), so the user never sees a line of hex after every command.
    /// Luma still reads it: the capture strips escape sequences and keeps text.
    fn definition(&self) -> String {
        format!(
            "{HELPER}() {{ printf '\\n{}%s:%s\\n\\033[1A\\r\\033[2K' \"$1\" \"$?\"; }}",
            self.marker
        )
    }

    /// The line to type: clear the prompt, the command, the helper call, Enter.
    ///
    /// `define` puts the helper definition on the same line, for the first
    /// command in a shell. Two writes would leave the command queued in the tty
    /// while the definition runs, the same hazard as a queued sentinel line.
    pub(crate) fn command_line(&self, command: &str, token: &str, define: bool) -> String {
        // A trailing newline would submit the command early and leave the
        // helper call on its own line, reporting the status of the wrong thing.
        let command = command.trim_end_matches(['\r', '\n']);
        if define {
            format!(
                "{CLEAR_LINE}{}; {command}; {HELPER} {token}\r",
                self.definition()
            )
        } else {
            format!("{CLEAR_LINE}{command}; {HELPER} {token}\r")
        }
    }

    /// Locate `token`'s marker in stripped output.
    ///
    /// Returns the text before it and the reported exit code; `None` while the
    /// command is still running. Only a marker at the start of a line counts:
    /// the terminal's echo of the command line has it mid-line, if at all.
    pub(crate) fn parse<'a>(&self, output: &'a str, token: &str) -> Option<(&'a str, Option<u32>)> {
        let needle = self.needle(token);
        let mut search_from = 0;
        while let Some(offset) = output[search_from..].find(&needle) {
            let start = search_from + offset;
            let at_line_start = start == 0 || output.as_bytes()[start - 1] == b'\n';
            if !at_line_start {
                search_from = start + needle.len();
                continue;
            }
            let after = &output[start + needle.len()..];
            // Without the newline printf emits, the line is still arriving.
            let newline = after.find('\n')?;
            let code = after[..newline].trim().parse::<u32>().ok();
            return Some((&output[..start], code));
        }
        None
    }
}

/// Drop the prompt and the terminal's echo of the typed line, so the agent sees
/// output rather than a transcript.
///
/// Only when the echo is actually there: with echo off there is no input line,
/// and dropping one anyway would eat a real line of output.
pub(crate) fn strip_echo<'a>(output: &'a str, typed: &str) -> &'a str {
    // The echo wraps at the terminal width, so anchor on the tail of what was
    // typed, `; __luma <token>`, rather than on the whole line.
    let line = typed.trim_end_matches(['\r', '\n']);
    let Some(cut) = line.rfind(';') else {
        return output;
    };
    let anchor = &line[cut..];

    // The LAST occurrence that ends a line: an interactive shell redraws its
    // input as it arrives (zsh-autosuggestions, syntax highlighting), and only
    // the final redraw is followed by the command running.
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
    use crate::mcp::taps::strip_terminal_output;

    fn sentinel() -> Sentinel {
        Sentinel::new("abc123")
    }

    #[test]
    fn the_command_line_clears_the_prompt_and_submits_once() {
        let line = sentinel().command_line("uptime", "t1", false);
        assert_eq!(line, "\u{15}uptime; __luma t1\r");
        assert_eq!(line.matches('\r').count(), 1);
        // The marker is not on the command line, so its echo cannot match.
        assert!(!line.contains("__LUMA_MCP_"));
    }

    #[test]
    fn the_first_command_defines_the_helper_on_the_same_line() {
        let line = sentinel().command_line("uptime", "t1", true);
        assert_eq!(line.matches('\r').count(), 1);
        assert!(line.starts_with("\u{15}__luma() {"));
        assert!(line.contains("__LUMA_MCP_abc123:"));
        assert!(line.contains("\"$1\" \"$?\""));
        assert!(line.ends_with("; uptime; __luma t1\r"));
    }

    #[test]
    fn a_trailing_newline_in_the_command_is_trimmed() {
        let line = sentinel().command_line("uptime\n", "t1", false);
        assert_eq!(line, "\u{15}uptime; __luma t1\r");
    }

    /// The producer and the parser must agree on the marker's exact bytes:
    /// take the literal format straight from the helper and feed it back.
    #[test]
    fn what_the_helper_prints_is_what_the_parser_matches() {
        let sentinel = sentinel();
        let setup = sentinel.command_line("true", "t1", true);
        let format = setup
            .split_once("printf '")
            .and_then(|(_, rest)| rest.split_once("%s:%s"))
            .map(|(format, _)| format)
            .expect("defining command line should contain a printf format");
        let emitted = format.strip_prefix("\\n").expect("format starts with \\n");

        let output = format!("prior output\n{emitted}t1:0\n");
        let (text, code) = sentinel
            .parse(&output, "t1")
            .expect("the parser must match what the helper emits");
        assert_eq!(code, Some(0));
        assert_eq!(text, "prior output\n");
    }

    #[test]
    fn parse_returns_output_and_exit_code() {
        let (text, code) = sentinel()
            .parse("hello\nworld\n__LUMA_MCP_abc123:t1:127\n", "t1")
            .unwrap();
        assert_eq!(text, "hello\nworld\n");
        assert_eq!(code, Some(127));
    }

    #[test]
    fn parse_waits_while_the_command_is_still_running() {
        assert!(sentinel().parse("partial output\n", "t1").is_none());
        assert!(sentinel()
            .parse("out\n__LUMA_MCP_abc123:t1:", "t1")
            .is_none());
        assert!(sentinel()
            .parse("out\n__LUMA_MCP_abc123:t1:1", "t1")
            .is_none());
    }

    /// A command that outlived its timeout reports later, inside the next
    /// command's capture. Its marker must not end the next command.
    #[test]
    fn another_commands_marker_does_not_complete_this_one() {
        let output = "late\n__LUMA_MCP_abc123:t1:0\nmine\n";
        assert!(sentinel().parse(output, "t2").is_none());
        let full = format!("{output}__LUMA_MCP_abc123:t2:0\n");
        let (text, _) = sentinel().parse(&full, "t2").unwrap();
        assert!(text.ends_with("mine\n"));
    }

    #[test]
    fn output_mentioning_the_marker_mid_line_does_not_end_the_command() {
        let output = "saw __LUMA_MCP_abc123:t1:0 inline\n__LUMA_MCP_other:t1:0\n";
        assert!(sentinel().parse(output, "t1").is_none());
    }

    #[test]
    fn the_echoed_setup_line_is_not_mistaken_for_the_sentinel() {
        let sentinel = sentinel();
        let typed = sentinel.command_line("uptime", "t1", true);
        let echo = format!("$ {}\n", typed.trim_end_matches('\r'));
        assert!(sentinel.parse(&echo, "t1").is_none());

        let full = format!("{echo}12:00:00 up 3 days\n__LUMA_MCP_abc123:t1:0\n");
        let (text, code) = sentinel.parse(&full, "t1").unwrap();
        assert_eq!(code, Some(0));
        assert_eq!(strip_echo(text, &typed), "12:00:00 up 3 days\n");
    }

    /// What a real PTY sends back, through the same stripper the capture uses:
    /// a coloured prompt, the echo, output, then the erased marker, in CRLF.
    #[test]
    fn a_sentinel_survives_terminal_stripping() {
        let sentinel = sentinel();
        let typed = sentinel.command_line("echo hi", "t1", false);
        let raw = format!(
            "\x1b[32muser@host\x1b[0m:~$ {}\r\nhi\r\n\r\n__LUMA_MCP_abc123:t1:0\r\n\x1b[1A\r\x1b[2K",
            typed.trim_end_matches('\r').trim_start_matches(CLEAR_LINE)
        );
        let stripped = strip_terminal_output(raw.as_bytes());
        let (output, code) = sentinel
            .parse(&stripped, "t1")
            .expect("sentinel should be found in stripped output");
        assert_eq!(code, Some(0));
        assert_eq!(strip_echo(output, &typed).trim_end(), "hi");
    }

    #[test]
    fn strip_echo_survives_a_shell_that_redraws_its_input_line() {
        let typed = sentinel().command_line("echo hello", "t1", false);
        let line = typed.trim_end_matches('\r').trim_start_matches(CLEAR_LINE);
        let output = format!(" ~ ubuntu@vps {line}\n ~ ubuntu@vps e{line}\nhello\n");
        assert_eq!(strip_echo(&output, &typed), "hello\n");
    }

    #[test]
    fn strip_echo_leaves_output_alone_when_nothing_was_echoed() {
        let typed = sentinel().command_line("uptime", "t1", false);
        assert_eq!(strip_echo("just output\n", &typed), "just output\n");
    }

    #[test]
    fn strip_echo_ignores_the_anchor_inside_real_output() {
        let typed = sentinel().command_line("grep luma file", "t1", false);
        let output = format!(
            "$ {}\nmatched: run it with ; __luma t1 appended\n",
            typed.trim_end_matches('\r')
        );
        assert_eq!(
            strip_echo(&output, &typed),
            "matched: run it with ; __luma t1 appended\n"
        );
    }
}
