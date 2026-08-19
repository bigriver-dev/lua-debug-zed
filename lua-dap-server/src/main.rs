pub mod dap;
pub mod engine;

use anyhow::Result;
use dap::session::DapSession;
use tokio::io::{stdin, stdout};

/*
 * boots DAP session loop
 */
#[tokio::main]
async fn main() -> Result<()> {
    let stdin = stdin();
    let stdout = stdout();

    let mut session = DapSession::new(stdin, stdout);

    // primary DAP request/response loop
    if let Err(err) = session.run_loop().await {
        eprintln!("DAP Session error: {:?}", err);
    }

    Ok(())
}
