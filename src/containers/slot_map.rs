//! Generational slot map: values live in slots, and a slot's generation changes every time its value is
//! removed. A handle remembers the generation it was issued with, so a stale handle reads as absent
//! instead of silently aliasing a newer value.
//!
//! Storage is ordinary `Vec`s on the global heap. Callers who need the values themselves in a chosen
//! allocator can store `OwnedPtr`s or allocator-backed containers in the slots.

/// A reference to a value in a [`SlotMap`]. Copyable; stale after the value is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    index: u32,
    generation: u32,
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A map from [`Handle`]s to values, with slot reuse and generation checks.
pub struct SlotMap<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
}

impl<T> SlotMap<T> {
    /// An empty map.
    pub fn new() -> Self {
        SlotMap { slots: Vec::new(), free: Vec::new(), len: 0 }
    }

    /// Stores `value` and returns its handle. Reuses a released slot when one exists.
    pub fn insert(&mut self, value: T) -> Handle {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            Handle { index, generation: slot.generation }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot { generation: 0, value: Some(value) });
            Handle { index, generation: 0 }
        }
    }

    /// The value for `handle`, or `None` if it was removed.
    pub fn get(&self, handle: Handle) -> Option<&T> {
        let slot = self.slots.get(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.value.as_ref()
    }

    /// Mutable access to the value for `handle`.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Removes and returns the value for `handle`. Later uses of the same handle return `None`.
    pub fn remove(&mut self, handle: Handle) -> Option<T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(handle.index);
        self.len -= 1;
        Some(value)
    }

    /// True when `handle` refers to a live value.
    pub fn contains(&self, handle: Handle) -> bool {
        self.get(handle).is_some()
    }

    /// Number of live values.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no values are stored.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Every live value with its handle, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.value.as_ref().map(|v| (Handle { index: i as u32, generation: slot.generation }, v))
        })
    }
}

impl<T> Default for SlotMap<T> {
    fn default() -> Self {
        Self::new()
    }
}
