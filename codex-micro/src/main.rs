fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if args.next().is_some() {
        anyhow::bail!("usage: codex-micro <serve|install|status|authorize|uninstall>")
    }
    match command.as_deref() {
        Some("serve") => codex_micro::service::run(),
        Some("install") => {
            println!(
                "{}",
                serde_json::to_string(&codex_micro::lifecycle::install()?)?
            );
            Ok(())
        }
        Some("status") => {
            println!(
                "{}",
                serde_json::to_string(&codex_micro::lifecycle::status()?)?
            );
            Ok(())
        }
        Some("authorize") => codex_micro::lifecycle::authorize(),
        Some("uninstall") => codex_micro::lifecycle::uninstall(),
        _ => anyhow::bail!("usage: codex-micro <serve|install|status|authorize|uninstall>"),
    }
}
