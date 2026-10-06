//! Compatibility adapter between native persisted patches and oriented views.
//! Old global-strip records are intersected in their original native strip
//! before orientation. New seam edits keep one record/undo identity and store
//! each participating page's native intersection in `native_parts`.
use super::{Job, NativePart, PatchRecord, Project, StoreError, StripMode, buffers};
use crate::{
    export::{StripPatch, patches_on_page},
    image::{self, Raster},
    mask::{Mask, Rect},
    patch::Patch,
    strip::Strip,
};

pub fn strip(project: &Project, display: bool) -> Strip {
    Strip::of_sizes(
        &project
            .strip
            .order
            .iter()
            .map(|i| {
                project
                    .sources
                    .get(*i)
                    .map(|s| {
                        if display {
                            s.orientation.size(s.w, s.h)
                        } else {
                            (s.w, s.h)
                        }
                    })
                    .unwrap_or((0, 0))
            })
            .collect::<Vec<_>>(),
    )
}
fn native_layout<'a>(project: &'a Project, record: &'a PatchRecord) -> (&'a [usize], Strip) {
    let order = if record.native_order.is_empty() {
        &project.strip.order
    } else {
        &record.native_order
    };
    let sizes = order
        .iter()
        .map(|i| {
            project
                .sources
                .get(*i)
                .map(|s| (s.w, s.h))
                .unwrap_or((0, 0))
        })
        .collect::<Vec<_>>();
    (order, Strip::of_sizes(&sizes))
}
/// Bounds in the anchor page's visible coordinates, calculated without reading buffers.
pub fn display_bbox(project: &Project, record: &PatchRecord) -> Rect {
    // Most edits never cross a page boundary. Resolve those directly so
    // chapter listings do not rebuild a whole strip for every layer.
    if record.native_parts.is_empty() {
        let bbox = record.display_bbox();
        if let Some(source) = project.sources.get(record.source_idx) {
            if project.strip.mode == StripMode::Single
                || (bbox.x >= 0 && bbox.y >= 0 && bbox.right() <= source.w as i64 && bbox.bottom() <= source.h as i64)
            { return source.orientation.rect(bbox, source.w, source.h); }
        }
        if (record.native_order.is_empty() || record.native_order == project.strip.order)
            && project.sources.iter().all(|source| source.orientation.0 == 1)
        { return bbox; }
    } else if record.native_parts.len() == 1 && record.native_parts[0].source_idx == record.source_idx {
        if let Some(source) = project.sources.get(record.source_idx) {
            let bbox=source.orientation.rect(record.native_parts[0].bbox,source.w,source.h);
            return crate::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot).display_bounds(bbox);
        }
    }
    let display = strip(project, true);
    let (native_order, native) = native_layout(project, record);
    let anchor = project
        .strip
        .order
        .iter()
        .position(|i| *i == record.source_idx)
        .unwrap_or(0);
    let origin = display
        .pages()
        .get(anchor)
        .map(|p| (p.x_offset, p.y_offset))
        .unwrap_or((0, 0));
    let mut bounds = Vec::new();
    for (position, &index) in project.strip.order.iter().enumerate() {
        let Some(source) = project.sources.get(index) else {
            continue;
        };
        let Some(placed) = display.pages().get(position) else {
            continue;
        };
        let local = if record.native_parts.is_empty() {
            if project.strip.mode == StripMode::Single {
                if index != record.source_idx {
                    continue;
                }
                Some(record.display_bbox())
            } else {
                let Some(native_anchor) = native_order.iter().position(|i| *i == record.source_idx)
                else {
                    continue;
                };
                let Some(native_position) = native_order.iter().position(|i| *i == index) else {
                    continue;
                };
                let Some(a) = native.pages().get(native_anchor) else {
                    continue;
                };
                let Some(p) = native.pages().get(native_position) else {
                    continue;
                };
                let x0 = (record.display_bbox().x + a.x_offset).max(p.x_offset);
                let y0 = (record.display_bbox().y + a.y_offset).max(p.y_offset);
                let x1 = (record.display_bbox().right() + a.x_offset).min(p.x_offset + p.width as i64);
                let y1 = (record.display_bbox().bottom() + a.y_offset).min(p.y_offset + p.height as i64);
                (x1 > x0 && y1 > y0).then(|| {
                    Rect::new(
                        x0 - p.x_offset,
                        y0 - p.y_offset,
                        (x1 - x0) as u32,
                        (y1 - y0) as u32,
                    )
                })
            }
        } else {
            record
                .native_parts
                .iter()
                .find(|p| p.source_idx == index)
                .map(|p| p.bbox)
        };
        if let Some(local) = local {
            let mut r = source.orientation.rect(local, source.w, source.h);
            r.x += placed.x_offset - origin.0;
            r.y += placed.y_offset - origin.1;
            bounds.push(r);
        }
    }
    let bbox = bounds
        .into_iter()
        .reduce(|a, b| {
            Rect::new(
                a.x.min(b.x),
                a.y.min(b.y),
                (a.right().max(b.right()) - a.x.min(b.x)) as u32,
                (a.bottom().max(b.bottom()) - a.y.min(b.y)) as u32,
            )
        })
        .unwrap_or(record.bbox);
    if record.native_parts.is_empty() { bbox } else {
        crate::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot).display_bounds(bbox)
    }
}
/// Conjugate a layer placement through an EXIF permutation. Offsets are
/// vectors, so the image-origin translation is deliberately absent.
pub fn orient_layer_style(mut style: crate::patch::LayerStyle, orientation: image::orientation::Orientation) -> crate::patch::LayerStyle {
    let (x,y)=orientation.point(style.offset_x as f64,style.offset_y as f64,0,0);
    style.offset_x=x as i32;style.offset_y=y as i32;
    if matches!(orientation.0,2|4|5|7) { style.rotation = -style.rotation; }
    style.sanitized()
}
pub fn display_layer_style(project:&Project,record:&PatchRecord)->crate::patch::LayerStyle {
    let style=crate::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot);
    if record.native_parts.is_empty() { orient_layer_style(style,project.sources.get(record.source_idx).map(|s|s.orientation).unwrap_or_default()) } else { style }
}
pub fn stored_layer_style(project:&Project,record:&PatchRecord,style:crate::patch::LayerStyle)->crate::patch::LayerStyle {
    if record.native_parts.is_empty() { orient_layer_style(style,project.sources.get(record.source_idx).map(|s|s.orientation.inverse()).unwrap_or_default()) } else { style }
}

impl Job {
    pub fn leave_display_untouched(
        &mut self,
        source_idx: usize,
        bbox: Rect,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.leave_untouched(source_idx, bbox, reason)
    }
    pub fn display_page(
        &self,
        source_idx: usize,
        bytes: &[u8],
    ) -> Result<Raster, image::ImageError> {
        let page = image::decode(bytes)?;
        Ok(self
            .project
            .sources
            .get(source_idx)
            .map(|s| s.orientation)
            .unwrap_or_default()
            .raster(&page))
    }
    fn load_native_part(
        &self,
        record: &PatchRecord,
        part: &NativePart,
    ) -> Result<Patch, StoreError> {
        let read = |name: &str| {
            let path = self.dir.join(name);
            std::fs::read(&path).map_err(|e| StoreError::io(&path, e))
        };
        Ok(Patch {
            id: record.id.clone(),
            mask: buffers::decode_mask(&read(&part.mask_ref)?)?,
            ink: buffers::decode_mask(&read(&part.ink_ref)?)?,
            pixels: buffers::decode_patch(&read(&part.buffer_ref)?)?,
            order: record.order,
            visible: record.visible,
            provenance: record.provenance.clone(),
        })
    }
    /// Source-local native patches, including intersections of old seam edits.
    pub fn native_patches_on_page(&self, position: usize) -> Result<Vec<Patch>, StoreError> {
        self.native_patches_for(position, &self.project.patches, false)
    }
    fn native_patches_for(
        &self,
        position: usize,
        records: &[PatchRecord],
        include_hidden: bool,
    ) -> Result<Vec<Patch>, StoreError> {
        let Some(&source_idx) = self.project.strip.order.get(position) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for record in records {
            if !include_hidden && !record.visible { continue; }
            if !record.native_parts.is_empty() && crate::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot).is_transformed() {
                let patch = self.load_display_patch(record)?;
                let display = strip(&self.project, true);
                let anchor = self.project.strip.order.iter().position(|i| *i == record.source_idx).unwrap_or(0);
                if self.project.strip.mode == StripMode::Single && position != anchor { continue; }
                if let Some(global) = StripPatch::lift(&display, anchor, &patch) {
                    let source = &self.project.sources[source_idx];
                    let size = source.orientation.size(source.w, source.h);
                    out.extend(patches_on_page(&display, position, &[global]).iter().map(|p| source.orientation.inverse().patch(p, size.0, size.1)));
                }
                continue;
            }
            if !record.native_parts.is_empty() {
                for part in &record.native_parts {
                    if part.source_idx == source_idx {
                        out.push(self.load_native_part(record, part)?);
                    }
                }
                continue;
            }
            let (native_order, native) = native_layout(&self.project, record);
            let Some(native_position) = native_order.iter().position(|i| *i == source_idx) else {
                continue;
            };
            let Some(anchor) = native_order.iter().position(|i| *i == record.source_idx) else {
                continue;
            };
            if self.project.strip.mode == StripMode::Single {
                if record.source_idx == source_idx {
                    out.push(self.load_patch(record)?);
                }
                continue;
            }
            // Bound before loading: preserve the one-page/window memory contract.
            let Some(a) = native.pages().get(anchor) else {
                continue;
            };
            let Some(p) = native.pages().get(native_position) else {
                continue;
            };
            let r = Rect::new(
                record.display_bbox().x + a.x_offset,
                record.display_bbox().y + a.y_offset,
                record.display_bbox().w,
                record.display_bbox().h,
            );
            if r.right() <= p.x_offset
                || r.x >= p.x_offset + p.width as i64
                || r.bottom() <= p.y_offset
                || r.y >= p.y_offset + p.height as i64
            {
                continue;
            }
            let patch = self.load_patch(record)?;
            if let Some(lifted) = StripPatch::lift(&native, anchor, &patch) {
                out.extend(patches_on_page(&native, native_position, &[lifted]));
            }
        }
        Ok(out)
    }
    pub fn display_patches_on_page(&self, position: usize) -> Result<Vec<Patch>, StoreError> {
        let Some(source) = self
            .project
            .strip
            .order
            .get(position)
            .and_then(|i| self.project.sources.get(*i))
        else {
            return Ok(Vec::new());
        };
        Ok(self
            .native_patches_on_page(position)?
            .iter()
            .map(|p| source.orientation.patch(p, source.w, source.h))
            .collect())
    }
    /// Reconstruct one grouped edit in anchor-relative display coordinates.
    pub fn load_display_patch(&self, record: &PatchRecord) -> Result<Patch, StoreError> {
        if record.native_parts.is_empty()
            && self.project.sources.iter().all(|s| s.orientation.0 == 1)
        {
            return self.load_patch(record);
        }
        let mut raw_record = record.clone();
        if !record.native_parts.is_empty() {
            if let Some(params) = raw_record.provenance.params_snapshot.as_object_mut() { params.remove("layer"); }
        }
        let display = strip(&self.project, true);
        let anchor = self
            .project
            .strip
            .order
            .iter()
            .position(|i| *i == record.source_idx)
            .unwrap_or(0);
        let origin = display
            .pages()
            .get(anchor)
            .map(|p| (p.x_offset, p.y_offset))
            .unwrap_or((0, 0));
        let mut pieces = Vec::new();
        for position in 0..display.pages().len() {
            let Some(source) = self
                .project
                .strip
                .order
                .get(position)
                .and_then(|i| self.project.sources.get(*i))
            else {
                continue;
            };
            for mut patch in self
                .native_patches_for(position, std::slice::from_ref(&raw_record), true)?
                .iter()
                .map(|p| source.orientation.patch(p, source.w, source.h))
            {
                let page = display.pages()[position];
                let shift = (page.x_offset - origin.0, page.y_offset - origin.1);
                for mask in [&mut patch.mask, &mut patch.ink] {
                    mask.bounds.x += shift.0;
                    mask.bounds.y += shift.1;
                }
                pieces.push(patch);
            }
        }
        if pieces.is_empty() {
            return self.load_patch(record);
        }
        let bounds = pieces
            .iter()
            .map(|p| p.mask.bounds)
            .reduce(|a, b| {
                Rect::new(
                    a.x.min(b.x),
                    a.y.min(b.y),
                    (a.right().max(b.right()) - a.x.min(b.x)) as u32,
                    (a.bottom().max(b.bottom()) - a.y.min(b.y)) as u32,
                )
            })
            .unwrap();
        if u64::from(bounds.w) * u64::from(bounds.h) > 16_777_216 {
            return Err(StoreError::Malformed("the reordered edit spans too large a window to rerun; its native parts remain available for display and export".into()));
        }
        let mut out = pieces[0].clone();
        out.mask = Mask::empty(bounds);
        out.ink = Mask::empty(bounds);
        out.pixels.width = bounds.w;
        out.pixels.height = bounds.h;
        out.pixels.data = vec![0; out.pixels.stride() * bounds.h as usize];
        for piece in pieces {
            for y in 0..piece.mask.bounds.h {
                for x in 0..piece.mask.bounds.w {
                    let gx = piece.mask.bounds.x + x as i64;
                    let gy = piece.mask.bounds.y + y as i64;
                    let dx = (gx - bounds.x) as u32;
                    let dy = (gy - bounds.y) as u32;
                    let at = dy as usize * bounds.w as usize + dx as usize;
                    out.mask.bits[at] =
                        piece.mask.bits[y as usize * piece.mask.bounds.w as usize + x as usize];
                    out.ink.bits[at] = if piece.ink.bounds.contains(gx, gy) {
                        piece.ink.bits[(gy - piece.ink.bounds.y) as usize
                            * piece.ink.bounds.w as usize
                            + (gx - piece.ink.bounds.x) as usize]
                    } else {
                        0
                    };
                    for c in 0..out.pixels.mode.samples() {
                        out.pixels
                            .set_sample(dx, dy, c, piece.pixels.sample(x, y, c));
                    }
                }
            }
        }
        if !record.native_parts.is_empty() {
            out.provenance = record.provenance.clone();
            return Ok(out.presented());
        }
        Ok(out)
    }
    /// Use the revision-aware, locked commit path for every display edit.
    pub fn complete_display_region(&mut self, source_idx: usize, patch: &Patch, review: Option<String>) -> Result<(), StoreError> {
        self.complete_region_with_policy(source_idx, patch, review, super::GeometryPolicy::Legacy, None, true)
    }

    pub(super) fn store_native_parts(&self, source_idx: usize, patch: &Patch) -> Result<Vec<NativePart>, StoreError> {
        if self.project.sources.iter().all(|s| s.orientation.0 == 1) { return Ok(Vec::new()); }
        let display = strip(&self.project, true);
        let anchor = self
            .project
            .strip
            .order
            .iter()
            .position(|i| *i == source_idx)
            .unwrap_or(0);
        let mut parts = Vec::new();
        if let Some(global) = StripPatch::lift(&display, anchor, patch) {
            for position in 0..display.pages().len() {
                if self.project.strip.mode == StripMode::Single && position != anchor {
                    continue;
                }
                let Some(&index) = self.project.strip.order.get(position) else {
                    continue;
                };
                let source = &self.project.sources[index];
                for local in patches_on_page(&display, position, std::slice::from_ref(&global)) {
                    let size = source.orientation.size(source.w, source.h);
                    let native = source.orientation.inverse().patch(&local, size.0, size.1);
                    // Content-addressed parts keep the old manifest fully readable if a
                    // process dies before publishing the replacement record.
                    use sha2::{Digest, Sha256};
                    let mut hash = Sha256::new();
                    hash.update(buffers::encode_mask(&native.mask));
                    hash.update(buffers::encode_mask(&native.ink));
                    hash.update(buffers::encode_patch(&native.pixels));
                    let directory = format!("native-parts/{:x}", Sha256::digest(patch.id.as_bytes()));
                    let directory_path = self.dir.join(&directory);
                    std::fs::create_dir_all(&directory_path).map_err(|e| StoreError::io(&directory_path,e))?;
                    let stem = format!("{directory}/{index}-{:x}", hash.finalize());
                    let part = NativePart {
                        source_idx: index,
                        bbox: native.mask.bounds,
                        mask_ref: format!("{stem}.mask"),
                        ink_ref: format!("{stem}.ink"),
                        buffer_ref: format!("{stem}.buf"),
                    };
                    buffers::write_atomic(
                        &self.dir.join(&part.mask_ref),
                        &buffers::encode_mask(&native.mask),
                    )?;
                    buffers::write_atomic(
                        &self.dir.join(&part.ink_ref),
                        &buffers::encode_mask(&native.ink),
                    )?;
                    buffers::write_atomic(
                        &self.dir.join(&part.buffer_ref),
                        &buffers::encode_patch(&native.pixels),
                    )?;
                    parts.push(part);
                }
            }
        }
        Ok(parts)
    }
}
