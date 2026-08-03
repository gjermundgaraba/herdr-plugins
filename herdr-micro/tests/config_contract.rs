use herdr_micro::config::parse_controls;
use serde_json::json;

fn controls(button: serde_json::Value) -> serde_json::Value {
    json!({
        "version": 2,
        "buttons": { "1": button },
        "hidKeys": {},
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

#[test]
fn controls_expose_six_logical_stock_buttons() {
    let mut config = controls(serde_json::Value::Null);
    config["buttons"]["7"] = serde_json::Value::Null;
    assert!(parse_controls(&config).is_err());
}
