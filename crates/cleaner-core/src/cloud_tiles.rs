//! Bounded upload planning in source page coordinates, and stitching the
//! tiles' answers back into one page.
//!
//! ## Overlap, ownership and stitching
//!
//! Cloud analysis cuts a page into tiles of at most [`MAX_TILE_SIDE`]. A glyph
//! or a detector box that a tile edge cuts is seen in two halves, each without
//! the context the model needs, so tiles **overlap**:
//!
//! - **Tiles.** Along each axis a page longer than 1024 is covered by tiles of
//!   exactly 1024, the first at 0 and the last flush with the far edge, spaced
//!   evenly (integer division) so neighbours overlap by at least
//!   `2 * TILE_CONTEXT`. Every tile of a page is therefore the same size and
//!   reaches the models at the same scale; an axis of 1024 or less is one tile.
//! - **Cores.** Each tile *owns* a core: the page is cut at the midpoint of each
//!   overlap, so cores partition the page and every core keeps at least
//!   [`TILE_CONTEXT`] pixels of its tile on each side that has a neighbour.
//! - **Masks** are stitched by ownership: a page pixel takes its value from the
//!   tile whose core holds it, never from a tile edge.
//! - **Boxes** from different tiles are joined in [`merge_tile_boxes`]: a box
//!   seen whole by one tile wins over the pieces other tiles cut, and a box
//!   every tile cuts is rebuilt as the hull of its pieces.
//!
//! The same plan is implemented by the gateway
//! (`deploy/cloud/common/contract.py::analysis_tile_plan`), which refuses a
//! tile that is not on it; both are checked against
//! `deploy/cloud/fixtures/analysis_v1/tile_plans.json`.
use crate::balloon::BalloonBox;
use crate::cloud_analysis_wire::{TileRect, MAX_TILE_SIDE};
use crate::mask::Rect;

pub const MAX_TILES: usize = 256;
pub const MAX_TOTAL_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_ESTIMATED_BYTES: u64 = 256 * 1024 * 1024;
/// The least context a tile carries past each side of its core that has a
/// neighbour. Neighbouring tiles overlap by at least twice this.
pub const TILE_CONTEXT: u32 = 128;
/// A plan of more tiles than this (columns times rows) is refused before it
/// is built, and so is every tile of such a page.
const MAX_GRID: u64 = 1_000_000;
/// Pixels from a tile's inner edge within which a box counts as cut by it.
const CUT_SLACK: i64 = 2;

#[derive(Debug, Clone)]
pub struct UploadExtent {
    pub tiles: Vec<TileRect>,
    /// Each tile's core, in the same order as `tiles`.
    pub cores: Vec<TileRect>,
    pub total_pixels: u64,
    pub encoded_byte_estimate: u64,
    pub pages: u32,
}

/// One tile and the part of the page it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedTile {
    pub tile: TileRect,
    pub core: TileRect,
}

/// One axis position: tile start, tile length, core start, core length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: u32,
    len: u32,
    core_start: u32,
    core_len: u32,
}

/// Tiles along an axis of `extent` pixels.
fn count(extent: u32) -> u64 {
    if extent <= MAX_TILE_SIDE {
        return 1;
    }
    let side = u64::from(MAX_TILE_SIDE);
    1 + (u64::from(extent) - side).div_ceil(side - 2 * u64::from(TILE_CONTEXT))
}

/// Both axes of a page's plan, or an error for an empty page or one whose
/// plan would exceed [`MAX_GRID`] tiles. Every function that plans, names or
/// checks a tile goes through here, so none accepts a tile of a page the
/// planner refuses.
fn grid(page_width: u32, page_height: u32) -> Result<(Vec<Span>, Vec<Span>), String> {
    if page_width == 0 || page_height == 0 {
        return Err("empty analysis page".into());
    }
    if count(page_width).checked_mul(count(page_height)).is_none_or(|v| v > MAX_GRID) {
        return Err("analysis page grid too large".into());
    }
    Ok((axis(page_width), axis(page_height)))
}

fn axis(extent: u32) -> Vec<Span> {
    if extent <= MAX_TILE_SIDE {
        return vec![Span { start: 0, len: extent, core_start: 0, core_len: extent }];
    }
    let side = u64::from(MAX_TILE_SIDE);
    let span = u64::from(extent) - side;
    let count = count(extent);
    let origin = |k: u64| k * span / (count - 1);
    let mut cuts = Vec::with_capacity(count as usize + 1);
    cuts.push(0u64);
    for k in 1..count {
        cuts.push((origin(k - 1) + side + origin(k)) / 2);
    }
    cuts.push(u64::from(extent));
    (0..count as usize)
        .map(|k| Span {
            start: origin(k as u64) as u32,
            len: MAX_TILE_SIDE,
            core_start: cuts[k] as u32,
            core_len: (cuts[k + 1] - cuts[k]) as u32,
        })
        .collect()
}

fn intersect(a: TileRect, b: TileRect) -> Option<TileRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom = a.y.saturating_add(a.height).min(b.y.saturating_add(b.height));
    if right > x && bottom > y {
        Some(TileRect { x, y, width: right - x, height: bottom - y })
    } else {
        None
    }
}

/// The whole plan for a page, row by row, left to right.
pub fn planned(page_width: u32, page_height: u32) -> Result<Vec<PlannedTile>, String> {
    let (columns, rows) = grid(page_width, page_height)?;
    Ok(rows
        .iter()
        .flat_map(|row| {
            columns.iter().map(move |column| PlannedTile {
                tile: TileRect { x: column.start, y: row.start, width: column.len, height: row.len },
                core: TileRect { x: column.core_start, y: row.core_start, width: column.core_len, height: row.core_len },
            })
        })
        .collect())
}

/// The tiles to upload for `regions` (the whole page when empty): every tile
/// whose core meets one, so each region's pixels come back from their owner.
pub fn plan(page_width: u32, page_height: u32, regions: &[TileRect]) -> Result<UploadExtent, String> {
    if page_width == 0 || page_height == 0 { return Err("empty analysis page".into()); }
    let page = TileRect { x: 0, y: 0, width: page_width, height: page_height };
    let requests = if regions.is_empty() { vec![page] } else { regions.to_vec() };
    for region in &requests {
        if region.width == 0 || region.height == 0 || region.x.checked_add(region.width).is_none_or(|v| v > page_width)
            || region.y.checked_add(region.height).is_none_or(|v| v > page_height)
        { return Err("analysis region outside page".into()); }
    }
    let (columns, rows) = grid(page_width, page_height)?;
    let (mut tiles, mut cores) = (Vec::new(), Vec::new());
    for row in &rows {
        for column in &columns {
            let core = TileRect { x: column.core_start, y: row.core_start, width: column.core_len, height: row.core_len };
            if requests.iter().any(|r| intersect(core, *r).is_some()) {
                tiles.push(TileRect { x: column.start, y: row.start, width: column.len, height: row.len });
                cores.push(core);
                if tiles.len() > MAX_TILES { return Err("analysis tile count exceeded".into()); }
            }
        }
    }
    let total_pixels = tiles.iter().map(|r| u64::from(r.width) * u64::from(r.height)).sum::<u64>();
    let encoded_byte_estimate = total_pixels.checked_mul(4).ok_or("analysis estimate overflow")?;
    if total_pixels > MAX_TOTAL_PIXELS || encoded_byte_estimate > MAX_ESTIMATED_BYTES {
        return Err("analysis upload extent exceeded".into());
    }
    Ok(UploadExtent { tiles, cores, total_pixels, encoded_byte_estimate, pages: 1 })
}

/// The core a tile of this page's plan owns, or `None` for a tile that is not
/// on the plan.
pub fn core_of(page_width: u32, page_height: u32, tile: TileRect) -> Option<TileRect> {
    let (columns, rows) = grid(page_width, page_height).ok()?;
    let column = columns.iter().find(|s| s.start == tile.x && s.len == tile.width)?;
    let row = rows.iter().find(|s| s.start == tile.y && s.len == tile.height)?;
    Some(TileRect { x: column.core_start, y: row.core_start, width: column.core_len, height: row.core_len })
}

/// The lines where stitched evidence changes owner: the internal core edges,
/// `x = n` and `y = n`. A glyph crossing one is assembled from two tiles;
/// grouping joins what those lines cut ([`crate::text_groups::Seam`]).
pub fn core_boundaries(page_width: u32, page_height: u32) -> (Vec<u32>, Vec<u32>) {
    let Ok((columns, rows)) = grid(page_width, page_height) else { return (Vec::new(), Vec::new()) };
    let cuts = |spans: Vec<Span>| -> Vec<u32> { spans.iter().skip(1).map(|s| s.core_start).collect() };
    (cuts(columns), cuts(rows))
}

/// Copy the part of one tile's mask its core owns into the page mask.
/// `tile_mask` is the tile's own raster, one byte per pixel.
pub fn stitch_mask(page: &mut [u8], page_width: u32, tile: TileRect, core: TileRect, tile_mask: &[u8]) {
    let (dx, dy) = ((core.x - tile.x) as usize, (core.y - tile.y) as usize);
    for y in 0..core.height as usize {
        let source = (dy + y) * tile.width as usize + dx;
        let target = (core.y as usize + y) * page_width as usize + core.x as usize;
        page[target..target + core.width as usize].copy_from_slice(&tile_mask[source..source + core.width as usize]);
    }
}

/// A detector box as one tile reported it, in page coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct TileBox {
    pub tile: TileRect,
    pub core: TileRect,
    pub found: BalloonBox,
}

#[derive(Clone, Copy, Default)]
struct Cuts {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Cuts {
    fn of(item: &TileBox, page_width: u32, page_height: u32) -> Cuts {
        let (t, r) = (item.tile, item.found.rect);
        let (tl, tt) = (i64::from(t.x), i64::from(t.y));
        let (tr, tb) = (tl + i64::from(t.width), tt + i64::from(t.height));
        Cuts {
            left: tl > 0 && r.x <= tl + CUT_SLACK,
            right: tr < i64::from(page_width) && r.right() >= tr - CUT_SLACK,
            top: tt > 0 && r.y <= tt + CUT_SLACK,
            bottom: tb < i64::from(page_height) && r.bottom() >= tb - CUT_SLACK,
        }
    }

    fn any(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

fn area(r: &Rect) -> i64 {
    i64::from(r.w) * i64::from(r.h)
}

fn overlap(a0: i64, a1: i64, b0: i64, b1: i64) -> i64 {
    (a1.min(b1) - a0.max(b0)).max(0)
}

fn hull(a: &Rect, b: &Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(x, y, (a.right().max(b.right()) - x) as u32, (a.bottom().max(b.bottom()) - y) as u32)
}

/// Whether `b` carries on past where `a`'s tile cut it: `b` crosses the cut
/// line and shares at least half the smaller extent along it.
fn continues(a: &TileBox, cuts: Cuts, b: &TileBox) -> bool {
    let (ra, rb) = (a.found.rect, b.found.rect);
    let t = a.tile;
    let rows = overlap(ra.y, ra.bottom(), rb.y, rb.bottom()) * 2 >= i64::from(ra.h.min(rb.h));
    let columns = overlap(ra.x, ra.right(), rb.x, rb.right()) * 2 >= i64::from(ra.w.min(rb.w));
    let crosses = |line: i64, from: i64, to: i64| from < line && to > line;
    let (tl, tt) = (i64::from(t.x), i64::from(t.y));
    let (tr, tb) = (tl + i64::from(t.width), tt + i64::from(t.height));
    (cuts.right && rows && crosses(tr - CUT_SLACK, rb.x, rb.right()) && rb.right() > ra.right())
        || (cuts.left && rows && crosses(tl + CUT_SLACK, rb.x, rb.right()) && rb.x < ra.x)
        || (cuts.bottom && columns && crosses(tb - CUT_SLACK, rb.y, rb.bottom()) && rb.bottom() > ra.bottom())
        || (cuts.top && columns && crosses(tt + CUT_SLACK, rb.y, rb.bottom()) && rb.y < ra.y)
}

/// Join the boxes every tile reported into one page's boxes.
///
/// Boxes of one class from different tiles are the same object when they
/// overlap by at least half the smaller box, or when one was cut by its tile's
/// inner edge and the other carries on past that edge. Within each such
/// cluster, the tile with the most uncut area supplies its uncut boxes (a box
/// seen whole beats every piece of it); a cluster of cut pieces only is
/// rebuilt as their hull, at the best score. Deterministic: the result does
/// not depend on the order tiles answered in.
pub fn merge_tile_boxes(page_width: u32, page_height: u32, items: &[TileBox]) -> Vec<BalloonBox> {
    let n = items.len();
    let cuts: Vec<Cuts> = items.iter().map(|item| Cuts::of(item, page_width, page_height)).collect();
    let mut root: Vec<usize> = (0..n).collect();
    fn find(root: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for a in 0..n {
        for b in a + 1..n {
            let (x, y) = (&items[a], &items[b]);
            if x.tile == y.tile || x.found.class != y.found.class {
                continue;
            }
            let (rx, ry) = (x.found.rect, y.found.rect);
            let shared = overlap(rx.x, rx.right(), ry.x, ry.right()) * overlap(rx.y, rx.bottom(), ry.y, ry.bottom());
            let duplicate = shared > 0 && shared * 2 >= area(&rx).min(area(&ry));
            if duplicate || continues(x, cuts[a], y) || continues(y, cuts[b], x) {
                let (ra, rb) = (find(&mut root, a), find(&mut root, b));
                let (low, high) = (ra.min(rb), ra.max(rb));
                root[high] = low;
            }
        }
    }
    let mut clusters: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..n {
        let r = find(&mut root, i);
        clusters.entry(r).or_default().push(i);
    }
    let tile_key = |t: TileRect| (t.y, t.x);
    let mut out = Vec::new();
    for members in clusters.values() {
        let uncut: Vec<usize> = members.iter().copied().filter(|&i| !cuts[i].any()).collect();
        if uncut.is_empty() {
            let rect = members.iter().map(|&i| items[i].found.rect).reduce(|a, b| hull(&a, &b)).unwrap();
            let score = members.iter().map(|&i| items[i].found.score).fold(f32::MIN, f32::max);
            out.push(BalloonBox { rect, class: items[members[0]].found.class, score });
            continue;
        }
        // The tile that saw most of this object whole.
        let owned = |i: usize| {
            let r = items[i].found.rect;
            let (cx, cy) = (r.x + i64::from(r.w) / 2, r.y + i64::from(r.h) / 2);
            let c = items[i].core;
            cx >= i64::from(c.x) && cx < i64::from(c.x + c.width) && cy >= i64::from(c.y) && cy < i64::from(c.y + c.height)
        };
        let mut tiles: Vec<TileRect> = uncut.iter().map(|&i| items[i].tile).collect();
        tiles.sort_by_key(|t| tile_key(*t));
        tiles.dedup();
        let best = tiles
            .iter()
            .copied()
            .max_by(|&p, &q| {
                let weight = |t: TileRect| {
                    let chosen = uncut.iter().copied().filter(|&i| items[i].tile == t);
                    let covered: i64 = chosen.clone().map(|i| area(&items[i].found.rect)).sum();
                    let owns = chosen.clone().filter(|&i| owned(i)).count();
                    let score = chosen.map(|i| items[i].found.score).fold(f32::MIN, f32::max);
                    (covered, owns, score)
                };
                let (wp, wq) = (weight(p), weight(q));
                wp.0.cmp(&wq.0)
                    .then(wp.1.cmp(&wq.1))
                    .then(wp.2.total_cmp(&wq.2))
                    .then(tile_key(q).cmp(&tile_key(p)))
            })
            .unwrap();
        out.extend(uncut.iter().filter(|&&i| items[i].tile == best).map(|&i| items[i].found.clone()));
    }
    out.sort_by(|a, b| {
        (a.rect.y, a.rect.x, a.rect.h, a.rect.w)
            .cmp(&(b.rect.y, b.rect.x, b.rect.h, b.rect.w))
            .then(b.score.total_cmp(&a.score))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balloon::BalloonClass;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> TileRect {
        TileRect { x, y, width, height }
    }

    #[test]
    fn tiles_overlap_and_cores_partition_the_page() {
        for (w, h) in [(1136, 1601), (2048, 2048), (1025, 700), (4000, 6000), (800, 20000), (1792, 1793)] {
            let plan = planned(w, h).unwrap();
            let mut owner = vec![0u8; (w * h) as usize];
            for p in &plan {
                assert!(p.tile.width <= MAX_TILE_SIDE && p.tile.height <= MAX_TILE_SIDE);
                assert!(p.tile.x + p.tile.width <= w && p.tile.y + p.tile.height <= h);
                // Every tile of a page is one size: one model scale.
                assert_eq!((p.tile.width, p.tile.height), (w.min(1024), h.min(1024)));
                assert!(intersect(p.tile, p.core) == Some(p.core), "core inside its tile");
                // At least TILE_CONTEXT of tile beyond every inner core edge.
                if p.core.x > 0 { assert!(p.core.x - p.tile.x >= TILE_CONTEXT); }
                if p.core.y > 0 { assert!(p.core.y - p.tile.y >= TILE_CONTEXT); }
                if p.core.x + p.core.width < w { assert!(p.tile.x + p.tile.width - (p.core.x + p.core.width) >= TILE_CONTEXT); }
                if p.core.y + p.core.height < h { assert!(p.tile.y + p.tile.height - (p.core.y + p.core.height) >= TILE_CONTEXT); }
                for y in p.core.y..p.core.y + p.core.height {
                    for x in p.core.x..p.core.x + p.core.width {
                        owner[(y * w + x) as usize] += 1;
                    }
                }
                assert_eq!(core_of(w, h, p.tile), Some(p.core));
            }
            assert!(owner.iter().all(|&o| o == 1), "cores partition {w}x{h}");
        }
        assert_eq!(core_of(1136, 1601, rect(1, 0, 1024, 1024)), None);
    }

    #[test]
    fn shared_tile_plan_vectors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/analysis_v1/tile_plans.json")).unwrap();
        for case in fixture["plans"].as_array().unwrap() {
            let (w, h) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
            let expected: Vec<PlannedTile> = case["tiles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    let r = |v: &serde_json::Value| serde_json::from_value::<TileRect>(v.clone()).unwrap();
                    PlannedTile { tile: r(&t["tile"]), core: r(&t["core"]) }
                })
                .collect();
            assert_eq!(planned(w, h).unwrap(), expected, "{w}x{h}");
            let (xs, ys) = core_boundaries(w, h);
            assert_eq!(serde_json::json!([xs, ys]), case["core_boundaries"], "{w}x{h}");
        }
        for case in fixture["refused"].as_array().unwrap() {
            let (w, h) = (case["width"].as_u64().unwrap() as u32, case["height"].as_u64().unwrap() as u32);
            assert!(plan(w, h, &[]).is_err() && planned(w, h).is_err(), "{w}x{h}");
            // No tile of a refused page is on a plan, including one each
            // axis alone would have.
            let tile = TileRect { x: 0, y: 0, width: w.min(MAX_TILE_SIDE), height: h.min(MAX_TILE_SIDE) };
            assert_eq!(core_of(w, h, tile), None, "{w}x{h}");
        }
        // The provenance evidence analysed on this plan records, as the mock
        // records it too.
        assert_eq!(
            serde_json::to_value(crate::text_groups::SpatialInput::OverlappingCloudTiles).unwrap(),
            fixture["spatial"]
        );
    }

    #[test]
    fn cloud_tiles_long_strip_and_regions() {
        let region = TileRect { x: 71, y: 9000, width: 62, height: 1300 };
        let extent = plan(800, 20000, &[region]).unwrap();
        assert!(extent.tiles.iter().all(|t| t.x + t.width <= 800 && t.y + t.height <= 20000));
        // Every region pixel comes back from the tile whose core owns it.
        for y in region.y..region.y + region.height {
            assert!(extent.cores.iter().any(|c| c.y <= y && y < c.y + c.height && c.x <= region.x && region.x + region.width <= c.x + c.width));
        }
        assert_eq!(extent.total_pixels, extent.tiles.iter().map(|t| u64::from(t.width) * u64::from(t.height)).sum::<u64>());
        assert_eq!(extent.encoded_byte_estimate, extent.total_pixels * 4);
        assert_eq!(extent.pages, 1);
        let whole = plan(800, 20000, &[]).unwrap();
        assert_eq!(whole.tiles.len(), 26);
        assert_eq!(whole.total_pixels, 26 * 800 * 1024);
        let crossing = TileRect { x: 1000, y: 1000, width: 50, height: 50 };
        let crossing_plan = plan(2048, 2048, &[crossing]).unwrap();
        for y in crossing.y..crossing.y + crossing.height {
            for x in crossing.x..crossing.x + crossing.width {
                assert!(crossing_plan.cores.iter().any(|c| c.x <= x && x < c.x + c.width && c.y <= y && y < c.y + c.height));
            }
        }
        assert!(plan(800, 400_000, &[]).is_err());
    }

    #[test]
    fn stitching_takes_each_pixel_from_its_owner() {
        let (w, h) = (1136u32, 1100u32);
        let plan = planned(w, h).unwrap();
        let mut page = vec![0u8; (w * h) as usize];
        // Each tile answers with its own index everywhere, edges included.
        for (k, p) in plan.iter().enumerate() {
            let tile = vec![k as u8 + 1; (p.tile.width * p.tile.height) as usize];
            stitch_mask(&mut page, w, p.tile, p.core, &tile);
        }
        for (k, p) in plan.iter().enumerate() {
            for y in p.core.y..p.core.y + p.core.height {
                for x in p.core.x..p.core.x + p.core.width {
                    assert_eq!(page[(y * w + x) as usize], k as u8 + 1);
                }
            }
        }
    }

    /// What tile `k` of `plan` reports of a box at `(x, y, w, h)` on the page.
    fn tile_box(plan: &[PlannedTile], k: usize, (x, y, w, h): (i64, i64, u32, u32), class: BalloonClass, score: f32) -> TileBox {
        let p = plan[k];
        // A tile sees only what lies inside it.
        let (l, t) = (x.max(i64::from(p.tile.x)), y.max(i64::from(p.tile.y)));
        let r = (x + i64::from(w)).min(i64::from(p.tile.x + p.tile.width));
        let b = (y + i64::from(h)).min(i64::from(p.tile.y + p.tile.height));
        TileBox { tile: p.tile, core: p.core, found: BalloonBox { rect: Rect::new(l, t, (r - l) as u32, (b - t) as u32), class, score } }
    }

    #[test]
    fn boxes_from_overlapping_tiles_are_joined_once() {
        // 2048 wide: tiles at x=0 and x=1024 overlap by 0; use 1600 so they
        // overlap 448 px with the core edge at x=800.
        let (w, h) = (1600u32, 900u32);
        let plan = planned(w, h).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].core.width, 800);
        let text = BalloonClass::TextInBubble;
        let boxes = vec![
            // Inside the overlap, whole in both tiles, centres either side of x=800.
            tile_box(&plan, 0, (760, 100, 80, 40), text, 0.8),
            tile_box(&plan, 1, (761, 100, 80, 40), text, 0.7),
            // Crosses tile 0's right edge (1024): cut there, whole in tile 1.
            tile_box(&plan, 0, (980, 300, 100, 60), text, 0.9),
            tile_box(&plan, 1, (980, 300, 100, 60), text, 0.6),
            // Wider than any overlap: cut by both tiles.
            tile_box(&plan, 0, (100, 600, 1400, 50), BalloonClass::TextFree, 0.5),
            tile_box(&plan, 1, (100, 600, 1400, 50), BalloonClass::TextFree, 0.75),
            // A bubble at the same place as a text box is a different object.
            tile_box(&plan, 0, (760, 100, 80, 40), BalloonClass::Bubble, 0.9),
            // Distinct boxes, one per tile, never joined.
            tile_box(&plan, 0, (50, 50, 30, 30), text, 0.9),
            tile_box(&plan, 1, (1500, 50, 30, 30), text, 0.9),
        ];
        let merged = merge_tile_boxes(w, h, &boxes);
        let at = |x: i64, y: i64, class: BalloonClass| merged.iter().filter(|b| b.class == class && b.rect.contains(x, y)).count();
        assert_eq!(merged.len(), 6, "{merged:?}");
        assert_eq!(at(800, 120, text), 1);
        let crossing: Vec<_> = merged.iter().filter(|b| b.rect.contains(1000, 320)).collect();
        assert_eq!(crossing.len(), 1);
        assert_eq!(crossing[0].rect, Rect::new(980, 300, 100, 60), "the whole view wins");
        let wide: Vec<_> = merged.iter().filter(|b| b.class == BalloonClass::TextFree).collect();
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].rect, Rect::new(100, 600, 1400, 50), "rebuilt from both pieces");
        assert_eq!(wide[0].score, 0.75);
        assert_eq!(at(800, 120, BalloonClass::Bubble), 1);
        assert_eq!(at(60, 60, text), 1);
        assert_eq!(at(1510, 60, text), 1);
        // Order of arrival does not matter.
        let mut reversed = boxes.clone();
        reversed.reverse();
        assert_eq!(merge_tile_boxes(w, h, &reversed), merged);
    }

    #[test]
    fn two_whole_boxes_one_tile_split_are_both_kept() {
        let (w, h) = (1600u32, 900u32);
        let plan = planned(w, h).unwrap();
        let text = BalloonClass::TextInBubble;
        // Tile 0 sees one cut box where tile 1 sees two whole ones.
        let boxes = vec![
            tile_box(&plan, 0, (900, 200, 300, 80), text, 0.6),
            tile_box(&plan, 1, (900, 200, 120, 80), text, 0.8),
            tile_box(&plan, 1, (1060, 200, 140, 80), text, 0.8),
        ];
        let merged = merge_tile_boxes(w, h, &boxes);
        assert_eq!(merged.iter().map(|b| b.rect).collect::<Vec<_>>(),
            vec![Rect::new(900, 200, 120, 80), Rect::new(1060, 200, 140, 80)]);
    }
}
