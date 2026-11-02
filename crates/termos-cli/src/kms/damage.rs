// SPDX-License-Identifier: GPL-3.0-or-later
//! The pixel rows a frame changed, as a few bands. Copying a frame out
//! costs one pass over its damaged rows, and on a Pi the copy into the
//! display's memory is the slow part (M9). A change at the top and one at
//! the bottom (the cursor and the clock) stay two bands rather than one
//! span over the whole screen in between.

/// The most bands kept; past it the two closest merge.
pub(super) const MAX_BANDS: usize = 8;

/// Changed rows: sorted, disjoint, non-touching `[start, end)` bands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Damage {
    bands: Vec<(u32, u32)>,
}

impl Damage {
    /// Everything from row 0 to `height`.
    pub(super) fn all(height: u32) -> Self {
        let mut d = Self::default();
        d.add(0, height);
        d
    }

    pub(super) fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    #[cfg(test)]
    pub(super) fn bands(&self) -> &[(u32, u32)] {
        &self.bands
    }

    /// Takes the bands, leaving none.
    pub(super) fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Adds rows `[y0, y1)`.
    pub(super) fn add(&mut self, y0: u32, y1: u32) {
        if y0 >= y1 {
            return;
        }
        // The bands that overlap or touch the new one join it.
        let first = self.bands.partition_point(|&(_, e)| e < y0);
        let last = self.bands.partition_point(|&(s, _)| s <= y1);
        let (mut a, mut b) = (y0, y1);
        if first < last {
            a = a.min(self.bands[first].0);
            b = b.max(self.bands[last - 1].1);
        }
        self.bands.splice(first..last, [(a, b)]);
        if self.bands.len() > MAX_BANDS {
            let i = (1..self.bands.len())
                .min_by_key(|&i| self.bands[i].0 - self.bands[i - 1].1)
                .expect("more than MAX_BANDS bands have a gap between two");
            self.bands[i - 1].1 = self.bands[i].1;
            self.bands.remove(i);
        }
    }

    /// Adds every band of `other`.
    pub(super) fn merge(&mut self, other: &Self) {
        for &(a, b) in &other.bands {
            self.add(a, b);
        }
    }

    /// The bands cut to `[0, height)`.
    pub(super) fn clipped(&self, height: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.bands
            .iter()
            .map(move |&(a, b)| (a, b.min(height)))
            .filter(|(a, b)| a < b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn bands_join_when_they_touch() {
        let mut d = Damage::default();
        d.add(10, 20);
        d.add(40, 50);
        assert_eq!(d.bands(), [(10, 20), (40, 50)]);
        d.add(20, 25);
        assert_eq!(d.bands(), [(10, 25), (40, 50)], "touching joins");
        d.add(5, 45);
        assert_eq!(d.bands(), [(5, 50)], "covering joins all");
        d.add(7, 7);
        assert_eq!(d.bands(), [(5, 50)], "an empty range is nothing");
        let mut e = Damage::all(100);
        e.merge(&d);
        assert_eq!(e.bands(), [(0, 100)]);
        assert_eq!(e.clipped(60).collect::<Vec<_>>(), [(0, 60)]);
        assert!(e.take().bands().len() == 1 && e.is_empty());
    }

    #[test]
    fn too_many_bands_merge_the_closest() {
        let mut d = Damage::default();
        for i in 0..MAX_BANDS as u32 {
            d.add(i * 100, i * 100 + 10);
        }
        d.add(715, 720);
        assert_eq!(d.bands().len(), MAX_BANDS);
        assert_eq!(d.bands()[MAX_BANDS - 1], (700, 720), "the 5-row gap went");
    }

    proptest! {
        #[test]
        fn bands_stay_sorted_and_cover_what_was_added(
            ranges in proptest::collection::vec((0u32..2000, 0u32..80), 0..40)
        ) {
            let mut d = Damage::default();
            for &(y, h) in &ranges {
                d.add(y, y + h);
            }
            let b = d.bands();
            prop_assert!(b.len() <= MAX_BANDS);
            prop_assert!(b.iter().all(|(s, e)| s < e));
            prop_assert!(b.windows(2).all(|w| w[0].1 < w[1].0));
            for &(y, h) in &ranges {
                for row in y..y + h {
                    prop_assert!(b.iter().any(|&(s, e)| s <= row && row < e));
                }
            }
        }
    }
}
