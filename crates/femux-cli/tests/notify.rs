//! `femux notify test [event]` (SPEC §11) posts through the daemon's notify relay.

mod common;

use common::Env;

#[test]
fn notify_test_posts_a_notification() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    let v = env.json(&["notify", "test"]);
    assert_eq!(v["event"], "agent_done");
    assert_eq!(v["title"], "femux test: agent_done");
    assert_eq!(
        env.run(&["notify", "test", "agent_error"]).trim(),
        "posted agent_error"
    );
    assert!(
        env.json(&["notify", "test", "agent_sleepy"])["error"]
            .as_str()
            .unwrap()
            .contains("unknown event")
    );
}
