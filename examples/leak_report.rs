//! Leak report by tag: a tracking decorator records live blocks, and the summary groups them.
//!
//! Run with: cargo run --example leak_report

use qs_memory::{Allocator, SystemAllocator, TagSummary, Tracking};

fn main() {
    let sys = SystemAllocator::new();
    let tracked = Tracking::new(&sys, 256).expect("tracker");

    let texture = tracked.allocate_tagged(4096, 64, "textures").unwrap();
    let mesh = tracked.allocate_tagged(1024, 16, "meshes").unwrap();
    let temp = tracked.allocate_tagged(128, 16, "temporary").unwrap();
    let mesh2 = tracked.allocate_tagged(2048, 16, "meshes").unwrap();

    // SAFETY: released once, through the decorator that issued it.
    unsafe { tracked.deallocate(temp).unwrap() };
    // `texture` and `mesh2` stay live on purpose so the report has something to show.

    let mut summary = [TagSummary { tag: "", count: 0, bytes: 0 }; 8];
    let groups = tracked.summarize(&mut summary);
    println!("live blocks: {}, live bytes: {}", tracked.live_count(), tracked.live_bytes());
    for s in &summary[..groups] {
        println!("  tag {:<10} {} block(s), {} bytes", s.tag, s.count, s.bytes);
    }

    // SAFETY: remaining blocks are live and released once each.
    unsafe {
        tracked.deallocate(mesh).unwrap();
        tracked.deallocate(texture).unwrap();
        tracked.deallocate(mesh2).unwrap();
    }
}
