//! `femux graphics list` (plugin `graphics`, REQ-GFX-001/004) against the real binary.

mod common;

use std::time::Duration;

use common::{Env, wait_until};

#[test]
fn graphics_list_shows_the_images_a_pane_holds() {
    let env = Env::new();
    env.run(&["daemon"]);
    env.run(&["new", "s"]);
    let pane = env.json(&["agent", "list", "--all"])["agents"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        env.json(&["graphics", "list"])["images"],
        serde_json::json!([])
    );
    env.run(&[
        "agent",
        "prompt",
        &pane,
        "printf '\\033_Ga=T,f=24,s=2,v=2,i=7,c=4,r=2;/wAA/wAA/wAA/wAA\\033\\\\'",
    ]);
    wait_until("an image", Duration::from_secs(10), || {
        env.json(&["graphics", "list"])["images"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    });
    let v = env.json(&["graphics", "list", &pane]);
    let img = &v["images"][0];
    assert_eq!(img["image_id"], 7);
    assert_eq!(img["bytes"], 12);
    assert_eq!(img["format"], "rgb");
    assert_eq!(img["visible"], true);
    let text = env.run(&["graphics", "list"]);
    assert!(
        text.contains("1 images, 12B") && text.contains("2x2 rgb"),
        "{text}"
    );
}
