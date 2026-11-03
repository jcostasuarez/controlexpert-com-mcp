//! Command-line entry point: parses arguments and serves MCP over stdio.
use controlexpert_com_mcp::{
    server::{SDK, SERVER_NAME, Server},
    versions::Profile,
};
use rmcp::{ServiceExt, transport::stdio};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut version = "15.1".to_owned();
    let mut slow_call_after_ms = 10_000u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ce-version" => version = args.next().ok_or("Missing value for --ce-version")?,
            "--version" => {
                println!(
                    "{SERVER_NAME} {} ({SDK}; Control Expert 15.1)",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            "--slow-call-ms" => {
                slow_call_after_ms = args
                    .next()
                    .ok_or("Missing value for --slow-call-ms")?
                    .parse()?;
                if slow_call_after_ms == 0 {
                    return Err("--slow-call-ms must be greater than zero".into());
                }
            }
            "--help" | "-h" => {
                println!(
                    "{SERVER_NAME} [--ce-version 15.1] [--slow-call-ms 10000]\n--slow-call-ms sets wait warnings; it does not cancel COM. --version prints the version."
                );
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    Profile::select(&version)?;
    let server = Server::with_options(&version, Duration::from_millis(slow_call_after_ms))?;
    let runtime = server.runtime.clone();
    eprintln!("{SERVER_NAME}: Control Expert {version}, stdio transport, {SDK}");
    let serving: Result<(), Box<dyn std::error::Error>> = async {
        let service = server.serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    }
    .await;
    let shutdown = runtime.shutdown().await;
    serving?;
    shutdown?;
    Ok(())
}
