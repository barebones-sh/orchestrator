use anyhow::{Context, Result};

mod activate_inject_restore;
mod inject_portal_libei;
mod kglobalaccel_raw;
mod kwin_windows;
mod portal_shortcuts;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let phase = args.next().unwrap_or_default();
    match phase.as_str() {
        "kglobalaccel-raw" => kglobalaccel_raw::run().await,
        "portal-shortcuts" => portal_shortcuts::run().await,
        "kwin-windows" => kwin_windows::run().await,
        "inject-portal-libei" => inject_portal_libei::run().await,
        "activate-inject-restore" => {
            let idx: usize = args
                .next()
                .context("usage: spike activate-inject-restore <window-index>")?
                .parse()
                .context("window index must be a number")?;
            activate_inject_restore::run(idx).await
        }
        _ => {
            eprintln!("usage: spike <phase>\nphases: kglobalaccel-raw, portal-shortcuts, kwin-windows, activate-inject-restore <index>");
            std::process::exit(2);
        }
    }
}
