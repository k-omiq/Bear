#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::live_demo::main() {
        eprintln!("live demo: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The live demo runner is available only in debug builds.");
    std::process::exit(1);
}
