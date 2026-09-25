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
fn artwork_is_one_layer_kind_whichever_the_asset_is() {
    // The same layer draws pixels or paths, and an older show writing
    // the kind and the name as `vector` still loads.
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "logo", "type": "image", "image": "logo", "x": 4, "y": 2 }"#,
        ))
        .unwrap();
    assert_eq!(resolved(&engine).len(), 2, "the artwork's two paths");
    assert!(engine.load_warnings().is_empty());

    engine
        .load_show(&show(
            r#"{ "name": "logo", "type": "vector", "vector": "logo", "x": 4, "y": 2 }"#,
        ))
        .unwrap();
    assert_eq!(resolved(&engine).len(), 2, "the old spelling of the same");
}

#[test]
fn a_layer_after_artwork_keeps_its_timelines() {
    // Every layer is found by its path in the tree, and a layer that
    // takes a shortcut through the walk leaves that path a step too
    // long: everything after it then looks its timelines up somewhere
    // that does not exist, and quietly stops moving.
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r##"{ "name": "art", "type": "image", "image": "logo" },
                { "name": "mover", "type": "shape", "x": 0,
                  "shape": { "rect": [0, 0, 4, 4] }, "fill": "#FFFFFF",
                  "timelines": [{ "name": "slide", "autoplay": true,
                    "tracks": [{ "property": "x",
                                 "keys": [{ "t": 0, "v": 0 }, { "t": 1, "v": 20 }] }] }] }"##,
        ))
        .unwrap();
    engine.advance_to(0.5);
    let moved = resolved(&engine)
        .into_iter()
        .find(|l| l.name == "mover")
        .map(|l| match l.shape {
            ResolvedShape::Rect { x, .. } => x,
            other => panic!("{other:?}"),
        });
    assert_eq!(moved, Some(10.0), "the timeline after the artwork ran");
}

#[test]
fn the_older_spelling_of_an_artwork_layer_is_not_a_field_nobody_read() {
    // A show writing `vector` is read, so it should not be told the
    // field was dropped: the model keeps it under the name it shares
    // with pixels.
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "art", "type": "vector", "vector": "logo" }"#,
        ))
        .unwrap();
    assert!(
        engine.load_warnings().is_empty(),
        "{:?}",
        engine.load_warnings()
    );
    // A field nothing reads is still reported.
    engine
        .load_show(&show(
            r#"{ "name": "art", "type": "vector", "vector": "logo", "vectors": "logo" }"#,
        ))
        .unwrap();
    assert_eq!(
        engine.load_warnings().len(),
        1,
        "{:?}",
        engine.load_warnings()
    );
}

#[test]
fn a_tint_stains_vector_artwork_as_it_does_pixels() {
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r##"{ "name": "logo", "type": "image", "image": "logo", "tint": "#808000" }"##,
        ))
        .unwrap();
    let layers = resolved(&engine);
    // Blue through a tint with no blue in it: gone. The stroke is
    // stained the same way.
    assert_eq!(layers[0].color, [0, 0, 0, 255]);
    let ResolvedShape::Path { stroke, .. } = &layers[1].shape else {
        panic!()
    };
    assert_eq!(stroke, &Some(([128, 0, 0, 255], 1.0)));
}

#[test]
fn vector_artwork_tiles_across_its_box_and_stops_at_it() {
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "wall", "type": "image", "image": "logo", "x": 0, "y": 0,
                 "size": [30, 10], "repeat": { "size": [10, 5] } }"#,
        ))
        .unwrap();
    let layers = resolved(&engine);
    // Three across and two down, each the artwork's two paths, inside a
    // clip of the layer's box.
    assert!(
        matches!(&layers[0].shape, ResolvedShape::ClipBegin { shape }
            if matches!(**shape, ResolvedShape::Rect { width: 30.0, height: 10.0, .. })),
        "{:?}",
        layers[0].shape
    );
    assert!(matches!(
        layers[layers.len() - 1].shape,
        ResolvedShape::ClipEnd
    ));
    assert_eq!(layers.len() - 2, 3 * 2 * 2);
    // The second tile starts one tile across.
    let corner = |layer: &cuelight::ResolvedLayer| match &layer.shape {
        ResolvedShape::Path { elements, .. } => match elements[0] {
            PathElement::MoveTo(at) => at,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert_eq!(corner(&layers[1]), [0.0, 0.0]);
    assert_eq!(corner(&layers[3]), [10.0, 0.0]);
    assert_eq!(corner(&layers[7]), [0.0, 5.0]);
}

#[test]
fn a_sheet_on_vector_artwork_is_reported() {
    let mut engine = Engine::new();
    engine.set_vector("logo", logo()).unwrap();
    engine
        .load_show(&show(
            r#"{ "name": "logo", "type": "image", "image": "logo",
                 "sheet": { "cell": [4, 4], "columns": 2 } }"#,
        ))
        .unwrap();
    let warnings = engine.load_warnings().join("\n");
    assert!(warnings.contains("sheet of cells"), "{warnings}");
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

#[test]
fn a_rect_can_have_rounded_corners() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "card", "type": "shape",
                  "shape": { "rect": [0, 0, 40, 20], "radius": 5 },
                  "fill": "#FF0000" }"##,
        ))
        .unwrap();
    let layers = engine.resolved_layers().unwrap();
    // A radius turns the rect into a path: four lines and four corners.
    let ResolvedShape::Path { elements, .. } = &layers[0].shape else {
        panic!(
            "a rounded rect should resolve to a path, got {:?}",
            layers[0].shape
        )
    };
    let cubics = elements
        .iter()
        .filter(|e| matches!(e, PathElement::CubicTo(..)))
        .count();
    assert_eq!(cubics, 4, "{elements:?}");
}

#[test]
fn a_rect_without_a_radius_is_still_a_rect() {
    let mut engine = Engine::new();
    engine
        .load_show(&show(
            r##"{ "name": "plain", "type": "shape",
                  "shape": { "rect": [0, 0, 40, 20] }, "fill": "#FF0000" }"##,
        ))
        .unwrap();
    let layers = engine.resolved_layers().unwrap();
    assert!(
        matches!(layers[0].shape, ResolvedShape::Rect { .. }),
        "{:?}",
        layers[0].shape
    );
}

#[test]
fn a_radius_is_clamped_to_half_the_shorter_side() {
    // 500 on a 40x20 rect is a pill: the corners meet in the middle.
    assert_eq!(
        cuelight::Shape::corner_radius([0.0, 0.0, 40.0, 20.0], Some(500.0)),
        10.0
    );
    assert_eq!(
        cuelight::Shape::corner_radius([0.0, 0.0, 40.0, 20.0], Some(-3.0)),
        0.0
    );
    assert_eq!(
        cuelight::Shape::corner_radius([0.0, 0.0, 40.0, 20.0], None),
        0.0
    );
}

#[test]
fn a_shape_that_names_nothing_says_so() {
    let mut engine = Engine::new();
    let err = engine
        .load_show(&show(
            r##"{ "name": "odd", "type": "shape", "shape": { "radius": 2 },
                  "fill": "#FF0000" }"##,
        ))
        .unwrap_err();
    assert!(err.to_string().contains("rect, circle or path"), "{err}");
}
