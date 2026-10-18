//! Growable bitset stored in allocator-backed words.

use crate::allocators::traits::Allocator;
use crate::containers::Vector;
use crate::foundation::error::AllocResult;

pub struct BitSet<'a, A: Allocator> {
    words: Vector<'a, u64, A>,
}

impl<'a, A: Allocator> BitSet<'a, A> {
    /// An empty set whose storage comes from `alloc`.
    pub fn new(alloc: &'a A) -> AllocResult<Self> {
        Ok(BitSet { words: Vector::new(alloc)? })
    }

    /// A set with room for `bits` bits, all clear.
    pub fn with_bits(alloc: &'a A, bits: usize) -> AllocResult<Self> {
        let mut set = Self::new(alloc)?;
        set.words.reserve(bits.div_ceil(64))?;
        for _ in 0..bits.div_ceil(64) {
            set.words.push(0)?;
        }
        Ok(set)
    }

    /// Number of bits the set can hold without growing.
    pub fn capacity_bits(&self) -> usize {
        self.words.len() * 64
    }

    fn ensure(&mut self, bit: usize) -> AllocResult<()> {
        let need = bit / 64 + 1;
        while self.words.len() < need {
            self.words.push(0)?;
        }
        Ok(())
    }

    /// Sets bit `i`, growing as needed.
    pub fn set(&mut self, i: usize) -> AllocResult<()> {
        self.ensure(i)?;
        self.words.as_mut_slice()[i / 64] |= 1u64 << (i % 64);
        Ok(())
    }

    /// Clears bit `i`. Bits beyond the current capacity are already clear.
    pub fn clear(&mut self, i: usize) {
        if let Some(word) = self.words.as_mut_slice().get_mut(i / 64) {
            *word &= !(1u64 << (i % 64));
        }
    }

    /// True when bit `i` is set.
    pub fn get(&self, i: usize) -> bool {
        self.words.as_slice().get(i / 64).map_or(false, |w| w & (1u64 << (i % 64)) != 0)
    }

    /// Number of set bits.
    pub fn count(&self) -> usize {
        self.words.as_slice().iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Indices of the set bits, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.capacity_bits()).filter(move |&i| self.get(i))
    }
}
