use std::time::Duration;
use seat_core::session::{self, SessionSpec};

#[test]
fn test_session_lifecycle_and_cleanup() {
    let name = "test-seat-ci";
    // Ensure clean state
    let _ = session::stop(name);

    let spec = SessionSpec::new(name);
    let info = session::start(&spec).expect("failed to start session");

    assert_eq!(info.name, name);
    assert!(info.kwin.alive());
    assert!(info.dbus.alive());

    let list = session::list().expect("list sessions");
    assert!(list.iter().any(|s| s.name == name));

    // Stop and verify cleanup
    session::stop(name).expect("failed to stop session");

    std::thread::sleep(Duration::from_millis(500));
    assert!(!info.kwin.alive(), "kwin should be terminated");
    assert!(!info.dbus.alive(), "dbus should be terminated");

    let list_after = session::list().expect("list sessions after stop");
    assert!(!list_after.iter().any(|s| s.name == name));
}
