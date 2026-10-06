#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::mask_stages::main() {
        eprintln!("mask stages: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Mask stage capture is available only in debug builds.");
    std::process::exit(1);
}
