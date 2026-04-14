use anyhow::Result;
use apple_silicon_fan_control::helper_server;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "apple-silicon-fan-control-helper",
    version,
    about = "Root helper daemon for Apple Silicon fan control"
)]
struct Args {
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    helper_server::serve(args.socket)
}
