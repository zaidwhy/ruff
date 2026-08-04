use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::Edit;

/// A pair of corresponding byte offsets in a source and its destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMarker {
    /// Position of the marker in the input being mapped.
    source: TextSize,
    /// Corresponding position in the destination.
    dest: TextSize,
}

impl SourceMarker {
    pub fn new(source: TextSize, dest: TextSize) -> Self {
        Self { source, dest }
    }

    pub const fn source(&self) -> TextSize {
        self.source
    }

    pub const fn dest(&self) -> TextSize {
        self.dest
    }
}

/// An ordered collection of [`SourceMarker`]s.
///
/// Markers record where the displacement between source and destination changes instead of
/// tracking every byte individually. Existing edit maps use original Python source as the input;
/// embedded metadata maps use extracted TOML as the input and Python source as the destination.
///
/// This mapping maintains the invariant that markers are in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap(Vec<SourceMarker>);

impl SourceMap {
    /// Maps an input offset to the corresponding destination offset.
    pub fn map_offset(&self, offset: TextSize) -> TextSize {
        let Some(index) = self
            .0
            .partition_point(|marker| marker.source <= offset)
            .checked_sub(1)
        else {
            return offset;
        };
        let marker = &self.0[index];

        marker.dest + (offset - marker.source)
    }

    /// Maps a range by translating both endpoints independently.
    pub fn map_range(&self, range: TextRange) -> TextRange {
        TextRange::new(self.map_offset(range.start()), self.map_offset(range.end()))
    }

    /// Returns a slice of all the markers in the sourcemap in source order.
    pub fn markers(&self) -> &[SourceMarker] {
        &self.0
    }

    /// Push the start marker for an [`Edit`].
    ///
    /// The `output_length` is the length of the transformed string before the
    /// edit is applied.
    ///
    /// ## Panics
    ///
    /// If the start of `edit` is less than previous markers.
    pub fn push_start_marker(&mut self, edit: &Edit, output_length: TextSize) {
        self.push_marker(edit.start(), output_length);
    }

    /// Push the end marker for an [`Edit`].
    ///
    /// The `output_length` is the length of the transformed string after the
    /// edit has been applied.
    ///
    /// ## Panics
    ///
    /// If `edit` falls before previous markers.
    pub fn push_end_marker(&mut self, edit: &Edit, output_length: TextSize) {
        if edit.is_insertion() {
            self.push_marker(edit.start(), output_length);
        } else {
            // Deletion or replacement
            self.push_marker(edit.end(), output_length);
        }
    }

    /// Push a new marker to the sourcemap.
    ///
    /// ## Panics
    ///
    /// If `offset` is less than previous markers.
    pub fn push_marker(&mut self, offset: TextSize, output_length: TextSize) {
        assert!(
            self.0.last().is_none_or(|last| offset >= last.source),
            "Markers must be pushed in source order",
        );

        self.0.push(SourceMarker {
            source: offset,
            dest: output_length,
        });
    }
}
