#[cfg(debug_assertions)]
fn main() {
    if let Err(error) = manga_cleaner_lib::release_recipe_probe::main() {
        eprintln!("release recipe probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("Release recipe probe is available only in debug builds.");
    std::process::exit(1);
}
