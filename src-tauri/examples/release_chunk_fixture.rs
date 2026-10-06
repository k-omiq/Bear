#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::release_chunk_fixture::main() {
        eprintln!("release chunk fixture: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Release chunk fixture is available only in debug builds.");
    std::process::exit(1);
}
