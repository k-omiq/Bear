#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::cloud_attempt_probe::main() {
        eprintln!("cloud attempt probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Cloud attempt probe is available only in debug builds.");
    std::process::exit(1);
}
