use anyhow::{Context, Result, bail};

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-micro-hid: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let first = args
        .next()
        .context("usage: herdr-micro-hid <allowed-uid>")?;
    if first == "--version" {
        if args.next().is_some() {
            bail!("usage: herdr-micro-hid --version")
        }
        println!("{}", herdr_micro::hid::running_helper_version()?);
        return Ok(());
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        bail!("must run as root")
    }
    let allowed_uid = first
        .parse::<libc::uid_t>()
        .context("allowed uid must be numeric")?;
    if allowed_uid == 0 {
        bail!("allowed uid must be non-root")
    }
    if args.next().is_some() {
        bail!("usage: herdr-micro-hid <allowed-uid>")
    }
    herdr_micro::hid::run_hid_server(allowed_uid)
}
