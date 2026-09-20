mod config;
mod daemon;
mod device;
mod frontends;
mod plus;
mod render;
mod routing;
mod slots;
mod system_input;

fn main() {
    if let Err(error) = daemon::main(std::env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
