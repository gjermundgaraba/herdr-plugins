use herdr_micro::config::parse_controls;
use serde_json::json;

fn controls(button: serde_json::Value) -> serde_json::Value {
    json!({
        "version": 1,
        "buttons": { "1": button },
        "dial": { "clockwise": null, "counterclockwise": null, "press": null },
        "joystick": { "engageDistance": 0.75, "releaseDistance": 0.3 }
    })
}

#[test]
fn config_rejects_extra_gesture_fields() {
    assert!(parse_controls(&controls(json!({
        "tap": { "action": "submit" },
        "unexpected": true
    })))
    .is_err());
}
