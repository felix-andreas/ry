//! Environment frames. A frame maps symbols to binding cells, which are heap objects of their own
//! (`SexpType::Binding`, a triple of symbol, value and spare slot). A cell has a stable address for
//! as long as anything references it, which is what lets compiled code watch a binding for
//! redefinition, and it outlives its removal from the frame if compiled code still holds it.
//!
//! The base environment and the base namespace have no `Frame`: their cells hang off the symbols
//! themselves, so a lookup of a base function is one load from the interned symbol.

use crate::object::Sexp;
use rustc_hash::FxHashMap;

/// Frames this small are scanned; larger ones keep a hash index beside the cells.
const INDEX_THRESHOLD: usize = 8;

#[derive(Default)]
pub struct Frame {
    cells: Vec<Sexp>,
    index: Option<FxHashMap<Sexp, u32>>,
}

impl Frame {
    pub fn cells(&self) -> &[Sexp] {
        &self.cells
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The binding cell for `symbol`, if the frame binds it.
    ///
    /// # Safety
    /// Every cell in the frame must be live, which holds while the environment is reachable.
    pub unsafe fn find(&self, symbol: Sexp) -> Option<Sexp> {
        match &self.index {
            Some(index) => index
                .get(&symbol)
                .and_then(|&position| self.cells.get(position as usize).copied()),
            None => unsafe { self.cells.iter().copied().find(|cell| cell.car() == symbol) },
        }
    }

    /// Adds a cell whose symbol the frame does not bind yet.
    pub(crate) unsafe fn insert(&mut self, cell: Sexp) {
        let position = self.cells.len() as u32;
        self.cells.push(cell);
        match &mut self.index {
            Some(index) => {
                index.insert(unsafe { cell.car() }, position);
            }
            None if self.cells.len() > INDEX_THRESHOLD => {
                let index = self
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(position, cell)| (unsafe { cell.car() }, position as u32))
                    .collect();
                self.index = Some(index);
            }
            None => {}
        }
    }

    /// Removes the cell for `symbol` and returns it.
    pub(crate) unsafe fn remove(&mut self, symbol: Sexp) -> Option<Sexp> {
        let position = match &mut self.index {
            Some(index) => index.remove(&symbol)? as usize,
            None => unsafe { self.cells.iter().position(|cell| cell.car() == symbol)? },
        };
        let cell = self.cells.swap_remove(position);
        if let (Some(index), Some(moved)) = (&mut self.index, self.cells.get(position)) {
            index.insert(unsafe { moved.car() }, position as u32);
        }
        Some(cell)
    }
}
