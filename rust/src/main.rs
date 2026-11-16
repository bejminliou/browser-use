//! Binary entrypoint: no args (or `mcp`) runs the stdio MCP server; anything
//! else is the terminal CLI.

use browser_use::{cli, mcp};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let code = match args.first().map(String::as_str) {
        None | Some("mcp") => mcp::run().await,
        _ => cli::run(args).await,
    };

    std::process::exit(code);
}
