//! Cross-process shared memory: the parent creates a named arena and runs this same test binary as a child.
//! The child opens the arena by name and allocates from it; the parent then sees the child's reservation.

use qs_memory::{Allocator, SharedArena};

const CHILD_ENV: &str = "QS_SHARED_CHILD_ARENA";

#[test]
fn shared_arena_is_shared_across_processes() {
    if let Ok(name) = std::env::var(CHILD_ENV) {
        // Child process: join the arena the parent created and reserve memory in it.
        let arena = SharedArena::open(&name, 4096).expect("child opens the named arena");
        arena.allocate(100, 16).expect("child allocates from the shared arena");
        return;
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("qs_xproc_{}_{nanos}", std::process::id());
    let arena = SharedArena::create(&name, 4096).expect("parent creates the named arena");
    assert_eq!(arena.used(), 0);

    let exe = std::env::current_exe().expect("test binary path");
    let status = std::process::Command::new(exe)
        .args(["--exact", "shared_process_api::shared_arena_is_shared_across_processes", "--test-threads=1"])
        .env(CHILD_ENV, &name)
        .status()
        .expect("child process starts");
    assert!(status.success(), "child process failed");
    assert!(arena.used() >= 100, "the child's reservation is visible to the parent");
}
