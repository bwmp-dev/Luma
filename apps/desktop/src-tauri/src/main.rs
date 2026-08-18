// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--mcp-stdio` turns this executable into the stdio bridge for an MCP
    // client, so it must return before Tauri initialises: nothing GUI runs in
    // that process, and in particular the single-instance plugin never gets a
    // chance to hand the arguments to an already-running Luma. Matched strictly
    // against the first argument, since deep links also arrive as argv.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == luma_lib::mcp::stdio::STDIO_FLAG)
    {
        std::process::exit(luma_lib::mcp::stdio::run());
    }

    luma_lib::run()
}
