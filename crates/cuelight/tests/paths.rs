//! Path shapes, strokes and vector artwork in the draw list, no GPU.

use cuelight::{Engine, PathElement, ResolvedShape, Vector, VectorPath};

fn show(layers: &str) -> String {
    format!(r#"{{ "name": "paths", "size": [64, 32], "layers": [{layers}] }}"#)
}

fn resolved(engine: &Engine) -> Vec<cuelight::ResolvedLayer> {
    engine.resolved_layers().unwrap()
}

#[test]
fn a_path_shape_resolves_into_canvas_coordinates() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "tri", "type": "shape", "x": 10, "y": 5, "scale": 2,
                 "shape": { "path": "M0 0 l4 0 L2 3z" }, "fill": "#FF0000" }"##,
        ))
        .unwrap();
    let layers = resolved(&engine);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].color, [255, 0, 0, 255]);
    let ResolvedShape::Path { elements, stroke } = &layers[0].shape else {
        panic!("{:?}", layers[0].shape);
    };
    assert_eq!(stroke, &None);
    assert_eq!(
        elements.as_slice(),
        &[
            PathElement::MoveTo([10.0, 5.0]),
            PathElement::LineTo([18.0, 5.0]),
            PathElement::LineTo([14.0, 11.0]),
            PathElement::Close,
        ]
    );
}

#[test]
fn bad_path_data_is_rejected_at_load() {
    let mut engine = Engine::new();
    let err = engine
        .load_show(&show(
            r##"{ "name": "bad", "type": "shape", "shape": { "path": "M 1 2 X" }, "fill": "#FF0000" }"##,
        ))
        .unwrap_err();
    assert!(err.to_string().contains("path data"), "{err}");
}

#[test]
fn a_stroked_rect_becomes_a_path_with_a_scaled_stroke() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "box", "type": "shape", "scale": 2,
                 "shape": { "rect": [1, 1, 4, 2] }, "fill": "#00000000",
                 "stroke": { "color": "#00FF00", "width": 1.5 } }"##,
        ))
        .unwrap();
    let layers = resolved(&engine);
    let ResolvedShape::Path { elements, stroke } = &layers[0].shape else {
        panic!("{:?}", layers[0].shape);
    };
    assert_eq!(stroke, &Some(([0, 255, 0, 255], 3.0)));
    assert_eq!(elements[0], PathElement::MoveTo([2.0, 2.0]));
    assert_eq!(elements[2], PathElement::LineTo([10.0, 6.0]));
    assert_eq!(elements[4], PathElement::Close);
    // Without a stroke it stays a rect.
    engine
        .load_show(&show(
            r##"{ "name": "box", "type": "shape", "shape": { "rect": [1, 1, 4, 2] }, "fill": "#00FF00" }"##,
        ))
        .unwrap();
    assert!(matches!(
        resolved(&engine)[0].shape,
        ResolvedShape::Rect { .. }
    ));
}

#[test]
fn a_stroked_circle_is_four_arcs() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "ring", "type": "shape", "shape": { "circle": [8, 8, 4] },
                 "fill": "#00000000", "stroke": { "color": "#FFFFFF" } }"##,
        ))
        .unwrap();
    let layers = resolved(&engine);
    let ResolvedShape::Path { elements, stroke } = &layers[0].shape else {
        panic!("{:?}", layers[0].shape);
    };
    assert_eq!(stroke, &Some(([255, 255, 255, 255], 1.0)));
    assert_eq!(elements[0], PathElement::MoveTo([12.0, 8.0]));
    assert!(matches!(
        elements[2],
        PathElement::CubicTo(_, _, [4.0, 8.0])
    ));
    assert_eq!(elements.len(), 6);
}

#[test]
fn rejects_a_bad_stroke() {
    let mut engine = Engine::new();
    let err = engine
        .load_show(&show(
            r##"{ "name": "box", "type": "shape", "shape": { "rect": [0, 0, 4, 2] }, "fill": "#FFFFFF",
                 "stroke": { "color": "#FFFFFF", "width": 0 } }"##,
        ))
        .unwrap_err();
    assert!(err.to_string().contains("stroke width"), "{err}");
    assert!(engine
        .load_show(&show(
            r##"{ "name": "box", "type": "shape", "shape": { "rect": [0, 0, 4, 2] }, "fill": "#FFFFFF",
                 "stroke": { "color": "green" } }"##,
        ))
        .is_err());
}

#[test]
fn a_path_clips_a_group() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "mask", "type": "group", "x": 2, "clip": { "path": "M0 0 H8 V8 Z" }, "children": [
                 { "name": "fill", "type": "shape", "shape": { "rect": [0, 0, 8, 8] }, "fill": "#FFFFFF" } ] }"##,
        ))
        .unwrap();
    let layers = resolved(&engine);
    let ResolvedShape::ClipBegin { shape } = &layers[0].shape else {
        panic!("{:?}", layers[0].shape);
    };
    let ResolvedShape::Path { elements, .. } = shape.as_ref() else {
        panic!("{shape:?}");
    };
    assert_eq!(elements[1], PathElement::LineTo([10.0, 0.0]));
    assert_eq!(layers[2].shape, ResolvedShape::ClipEnd);
}

#[test]
fn a_path_anchors_by_its_bounds() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "tri", "type": "shape", "x": 32, "y": 16, "anchor": "center",
                 "shape": { "path": "M0 0 L8 0 L4 4 Z" }, "fill": "#FFFFFF" }"##,
        ))
        .unwrap();
    let ResolvedShape::Path { elements, .. } = &resolved(&engine)[0].shape else {
        panic!()
    };
    // The 8x4 box is centered on 32,16.
    assert_eq!(elements[0], PathElement::MoveTo([28.0, 14.0]));
}

fn logo() -> Vector {
    Vector {
        width: 10.0,
        height: 5.0,
        paths: vec![
            VectorPath {
                elements: vec![
                    PathElement::MoveTo([0.0, 0.0]),
                    PathElement::LineTo([10.0, 0.0]),
                    PathElement::LineTo([10.0, 5.0]),
                    PathElement::Close,
                ],
                fill: Some([0, 0, 255, 255]),
                stroke: None,
            },
            VectorPath {
                elements: vec![
                    PathElement::MoveTo([0.0, 5.0]),
                    PathElement::LineTo([10.0, 5.0]),
                ],
                fill: None,
                stroke: Some(([255, 0, 0, 255], 1.0)),
            },
        ],
    }
}

#[test]
fn a_vector_layer_draws_its_paths_scaled_into_size() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r#"{ "name": "logo", "type": "vector", "vector": "logo", "x": 4, "y": 2, "size": [20, 20] }"#,
        ))
        .unwrap();
    // Not registered yet: nothing draws, like a missing image.
    assert!(resolved(&engine).is_empty());
    engine.set_vector("logo", logo()).unwrap();
    let layers = resolved(&engine);
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].color, [0, 0, 255, 255]);
    let ResolvedShape::Path { elements, stroke } = &layers[0].shape else {
        panic!()
    };
    assert_eq!(stroke, &None);
    // 10x5 into 20x20: x times 2, y times 4.
    assert_eq!(elements[2], PathElement::LineTo([24.0, 22.0]));
    // An outline-only path: no fill, a stroke scaled by the mean factor.
    assert_eq!(layers[1].color, [0; 4]);
    let ResolvedShape::Path { stroke, .. } = &layers[1].shape else {
        panic!()
    };
    assert_eq!(stroke, &Some(([255, 0, 0, 255], 3.0)));
}

#[test]
fn a_vector_layer_at_natural_size_with_an_anchor() {
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "logo", "type": "vector", "vector": "logo", "x": 32, "y": 16, "anchor": "center" }"#,
        ))
        .unwrap();
    let ResolvedShape::Path { elements, .. } = &resolved(&engine)[0].shape else {
        panic!()
    };
    assert_eq!(elements[0], PathElement::MoveTo([27.0, 13.5]));
    assert!(Engine::new()
        .set_vector(
            "flat",
            Vector {
                width: 0.0,
                height: 1.0,
                paths: vec![]
            }
        )
        .is_err());
}
