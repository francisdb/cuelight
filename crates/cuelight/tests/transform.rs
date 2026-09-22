//! Rotation, uneven scale and inherited group transforms in the draw list.

use cuelight::{Engine, ResolvedShape, Transform};

fn engine(layers: &str) -> Engine {
    let mut engine = Engine::new();
    engine
        .load_show(&format!(
            r#"{{ "name": "transform", "size": [100, 100], "variables": {{ "angle": 0 }}, "layers": [{layers}] }}"#
        ))
        .unwrap();
    engine
}

const BOX: &str = r##""type": "shape", "shape": { "rect": [0, 0, 10, 4] }, "fill": "#FFFFFF""##;

fn near(a: [f64; 2], b: [f64; 2]) {
    assert!(
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9,
        "{a:?} vs {b:?}"
    );
}

#[test]
fn plain_placement_is_baked_into_coordinates() {
    let engine = engine(&format!(
        r#"{{ "name": "g", "type": "group", "x": 10, "y": 20, "scale": 2, "children": [
              {{ "name": "box", {BOX}, "x": 5, "scale": 0.5 }} ] }}"#
    ));
    let item = &engine.resolved_layers().unwrap()[0];
    assert_eq!(item.transform, Transform::IDENTITY);
    // The group's scale now reaches its children: 10 + 5 * 2 = 20, size 10.
    assert_eq!(
        item.shape,
        ResolvedShape::Rect {
            x: 20.0,
            y: 20.0,
            width: 10.0,
            height: 4.0
        }
    );
}

#[test]
fn rotation_keeps_the_shape_local_and_places_it_with_the_transform() {
    let engine = engine(&format!(
        r#"{{ "name": "box", {BOX}, "x": 50, "y": 50, "rotation": 90 }}"#
    ));
    let item = &engine.resolved_layers().unwrap()[0];
    assert_eq!(
        item.shape,
        ResolvedShape::Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 4.0
        }
    );
    // Clockwise: the far end of the bar, along +x, points down (+y).
    near(item.transform.apply([10.0, 0.0]), [50.0, 60.0]);
    near(item.transform.apply([0.0, 4.0]), [46.0, 50.0]);
}

#[test]
fn rotation_turns_around_the_anchor() {
    let engine = engine(&format!(
        r#"{{ "name": "box", {BOX}, "x": 50, "y": 50, "anchor": "center", "rotation": 180, "scale": 2 }}"#
    ));
    let item = &engine.resolved_layers().unwrap()[0];
    // The center stays put; the corners swap.
    near(item.transform.apply([5.0, 2.0]), [50.0, 50.0]);
    near(item.transform.apply([0.0, 0.0]), [60.0, 54.0]);
}

#[test]
fn groups_pass_their_rotation_down() {
    let mut engine = engine(&format!(
        r#"{{ "name": "arm", "type": "group", "x": 50, "y": 50,
              "bindings": [ {{ "property": "rotation", "variable": "angle" }} ],
              "children": [ {{ "name": "tip", {BOX}, "x": 20 }} ] }}"#
    ));
    let tip = |e: &Engine| e.resolved_layers().unwrap()[0].clone();
    assert_eq!(tip(&engine).transform, Transform::IDENTITY);
    assert_eq!(
        tip(&engine).shape,
        ResolvedShape::Rect {
            x: 70.0,
            y: 50.0,
            width: 10.0,
            height: 4.0
        }
    );
    engine.set_variable("angle", 90.0);
    let item = tip(&engine);
    near(item.transform.apply([0.0, 0.0]), [50.0, 70.0]);
}

#[test]
fn uneven_scale_uses_the_transform() {
    let engine = engine(&format!(
        r#"{{ "name": "box", {BOX}, "x": 10, "scale_x": 2, "scale_y": -1 }}"#
    ));
    let item = &engine.resolved_layers().unwrap()[0];
    assert_ne!(item.transform, Transform::IDENTITY);
    near(item.transform.apply([10.0, 4.0]), [30.0, -4.0]);
}

#[test]
fn rotation_is_animatable() {
    let mut engine = engine(&format!(
        r#"{{ "name": "box", {BOX}, "x": 50, "y": 50, "timelines": [ {{ "name": "spin", "autoplay": true,
              "tracks": [ {{ "property": "rotation", "keys": [ {{ "t": 0, "v": 0 }}, {{ "t": 1, "v": 360 }} ] }} ] }} ] }}"#
    ));
    engine.advance_frame(0.25);
    let item = &engine.resolved_layers().unwrap()[0];
    near(item.transform.apply([10.0, 0.0]), [50.0, 60.0]);
}

#[test]
fn a_clip_is_placed_by_the_transform_too() {
    let engine = engine(&format!(
        r#"{{ "name": "window", "type": "group", "x": 50, "y": 50, "rotation": 45,
              "clip": {{ "rect": [0, 0, 10, 10] }},
              "children": [ {{ "name": "box", {BOX} }} ] }}"#
    ));
    let layers = engine.resolved_layers().unwrap();
    assert!(matches!(layers[0].shape, ResolvedShape::ClipBegin { .. }));
    assert_ne!(layers[0].transform, Transform::IDENTITY);
    assert_eq!(layers[0].transform, layers[1].transform);
}

#[test]
fn transform_algebra() {
    let t = Transform::translate(10.0, 0.0).then(Transform::rotate(90.0));
    near(t.apply([1.0, 0.0]), [10.0, 1.0]);
    assert_eq!(
        Transform::translate(3.0, 4.0)
            .then(Transform::scale(2.0, 2.0))
            .plain(),
        Some((2.0, 3.0, 4.0))
    );
    assert_eq!(Transform::rotate(10.0).plain(), None);
    assert_eq!(Transform::scale(1.0, 2.0).plain(), None);
}

#[test]
fn paths_ride_the_transform_too() {
    let engine = engine(
        r##"{ "name": "tri", "type": "shape", "shape": { "path": "M0 0 L10 0 L0 4 Z" }, "fill": "#FFFFFF",
              "x": 50, "y": 50, "rotation": 90 }"##,
    );
    let item = &engine.resolved_layers().unwrap()[0];
    // Local geometry, placed by the transform: the tip along +x points down.
    match &item.shape {
        ResolvedShape::Path { elements, .. } => assert_eq!(elements.len(), 4),
        other => panic!("{other:?}"),
    }
    near(item.transform.apply([10.0, 0.0]), [50.0, 60.0]);
}
