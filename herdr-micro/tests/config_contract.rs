use herdr_micro::config::parse_controls;
use serde_json::json;

fn controls(button: serde_json::Value) -> serde_json::Value {
    json!({
        "version": 1,
        "buttons": { "1": button },
        "agentMacosKeys": {"1":"F13","2":"F14","3":"F15","4":"F16","5":"F17","6":"F18"},
        "actionDeviceKeys": {"1":"F20","2":"F21","3":"F22","4":"F23","5":"F19","6":null,"7":"F24"},
        "actionMacosKeys": {"1":null,"2":null,"3":null,"4":null,"5":"F19","6":null,"7":null},
        "dial": { "clockwise": null, "counterclockwise": null, "press": null },
        "joystick": { "engageDistance": 0.75, "releaseDistance": 0.3 }
    })
}

#[test]
fn config_rejects_extra_gesture_fields() {
    assert!(
        parse_controls(&controls(json!({
            "tap": { "action": "submit" },
            "unexpected": true
        })))
        .is_err()
    );
}

#[test]
fn controls_expose_seven_physical_buttons() {
    let mut config = controls(serde_json::Value::Null);
    config["buttons"]["7"] = serde_json::Value::Null;
    assert!(parse_controls(&config).is_ok());
    config["buttons"]["8"] = serde_json::Value::Null;
    assert!(parse_controls(&config).is_err());
}
