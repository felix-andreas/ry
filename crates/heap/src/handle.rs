//! Typed pointers and the context that make code above the heap safe Rust
//! (`docs/src/content/docs/development/design/memory-safety.md`).
//!
//! A safepoint is a point where the heap may free an object or foreign code may run: an
//! allocation, a call into R code, a call into native code. A function that can reach one takes
//! `&mut Cx`, and a value read out of the heap borrows `&Cx`, so the borrow checker refuses a
//! value held across a safepoint without a root:
//!
//! ```compile_fail,E0502
//! use heap::handle::{Cx, List, Local, Real};
//! fn held_across_an_allocation<'s>(cx: &mut Cx<'s>, list: Local<'s, List>) {
//!     let first = list.get(cx, 0);
//!     let copy = cx.alloc::<Real>(1);
//!     let _ = (first, copy);
//! }
//! ```
//!
//! Rooting it is what the rule asks for:
//!
//! ```no_run
//! use heap::handle::{Cx, List, Local, Real};
//! fn rooted_across_an_allocation<'s>(cx: &mut Cx<'s>, list: Local<'s, List>) {
//!     let first = cx.root(list.get(cx, 0));
//!     let copy = cx.alloc::<Real>(1);
//!     let _ = (first, copy);
//! }
//! ```
//!
//! A slice of a vector's elements borrows the context the same way:
//!
//! ```compile_fail,E0502
//! use heap::handle::{Cx, Local, Real};
//! fn slice_across_an_allocation<'s>(cx: &mut Cx<'s>, vector: Local<'s, Real>) {
//!     let elements = vector.as_slice(cx);
//!     let copy = cx.alloc::<Real>(1);
//!     let _ = (elements, copy);
//! }
//! ```
//!
//! So do a string's bytes:
//!
//! ```compile_fail,E0502
//! use heap::handle::{Chars, Cx, Local, Real};
//! fn bytes_across_an_allocation<'s>(cx: &mut Cx<'s>, text: Local<'s, Chars>) {
//!     let bytes = text.bytes(cx);
//!     let copy = cx.alloc::<Real>(1);
//!     let _ = (bytes, copy);
//! }
//! ```
//!
//! A rooted value cannot leave the scope that roots it, except as the scope's result:
//!
//! ```compile_fail
//! use heap::handle::{Cx, Local, Real};
//! use heap::HeapError;
//! fn leaves_its_scope<'s>(cx: &mut Cx<'s>) {
//!     let mut leaked = None;
//!     let _ = cx.scope(|cx| -> Result<Local<'_, Real>, HeapError> {
//!         let inner = cx.alloc::<Real>(1)?.finish();
//!         leaked = Some(inner);
//!         Ok(inner)
//!     });
//!     let _ = leaked;
//! }
//! ```
//!
//! ```no_run
//! use heap::handle::{Cx, Local, Real};
//! use heap::HeapError;
//! fn returned_from_its_scope<'s>(cx: &mut Cx<'s>) -> Result<Local<'s, Real>, HeapError> {
//!     cx.scope(|cx| Ok(cx.alloc::<Real>(1)?.finish()))
//! }
//! ```
//!
//! While an object is written in place, the context is borrowed mutably, so nothing allocates
//! and no slice taken earlier is alive:
//!
//! ```compile_fail,E0499
//! use heap::handle::{Cx, Owned, Real};
//! fn allocates_during_a_write<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>) {
//!     let mut write = vector.write(cx);
//!     let (elements, _reader) = write.split();
//!     let copy = cx.alloc::<Real>(1);
//!     elements[0] = 1.0;
//!     let _ = copy;
//! }
//! ```
//!
//! ```compile_fail,E0502
//! use heap::handle::{Cx, Owned, Real};
//! fn reads_an_earlier_slice_during_a_write<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>) {
//!     let before = vector.get().as_slice(cx);
//!     let mut write = vector.write(cx);
//!     let (elements, _reader) = write.split();
//!     elements[0] = before[1];
//! }
//! ```
//!
//! ```no_run
//! use heap::handle::{Cx, Local, Owned, Real};
//! fn reads_through_the_reader<'s>(cx: &mut Cx<'s>, mut vector: Owned<'s, Real>, source: Local<'s, Real>) {
//!     let mut write = vector.write(cx);
//!     let (elements, reader) = write.split();
//!     match reader.slice(source) {
//!         Some(source) => elements[0] = source[1],
//!         None => elements.copy_within(1..2, 0),
//!     }
//! }
//! ```
//!
//! A new object is the only one that can be filled without a check, and it is not `Copy`:
//!
//! ```compile_fail,E0382
//! use heap::handle::{Cx, Integer};
//! use heap::HeapError;
//! fn written_after_it_is_visible<'s>(cx: &mut Cx<'s>) -> Result<(), HeapError> {
//!     let mut fresh = cx.alloc::<Integer>(4)?;
//!     let visible = fresh.finish();
//!     fresh.as_mut_slice()[0] = 1;
//!     let _ = visible;
//!     Ok(())
//! }
//! ```
//!
//! An object with no element storage has no element access. An ALTREP object is never a vector
//! kind, so a write into one has to go through a copy the allocator made:
//!
//! ```compile_fail,E0599
//! use heap::handle::{Cx, Owned};
//! fn written_without_storage<'s>(cx: &mut Cx<'s>, mut copy: Owned<'s>) {
//!     copy.write(cx).split().0[0] = 1.0;
//! }
//! ```
//!
//! Whether a vector may be written where it is, is one value. An [`Owned`] comes from a new
//! vector or from the one `unsafe` claim that nothing else refers to an existing one, so a
//! shared value has to be copied first, and the copy has to be one with storage:
//!
//! ```compile_fail,E0599
//! use heap::handle::{Cx, Local, Real};
//! fn written_while_shared<'s>(cx: &mut Cx<'s>, shared: Local<'s, Real>) {
//!     shared.write(cx).split().0[0] = 1.0;
//! }
//! ```
//!
//! ```no_run
//! use heap::handle::{Cx, Local, Owned, Real};
//! use heap::HeapError;
//! fn written_into_a_copy<'s>(
//!     cx: &mut Cx<'s>,
//!     shared: Local<'s, Real>,
//! ) -> Result<Local<'s, Real>, HeapError> {
//!     let mut copy = cx.alloc::<Real>(shared.len())?;
//!     copy.as_mut_slice().copy_from_slice(shared.as_slice(cx));
//!     let mut owned: Owned<'s, Real> = copy.owned();
//!     owned.write(cx).split().0[0] = 1.0;
//!     Ok(owned.finish())
//! }
//! ```
//!
//! A pointer has no `==`. Two objects are compared with `is`, and strings are not, because one
//! text can be several objects:
//!
//! ```compile_fail,E0369
//! use heap::handle::{Chars, Gc};
//! fn compared_by_address(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
//!     left == right
//! }
//! ```
//!
//! ```compile_fail,E0277
//! use heap::handle::{Chars, Gc};
//! fn compared_by_identity(left: Gc<'_, Chars>, right: Gc<'_, Chars>) -> bool {
//!     left.is(right)
//! }
//! ```
//!
//! ```no_run
//! use heap::handle::{Gc, Symbol};
//! fn same_symbol(left: Gc<'_, Symbol>, right: Gc<'_, Symbol>) -> bool {
//!     left.is(right)
//! }
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

use {
    crate::{
        Heap, HeapResult,
        object::{Rcomplex, Sexp, SexpType},
    },
    std::{marker::PhantomData, mem::MaybeUninit, ptr::NonNull},
};

/// The permission to use the heap, under the handle scope `'s`.
///
/// It is a capability and not a reference to the heap's memory: each method dereferences the
/// heap for one operation. Native code that re-enters the runtime makes a second `Cx` while the
/// first is suspended in the native call, and no two `&mut Heap` exist at once.
pub struct Cx<'s> {
    heap: NonNull<Heap>,
    scope: Brand<'s>,
}

impl<'s> Cx<'s> {
    /// A context over `heap` whose scope is the handle stack as it stands.
    ///
    /// # Safety
    ///
    /// `heap` outlives the context, only this thread uses it, and no other context is in use
    /// until this one is dropped or lent to the code that makes the next one. The caller
    /// chooses `'s`, so it must end before the handle stack is released below its current
    /// height.
    pub unsafe fn enter(heap: NonNull<Heap>) -> Cx<'s> {
        Cx {
            heap,
            scope: PhantomData,
        }
    }

    /// The heap, for code that calls the raw API.
    ///
    /// # Safety
    ///
    /// The caller releases no handle that a `Local` of a live scope relies on, and upholds the
    /// raw API's own rules.
    pub unsafe fn raw_heap(&mut self) -> &mut Heap {
        // SAFETY: `enter`'s caller promised the heap is live and this context is the one in use.
        unsafe { &mut *self.heap.as_ptr() }
    }

    /// Keeps `value` alive until the scope ends. It takes `&self` because a push onto the
    /// handle stack frees nothing, and the value being rooted still borrows the context. A
    /// `Local` of an enclosing scope is rooted again here to become this scope's result.
    #[inline]
    pub fn root<T: Kind>(&self, value: impl Pointer<T>) -> Local<'s, T> {
        let raw = value.raw();
        // SAFETY: the heap is live (`enter`), and the handle stack is not borrowed by anything
        // a `&Cx` hands out.
        unsafe { (*self.heap.as_ptr()).root(raw) };
        Local {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// The answer `read` finds in the heap, or else the answer of `rest`, which may allocate.
    /// Because the answer borrows the context, a caller returns it before the next safepoint
    /// and it needs no root.
    #[inline]
    pub fn read_or<'a, T: Kind, E>(
        &'a mut self,
        read: impl for<'r> FnOnce(&'r Cx<'s>) -> Option<Gc<'r, T>>,
        rest: impl FnOnce(&'a mut Cx<'s>) -> Result<Option<Gc<'a, T>>, E>,
    ) -> Result<Option<Gc<'a, T>>, E> {
        let this: *mut Cx<'s> = self;
        // SAFETY: `this` is the exclusive borrow `self`, used as a shared borrow for `'a` when
        // `read` answers and as itself when `read` declines, never both: `read` must accept
        // every lifetime, so it keeps neither the context nor a pointer when it declines.
        // The borrow checker cannot see that the two uses exclude each other (its "problem
        // case 3"), which is the only reason this is not written as a plain `if let`.
        if let Some(answer) = read(unsafe { &*this }) {
            return Ok(Some(answer));
        }
        rest(unsafe { &mut *this })
    }

    /// A new vector of `length` elements: zeros, `NULL`s or empty strings.
    #[inline]
    pub fn alloc<T: Vector>(&mut self, length: usize) -> HeapResult<Fresh<'s, T>> {
        // SAFETY: the heap is live, and `&mut self` means no slice or unrooted value is alive.
        let heap = unsafe { &mut *self.heap.as_ptr() };
        // SAFETY: `T::TYPE` is a vector type, and a length over `isize::MAX` casts to a
        // negative one, which the heap refuses.
        let raw = unsafe { heap.alloc_vector_rooted(T::TYPE, length as isize)? };
        Ok(Fresh {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        })
    }

    /// A new vector of `length` elements for one fill that writes every element: a number
    /// vector skips the zeroing [`Cx::alloc`] does.
    #[inline]
    pub fn alloc_unfilled<T: Vector>(&mut self, length: usize) -> HeapResult<Unfilled<'s, T>> {
        // SAFETY: the heap is live, and `&mut self` means no slice or unrooted value is alive.
        let heap = unsafe { &mut *self.heap.as_ptr() };
        let raw = match T::TYPE {
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::I64
            | SexpType::Complex
            | SexpType::Raw => {
                // SAFETY: the elements are written before anything reads them, by
                // `Unfilled::fill`, the only way to the vector.
                unsafe { heap.alloc_vector_uninit(T::TYPE, length)? }
            }
            // A list or string vector is filled with `NULL` or the empty string, because a
            // collection may trace it before the fill. The heap's inline path keeps that one
            // call deep, as for a number vector.
            // SAFETY: `T::TYPE` is a vector type, and a length past `isize` wraps to a
            // negative one, which the heap refuses.
            _ => unsafe { heap.alloc_vector_inline(T::TYPE, length as isize)? },
        };
        heap.root(raw);
        Ok(Unfilled {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        })
    }

    /// A new vector of the one element `value`.
    #[inline]
    pub fn scalar<T: Atomic>(&mut self, value: T::Element) -> HeapResult<Fresh<'s, T>> {
        // The heap's scalar slot covers the five R number types; an `I64` takes the general path.
        if T::TYPE == SexpType::I64 {
            return Ok(self.alloc_unfilled::<T>(1)?.fill(std::iter::once(value)));
        }
        const { assert!(size_of::<T::Element>() <= 16) };
        let mut payload = [0u64; 2];
        // SAFETY: an atomic element is plain data of at most the payload's 16 bytes, and the
        // two do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&raw const value).cast::<u8>(),
                payload.as_mut_ptr().cast::<u8>(),
                size_of::<T::Element>(),
            );
        }
        // SAFETY: the heap is live, and `&mut self` means no slice or unrooted value is alive.
        let heap = unsafe { &mut *self.heap.as_ptr() };
        // SAFETY: `T::TYPE` is one of the number types the scalar slot holds.
        let raw = unsafe { heap.alloc_scalar(T::TYPE, payload)? };
        heap.root(raw);
        Ok(Fresh {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        })
    }

    /// Runs `body` in a nested handle scope: whatever it roots is released on return, and its
    /// result is rooted again in this scope.
    #[inline]
    pub fn scope<T: Kind, E: Reroot>(
        &mut self,
        body: impl for<'inner> FnOnce(&mut Cx<'inner>) -> Result<Local<'inner, T>, E>,
    ) -> Result<Local<'s, T>, E> {
        // SAFETY: the heap is live.
        let mark = unsafe { (*self.heap.as_ptr()).handle_mark() };
        let mut inner = Cx {
            heap: self.heap,
            scope: PhantomData,
        };
        let result = body(&mut inner);
        // SAFETY: the heap is live. Every `Local` and `Fresh` of the inner scope is gone,
        // because `'inner` is not nameable outside `body`, so nothing relies on the handles
        // above `mark` except the result, which is rooted again before anything can collect.
        let heap = unsafe { &mut *self.heap.as_ptr() };
        heap.release_handles(mark);
        match result {
            Ok(value) => {
                heap.root(value.raw);
                Ok(Local {
                    raw: value.raw,
                    scope: PhantomData,
                    kind: PhantomData,
                })
            }
            Err(error) => {
                error.reroot(heap);
                Err(error)
            }
        }
    }
}

/// An error type a scope can pass outward: it roots again whatever heap objects it carries.
///
/// # Safety
///
/// After `reroot`, every object the error gives access to is on the handle stack.
pub unsafe trait Reroot {
    fn reroot(&self, heap: &mut Heap);
}

// SAFETY: a heap error carries no object.
unsafe impl Reroot for crate::HeapError {
    fn reroot(&self, _heap: &mut Heap) {}
}

/// A pointer read from the heap and not rooted. It borrows the context it was read through, so
/// it ends at the next safepoint.
pub struct Gc<'a, T: Kind = Value> {
    raw: Sexp,
    borrow: PhantomData<&'a ()>,
    kind: PhantomData<T>,
}

impl<T: Kind> Clone for Gc<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Kind> Copy for Gc<'_, T> {}

impl<'a, T: Kind> Gc<'a, T> {
    /// # Safety
    ///
    /// `raw` is a live object that `T` admits, and it stays live for `'a`.
    #[inline]
    pub unsafe fn adopt(raw: Sexp) -> Gc<'a, T> {
        Gc {
            raw,
            borrow: PhantomData,
            kind: PhantomData,
        }
    }

    #[inline]
    pub fn raw(self) -> Sexp {
        self.raw
    }

    #[inline]
    pub fn value(self) -> Gc<'a, Value> {
        Gc {
            raw: self.raw,
            borrow: PhantomData,
            kind: PhantomData,
        }
    }

    #[inline]
    pub fn r_type(self) -> SexpType {
        // SAFETY: the object is live for `'a`.
        unsafe { self.raw.kind() }
    }

    /// Whether this and `other` are one object. Strings have no such test, because one text
    /// can be several objects: compare them with `interp::handles::same_text`.
    #[inline]
    pub fn is<U: Identity>(self, other: impl Pointer<U>) -> bool
    where
        T: Identity,
    {
        self.raw == other.raw()
    }

    /// This pointer as a `U`, when the object is one.
    #[inline]
    pub fn downcast<U: Kind>(self) -> Option<Gc<'a, U>> {
        admits::<U>(self.raw).then_some(Gc {
            raw: self.raw,
            borrow: PhantomData,
            kind: PhantomData,
        })
    }
}

impl<'a, T: Vector> Gc<'a, T> {
    #[inline]
    pub fn len(self) -> usize {
        // SAFETY: a standard vector keeps its length in its header.
        unsafe { self.raw.length() as usize }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

impl<'a, T: Atomic> Gc<'a, T> {
    /// The elements, which live as long as the borrow that keeps the object from collection.
    #[inline]
    pub fn as_slice(self) -> &'a [T::Element] {
        // SAFETY: the vector stores `length` initialized elements, and no write is open while
        // the context is borrowed shared.
        unsafe { self.raw.slice() }
    }
}

impl<'a, T: Elements> Gc<'a, T> {
    #[inline]
    pub fn get(self, index: usize) -> Gc<'a, T::Element> {
        assert!(index < self.len(), "element index out of bounds");
        // SAFETY: the index is in bounds of a standard vector of pointers, each a live object
        // of the element kind.
        unsafe { Gc::adopt(*(self.raw.data() as *const Sexp).add(index)) }
    }
}

impl<'a> Gc<'a, Chars> {
    /// The string's bytes.
    #[inline]
    pub fn bytes(self) -> &'a [u8] {
        // SAFETY: a live `CHARSXP` holds `length` bytes, and it is live for `'a`.
        unsafe { self.raw.bytes() }
    }

    /// Whether any of the encoding marks `bits` (`object::gp`) is set.
    #[inline]
    pub fn has_mark(self, bits: u16) -> bool {
        // SAFETY: the pointer is a live object, whose header holds the marks.
        unsafe { self.raw.has_gp(bits) }
    }

    /// Whether the heap found the bytes to be valid UTF-8 when it interned them.
    #[inline]
    pub fn is_valid_utf8(self) -> bool {
        // SAFETY: the pointer is a live `CHARSXP`.
        unsafe { self.raw.is_valid_utf8() }
    }
}

impl<'a> Gc<'a, Symbol> {
    /// The symbol's print name.
    #[inline]
    pub fn name(self) -> Gc<'a, Chars> {
        // SAFETY: a symbol's `car` is its print name, a `CHARSXP` that the symbol keeps alive.
        unsafe { Gc::adopt(self.raw.car()) }
    }
}

impl<'a> Gc<'a, Pairlist> {
    /// The cell's value.
    #[inline]
    pub fn head(self) -> Gc<'a> {
        // SAFETY: the cell is live for `'a` and keeps its fields alive.
        unsafe { Gc::adopt(self.raw.car()) }
    }

    /// The cell's tag: a symbol, or `NULL`.
    #[inline]
    pub fn tag(self) -> Gc<'a> {
        // SAFETY: as for `head`.
        unsafe { Gc::adopt(self.raw.tag()) }
    }

    /// The next cell, or what ends the list.
    #[inline]
    pub fn rest(self) -> Gc<'a> {
        // SAFETY: as for `head`.
        unsafe { Gc::adopt(self.raw.cdr()) }
    }
}

impl<'a> Gc<'a, Language> {
    /// The function position of the call.
    #[inline]
    pub fn head(self) -> Gc<'a> {
        // SAFETY: the cell is live for `'a` and keeps its fields alive.
        unsafe { Gc::adopt(self.raw.car()) }
    }

    /// The call's arguments: a pairlist, or `NULL`.
    #[inline]
    pub fn rest(self) -> Gc<'a> {
        // SAFETY: as for `head`.
        unsafe { Gc::adopt(self.raw.cdr()) }
    }
}

impl<'a> Gc<'a, Promise> {
    /// The expression the promise evaluates.
    #[inline]
    pub fn code(self) -> Gc<'a> {
        // SAFETY: the promise is live for `'a` and keeps its expression alive.
        unsafe { Gc::adopt(self.raw.cdr()) }
    }
}

/// A pointer rooted in the handle scope `'s`.
#[repr(transparent)]
pub struct Local<'s, T: Kind = Value> {
    raw: Sexp,
    scope: Brand<'s>,
    kind: PhantomData<T>,
}

impl<T: Kind> Clone for Local<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Kind> Copy for Local<'_, T> {}

impl<'s, T: Kind> Local<'s, T> {
    /// # Safety
    ///
    /// `raw` is a live object that `T` admits, and a root keeps it alive for `'s`.
    #[inline]
    pub unsafe fn adopt(raw: Sexp) -> Local<'s, T> {
        Local {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// A caller's raw values as values of the scope, without copying them.
    ///
    /// # Safety
    ///
    /// Each of `raw` is a live object that `T` admits, and a root keeps it alive for `'s`.
    #[inline]
    pub unsafe fn adopt_slice<'a>(raw: &'a [Sexp]) -> &'a [Local<'s, T>] {
        // SAFETY: the caller's contract, and `Local` is `repr(transparent)` over `Sexp`.
        unsafe { &*(std::ptr::from_ref(raw) as *const [Local<'s, T>]) }
    }

    #[inline]
    pub fn raw(self) -> Sexp {
        self.raw
    }

    #[inline]
    pub fn value(self) -> Local<'s, Value> {
        Local {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    #[inline]
    pub fn r_type(self) -> SexpType {
        // SAFETY: the object is rooted.
        unsafe { self.raw.kind() }
    }

    /// Whether more than one object refers to this one, so that a change must go into a copy.
    /// An element of a shared list is reachable through that list's other owners even when
    /// this is false for the element.
    #[inline]
    pub fn is_shared(self) -> bool {
        // SAFETY: the object is rooted.
        unsafe { self.raw.refcount() > 1 }
    }

    /// Whether this and `other` are one object. Strings have no such test, because one text
    /// can be several objects: compare them with `interp::handles::same_text`.
    #[inline]
    pub fn is<U: Identity>(self, other: impl Pointer<U>) -> bool
    where
        T: Identity,
    {
        self.raw == other.raw()
    }

    /// This pointer as a `U`, when the object is one.
    #[inline]
    pub fn downcast<U: Kind>(self) -> Option<Local<'s, U>> {
        admits::<U>(self.raw).then_some(Local {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        })
    }

    /// The object as a pointer that borrows the context, for the readers [`Gc`] has.
    #[inline]
    pub fn read<'a>(self, _cx: &'a Cx<'_>) -> Gc<'a, T>
    where
        's: 'a,
    {
        Gc {
            raw: self.raw,
            borrow: PhantomData,
            kind: PhantomData,
        }
    }

    /// The attribute list, or `NULL`.
    #[inline]
    pub fn attributes<'a>(self, _cx: &'a Cx<'_>) -> Gc<'a, Value>
    where
        's: 'a,
    {
        // SAFETY: the object is rooted, and what it refers to is live until a safepoint.
        unsafe { Gc::adopt(self.raw.attrib()) }
    }
}

impl<'s, T: Vector> Local<'s, T> {
    #[inline]
    pub fn len(self) -> usize {
        // SAFETY: a standard vector keeps its length in its header.
        unsafe { self.raw.length() as usize }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

impl<'s, T: Atomic> Local<'s, T> {
    /// The elements. The slice borrows the context, so no safepoint and no in-place write
    /// happens while it lives.
    #[inline]
    pub fn as_slice<'a>(self, _cx: &'a Cx<'_>) -> &'a [T::Element]
    where
        's: 'a,
    {
        // SAFETY: a standard vector of this kind stores `length` elements of `T::Element`,
        // and writes to a visible object need `&mut Cx`.
        unsafe { self.raw.slice() }
    }
}

impl<'s, T: Elements> Local<'s, T> {
    /// One element. A list or string vector hands out no slice, so a store through `&Cx`
    /// never changes memory a slice borrows.
    #[inline]
    pub fn get<'a>(self, _cx: &'a Cx<'_>, index: usize) -> Gc<'a, T::Element>
    where
        's: 'a,
    {
        assert!(index < self.len(), "element index out of bounds");
        // SAFETY: the index is in bounds, a standard vector of this kind stores pointers to
        // live objects of the element kind, and they stay live until a safepoint.
        unsafe { Gc::adopt(*(self.raw.data() as *const Sexp).add(index)) }
    }

    /// The elements in order. The walk reads the length once, which is what lets a loop that
    /// also fills a result compile to one counter: a length reloaded for each element cannot
    /// be hoisted past the stores into the result.
    #[inline]
    pub fn iter<'a>(self, _cx: &'a Cx<'_>) -> impl ExactSizeIterator<Item = Gc<'a, T::Element>>
    where
        's: 'a,
    {
        // SAFETY: a standard vector of this kind stores `length` pointers to live objects of
        // the element kind. Neither the length nor an element of a visible object changes
        // without `&mut Cx`, which the borrow of the context rules out while the walk lives.
        let elements: &'a [Sexp] = unsafe { self.raw.slice() };
        // SAFETY: each element stays live until a safepoint, which also needs `&mut Cx`.
        elements
            .iter()
            .map(|&element| unsafe { Gc::adopt(element) })
    }
}

impl<'s> Local<'s, Chars> {
    /// The string's bytes. They borrow the context although the string is rooted, because
    /// native code can write into a string's bytes in place.
    #[inline]
    pub fn bytes<'a>(self, _cx: &'a Cx<'_>) -> &'a [u8]
    where
        's: 'a,
    {
        // SAFETY: a live `CHARSXP` holds `length` bytes, it is rooted for `'s`, and nothing
        // runs native code while `_cx` is borrowed.
        unsafe { self.raw.bytes() }
    }
}

/// A new object nothing else can see. It is the only pointer to its object until `finish`,
/// which is why filling it needs no ownership check.
pub struct Fresh<'s, T: Vector> {
    raw: Sexp,
    scope: Brand<'s>,
    kind: PhantomData<T>,
}

impl<'s, T: Vector> Fresh<'s, T> {
    /// A new object some other code allocated, such as a copy.
    ///
    /// # Safety
    ///
    /// `raw` is a standard vector of type `T::TYPE`, it is rooted until `'s` ends, and no other
    /// pointer to it exists.
    #[inline]
    pub unsafe fn adopt(raw: Sexp) -> Fresh<'s, T> {
        debug_assert!(admits::<T>(raw), "a pointer of the wrong kind");
        Fresh {
            raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        // SAFETY: a standard vector keeps its length in its header.
        unsafe { self.raw.length() as usize }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The raw pointer, for code that calls the raw API.
    #[inline]
    pub fn raw(&self) -> Sexp {
        self.raw
    }

    /// The object as a value other code may see. It was rooted when it was allocated.
    #[inline]
    pub fn finish(self) -> Local<'s, T> {
        Local {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// The object as one its holder may keep writing after other code has read it.
    #[inline]
    pub fn owned(self) -> Owned<'s, T> {
        Owned {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }
}

impl<T: Atomic> Fresh<'_, T> {
    #[inline]
    pub fn as_slice(&self) -> &[T::Element] {
        // SAFETY: the vector stores `length` initialized elements, and only `self` reaches it.
        unsafe { self.raw.slice() }
    }

    /// The elements, to fill. It needs no context, because the object holds no pointers and a
    /// collection in between leaves it alone.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T::Element] {
        // SAFETY: as for `as_slice`, and `&mut self` makes this the only borrow.
        unsafe { self.raw.slice_mut() }
    }
}

impl<T: Elements> Fresh<'_, T> {
    /// Stores one element. A collection between two stores can promote the object, so each
    /// store runs the write barrier.
    #[inline]
    pub fn set(&mut self, cx: &Cx<'_>, index: usize, value: impl Pointer<T::Element>) {
        assert!(index < self.len(), "element index out of bounds");
        // SAFETY: the heap is live, the index is in bounds, the vector is a standard one, and
        // the value is a live object of the element kind. A store frees nothing.
        unsafe { (*cx.heap.as_ptr()).set_element(self.raw, index, value.raw()) }
    }
}

/// A new vector whose elements are still to be written, from [`Cx::alloc_unfilled`]. It holds
/// no borrow of the context, so the elements may come from slices the context lends after the
/// allocation, and its one fill writes them in a single pass.
pub struct Unfilled<'s, T: Vector> {
    raw: Sexp,
    scope: Brand<'s>,
    kind: PhantomData<T>,
}

impl<'s, T: Atomic> Unfilled<'s, T> {
    /// The vector with `elements` in order, and zeros after them when they run out.
    #[inline(always)]
    pub fn fill(self, elements: impl Iterator<Item = T::Element>) -> Fresh<'s, T> {
        // SAFETY: the vector is a standard one of `T` that only `self` reaches, with room for
        // `length` elements, and any bit pattern written as `MaybeUninit` is a valid slot.
        let slots = unsafe {
            std::slice::from_raw_parts_mut(
                self.raw.data() as *mut MaybeUninit<T::Element>,
                self.raw.length() as usize,
            )
        };
        let mut slots = slots.iter_mut();
        for (element, slot) in elements.zip(slots.by_ref()) {
            slot.write(element);
        }
        let rest = slots.into_slice();
        // SAFETY: zero bytes are a valid number of every atomic element type.
        unsafe { std::ptr::write_bytes(rest.as_mut_ptr(), 0, rest.len()) };
        Fresh {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }
}

impl<'s, T: Elements> Unfilled<'s, T> {
    /// The vector with `elements` in order, and the type's fill (`NULL` or the empty string)
    /// after them. The slots are written directly and counted as references once afterwards,
    /// which costs a list or string vector one pass instead of a barrier per element.
    #[inline(always)]
    pub fn fill_elements<'a>(
        self,
        cx: &'a Cx<'_>,
        elements: impl Iterator<Item = Gc<'a, T::Element>>,
    ) -> Fresh<'s, T> {
        // SAFETY: the vector is a standard one of `T` that only `self` reaches, so no slice of
        // it is alive, and each element is a live object of the element kind. Every slot held
        // the uncounted fill `alloc_vector` writes, which `count_written_elements` requires.
        unsafe {
            for (slot, element) in self.raw.slice_mut::<Sexp>().iter_mut().zip(elements) {
                *slot = element.raw();
            }
            (*cx.heap.as_ptr()).count_written_elements(self.raw);
        }
        Fresh {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }
}

/// Exclusive access to an existing object. It holds the context, so no R code runs, nothing
/// allocates and no slice taken earlier is alive while the object is written.
pub struct Write<'a, 's, T: Vector> {
    cx: &'a mut Cx<'s>,
    raw: Sexp,
    kind: PhantomData<T>,
}

impl<T: Vector> Write<'_, '_, T> {
    #[inline]
    pub fn len(&self) -> usize {
        // SAFETY: a standard vector keeps its length in its header.
        unsafe { self.raw.length() as usize }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Atomic> Write<'_, '_, T> {
    /// The elements to write, and the reader for every other object.
    #[inline]
    pub fn split(&mut self) -> (&mut [T::Element], Reader<'_>) {
        (
            // SAFETY: the object is the caller's alone (the `Owned` it came from), and the reader
            // refuses to hand out its storage a second time.
            unsafe { self.raw.slice_mut() },
            Reader {
                target: self.raw,
                borrow: PhantomData,
            },
        )
    }
}

impl<T: Elements> Write<'_, '_, T> {
    /// The element stores, and the reader for every object including this one.
    #[inline]
    pub fn split_elements(&mut self) -> (Stores<'_, T>, Reader<'_>) {
        (
            Stores {
                heap: self.cx.heap,
                raw: self.raw,
                borrow: PhantomData,
                kind: PhantomData,
            },
            Reader {
                target: self.raw,
                borrow: PhantomData,
            },
        )
    }
}

/// The writing half of a [`Write`] to a list or string vector.
pub struct Stores<'r, T: Elements> {
    heap: NonNull<Heap>,
    raw: Sexp,
    borrow: PhantomData<&'r mut ()>,
    kind: PhantomData<T>,
}

impl<T: Elements> Stores<'_, T> {
    #[inline(always)]
    pub fn set(&mut self, index: usize, value: impl Pointer<T::Element>) {
        // SAFETY: a standard vector keeps its length in its header.
        assert!(
            index < unsafe { self.raw.length() as usize },
            "element index out of bounds"
        );
        // SAFETY: as for `Fresh::set`.
        unsafe { (*self.heap.as_ptr()).set_element(self.raw, index, value.raw()) }
    }
}

/// Reads other objects while a [`Write`] is open.
pub struct Reader<'r> {
    target: Sexp,
    borrow: PhantomData<&'r ()>,
}

impl<'r> Reader<'r> {
    /// The elements of `source`, or `None` when `source` is the object being written. The
    /// caller then reads from the mutable slice it already holds.
    #[inline]
    pub fn slice<'s: 'r, T: Atomic>(&self, source: Local<'s, T>) -> Option<&'r [T::Element]> {
        // SAFETY: the source is rooted, nothing allocates while the write is open, and it is
        // not the object being written.
        (source.raw != self.target).then(|| unsafe { source.raw.slice() })
    }

    /// One element of `source`, which may be the object being written: a single load never
    /// overlaps a store.
    #[inline]
    pub fn element<'s: 'r, T: Elements>(
        &self,
        source: Local<'s, T>,
        index: usize,
    ) -> Gc<'r, T::Element> {
        assert!(index < source.len(), "element index out of bounds");
        // SAFETY: as for `Local::get`. Stores made meanwhile free nothing.
        unsafe { Gc::adopt(*(source.raw.data() as *const Sexp).add(index)) }
    }
}

/// An object the caller alone refers to, apart from the one place it is stored in, so changing
/// it where it is changes no other R value. A [`Fresh`] vector is one; an existing object
/// becomes one through [`Owned::assume`], after the test its holder defines (a reference count
/// of one and no uncounted holder). Code that updates a variable asks for its `Owned` value
/// once, copies when there is none, and writes only through the result.
#[repr(transparent)]
pub struct Owned<'s, T: Kind = Value> {
    raw: Sexp,
    scope: Brand<'s>,
    kind: PhantomData<T>,
}

impl<'s, T: Kind> Owned<'s, T> {
    /// # Safety
    ///
    /// Nothing else can observe `object`: no variable, register, handle or other object refers
    /// to it except the one place the caller is updating.
    #[inline]
    pub unsafe fn assume(object: Local<'s, T>) -> Owned<'s, T> {
        Owned {
            raw: object.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// The object, to read.
    #[inline]
    pub fn get(&self) -> Local<'s, T> {
        Local {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// The object as the value its holder stores.
    #[inline]
    pub fn finish(self) -> Local<'s, T> {
        self.get()
    }

    /// The object as a value of any kind.
    #[inline]
    pub fn value(self) -> Owned<'s, Value> {
        Owned {
            raw: self.raw,
            scope: PhantomData,
            kind: PhantomData,
        }
    }

    /// The object as kind `U`, or itself when it is not one.
    #[inline]
    pub fn downcast<U: Kind>(self) -> Result<Owned<'s, U>, Owned<'s, T>> {
        if admits::<U>(self.raw) {
            Ok(Owned {
                raw: self.raw,
                scope: PhantomData,
                kind: PhantomData,
            })
        } else {
            Err(self)
        }
    }

    /// The object as kind `U` while the borrow lasts, or `None` when it is not one. Code whose
    /// vector kind is known only at run time keeps one `Owned` value and narrows it per write.
    #[inline]
    pub fn narrow<U: Kind>(&mut self) -> Option<&mut Owned<'s, U>> {
        // SAFETY: `Owned` is `repr(transparent)` over the pointer and the kind is a marker, so
        // the two types have one layout, and `admits` checked the object's kind.
        admits::<U>(self.raw)
            .then(|| unsafe { &mut *std::ptr::from_mut(self).cast::<Owned<'s, U>>() })
    }
}

impl<'s, T: Vector> Owned<'s, T> {
    /// Access to write the vector's elements where they are.
    #[inline]
    pub fn write<'a>(&'a mut self, cx: &'a mut Cx<'s>) -> Write<'a, 's, T> {
        Write {
            cx,
            raw: self.raw,
            kind: PhantomData,
        }
    }
}

/// A pointer to a live object of kind `T`, rooted or not. Only this module's pointer types
/// implement it, because whatever does is trusted to name a live object of that kind.
pub trait Pointer<T: Kind>: Copy + sealed::Sealed {
    fn raw(self) -> Sexp;
}

impl<T: Kind> sealed::Sealed for Gc<'_, T> {}

impl<T: Kind> sealed::Sealed for Local<'_, T> {}

impl<T: Kind> Pointer<T> for Gc<'_, T> {
    #[inline]
    fn raw(self) -> Sexp {
        self.raw
    }
}

impl<T: Kind> Pointer<T> for Local<'_, T> {
    #[inline]
    fn raw(self) -> Sexp {
        self.raw
    }
}

/// What a pointer's object is. A vector kind means a standard vector, with its length in the
/// header and its elements behind it. An ALTREP object is an [`Alt`] whatever R type it
/// reports, so no element access reaches an object without element storage.
pub trait Kind: sealed::Sealed + 'static {
    #[doc(hidden)]
    fn admits(kind: SexpType, altrep: bool) -> bool;
}

/// A kind whose objects are compared by identity. Every kind but [`Chars`] is one: the heap
/// interns a text once per encoding mark, so two strings with the same text can be two objects.
pub trait Identity: Kind {}

/// A standard vector.
pub trait Vector: Kind {
    const TYPE: SexpType;
}

/// A vector of plain data.
pub trait Atomic: Vector {
    type Element: Copy + 'static;
}

/// A vector of pointers.
pub trait Elements: Vector {
    type Element: Kind;
}

/// Any object.
pub struct Value;

/// An ALTREP object. Its elements and its length come from its class's methods, which are
/// native code, so reading them is a safepoint.
pub struct Alt;

impl sealed::Sealed for Value {}

impl Kind for Value {
    fn admits(_kind: SexpType, _altrep: bool) -> bool {
        true
    }
}

impl Identity for Value {}

impl sealed::Sealed for Alt {}

impl Identity for Alt {}

impl Kind for Alt {
    fn admits(_kind: SexpType, altrep: bool) -> bool {
        altrep
    }
}

macro_rules! kinds {
    ($($(#[$doc:meta])* $name:ident = $type:ident, |$altrep:ident| $standard:expr;)*) => {$(
        $(#[$doc])*
        pub struct $name;

        impl sealed::Sealed for $name {}

        impl Kind for $name {
            #[inline]
            fn admits(kind: SexpType, $altrep: bool) -> bool {
                kind == SexpType::$type && $standard
            }
        }

        impl Identity for $name {}
    )*};
}

// Only a vector can be an ALTREP object (`Heap::alloc_altrep`), so the other kinds are told by
// their type alone, which a walk over an attribute list tests for every cell and tag.
kinds! {
    Logical = Logical, |altrep| !altrep;
    Integer = Integer, |altrep| !altrep;
    Real = Real, |altrep| !altrep;
    Complex = Complex, |altrep| !altrep;
    Raw = Raw, |altrep| !altrep;
    /// A vector of 64-bit integers.
    I64 = I64, |altrep| !altrep;
    /// A character vector.
    Str = String, |altrep| !altrep;
    List = List, |altrep| !altrep;
    Expression = Expression, |altrep| !altrep;
    Symbol = Symbol, |_altrep| true;
    Pairlist = Pairlist, |_altrep| true;
    Language = Language, |_altrep| true;
    Closure = Closure, |_altrep| true;
    Environment = Environment, |_altrep| true;
    Promise = Promise, |_altrep| true;
}

/// One string, as a character vector's element. It is not an [`Identity`] kind.
pub struct Chars;

impl sealed::Sealed for Chars {}

impl Kind for Chars {
    #[inline]
    fn admits(kind: SexpType, _altrep: bool) -> bool {
        kind == SexpType::Char
    }
}

macro_rules! vectors {
    ($($name:ident: $trait:ident<$element:ty>;)*) => {$(
        impl Vector for $name {
            const TYPE: SexpType = SexpType::$name;
        }

        impl $trait for $name {
            type Element = $element;
        }
    )*};
}

vectors! {
    Logical: Atomic<i32>;
    Integer: Atomic<i32>;
    Real: Atomic<f64>;
    Complex: Atomic<Rcomplex>;
    Raw: Atomic<u8>;
    I64: Atomic<i64>;
    List: Elements<Value>;
    Expression: Elements<Value>;
}

impl Vector for Str {
    const TYPE: SexpType = SexpType::String;
}

impl Elements for Str {
    type Element = Chars;
}

mod sealed {
    pub trait Sealed {}
}

/// Invariant in `'s`, so a pointer of one scope never passes for a pointer of another.
type Brand<'s> = PhantomData<fn(&'s ()) -> &'s ()>;

#[inline]
fn admits<T: Kind>(raw: Sexp) -> bool {
    // SAFETY: every caller holds a live object.
    unsafe { T::admits(raw.kind(), raw.is_altrep()) }
}
