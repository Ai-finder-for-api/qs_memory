//! Doubly-linked intrusive list: the links live inside the elements, so the list itself
//! never allocates.
//!
//! An element embeds a [`Link`] and implements [`Linked`] (the `linked_by!` macro does
//! this safely, checking that the field really is a `Link`). The list stores only head,
//! tail and length, so it can be moved freely; elements must stay at a fixed address
//! while they are linked.
//!
//! Insertion and removal take `&self`, because the links use `Cell`. The list is
//! therefore not `Sync`.

use core::cell::Cell;
use core::marker::PhantomData;
use core::ptr;

/// Link fields embedded in a list element.
#[derive(Debug)]
pub struct Link {
    prev: Cell<*const Link>,
    next: Cell<*const Link>,
    linked: Cell<bool>,
}

impl Link {
    /// An unlinked link.
    pub const fn new() -> Self {
        Link { prev: Cell::new(ptr::null()), next: Cell::new(ptr::null()), linked: Cell::new(false) }
    }

    /// True while the element is on a list.
    pub fn is_linked(&self) -> bool {
        self.linked.get()
    }
}

impl Default for Link {
    fn default() -> Self {
        Self::new()
    }
}

/// An element that embeds a [`Link`].
///
/// # Safety
/// `LINK_OFFSET` must be the byte offset of a `Link` field inside `Self`. Use the
/// `linked_by!` macro, which derives it from the field name.
pub unsafe trait Linked: Sized {
    /// Byte offset of the embedded `Link`.
    const LINK_OFFSET: usize;

    /// The element's link.
    fn link(&self) -> &Link {
        // SAFETY: per the trait contract, `LINK_OFFSET` lies inside `self` and holds a `Link`.
        unsafe { &*((self as *const Self as *const u8).add(Self::LINK_OFFSET) as *const Link) }
    }

    /// Recovers the element that embeds `link`.
    ///
    /// # Safety
    /// `link` must be the link of a live `Self`.
    unsafe fn from_link(link: *const Link) -> *const Self {
        // SAFETY: per the contract, subtracting the offset yields the start of the element.
        unsafe { (link as *const u8).sub(Self::LINK_OFFSET) as *const Self }
    }
}

/// Implements [`Linked`] for a type whose field `$field` is a `Link`.
#[macro_export]
macro_rules! linked_by {
    ($ty:ty, $field:ident) => {
        const _: () = {
            #[allow(dead_code)]
            fn assert_link_field(t: &$ty) -> &$crate::structures::intrusive::Link {
                &t.$field
            }
        };
        // SAFETY: `offset_of!` gives the exact offset of the field, and the assertion above
        // checks at compile time that the field's type is `Link`.
        unsafe impl $crate::structures::intrusive::Linked for $ty {
            const LINK_OFFSET: usize = core::mem::offset_of!($ty, $field);
        }
    };
}

/// A doubly-linked list of elements that embed a [`Link`].
pub struct IntrusiveList<T: Linked> {
    head: Cell<*const Link>,
    tail: Cell<*const Link>,
    len: Cell<usize>,
    _owner: PhantomData<*const T>,
}

impl<T: Linked> Default for IntrusiveList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Linked> IntrusiveList<T> {
    /// An empty list.
    pub const fn new() -> Self {
        IntrusiveList {
            head: Cell::new(ptr::null()),
            tail: Cell::new(ptr::null()),
            len: Cell::new(0),
            _owner: PhantomData,
        }
    }

    /// Number of linked elements.
    pub fn len(&self) -> usize {
        self.len.get()
    }

    /// True when no element is linked.
    pub fn is_empty(&self) -> bool {
        self.len.get() == 0
    }

    /// Appends `item`.
    ///
    /// # Safety
    /// `item` must not already be linked, and it must stay at this address until it is
    /// removed from the list or the list is dropped.
    pub unsafe fn push_back(&self, item: &T) {
        let l = item.link();
        debug_assert!(!l.is_linked(), "element is already linked");
        let tail = self.tail.get();
        l.linked.set(true);
        l.prev.set(tail);
        l.next.set(ptr::null());
        if tail.is_null() {
            self.head.set(l);
        } else {
            // SAFETY: a non-null tail is a linked element of this list, so it is valid.
            unsafe { (*tail).next.set(l) };
        }
        self.tail.set(l);
        self.len.set(self.len.get() + 1);
    }

    /// Prepends `item`.
    ///
    /// # Safety
    /// Same contract as [`IntrusiveList::push_back`].
    pub unsafe fn push_front(&self, item: &T) {
        let l = item.link();
        debug_assert!(!l.is_linked(), "element is already linked");
        let head = self.head.get();
        l.linked.set(true);
        l.prev.set(ptr::null());
        l.next.set(head);
        if head.is_null() {
            self.tail.set(l);
        } else {
            // SAFETY: a non-null head is a linked element of this list, so it is valid.
            unsafe { (*head).prev.set(l) };
        }
        self.head.set(l);
        self.len.set(self.len.get() + 1);
    }

    /// Unlinks `l` from this list.
    ///
    /// # Safety
    /// `l` must be a link currently on this list.
    unsafe fn unlink(&self, l: &Link) {
        let prev = l.prev.get();
        let next = l.next.get();
        // SAFETY: neighbours of a linked element are linked elements of this list.
        unsafe {
            if prev.is_null() {
                self.head.set(next);
            } else {
                (*prev).next.set(next);
            }
            if next.is_null() {
                self.tail.set(prev);
            } else {
                (*next).prev.set(prev);
            }
        }
        l.prev.set(ptr::null());
        l.next.set(ptr::null());
        l.linked.set(false);
        self.len.set(self.len.get() - 1);
    }

    /// Removes `item` if it is on this list. Returns whether it was removed.
    ///
    /// # Safety
    /// `item` must be an element that was linked into *some* list of type `T`; if it is
    /// linked into a different list, that list's invariants are broken.
    pub unsafe fn erase(&self, item: &T) -> bool {
        let l = item.link();
        if !l.is_linked() {
            return false;
        }
        // SAFETY: `l` is linked, so it is on a list of this type; the caller guarantees it is ours.
        unsafe { self.unlink(l) };
        true
    }

    /// The first element, if any.
    pub fn front(&self) -> Option<&T> {
        let h = self.head.get();
        if h.is_null() {
            None
        } else {
            // SAFETY: a non-null head is a live element linked into this list.
            Some(unsafe { &*T::from_link(h) })
        }
    }

    /// Removes and returns the first element.
    ///
    /// # Safety
    /// The returned reference must not be used after the element is dropped.
    pub unsafe fn pop_front(&self) -> Option<&T> {
        let h = self.head.get();
        if h.is_null() {
            return None;
        }
        // SAFETY: `h` is the head, a linked element of this list.
        unsafe {
            self.unlink(&*h);
            Some(&*T::from_link(h))
        }
    }

    /// Iterates from front to back.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter { cur: self.head.get(), _list: PhantomData }
    }
}

/// Front-to-back iterator over an [`IntrusiveList`].
pub struct Iter<'a, T: Linked> {
    cur: *const Link,
    _list: PhantomData<&'a IntrusiveList<T>>,
}

impl<'a, T: Linked> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        if self.cur.is_null() {
            return None;
        }
        let l = self.cur;
        // SAFETY: `cur` is a linked element of the list borrowed for `'a`.
        unsafe {
            self.cur = (*l).next.get();
            Some(&*T::from_link(l))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Job {
        id: u32,
        hook: Link,
    }

    crate::linked_by!(Job, hook);

    fn jobs(n: u32) -> Vec<Job> {
        (0..n).map(|id| Job { id, hook: Link::new() }).collect()
    }

    fn ids(list: &IntrusiveList<Job>) -> Vec<u32> {
        list.iter().map(|j| j.id).collect()
    }

    #[test]
    fn push_erase_and_order() {
        let js = jobs(4);
        let list = IntrusiveList::<Job>::new();
        // SAFETY: every element stays in `js`, which outlives the list, and is linked once.
        unsafe {
            list.push_back(&js[1]);
            list.push_back(&js[2]);
            list.push_front(&js[0]);
            list.push_back(&js[3]);
        }
        assert_eq!(ids(&list), vec![0, 1, 2, 3]);
        assert_eq!(list.len(), 4);
        // SAFETY: each element is on this list.
        unsafe {
            assert!(list.erase(&js[2]));
            assert!(!list.erase(&js[2]));
        }
        assert_eq!(ids(&list), vec![0, 1, 3]);
        assert!(!js[2].hook.is_linked());
    }

    #[test]
    fn pop_front_and_empty() {
        let js = jobs(2);
        let list = IntrusiveList::<Job>::new();
        assert!(list.front().is_none());
        // SAFETY: elements outlive the list and are linked once.
        unsafe {
            list.push_back(&js[0]);
            list.push_back(&js[1]);
            assert_eq!(list.pop_front().map(|j| j.id), Some(0));
            assert_eq!(list.pop_front().map(|j| j.id), Some(1));
            assert!(list.pop_front().is_none());
        }
        assert!(list.is_empty());
    }

    #[test]
    fn randomized_matches_a_deque_model() {
        let js = jobs(48);
        let list = IntrusiveList::<Job>::new();
        let mut model: VecDeque<u32> = VecDeque::new();
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..5000 {
            let id = (rnd() % js.len() as u64) as u32;
            let item = &js[id as usize];
            let linked = item.hook.is_linked();
            match rnd() % 4 {
                0 if !linked => {
                    // SAFETY: the element is unlinked and lives in `js`.
                    unsafe { list.push_back(item) };
                    model.push_back(id);
                }
                1 if !linked => {
                    // SAFETY: as above.
                    unsafe { list.push_front(item) };
                    model.push_front(id);
                }
                2 if linked => {
                    // SAFETY: the element is on `list`.
                    assert!(unsafe { list.erase(item) });
                    model.retain(|&x| x != id);
                }
                _ => {
                    // SAFETY: the popped element stays alive in `js`.
                    let popped = unsafe { list.pop_front() }.map(|j| j.id);
                    assert_eq!(popped, model.pop_front());
                }
            }
            assert_eq!(list.len(), model.len());
        }
        assert_eq!(ids(&list), model.iter().copied().collect::<Vec<_>>());
    }
}
