//! Preserve portable descriptive EXIF while removing stale image geometry,
//! thumbnails, offsets, maker notes and color-space claims after re-encoding.
use super::metadata::{AncillaryChunk, ColorDescription};

pub fn payload(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.starts_with(b"II\x2a\0") || bytes.starts_with(b"MM\0\x2a") {
        return Some(bytes);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut at = 8usize;
        while at.checked_add(12)? <= bytes.len() {
            let n = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
            let end = at.checked_add(n)?.checked_add(12)?;
            let data = bytes.get(at + 8..end.checked_sub(4)?)?;
            if bytes.get(at + 4..at + 8) == Some(b"eXIf") {
                return Some(data);
            }
            at = end;
        }
    } else if bytes.starts_with(&[255, 216]) {
        let mut at = 2usize;
        while bytes.get(at) == Some(&255) {
            while bytes.get(at) == Some(&255) {
                at += 1;
            }
            let marker = *bytes.get(at)?;
            at += 1;
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            if marker == 1 || (0xd0..=0xd8).contains(&marker) {
                continue;
            }
            let n = u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize;
            if n < 2 {
                return None;
            }
            let data = bytes.get(at + 2..at.checked_add(n)?)?;
            if marker == 0xe1 && data.starts_with(b"Exif\0\0") {
                return Some(&data[6..]);
            }
            at += n;
        }
    }
    None
}

/// Rebuild a bounded IFD containing only self-contained descriptive ASCII tags.
/// All offsets point into this new payload; IFD1 and nested opaque blobs vanish.
pub fn portable(input: &[u8]) -> Option<Vec<u8>> {
    let little = match input.get(..4)? {
        b"II\x2a\0" => true,
        b"MM\0\x2a" => false,
        _ => return None,
    };
    let u16at = |at: usize| {
        let a = input.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    };
    let u32at = |at: usize| {
        let a = input.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let start = u32at(4)? as usize;
    let count = u16at(start)? as usize;
    if count > 4096 {
        return None;
    }
    let mut entries = Vec::new();
    let mut total = 0usize;
    for i in 0..count {
        let at = start.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        let tag = u16at(at)?;
        if !matches!(tag, 270 | 271 | 272 | 305 | 306 | 315 | 33432) || u16at(at + 2)? != 2 {
            continue;
        }
        let len = u32at(at + 4)? as usize;
        total = total.checked_add(len)?;
        if len == 0 || total > 65536 {
            return None;
        }
        let offset = if len <= 4 {
            at + 8
        } else {
            u32at(at + 8)? as usize
        };
        let value = input.get(offset..offset.checked_add(len)?)?;
        if value.last() != Some(&0) || value[..len - 1].iter().any(|b| !b.is_ascii()) {
            continue;
        }
        entries.push((tag, value));
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|(tag, _)| *tag);
    entries.dedup_by_key(|(tag, _)| *tag);
    let base = 8 + 2 + entries.len() * 12 + 4;
    let mut out = vec![0; base];
    out[..8].copy_from_slice(b"II\x2a\0\x08\0\0\0");
    out[8..10].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    for (i, (tag, value)) in entries.into_iter().enumerate() {
        let at = 10 + i * 12;
        out[at..at + 2].copy_from_slice(&tag.to_le_bytes());
        out[at + 2..at + 4].copy_from_slice(&2u16.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&(value.len() as u32).to_le_bytes());
        if value.len() <= 4 {
            out[at + 8..at + 8 + value.len()].copy_from_slice(value);
        } else {
            let offset = out.len() as u32;
            out[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(value);
        }
    }
    Some(out)
}

/// Describe native samples that intentionally retain an EXIF orientation.
/// The new root IFD reuses portable descriptive values without changing their
/// absolute offsets; stale geometry was removed by `portable` first.
pub fn set_orientation(color: &mut ColorDescription, orientation: super::orientation::Orientation) {
    let mut data=color.safe_ancillary.iter().find(|chunk|chunk.name==*b"eXIf")
        .and_then(|chunk|portable(&chunk.data))
        .unwrap_or_else(||b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec());
    let count=u16::from_le_bytes(data[8..10].try_into().unwrap()) as usize;
    let mut entries:Vec<[u8;12]>=data[10..10+count*12].chunks_exact(12).map(|row|row.try_into().unwrap()).collect();
    let mut entry=[0u8;12];entry[..2].copy_from_slice(&274u16.to_le_bytes());entry[2..4].copy_from_slice(&3u16.to_le_bytes());
    entry[4..8].copy_from_slice(&1u32.to_le_bytes());entry[8..10].copy_from_slice(&u16::from(orientation.0).to_le_bytes());
    entries.push(entry);entries.sort_by_key(|entry|u16::from_le_bytes(entry[..2].try_into().unwrap()));
    if data.len()%2!=0 {data.push(0);}
    let offset=data.len() as u32;data[4..8].copy_from_slice(&offset.to_le_bytes());
    data.extend_from_slice(&(entries.len() as u16).to_le_bytes());for entry in entries {data.extend_from_slice(&entry);}
    data.extend_from_slice(&0u32.to_le_bytes());
    color.safe_ancillary.retain(|chunk|chunk.name!=*b"eXIf");
    color.safe_ancillary.push(AncillaryChunk{name:*b"eXIf",data,after_data:false});
}

pub fn carry_portable(color: &mut ColorDescription, source: &[u8]) {
    if let Some(data) = payload(source).and_then(portable) {
        color.safe_ancillary.push(AncillaryChunk {
            name: *b"eXIf",
            data,
            after_data: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_native_orientation_keeps_long_descriptive_values() {
        let artist=b"A long descriptive artist\0";
        let mut original=b"II\x2a\0\x08\0\0\0".to_vec();original.extend_from_slice(&1u16.to_le_bytes());
        original.extend_from_slice(&315u16.to_le_bytes());original.extend_from_slice(&2u16.to_le_bytes());
        original.extend_from_slice(&(artist.len() as u32).to_le_bytes());original.extend_from_slice(&26u32.to_le_bytes());original.extend_from_slice(&0u32.to_le_bytes());original.extend_from_slice(artist);
        for value in 1..=8 {
            let mut color=ColorDescription::default();color.safe_ancillary.push(AncillaryChunk{name:*b"eXIf",data:original.clone(),after_data:false});
            set_orientation(&mut color,super::super::orientation::Orientation(value));
            let payload=&color.safe_ancillary[0].data;assert_eq!(super::super::orientation::from_bytes(payload).0,value);
            assert_eq!(portable(payload),portable(&original));
        }
    }

    #[test]
    fn descriptive_tags_survive_but_orientation_dimensions_and_thumbnail_do_not() {
        // Pillow-style little-endian IFD: Artist, Orientation, ImageWidth.
        let mut input = b"II\x2a\0\x08\0\0\0".to_vec();
        input.extend_from_slice(&3u16.to_le_bytes());
        for (tag, kind, count, value) in [
            (315u16, 2u16, 4u32, u32::from_le_bytes(*b"Art\0")),
            (274, 3, 1, 6),
            (256, 4, 1, 640),
        ] {
            input.extend_from_slice(&tag.to_le_bytes());
            input.extend_from_slice(&kind.to_le_bytes());
            input.extend_from_slice(&count.to_le_bytes());
            input.extend_from_slice(&value.to_le_bytes());
        }
        input.extend_from_slice(&1234u32.to_le_bytes());
        let safe = portable(&input).unwrap();
        assert_eq!(u16::from_le_bytes(safe[8..10].try_into().unwrap()), 1);
        assert_eq!(&safe[18..22], b"Art\0");
        assert_eq!(&safe[22..26], &[0; 4]);
        assert_eq!(crate::image::orientation::from_bytes(&safe).0, 1);
        assert_eq!(portable(&safe).unwrap(), safe);
        assert!(portable(&input[..11]).is_none());
    }
}
