//! Shared receipt validation for Move and mount multipart recovery.
//! ListParts is evidence to compare against our journal, never a substitute
//! for a receipt tying a completed part to the frozen source/payload.
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartReceipt {
    pub number: i32,
    pub etag: String,
    pub size: u64,
}

pub struct PartReconciler {
    total: u64,
    part_size: u64,
    local: BTreeMap<i32, PartReceipt>,
    remote_seen: HashSet<i32>,
    confirmed: BTreeMap<i32, PartReceipt>,
}

fn part_count(total: u64, part_size: u64) -> Result<u64, String> {
    if total == 0 || part_size == 0 {
        return Err("conflict: Invalid multipart geometry".into());
    }
    let count = total.div_ceil(part_size);
    if count > 10_000 {
        return Err("conflict: Multipart geometry exceeds part limit".into());
    }
    Ok(count)
}

fn validate(total: u64, part_size: u64, part: &PartReceipt) -> Result<(), String> {
    let count = part_count(total, part_size)?;
    if part.number < 1 || part.number as u64 > count {
        return Err("conflict: Multipart receipt has an unexpected part number".into());
    }
    let expected = part_size.min(total - (part.number as u64 - 1) * part_size);
    if part.size != expected || part.etag.is_empty() {
        return Err("conflict: Multipart receipt has an invalid size or empty ETag".into());
    }
    Ok(())
}

impl PartReconciler {
    pub fn new(
        total: u64,
        part_size: u64,
        local: impl IntoIterator<Item = PartReceipt>,
    ) -> Result<Self, String> {
        part_count(total, part_size)?;
        let mut receipts = BTreeMap::new();
        for part in local {
            validate(total, part_size, &part)?;
            if receipts.insert(part.number, part).is_some() {
                return Err("conflict: Duplicate local multipart receipt".into());
            }
        }
        Ok(Self {
            total,
            part_size,
            local: receipts,
            remote_seen: HashSet::new(),
            confirmed: BTreeMap::new(),
        })
    }

    pub fn accept(&mut self, remote: PartReceipt) -> Result<(), String> {
        validate(self.total, self.part_size, &remote)?;
        if !self.remote_seen.insert(remote.number) {
            return Err("conflict: ListParts returned a duplicate part number".into());
        }
        if self.local.get(&remote.number) == Some(&remote) {
            self.confirmed.insert(remote.number, remote);
        }
        // Missing/mismatching durable receipt means recopy/reupload this fixed
        // part with its original identity; it is not eligible for completion.
        Ok(())
    }

    pub fn finish(self) -> BTreeMap<i32, PartReceipt> {
        self.confirmed
    }
}

pub fn complete_receipts(
    total: u64,
    part_size: u64,
    parts: impl IntoIterator<Item = PartReceipt>,
) -> Result<Vec<PartReceipt>, String> {
    let count = part_count(total, part_size)?;
    let mut ordered = BTreeMap::new();
    for part in parts {
        validate(total, part_size, &part)?;
        if ordered.insert(part.number, part).is_some() {
            return Err("conflict: Duplicate multipart completion receipt".into());
        }
    }
    if ordered.len() as u64 != count {
        return Err("conflict: Multipart completion is missing receipts".into());
    }
    Ok(ordered.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(number: i32, size: u64) -> PartReceipt {
        PartReceipt {
            number,
            size,
            etag: format!("etag-{number}"),
        }
    }

    #[test]
    fn remote_parts_require_matching_local_receipts() {
        let mut reconciler = PartReconciler::new(25, 10, [part(1, 10), part(2, 10)]).unwrap();
        reconciler.accept(part(3, 5)).unwrap();
        let mut changed = part(2, 10);
        changed.etag = "different-source".into();
        reconciler.accept(changed).unwrap();
        reconciler.accept(part(1, 10)).unwrap();
        assert_eq!(
            reconciler.finish().into_values().collect::<Vec<_>>(),
            [part(1, 10)]
        );
    }

    #[test]
    fn duplicates_and_wrong_geometry_fail_closed() {
        let mut reconciler = PartReconciler::new(25, 10, []).unwrap();
        reconciler.accept(part(1, 10)).unwrap();
        assert!(reconciler.accept(part(1, 10)).is_err());
        assert!(reconciler.accept(part(3, 10)).is_err());
        assert!(reconciler.accept(part(4, 5)).is_err());
        assert!(PartReconciler::new(25, 10, [part(1, 10), part(1, 10)]).is_err());
    }

    #[test]
    fn completion_sorts_by_identity_and_requires_all_parts() {
        let sorted = complete_receipts(25, 10, [part(3, 5), part(1, 10), part(2, 10)]).unwrap();
        assert_eq!(sorted, [part(1, 10), part(2, 10), part(3, 5)]);
        assert!(complete_receipts(25, 10, [part(1, 10), part(3, 5)]).is_err());
    }

    #[test]
    fn many_pages_preserve_unique_receipts() {
        let local = (1..=1001)
            .map(|number| part(number, 10))
            .collect::<Vec<_>>();
        let mut reconciler = PartReconciler::new(10010, 10, local.clone()).unwrap();
        for page in local.chunks(1000) {
            for receipt in page {
                reconciler.accept(receipt.clone()).unwrap();
            }
        }
        assert_eq!(reconciler.finish().len(), 1001);
    }
}
