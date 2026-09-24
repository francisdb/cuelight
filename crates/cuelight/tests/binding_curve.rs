//! A binding that bends its value against the input.

use cuelight::{Engine, Error, ResolvedShape};

fn show(binding: &str) -> String {
    format!(
        r##"{{ "format": 1, "name": "c", "size": [200, 60],
               "variables": {{ "n": 0 }},
               "layers": [{{ "name": "box", "type": "shape",
                 "shape": {{ "rect": [0,0,4,4] }}, "fill": "#FFFFFF", "x": 0,
                 "bindings": [{binding}] }}] }}"##
    )
}

fn x_at(show: &str, n: f64) -> f64 {
    let mut engine = Engine::new();
    engine.load_show(show).unwrap();
    engine.set_variable("n", n);
    engine.advance_to(0.0);
    match engine.resolved_layers().unwrap()[0].shape {
        ResolvedShape::Rect { x, .. } => x,
        _ => panic!("box should be a rect"),
    }
}

#[test]
fn a_curve_bends_the_value_against_the_input() {
    // Flat for the first half of the input, then steep.
    let s = show(
        r#"{ "property": "x", "variable": "n", "curve": [
             { "t": 0, "v": 0 }, { "t": 0.5, "v": 10 }, { "t": 1, "v": 100 } ] }"#,
    );
    assert_eq!(x_at(&s, 0.0), 0.0);
    assert_eq!(x_at(&s, 0.25), 5.0);
    assert_eq!(x_at(&s, 0.5), 10.0);
    assert_eq!(x_at(&s, 0.75), 55.0);
    assert_eq!(x_at(&s, 1.0), 100.0);
}

#[test]
fn a_curve_holds_its_ends() {
    let s = show(
        r#"{ "property": "x", "variable": "n", "curve": [
             { "t": 10, "v": 3 }, { "t": 20, "v": 7 } ] }"#,
    );
    assert_eq!(x_at(&s, -100.0), 3.0);
    assert_eq!(x_at(&s, 15.0), 5.0);
    assert_eq!(x_at(&s, 1000.0), 7.0);
}

#[test]
fn a_curve_takes_the_eases_a_track_takes() {
    let s = show(
        r#"{ "property": "x", "variable": "n", "curve": [
             { "t": 0, "v": 0 }, { "t": 1, "v": 100, "ease": "quad_in" } ] }"#,
    );
    // quad_in at the half way point is a quarter of the way.
    assert_eq!(x_at(&s, 0.5), 25.0);
}

#[test]
fn scale_and_offset_still_change_the_unit_afterwards() {
    let s = show(
        r#"{ "property": "x", "variable": "n", "scale": 2, "offset": 1,
             "curve": [ { "t": 0, "v": 0 }, { "t": 1, "v": 10 } ] }"#,
    );
    assert_eq!(x_at(&s, 0.5), 11.0);
}

#[test]
fn a_curve_and_a_threshold_are_the_same_job() {
    let s = show(
        r#"{ "property": "x", "variable": "n", "threshold": 0.5,
             "curve": [ { "t": 0, "v": 0 }, { "t": 1, "v": 10 } ] }"#,
    );
    let mut engine = Engine::new();
    match engine.load_show(&s) {
        Err(Error::InvalidShow(why)) => assert!(why.contains("same job"), "{why}"),
        other => panic!("should be refused, got {other:?}"),
    }
}

#[test]
fn a_curve_out_of_order_is_refused() {
    let s = show(
        r#"{ "property": "x", "variable": "n", "curve": [
             { "t": 1, "v": 0 }, { "t": 0, "v": 10 } ] }"#,
    );
    assert!(Engine::new().load_show(&s).is_err());
}

#[test]
fn a_curve_where_no_number_lands_is_reported() {
    let s = r##"{ "format": 1, "name": "c", "size": [8, 8],
          "variables": { "n": "#FF0000" },
          "layers": [{ "name": "pic", "type": "image", "image": "none",
            "bindings": [{ "property": "tint", "variable": "n",
              "curve": [ { "t": 0, "v": 0 }, { "t": 1, "v": 1 } ] }] }] }"##;
    let mut engine = Engine::new();
    engine.load_show(s).unwrap();
    let warnings = engine.load_warnings();
    assert!(
        warnings.iter().any(|w| w.contains("only bends a number")),
        "{warnings:?}"
    );
}
