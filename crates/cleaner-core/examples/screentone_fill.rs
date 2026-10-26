//! Runs the screentone fill on one grayscale crop, for the research harness in
//! `spikes/screentone-fill/` (candidate `c_rust`), which compares it with the
//! Python prototype on the corpus.
//!
//! usage: screentone_fill <w> <h> <gray.bin> <hole.bin> <avoid.bin> <out.bin> <used.bin>
//! Every file is `w * h` bytes; masks are 0 or 1.

use cleaner_core::engines::grid::{Grid, Mask};
use cleaner_core::engines::screentone;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (w, h): (usize, usize) = (a[1].parse().unwrap(), a[2].parse().unwrap());
    let read = |p: &str| std::fs::read(p).unwrap();
    let mask = |p: &str| Mask { w, h, data: read(p).iter().map(|&v| v != 0).collect() };
    let gray = Grid::from_u8(w, h, &read(&a[3]));
    let f = screentone::fill(&gray, &mask(&a[4]), &mask(&a[5]));
    std::fs::write(&a[6], f.gray.to_u8()).unwrap();
    std::fs::write(&a[7], f.used.data.iter().map(|&v| v as u8).collect::<Vec<_>>()).unwrap();
}
