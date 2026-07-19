use tellur::core::timeline_container::{TimeBox, Timeline};

#[tellur::core::component(timeline)]
fn Main() -> impl tellur::core::timeline_component::TimelineComponent {
    Timeline::builder()
        .child(TimeBox::builder().duration(1.5))
        .build()
}

tellur::export_timeline!(root = Main::builder().build(), title = "Facade Timeline",);

#[test]
fn facade_exports_a_function_form_timeline_root() {
    let collection = tellur_timeline_collection_v9();
    let timelines = collection.timelines();

    assert_eq!(timelines.len(), 1);
    assert_eq!(timelines[0].id, "main");
    assert_eq!(timelines[0].title, "Facade Timeline");
    assert_eq!(timelines[0].duration, 1.5);
}
