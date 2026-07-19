use tellur_core::timeline_container::TimeBox;

tellur_plugin::export_timeline!(
    root = TimeBox::builder().duration(3.0).build(),
    title = "Preview Timeline",
    id = "preview",
    canvas = (640.0, 360.0),
);

#[test]
fn export_timeline_accepts_id_and_canvas_overrides() {
    let collection = tellur_timeline_collection_v9();
    let timelines = collection.timelines();

    assert_eq!(timelines.len(), 1);
    assert_eq!(timelines[0].id, "preview");
    assert_eq!(timelines[0].title, "Preview Timeline");
    assert_eq!(timelines[0].duration, 3.0);
}
