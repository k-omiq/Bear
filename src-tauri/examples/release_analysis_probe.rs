#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::release_analysis_probe::main() {
        eprintln!("release analysis probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Release analysis probe is available only in debug builds.");
    std::process::exit(1);
}
