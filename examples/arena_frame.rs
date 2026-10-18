//! Per-frame scratch memory: an arena for temporaries, rolled back at the end of each frame.
//!
//! Run with: cargo run --example arena_frame

use qs_memory::{Allocator, Arena, ArenaScope};

fn main() {
    let frame = Arena::new(64 * 1024).expect("arena");
    for n in 0..3u8 {
        let _scope = ArenaScope::new(&frame); // everything below is reclaimed at the end of the frame
        let _names = frame.allocate(256, 16).expect("names");
        let scratch = frame.allocate(1024, 64).expect("scratch");
        // SAFETY: the block is live, large enough, and exclusively used in this frame.
        unsafe { scratch.as_mut_slice().fill(n) };
        println!("frame {n}: {} of {} bytes in use inside the scope", frame.used(), frame.capacity());
    }
    println!("after all frames: {} bytes in use", frame.used());
    assert_eq!(frame.used(), 0);
}
