#![cfg(not(target_family = "wasm"))]

use std::thread;

use sonic_rs::{from_slice, from_str, Value};

#[test]
fn native_value_retains_upstream_depth_limit() {
    // This covers the native parser/DOM path, not a foreign Serde visitor's depth policy.
    // Match upstream's depth fixtures: debug object parsing needs about 19 MiB
    // of stack at the supported boundary, unlike optimized production builds.
    thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for (open, close) in [("[", "]"), (r#"{"a":"#, "}")] {
                let shallow = format!("{}0{}", open.repeat(8), close.repeat(8));
                assert!(from_str::<Value>(&shallow).is_ok());
                assert!(from_slice::<Value>(shallow.as_bytes()).is_ok());

                // Fail at the boundary before testing extreme depth on an unfixed parser.
                for depth in [256, 4096] {
                    let input = format!("{}0{}", open.repeat(depth), close.repeat(depth));
                    assert!(
                        from_str::<Value>(&input).is_err(),
                        "native Value parsing must reject nesting depth {depth}"
                    );
                    assert!(from_slice::<Value>(input.as_bytes()).is_err());
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
