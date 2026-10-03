mod gui;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // No args -> GUI. Any args -> headless CLI (or --help/--version).
    if args.is_empty() {
        std::process::exit(gui::run());
    } else {
        std::process::exit(astctool::cli::run(&args));
    }
}
